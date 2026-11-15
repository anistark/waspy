//! `logging.config.fileConfig()`, written out where it is called.
//!
//! The file names handler classes, their arguments, and formats, which decide
//! what code the program is, as `dictConfig()`'s dict does. So it is read when
//! the program is compiled (`core::data_files`), parsed as `configparser`
//! parses it (`ini.rs`), and the call becomes the statements CPython 3.13's
//! `fileConfig()` runs: the formatters, `_clearExistingHandlers()`, the
//! handlers in the order of their `keys`, a `MemoryHandler`'s target once
//! every handler is made, the root logger, the named loggers, and what
//! becomes of the loggers the file does not name.
//!
//! The statements start with a check of the file the program finds when it
//! runs: missing, it is CPython's `FileNotFoundError`, and holding any other
//! text than the one compiled, a `RuntimeError`, since the compiled program
//! cannot follow a file that changed. A file CPython would reject is
//! reported when compiling, with its exception.

use super::ini::{py_repr, Ini, Raised};
use super::{
    class_name, classes, const_str, fname, module_call, Formats, HANDLER_CLASSES, LEVELS,
    REFUSED_HANDLERS,
};
use crate::core::data_files::{self, Lookup};
use crate::ir::build::*;
use crate::ir::{IRConstant, IRExpr, IRStatement, IRType};
use anyhow::Result;
use rustpython_parser::ast::{self, Constant, Expr, Stmt, Suite};
use std::collections::HashMap;

/// What the expansion reads of the program around the call.
pub(super) struct Program {
    /// The module constants that are strs, which a call can name its file by.
    pub paths: HashMap<String, String>,
    /// Whether the program names levels of its own (`addLevelName()`), so a
    /// level's name is looked up when the program runs.
    pub names_levels: bool,
    /// Set once a `fileConfig()` call is written out.
    pub configured: std::cell::Cell<bool>,
}

fn raises(e: Raised) -> anyhow::Error {
    refused(format!(
        "logging.config.fileConfig() raises {}: {}",
        e.class, e.message
    ))
}

fn raise(class: &str, message: String) -> anyhow::Error {
    raises(Raised::new(class, message))
}

/// `value.split(",")`, each name stripped.
fn names(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|n| super::ini::strip(n).to_string())
        .collect()
}

/// A value of a handler's `args` or `kwargs`, as `eval()` gives it.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Str(String),
    Int(i32),
    Float(f64),
    Bool(bool),
    None,
    /// `sys.stdout` or `sys.stderr`.
    Stream(&'static str),
}

impl Value {
    fn expr(&self) -> IRExpr {
        match self {
            Value::Str(s) => text(s),
            Value::Int(n) => int(*n),
            Value::Float(f) => float(*f),
            Value::Bool(b) => boolean(*b),
            Value::None => none(),
            Value::Stream(name) => attr(v("sys"), name),
        }
    }
}

/// One value of a display: a constant, a level's name, or a standard stream.
fn value_of(e: &Expr) -> std::result::Result<Value, String> {
    match e {
        Expr::Constant(c) => match &c.value {
            Constant::Str(s) => Ok(Value::Str(s.clone())),
            Constant::Int(n) => n
                .to_string()
                .parse::<i32>()
                .map(Value::Int)
                .map_err(|_| format!("the int {n}, wider than an int here")),
            Constant::Float(f) => Ok(Value::Float(*f)),
            Constant::Bool(b) => Ok(Value::Bool(*b)),
            Constant::None => Ok(Value::None),
            _ => Err("a value that is not a str, a number, a bool, or None".to_string()),
        },
        Expr::UnaryOp(u) => match (&u.op, value_of(&u.operand)?) {
            (ast::UnaryOp::USub, Value::Int(n)) => n
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| "an int wider than an int here".to_string()),
            (ast::UnaryOp::USub, Value::Float(f)) => Ok(Value::Float(-f)),
            (ast::UnaryOp::UAdd, n @ (Value::Int(_) | Value::Float(_))) => Ok(n),
            _ => Err("an operator".to_string()),
        },
        // `eval()` runs in the logging module's namespace, which holds the
        // levels and `sys`.
        Expr::Name(n) => match LEVELS.iter().find(|(l, _)| *l == n.id.as_str()) {
            Some(&(_, level)) => Ok(Value::Int(level)),
            None => Err(format!("the name '{}'", n.id)),
        },
        Expr::Attribute(a) => match (&*a.value, a.attr.as_str()) {
            (Expr::Name(m), "stdout") if m.id.as_str() == "sys" => Ok(Value::Stream("stdout")),
            (Expr::Name(m), "stderr") if m.id.as_str() == "sys" => Ok(Value::Stream("stderr")),
            _ => Err("an attribute other than sys.stdout or sys.stderr".to_string()),
        },
        _ => Err("an expression that is not a constant".to_string()),
    }
}

/// The expression a handler's `args` or `kwargs` holds.
fn parse_expr(source: &str, hand: &str, what: &str) -> Result<Expr> {
    // One expression, as `eval()` takes: a value continued over lines is one
    // only inside its brackets.
    let parsed = <Suite as rustpython_parser::Parse>::parse(source, "<fileConfig>");
    match parsed.ok().as_deref() {
        Some([Stmt::Expr(e)]) => Ok((*e.value).clone()),
        _ => Err(raise(
            "SyntaxError",
            format!(
                "the {what} of handler {} are not a Python expression: {}",
                py_repr(hand),
                py_repr(source)
            ),
        )),
    }
}

fn unsupported(hand: &str, what: &str, source: &str, why: &str) -> anyhow::Error {
    refused(format!(
        "fileConfig()'s handler {} has the {what} {}, which hold {why}: the file is read at \
         compile time, so they are a display of constants here, with sys.stdout or sys.stderr \
         for a StreamHandler's stream",
        py_repr(hand),
        py_repr(source)
    ))
}

/// A handler's `args`: a tuple or a list written out.
fn args_of(source: &str, hand: &str) -> Result<Vec<Value>> {
    let items = match parse_expr(source, hand, "args")? {
        Expr::Tuple(t) => t.elts,
        Expr::List(l) => l.elts,
        _ => {
            return Err(refused(format!(
                "fileConfig()'s handler {} has the args {}, which are not a tuple or a list \
                 written out. Hint: a tuple of one argument takes a comma, as in ('app.log',)",
                py_repr(hand),
                py_repr(source)
            )))
        }
    };
    items
        .iter()
        .map(|e| value_of(e).map_err(|why| unsupported(hand, "args", source, &why)))
        .collect()
}

/// A handler's `kwargs`: a dict written out, its keys strs.
fn kwargs_of(source: &str, hand: &str) -> Result<Vec<(String, Value)>> {
    let Expr::Dict(d) = parse_expr(source, hand, "kwargs")? else {
        return Err(refused(format!(
            "fileConfig()'s handler {} has the kwargs {}, which are not a dict written out",
            py_repr(hand),
            py_repr(source)
        )));
    };
    let mut out: Vec<(String, Value)> = Vec::new();
    for (key, value) in d.keys.iter().zip(&d.values) {
        let name = match key {
            Some(Expr::Constant(c)) => match &c.value {
                Constant::Str(s) => s.clone(),
                _ => return Err(raise("TypeError", "keywords must be strings".to_string())),
            },
            _ => {
                return Err(unsupported(
                    hand,
                    "kwargs",
                    source,
                    "a key that is not a str",
                ))
            }
        };
        let value = value_of(value).map_err(|why| unsupported(hand, "kwargs", source, &why))?;
        // A key written twice keeps its last value, as a dict does.
        out.retain(|(n, _)| *n != name);
        out.push((name, value));
    }
    Ok(out)
}

/// The class a handler's `class` names.
enum Class {
    /// A class of `logging`, or of `logging.handlers`.
    Module(String),
    /// A class of the program, named as `__main__.Class`.
    Program(String),
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// The class `eval(klass, vars(logging))` or `_resolve(klass)` finds.
fn class_of(klass: &str, hand: &str) -> Result<Class> {
    const OF_LOGGING: &[&str] = &["Handler", "StreamHandler", "FileHandler", "NullHandler"];
    let not_one = || {
        refused(format!(
            "fileConfig()'s handler {} has the class {}, which is not one this module can \
             name. Hint: name a class of logging (StreamHandler), of logging.handlers \
             (handlers.RotatingFileHandler), or of the program (__main__.MyHandler)",
            py_repr(hand),
            py_repr(klass)
        ))
    };
    let (path, short) = match klass.rsplit_once('.') {
        Some((path, short)) => (path, short),
        None => ("", klass),
    };
    match path {
        "" | "logging" if OF_LOGGING.contains(&short) => Ok(Class::Module(short.to_string())),
        "handlers" | "logging.handlers" => {
            if let Some((_, why)) = REFUSED_HANDLERS.iter().find(|(h, _)| *h == short) {
                return Err(refused(format!("logging.handlers.{short} {why}")));
            }
            if HANDLER_CLASSES.contains(&short) {
                Ok(Class::Module(short.to_string()))
            } else {
                Err(not_one())
            }
        }
        "__main__" if is_identifier(short) => Ok(Class::Program(short.to_string())),
        // A bare name `logging` does not hold is looked for as a module.
        "" if HANDLER_CLASSES.contains(&short)
            || REFUSED_HANDLERS.iter().any(|(h, _)| *h == short) =>
        {
            Err(raise(
                "ModuleNotFoundError",
                format!("No module named {}", py_repr(short)),
            ))
        }
        _ => Err(not_one()),
    }
}

/// A level as `logging._checkLevel()` takes its name.
fn level_of(name: &str, program: &Program) -> Result<IRExpr> {
    if program.names_levels {
        // A name `addLevelName()` may have given a level, looked up when the
        // program runs.
        return Ok(call(&fname("level_of"), vec![text(name)]));
    }
    match LEVELS.iter().find(|(l, _)| *l == name) {
        Some(&(_, n)) => Ok(int(n)),
        None => Err(raise(
            "ValueError",
            format!("Unknown level: {}", py_repr(name)),
        )),
    }
}

/// `StreamHandler(*args, **kwargs)`: one stream, by position or by name.
fn stream_handler(
    args: &[Value],
    kwargs: &[(String, Value)],
    formats: &mut Formats,
) -> Result<IRExpr> {
    if args.len() > 1 {
        return Err(raise(
            "TypeError",
            format!(
                "StreamHandler.__init__() takes from 1 to 2 positional arguments but {} were \
                 given",
                args.len() + 1
            ),
        ));
    }
    if let Some((name, _)) = kwargs.iter().find(|(n, _)| n != "stream") {
        return Err(raise(
            "TypeError",
            format!("StreamHandler.__init__() got an unexpected keyword argument '{name}'"),
        ));
    }
    if !args.is_empty() && !kwargs.is_empty() {
        return Err(raise(
            "TypeError",
            "StreamHandler.__init__() got multiple values for argument 'stream'".to_string(),
        ));
    }
    let stream = args.first().or(kwargs.first().map(|(_, value)| value));
    module_call(
        "StreamHandler",
        stream.map(Value::expr).into_iter().collect(),
        formats,
    )
}

/// The call that makes a handler, and the type of the local that holds it.
fn handler_call(
    ini: &Ini,
    hand: &str,
    program: &Program,
    formats: &mut Formats,
) -> Result<(IRExpr, IRType)> {
    let section = format!("handler_{hand}");
    let klass = ini.item(&section, "class").map_err(raises)?;
    let class = class_of(&klass, hand)?;
    let args_text = ini
        .get(&section, "args")
        .map_err(raises)?
        .unwrap_or_else(|| "()".to_string());
    let args = args_of(&args_text, hand)?;
    let kwargs_text = ini
        .get(&section, "kwargs")
        .map_err(raises)?
        .unwrap_or_else(|| "{}".to_string());
    let kwargs = kwargs_of(&kwargs_text, hand)?;
    let handler = classes::ty("Handler");
    if matches!(&class, Class::Module(short) if short == "StreamHandler") {
        return Ok((stream_handler(&args, &kwargs, formats)?, handler));
    }
    if args
        .iter()
        .chain(kwargs.iter().map(|(_, value)| value))
        .any(|value| matches!(value, Value::Stream(_)))
    {
        return Err(refused(format!(
            "fileConfig()'s handler {} is given sys.stdout or sys.stderr, which is the stream \
             of a StreamHandler here",
            py_repr(hand)
        )));
    }
    let memory = matches!(&class, Class::Module(short) if short == "MemoryHandler");
    let mut arguments: Vec<IRExpr> = Vec::new();
    for (k, value) in args.iter().enumerate() {
        arguments.push(match value {
            // `MemoryHandler` takes its flush level by name too.
            Value::Str(name) if memory && k == 1 => level_of(name, program)?,
            value => value.expr(),
        });
    }
    for (name, value) in &kwargs {
        let value = match value {
            Value::Str(level) if memory && name == "flushLevel" => level_of(level, program)?,
            value => value.expr(),
        };
        arguments.push(IRExpr::Keyword {
            name: name.clone(),
            value: Box::new(value),
        });
    }
    match class {
        Class::Program(name) => {
            if ini
                .get(&section, "target")
                .map_err(raises)?
                .is_some_and(|t| !t.is_empty())
            {
                return Err(refused(format!(
                    "fileConfig()'s handler {} names a target, which is supported for \
                     handlers.MemoryHandler, not for a class of the program",
                    py_repr(hand)
                )));
            }
            Ok((call(&name, arguments), handler))
        }
        Class::Module(short) => {
            let made = module_call(&short, arguments, formats)?;
            let ty = if memory {
                classes::ty("handlers.MemoryHandler")
            } else {
                handler
            };
            Ok((made, ty))
        }
    }
}

/// The path a call names its file by: a str written out, or a module
/// constant that is one.
fn path_of(e: Option<&IRExpr>, program: &Program) -> Result<String> {
    let path = match e {
        Some(e) => match (const_str(e), e) {
            (Some(path), _) => Some(path.to_string()),
            (None, IRExpr::Variable(name)) => program.paths.get(name).cloned(),
            _ => None,
        },
        None => {
            return Err(raise(
                "TypeError",
                "fileConfig() missing 1 required positional argument: 'fname'".to_string(),
            ))
        }
    };
    path.ok_or_else(|| {
        refused(
            "fileConfig() reads its file at compile time, so the file is named by a str \
             written out in the call or a module constant here (a path computed at run time, \
             an open file, or a ConfigParser is not supported). Hint: use dictConfig() for a \
             configuration the program builds"
                .to_string(),
        )
    })
}

/// `defaults=`: a dict of strs written out.
fn defaults_of(e: Option<&IRExpr>) -> Result<Vec<(String, String)>> {
    let not_strs = || {
        refused(
            "fileConfig()'s defaults are a dict of strs written out in the call here, as the \
             file is read at compile time"
                .to_string(),
        )
    };
    match e {
        None | Some(IRExpr::Const(IRConstant::None)) => Ok(Vec::new()),
        Some(IRExpr::DictLiteral(pairs)) => pairs
            .iter()
            .map(|(k, value)| match (const_str(k), const_str(value)) {
                (Some(k), Some(value)) => Ok((k.to_string(), value.to_string())),
                _ => Err(not_strs()),
            })
            .collect(),
        Some(_) => Err(not_strs()),
    }
}

/// What `fileConfig(...)` does, as statements, or `None` when nothing is
/// being compiled (the front end run for a program's signatures), where the
/// call is left as it is.
pub(super) fn file_config(
    arguments: &[IRExpr],
    program: &Program,
    formats: &mut Formats,
) -> Result<Option<IRStatement>> {
    const PARAMS: &[&str] = &["fname", "defaults", "disable_existing_loggers", "encoding"];
    let positional = arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .count();
    if positional > PARAMS.len() {
        return Err(raise(
            "TypeError",
            format!(
                "fileConfig() takes from 1 to 4 positional arguments but {positional} were given"
            ),
        ));
    }
    for a in arguments {
        if let IRExpr::Keyword { name, .. } = a {
            match PARAMS.iter().position(|p| p == name) {
                None => {
                    return Err(raise(
                        "TypeError",
                        format!("fileConfig() got an unexpected keyword argument '{name}'"),
                    ))
                }
                Some(k) if k < positional => {
                    return Err(raise(
                        "TypeError",
                        format!("fileConfig() got multiple values for argument '{name}'"),
                    ))
                }
                Some(_) => {}
            }
        }
    }
    let arg = |k: usize| super::argument(arguments, k, PARAMS[k]);
    let path = path_of(arg(0).as_ref(), program)?;
    let defaults = defaults_of(arg(1).as_ref())?;
    let disable = arg(2).unwrap_or_else(|| boolean(true));
    match arg(3) {
        None | Some(IRExpr::Const(IRConstant::None)) => {}
        Some(e)
            if const_str(&e)
                .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "utf-8" | "utf8")) => {}
        Some(_) => {
            return Err(refused(
                "fileConfig() reads its file as UTF-8 here, so another encoding is not supported"
                    .to_string(),
            ))
        }
    }

    let text_of_file = match data_files::lookup(&path) {
        Lookup::Found(text) => text,
        Lookup::NoCompilation => return Ok(None),
        Lookup::Missing => {
            return Err(refused(format!(
                "fileConfig() reads {} at compile time, and the compiler was not given it. \
                 Hint: compile the program with compile_python_file() or \
                 compile_python_project(), which read a file in the program's directory, or \
                 give its text in CompilerOptions::data_files",
                py_repr(&path)
            )))
        }
    };
    if text_of_file.is_empty() {
        return Err(raise("RuntimeError", format!("{path} is an empty file")));
    }
    let ini = Ini::read(&text_of_file, &path, &defaults).map_err(raises)?;

    let mut at = 0;
    let mut local = |kind: &str| {
        at += 1;
        format!("__logging_fc_{kind}_{at}")
    };
    // The arguments are evaluated before the file is looked at.
    let disabled = local("disable");
    let mut out = vec![
        assign_typed(&disabled, disable, &IRType::Bool),
        expr_stmt(call(
            super::FILE_CONFIG_CHECK_FN,
            vec![text(&path), text(&text_of_file)],
        )),
    ];

    // `_create_formatters()`.
    let mut formatters: HashMap<String, String> = HashMap::new();
    let flist = ini.item("formatters", "keys").map_err(raises)?;
    for form in names(&flist).into_iter().filter(|_| !flist.is_empty()) {
        let section = format!("formatter_{form}");
        let fs = ini.get_raw(&section, "format");
        let dfs = ini.get_raw(&section, "datefmt");
        let style = ini
            .get_raw(&section, "style")
            .unwrap_or_else(|| "%".to_string());
        if ini.get_raw(&section, "defaults").is_some() {
            return Err(refused(format!(
                "fileConfig()'s formatter {} has defaults, which Formatter() does not support \
                 here",
                py_repr(&form)
            )));
        }
        if !ini.has_section(&section) {
            return Err(raises(Raised::key(&section)));
        }
        if let Some(class) = ini.get(&section, "class").map_err(raises)? {
            if !class.is_empty() && class != "logging.Formatter" {
                return Err(refused(format!(
                    "fileConfig()'s formatter {} has the class {}, where a formatter is a \
                     logging.Formatter here. Hint: leave the class out, or configure a \
                     formatter class of the program with dictConfig()'s '()'",
                    py_repr(&form),
                    py_repr(&class)
                )));
            }
        }
        if !matches!(style.as_str(), "%" | "{" | "$") {
            return Err(raise(
                "ValueError",
                "Style must be one of: %,{,$".to_string(),
            ));
        }
        if let Some(f) = fs.as_deref().filter(|f| !f.is_empty()) {
            formats.literal(f, &style)?;
        }
        let made = local("formatter");
        out.push(assign_typed(
            &made,
            call(
                &class_name("Formatter"),
                vec![
                    fs.as_deref().map_or_else(none, text),
                    dfs.as_deref().map_or_else(none, text),
                    text(&style),
                    boolean(true),
                ],
            ),
            &classes::ty("Formatter"),
        ));
        formatters.insert(form, made);
    }

    out.push(expr_stmt(call(&fname("config_reset"), Vec::new())));

    // `_install_handlers()`.
    let mut handlers: HashMap<String, String> = HashMap::new();
    let mut fixups: Vec<(String, String)> = Vec::new();
    let hlist = ini.item("handlers", "keys").map_err(raises)?;
    for hand in names(&hlist).into_iter().filter(|_| !hlist.is_empty()) {
        let section = format!("handler_{hand}");
        if !ini.has_section(&section) {
            return Err(raises(Raised::key(&section)));
        }
        let (made, ty) = handler_call(&ini, &hand, program, formats)?;
        let memory = ty == classes::ty("handlers.MemoryHandler");
        let held = local(if memory { "memory" } else { "handler" });
        out.push(assign_typed(&held, made, &ty));
        if ini.has_option(&section, "level") {
            let level = ini.item(&section, "level").map_err(raises)?;
            out.push(expr_stmt(mcall(
                v(&held),
                "setLevel",
                vec![level_of(&level, program)?],
            )));
        }
        let fmt = ini
            .get(&section, "formatter")
            .map_err(raises)?
            .unwrap_or_default();
        if !fmt.is_empty() {
            let formatter = formatters
                .get(&fmt)
                .ok_or_else(|| raises(Raised::key(&fmt)))?;
            out.push(expr_stmt(mcall(
                v(&held),
                "setFormatter",
                vec![v(formatter)],
            )));
        }
        if memory {
            let target = ini
                .get(&section, "target")
                .map_err(raises)?
                .unwrap_or_default();
            if !target.is_empty() {
                fixups.push((held.clone(), target));
            }
        }
        handlers.insert(hand, held);
    }
    for (held, target) in fixups {
        let target = handlers
            .get(&target)
            .ok_or_else(|| raises(Raised::key(&target)))?;
        out.push(expr_stmt(mcall(v(&held), "setTarget", vec![v(target)])));
    }

    // `_install_loggers()`: the root first.
    let mut llist = names(&ini.item("loggers", "keys").map_err(raises)?);
    match llist.iter().position(|n| n == "root") {
        Some(k) => {
            llist.remove(k);
        }
        None => {
            return Err(raise(
                "ValueError",
                "list.remove(x): x not in list".to_string(),
            ))
        }
    }
    let configure = |section: &str, target: &str, out: &mut Vec<IRStatement>| -> Result<()> {
        if ini.has_option(section, "level") {
            let level = ini.item(section, "level").map_err(raises)?;
            out.push(expr_stmt(mcall(
                v(target),
                "setLevel",
                vec![level_of(&level, program)?],
            )));
        }
        out.push(expr_stmt(call(&fname("drop_handlers"), vec![v(target)])));
        Ok(())
    };
    let add_handlers = |section: &str, target: &str, out: &mut Vec<IRStatement>| -> Result<()> {
        let hlist = ini.item(section, "handlers").map_err(raises)?;
        for hand in names(&hlist).into_iter().filter(|_| !hlist.is_empty()) {
            let held = handlers
                .get(&hand)
                .ok_or_else(|| raises(Raised::key(&hand)))?;
            out.push(expr_stmt(mcall(v(target), "addHandler", vec![v(held)])));
        }
        Ok(())
    };
    if !ini.has_section("logger_root") {
        return Err(raises(Raised::key("logger_root")));
    }
    let root = local("logger");
    out.push(assign_typed(
        &root,
        call(&fname("get_logger"), Vec::new()),
        &classes::ty("Logger"),
    ));
    configure("logger_root", &root, &mut out)?;
    add_handlers("logger_root", &root, &mut out)?;

    let mut named: Vec<String> = Vec::new();
    for log in &llist {
        let section = format!("logger_{log}");
        if !ini.has_section(&section) {
            return Err(raises(Raised::key(&section)));
        }
        let qualname = ini.item(&section, "qualname").map_err(raises)?;
        let propagate = match ini.get(&section, "propagate").map_err(raises)? {
            None => 1,
            Some(written) => parse_int(&written).ok_or_else(|| {
                raise(
                    "ValueError",
                    format!(
                        "invalid literal for int() with base 10: {}",
                        py_repr(&written)
                    ),
                )
            })?,
        };
        let made = local("logger");
        out.push(assign_typed(
            &made,
            call(&fname("get_logger"), vec![text(&qualname)]),
            &classes::ty("Logger"),
        ));
        configure(&section, &made, &mut out)?;
        out.push(set_attr(v(&made), "propagate", boolean(propagate != 0)));
        out.push(set_attr(v(&made), "disabled", boolean(false)));
        add_handlers(&section, &made, &mut out)?;
        named.push(qualname);
    }
    // `_handle_existing_loggers()`: every logger made above is named, so the
    // others are the ones that existed before.
    let mut listed: Vec<IRExpr> = named.iter().map(|n| text(n)).collect();
    // A name no logger has, so the list is never empty.
    listed.push(text(""));
    out.push(expr_stmt(call(
        &fname("config_existing"),
        vec![IRExpr::ListLiteral(listed), v(&disabled)],
    )));
    program.configured.set(true);
    Ok(Some(if_(boolean(true), out)))
}

/// `int(s)` of an option's value, where it is one an `int` here holds.
fn parse_int(written: &str) -> Option<i32> {
    let s = written.trim();
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    // Underscores stand between digits.
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let n: i64 = digits.replace('_', "").parse().ok()?;
    i32::try_from(if negative { -n } else { n }).ok()
}

/// The files a program names in its `fileConfig()` calls, by a str written
/// out or a module constant: what the entry points that compile from disk
/// read for it.
pub(crate) fn config_files(suite: &Suite) -> Vec<String> {
    use ast::fold::Fold;
    use ast::text_size::TextRange;

    struct Collect {
        constants: HashMap<String, String>,
        /// The names `fileConfig` is imported under.
        imported: Vec<String>,
        found: Vec<String>,
    }
    impl Fold<TextRange> for Collect {
        type TargetU = TextRange;
        type Error = std::convert::Infallible;
        type UserContext = ();
        fn will_map_user(&mut self, _user: &TextRange) -> Self::UserContext {}
        fn map_user(
            &mut self,
            user: TextRange,
            _context: (),
        ) -> std::result::Result<TextRange, Self::Error> {
            Ok(user)
        }
        fn fold_stmt(&mut self, stmt: Stmt) -> std::result::Result<Stmt, Self::Error> {
            if let Stmt::ImportFrom(i) = &stmt {
                for a in &i.names {
                    if a.name.as_str() == "fileConfig" {
                        self.imported
                            .push(a.asname.as_ref().unwrap_or(&a.name).to_string());
                    }
                }
            }
            ast::fold::fold_stmt(self, stmt)
        }
        fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
            if let Expr::Call(call) = &expr {
                let named = match &*call.func {
                    Expr::Attribute(a) => a.attr.as_str() == "fileConfig",
                    Expr::Name(n) => self.imported.iter().any(|i| i == n.id.as_str()),
                    _ => false,
                };
                let fname = call.args.first().or_else(|| {
                    call.keywords
                        .iter()
                        .find(|k| k.arg.as_ref().is_some_and(|a| a.as_str() == "fname"))
                        .map(|k| &k.value)
                });
                let path = match fname {
                    Some(Expr::Constant(c)) => match &c.value {
                        Constant::Str(s) => Some(s.clone()),
                        _ => None,
                    },
                    Some(Expr::Name(n)) => self.constants.get(n.id.as_str()).cloned(),
                    _ => None,
                };
                if let (true, Some(path)) = (named, path) {
                    if !self.found.contains(&path) {
                        self.found.push(path);
                    }
                }
            }
            ast::fold::fold_expr(self, expr)
        }
    }
    // The module's names bound to a str once.
    let mut constants: HashMap<String, String> = HashMap::new();
    let mut rebound: Vec<String> = Vec::new();
    for stmt in suite {
        let (target, value) = match stmt {
            Stmt::Assign(a) => match a.targets.as_slice() {
                [Expr::Name(n)] => (n.id.to_string(), Some(&*a.value)),
                _ => continue,
            },
            Stmt::AnnAssign(a) => match &*a.target {
                Expr::Name(n) => (n.id.to_string(), a.value.as_deref()),
                _ => continue,
            },
            _ => continue,
        };
        match value {
            Some(Expr::Constant(c)) if !constants.contains_key(&target) => match &c.value {
                Constant::Str(s) => {
                    constants.insert(target, s.clone());
                }
                _ => rebound.push(target),
            },
            _ => rebound.push(target),
        }
    }
    for name in rebound {
        constants.remove(&name);
    }
    let mut collect = Collect {
        constants,
        imported: Vec::new(),
        found: Vec::new(),
    };
    for stmt in suite.iter().cloned() {
        let _ = collect.fold_stmt(stmt);
    }
    collect.found
}
