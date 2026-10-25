//! `logging`, as classes written out in IR.
//!
//! The module's loggers, handlers, formatters, filters, and records are
//! classes of the module (`logging.Logger`, ...) whose methods are CPython's
//! own, and its state (the root logger, the loggers by name, `disable()`'s
//! level, `lastResort`) is class variables of `logging._State`, defined
//! before any of the program's. The pass makes them when the module imports
//! `logging`, and makes the program's names for them name them.
//!
//! A call that logs (`logger.info("x=%d", x)`) is compiled where its
//! receiver's type is known (`compiler::dunders`): its arguments are
//! evaluated, and its message formatted only when the logger is enabled for
//! the level, as in CPython. The record carries the call's line, which the
//! converter attaches, and its function's name, which this pass does.
//!
//! A formatter's format string is compiled where the program writes it
//! (`format.rs`); one CPython would fail to render at every record is
//! refused here.

mod classes;
mod format;

use crate::ir::build::*;
use crate::ir::{
    IRBody, IRClass, IRCompareOp, IRConstant, IRExpr, IRFunction, IRModule, IROp, IRParam,
    IRStatement, IRType, IRVariable, LOG_FUNC_ARG, LOG_LINE_ARG,
};
use anyhow::Result;
use std::collections::{BTreeSet, HashMap};

/// The prefix of the functions the pass makes; they are internal.
pub const LOGGING_FN_PREFIX: &str = "__logging_";
/// The method a logging call becomes once its message is made.
pub const LOG_METHOD: &str = "_log";
/// The same, where the call is an expression rather than a statement.
pub const LOG_NOW_METHOD: &str = "_log_now";
/// The function the module's own logging functions log through.
pub const MODULE_ROOT_FN: &str = "__logging_module_root";

const BASIC_FORMAT: &str = "%(levelname)s:%(name)s:%(message)s";
/// `basicConfig(...)`, until the statement it is is written out.
const BASIC_CONFIG_MARK: &str = "__logging_basicConfig";

/// The classes, by their name in the module.
const CLASSES: &[&str] = &[
    "Logger",
    "RootLogger",
    "Handler",
    "StreamHandler",
    "FileHandler",
    "NullHandler",
    "Formatter",
    "Filter",
    "LogRecord",
];

const LEVELS: &[(&str, i32)] = &[
    ("CRITICAL", 50),
    ("FATAL", 50),
    ("ERROR", 40),
    ("WARNING", 30),
    ("WARN", 30),
    ("INFO", 20),
    ("DEBUG", 10),
    ("NOTSET", 0),
];

pub(crate) fn class_name(short: &str) -> String {
    format!("logging.{short}")
}

fn fname(name: &str) -> String {
    format!("{LOGGING_FN_PREFIX}{name}")
}

fn function(
    name: &str,
    params: Vec<IRParam>,
    ret: IRType,
    statements: Vec<IRStatement>,
) -> IRFunction {
    IRFunction {
        name: fname(name),
        params,
        body: IRBody { statements },
        return_type: ret,
        decorators: Vec::new(),
    }
}

fn concat(parts: Vec<IRExpr>) -> IRExpr {
    parts
        .into_iter()
        .reduce(|a, b| bin(a, IROp::Add, b))
        .unwrap_or_else(|| text(""))
}

/// What the module calls the module and its names by.
struct Bindings {
    /// `import logging [as lg]`.
    modules: Vec<String>,
    /// `from logging import getLogger [as gl]`: the name, and what it names.
    names: HashMap<String, String>,
}

impl Bindings {
    fn of(module: &IRModule) -> Bindings {
        let mut modules = Vec::new();
        let mut names = HashMap::new();
        for imp in &module.imports {
            if imp.module != "logging" {
                continue;
            }
            match (&imp.name, imp.is_from_import) {
                (Some(name), true) => {
                    names.insert(
                        imp.alias.clone().unwrap_or_else(|| name.clone()),
                        name.clone(),
                    );
                }
                (None, false) => {
                    modules.push(imp.alias.clone().unwrap_or_else(|| "logging".to_string()))
                }
                _ => {}
            }
        }
        Bindings { modules, names }
    }

    /// The module's name an expression is: `logging.X`, or a name imported
    /// from it.
    fn member(&self, expr: &IRExpr) -> Option<String> {
        match expr {
            IRExpr::Variable(n) => self.names.get(n).cloned(),
            IRExpr::Attribute { object, attribute } => match object.as_ref() {
                IRExpr::Variable(m) if self.modules.contains(m) => Some(attribute.clone()),
                _ => None,
            },
            _ => None,
        }
    }
}

fn keyword(arguments: &[IRExpr], name: &str) -> Option<IRExpr> {
    arguments.iter().find_map(|a| match a {
        IRExpr::Keyword { name: n, value } if n == name => Some((**value).clone()),
        _ => None,
    })
}

/// The `n`th positional argument, or the keyword `name`.
fn argument(arguments: &[IRExpr], n: usize, name: &str) -> Option<IRExpr> {
    arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .nth(n)
        .cloned()
        .or_else(|| keyword(arguments, name))
}

fn const_str(e: &IRExpr) -> Option<&str> {
    match e {
        IRExpr::Const(IRConstant::String(s)) => Some(s),
        _ => None,
    }
}

/// The stream an argument names, as its file descriptor.
fn stream_fd(stream: Option<&IRExpr>, what: &str) -> Result<i32> {
    match stream {
        None | Some(IRExpr::Const(IRConstant::None)) => Ok(2),
        Some(IRExpr::Attribute { object, attribute }) if matches!(object.as_ref(), IRExpr::Variable(m) if m == "sys") => {
            match attribute.as_str() {
                "stdout" => Ok(1),
                "stderr" => Ok(2),
                _ => Err(refused(format!(
                    "{what} writes to sys.stdout or sys.stderr here"
                ))),
            }
        }
        Some(_) => Err(refused(format!(
            "{what} writes to sys.stdout or sys.stderr here, named as such (a file object or \
             another stream is not supported). Hint: use a FileHandler for a file"
        ))),
    }
}

fn check_encoding(encoding: Option<&IRExpr>, what: &str) -> Result<()> {
    match encoding {
        None | Some(IRExpr::Const(IRConstant::None)) => Ok(()),
        Some(e)
            if const_str(e)
                .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "utf-8" | "utf8")) =>
        {
            Ok(())
        }
        Some(_) => Err(refused(format!(
            "{what} writes UTF-8 here, so another encoding is not supported"
        ))),
    }
}

/// The formats the program spells out, and how each compiles.
#[derive(Default)]
struct Formats {
    /// `(fmt, style)` in the order found, each once.
    found: Vec<(String, String)>,
}

impl Formats {
    fn add(&mut self, fmt: &str, style: &str) {
        let key = (fmt.to_string(), style.to_string());
        if !self.found.contains(&key) {
            self.found.push(key);
        }
    }

    /// A format written at a `Formatter()` or `basicConfig()` call: one
    /// CPython could not render is refused.
    fn literal(&mut self, fmt: &str, style: &str) -> Result<()> {
        if let Err(why) = format::compile(fmt, style).render {
            return Err(refused(format!(
                "the logging format {fmt:?} cannot be rendered: {why}"
            )));
        }
        self.add(fmt, style);
        Ok(())
    }
}

fn style_of(style: Option<&IRExpr>) -> Result<Option<String>> {
    match style {
        None => Ok(Some("%".to_string())),
        Some(e) => match const_str(e) {
            Some(s @ ("%" | "{" | "$")) => Ok(Some(s.to_string())),
            Some(_) => Err(refused(
                "a logging style other than '%', '{', or '$', where CPython raises ValueError: \
                 Style must be one of: %,{,$"
                    .to_string(),
            )),
            None => Ok(None),
        },
    }
}

/// The program's names for the module's members made the classes' and
/// functions', in an expression, after its children.
fn rewrite_expr(expr: &mut IRExpr, b: &Bindings, formats: &mut Formats) -> Result<()> {
    // `logging.f(...)` and `f(...)` for a name imported from the module.
    let handled = |n: &str| {
        matches!(
            n,
            "getLogger" | "disable" | "getLevelName" | "shutdown" | "basicConfig"
        ) || crate::ir::LOG_METHODS.contains(&n)
            || CLASSES.contains(&n)
    };
    let called = match expr {
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => match object.as_ref() {
            IRExpr::Variable(m) if b.modules.contains(m) && handled(method_name) => {
                Some((method_name.clone(), std::mem::take(arguments)))
            }
            _ => None,
        },
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => b
            .names
            .get(function_name)
            .filter(|n| handled(n))
            .map(|n| (n.clone(), std::mem::take(arguments))),
        _ => None,
    };
    if let Some((name, arguments)) = called {
        *expr = module_call(&name, arguments, formats)?;
        return Ok(());
    }
    if let Some(member) = b.member(expr) {
        if let Some(&(_, n)) = LEVELS.iter().find(|(l, _)| *l == member) {
            *expr = int(n);
        } else if CLASSES.contains(&member.as_str()) {
            *expr = v(&class_name(&member));
        } else if member == "root" {
            *expr = attr(v(&class_name("_State")), "root");
        } else if member == "lastResort" {
            *expr = attr(v(&class_name("_State")), "last_resort");
        }
    }
    Ok(())
}

/// A call of the module's function or class `name`.
fn module_call(name: &str, arguments: Vec<IRExpr>, formats: &mut Formats) -> Result<IRExpr> {
    let positional = arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .count();
    Ok(match name {
        "getLogger" => call(
            &fname("get_logger"),
            vec![argument(&arguments, 0, "name").unwrap_or_else(none)],
        ),
        "disable" => call(
            &fname("disable"),
            vec![argument(&arguments, 0, "level").unwrap_or_else(|| int(50))],
        ),
        "getLevelName" => call(&fname("level_name"), arguments),
        "shutdown" => call(&fname("shutdown"), Vec::new()),
        "basicConfig" => {
            if positional > 0 {
                return Err(refused(
                    "basicConfig() takes its arguments by name, where CPython raises \
                     TypeError: basicConfig() takes 0 positional arguments"
                        .to_string(),
                ));
            }
            call(BASIC_CONFIG_MARK, arguments)
        }
        n if crate::ir::LOG_METHODS.contains(&n) => {
            mcall(call(MODULE_ROOT_FN, Vec::new()), n, arguments)
        }
        "StreamHandler" => {
            let fd = stream_fd(
                argument(&arguments, 0, "stream").as_ref(),
                "StreamHandler()",
            )?;
            call(&class_name("StreamHandler"), vec![int(fd)])
        }
        "FileHandler" => {
            check_encoding(
                argument(&arguments, 2, "encoding").as_ref(),
                "FileHandler()",
            )?;
            if argument(&arguments, 4, "errors").is_some() {
                return Err(refused(
                    "FileHandler()'s errors= is not supported".to_string(),
                ));
            }
            call(&class_name("FileHandler"), arguments)
        }
        "Formatter" => {
            formatter_call(&arguments, formats)?;
            call(&class_name("Formatter"), arguments)
        }
        "LogRecord" => {
            return Err(refused(
                "making a LogRecord directly is not supported. Hint: log through a Logger"
                    .to_string(),
            ))
        }
        n => call(&class_name(n), arguments),
    })
}

/// Check a `Formatter(...)` call's format, written as a literal.
fn formatter_call(arguments: &[IRExpr], formats: &mut Formats) -> Result<()> {
    if keyword(arguments, "defaults").is_some() {
        return Err(refused(
            "Formatter()'s defaults= is not supported".to_string(),
        ));
    }
    let style = style_of(argument(arguments, 2, "style").as_ref())?;
    match (argument(arguments, 0, "fmt"), style) {
        (Some(e), Some(style)) => match const_str(&e) {
            Some(fmt) => formats.literal(fmt, &style),
            None if matches!(e, IRExpr::Variable(_) | IRExpr::Const(IRConstant::None)) => Ok(()),
            None => Err(refused(
                "a logging format computed at run time is not supported: formats are \
                 compiled. Hint: write the format as a literal or a module constant"
                    .to_string(),
            )),
        },
        _ => Ok(()),
    }
}

/// `basicConfig(...)` as the statements it is.
fn basic_config(arguments: &[IRExpr], formats: &mut Formats) -> Result<IRStatement> {
    const KNOWN: &[&str] = &[
        "level", "format", "datefmt", "style", "stream", "filename", "filemode", "handlers",
        "force", "encoding", "errors",
    ];
    for a in arguments {
        if let IRExpr::Keyword { name, .. } = a {
            if !KNOWN.contains(&name.as_str()) && name != LOG_LINE_ARG && name != LOG_FUNC_ARG {
                return Err(refused(format!(
                    "basicConfig() got the argument '{name}', where CPython raises \
                     ValueError: Unrecognised argument(s): {name}"
                )));
            }
        }
    }
    let kw = |n: &str| keyword(arguments, n);
    let (stream, filename, handlers) = (kw("stream"), kw("filename"), kw("handlers"));
    if stream.is_some() && filename.is_some() {
        return Err(refused(
            "basicConfig() with both stream= and filename=, where CPython raises ValueError: \
             'stream' and 'filename' should not be specified together"
                .to_string(),
        ));
    }
    if handlers.is_some() && (stream.is_some() || filename.is_some()) {
        return Err(refused(
            "basicConfig() with handlers= and stream= or filename=, where CPython raises \
             ValueError: 'stream' or 'filename' should not be specified together with \
             'handlers'"
                .to_string(),
        ));
    }
    check_encoding(kw("encoding").as_ref(), "basicConfig()")?;
    if kw("errors").is_some() {
        return Err(refused(
            "basicConfig()'s errors= is not supported".to_string(),
        ));
    }
    let Some(style) = style_of(kw("style").as_ref())? else {
        return Err(refused(
            "basicConfig()'s style must be written as a literal".to_string(),
        ));
    };
    let fmt = match kw("format") {
        Some(e) => {
            match const_str(&e) {
                Some(f) => formats.literal(f, &style)?,
                None if matches!(e, IRExpr::Variable(_)) => {}
                None => {
                    return Err(refused(
                        "a logging format computed at run time is not supported: formats are \
                         compiled. Hint: write the format as a literal or a module constant"
                            .to_string(),
                    ))
                }
            }
            e
        }
        None => {
            let basic = match style.as_str() {
                "{" => "{levelname}:{name}:{message}",
                "$" => "${levelname}:${name}:${message}",
                _ => BASIC_FORMAT,
            };
            formats.add(basic, &style);
            text(basic)
        }
    };
    let (level, has_level) = match kw("level") {
        None => (int(0), false),
        Some(e) => match const_str(&e) {
            Some(name) => match LEVELS.iter().find(|(l, _)| *l == name) {
                Some(&(_, n)) => (int(n), true),
                None => {
                    return Err(refused(format!(
                        "basicConfig(level={name:?}), where CPython raises ValueError: \
                         Unknown level: {name:?}"
                    )))
                }
            },
            None => (e, true),
        },
    };
    let fd = stream_fd(stream.as_ref(), "basicConfig(stream=...)")?;
    let list = "__logging_handlers";
    let mut statements = vec![IRStatement::Assign {
        target: list.to_string(),
        value: IRExpr::ListLiteral(Vec::new()),
        var_type: Some(IRType::List(Box::new(classes::ty("Handler")))),
    }];
    let append = |e: IRExpr| expr_stmt(mcall(v(list), "append", vec![e]));
    match &handlers {
        Some(IRExpr::ListLiteral(items)) | Some(IRExpr::TupleLiteral(items)) => {
            statements.extend(items.iter().cloned().map(append))
        }
        Some(other) => statements.push(for_(
            "__logging_h",
            other.clone(),
            vec![append(v("__logging_h"))],
        )),
        None => {}
    }
    statements.push(expr_stmt(call(
        &fname("basic_config"),
        vec![
            level,
            boolean(has_level),
            fmt,
            kw("datefmt").unwrap_or_else(none),
            text(&style),
            int(fd),
            filename.unwrap_or_else(none),
            kw("filemode").unwrap_or_else(|| text("a")),
            kw("force").unwrap_or_else(|| boolean(false)),
            v(list),
            boolean(handlers.is_some()),
        ],
    )));
    Ok(if_(boolean(true), statements))
}

/// The bodies directly inside a statement.
fn bodies_mut(stmt: &mut IRStatement) -> Vec<&mut IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            let mut out = vec![then_body.as_mut()];
            out.extend(else_body.iter_mut().map(|b| b.as_mut()));
            out
        }
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body.as_mut()],
        IRStatement::For {
            body, else_body, ..
        } => {
            let mut out = vec![body.as_mut()];
            out.extend(else_body.iter_mut().map(|b| b.as_mut()));
            out
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            let mut out = vec![try_body.as_mut()];
            out.extend(except_handlers.iter_mut().map(|h| &mut h.body));
            out.extend(finally_body.iter_mut().map(|b| b.as_mut()));
            out
        }
        IRStatement::FunctionDef(def) => vec![&mut def.body],
        _ => Vec::new(),
    }
}

/// Write out each `basicConfig(...)` statement; one used as a value is
/// refused.
fn expand_basic_config(body: &mut IRBody, formats: &mut Formats) -> Result<()> {
    for stmt in &mut body.statements {
        if let IRStatement::Expression(IRExpr::FunctionCall {
            function_name,
            arguments,
        }) = stmt
        {
            if function_name == BASIC_CONFIG_MARK {
                *stmt = basic_config(arguments, formats)?;
                continue;
            }
        }
        for nested in bodies_mut(stmt) {
            expand_basic_config(nested, formats)?;
        }
    }
    crate::ir::visit_body(body, &mut |e: &mut IRExpr| match e {
        IRExpr::FunctionCall { function_name, .. } if function_name == BASIC_CONFIG_MARK => Err(
            refused("basicConfig() is supported as a statement".to_string()),
        ),
        _ => Ok(()),
    })
}

/// Give each logging call in a body the name of the function it is in: a
/// nested function's or a lambda's own, else `name`.
fn tag_functions(body: &mut IRBody, name: &str) {
    fn nested(body: &mut IRBody) {
        for stmt in &mut body.statements {
            if let IRStatement::FunctionDef(def) = stmt {
                let name = def.name.clone();
                tag_functions(&mut def.body, &name);
                continue;
            }
            for inner in bodies_mut(stmt) {
                nested(inner);
            }
        }
    }
    fn tag(name: &str) -> impl FnMut(&mut IRExpr) -> Result<()> + '_ {
        move |e: &mut IRExpr| {
            if let IRExpr::FunctionCall { arguments, .. } | IRExpr::MethodCall { arguments, .. } = e
            {
                let lines = arguments
                    .iter()
                    .any(|a| matches!(a, IRExpr::Keyword { name, .. } if name == LOG_LINE_ARG));
                let named = arguments
                    .iter()
                    .any(|a| matches!(a, IRExpr::Keyword { name, .. } if name == LOG_FUNC_ARG));
                if lines && !named {
                    arguments.push(IRExpr::Keyword {
                        name: LOG_FUNC_ARG.to_string(),
                        value: Box::new(text(name)),
                    });
                }
            }
            Ok(())
        }
    }
    nested(body);
    // Lambdas, innermost first.
    let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        if let IRExpr::Lambda { body: lambda, .. } = e {
            let mut wrapped = IRBody {
                statements: vec![IRStatement::Expression(std::mem::replace(
                    lambda.as_mut(),
                    none(),
                ))],
            };
            let _ = crate::ir::visit_body(&mut wrapped, &mut tag("<lambda>"));
            if let Some(IRStatement::Expression(back)) = wrapped.statements.pop() {
                **lambda = back;
            }
        }
        Ok(())
    });
    let _ = crate::ir::visit_body(body, &mut tag(name));
}

fn expr_body<T>(e: &mut IRExpr, f: impl FnOnce(&mut IRBody) -> T) -> T {
    let mut wrapped = IRBody {
        statements: vec![IRStatement::Expression(std::mem::replace(e, none()))],
    };
    let out = f(&mut wrapped);
    if let Some(IRStatement::Expression(back)) = wrapped.statements.pop() {
        *e = back;
    }
    out
}

/// The strings in the module, which may be formats a `Formatter` is given
/// through a variable or a subclass.
fn strings(module: &mut IRModule) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut collect = |e: &mut IRExpr| {
        if let IRExpr::Const(IRConstant::String(s)) = e {
            out.insert(s.clone());
        }
        Ok(())
    };
    for f in &mut module.functions {
        let _ = crate::ir::visit_body(&mut f.body, &mut collect);
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            let _ = crate::ir::visit_body(&mut m.body, &mut collect);
        }
    }
    for var in &mut module.variables {
        expr_body(&mut var.value, |b| {
            let _ = crate::ir::visit_body(b, &mut collect);
        });
    }
    out
}

/// `format_id(fmt, style, validate)` and `render(id, record)` over the
/// formats found, and a renderer for each.
fn format_functions(formats: &Formats, constants: &BTreeSet<String>) -> Vec<IRFunction> {
    let mut table: Vec<(String, String)> = formats.found.clone();
    for s in constants {
        for (style, marker) in [("%", "%("), ("{", "{"), ("$", "$")] {
            if s.contains(marker) && format::compile(s, style).render.is_ok() {
                let key = (s.clone(), style.to_string());
                if !table.contains(&key) {
                    table.push(key);
                }
            }
        }
    }
    for (fmt, style) in [
        ("%(message)s", "%"),
        ("{message}", "{"),
        ("${message}", "$"),
        (BASIC_FORMAT, "%"),
    ] {
        let key = (fmt.to_string(), style.to_string());
        if !table.contains(&key) {
            table.push(key);
        }
    }
    let record = || param("record", &classes::ty("LogRecord"));
    let mut out = Vec::new();
    let mut lookup = Vec::new();
    let mut dispatch = Vec::new();
    for (k, (fmt, style)) in table.iter().enumerate() {
        let compiled = format::compile(fmt, style);
        let mut then = Vec::new();
        if let Some(why) = &compiled.invalid {
            then.push(if_(
                v("validate"),
                vec![raise_expr("ValueError", text(why))],
            ));
        }
        match compiled.render {
            Ok(render) => {
                then.push(ret_val(int(k as i32)));
                let name = format!("render_{k}");
                dispatch.push(if_(
                    cmp(v("id"), IRCompareOp::Eq, int(k as i32)),
                    vec![ret_val(call(&fname(&name), vec![v("record")]))],
                ));
                out.push(function(
                    &name,
                    vec![record()],
                    IRType::String,
                    vec![ret_val(render)],
                ));
            }
            Err(why) => then.push(raise_expr(
                "NotImplementedError",
                text(&format!(
                    "the logging format {fmt:?} cannot be rendered: {why}"
                )),
            )),
        }
        lookup.push(if_(
            and(
                cmp(v("fmt"), IRCompareOp::Eq, text(fmt)),
                cmp(v("style"), IRCompareOp::Eq, text(style)),
            ),
            then,
        ));
    }
    lookup.push(raise_expr(
        "NotImplementedError",
        concat(vec![
            text("the logging format "),
            call("repr", vec![v("fmt")]),
            text(" is not one the program spells out, so it was not compiled"),
        ]),
    ));
    dispatch.push(raise_expr(
        "NotImplementedError",
        text("an unknown logging format"),
    ));
    out.push(function(
        "format_id",
        vec![
            param("fmt", &IRType::String),
            param("style", &IRType::String),
            param("validate", &IRType::Bool),
        ],
        IRType::Int,
        lookup,
    ));
    out.push(function(
        "render",
        vec![param("id", &IRType::Int), record()],
        IRType::String,
        dispatch,
    ));
    out
}

/// Give each logging call whose message is a literal formatted with its
/// arguments that message as `%` lowers it, over placeholder arguments.
fn format_messages(body: &mut IRBody, layout: &mut crate::ir::MemoryLayout) {
    let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        let IRExpr::MethodCall {
            method_name,
            arguments,
            ..
        } = e
        else {
            return Ok(());
        };
        let logs = arguments
            .iter()
            .any(|a| matches!(a, IRExpr::Keyword { name, .. } if name == LOG_LINE_ARG));
        if !logs
            || arguments.iter().any(
                |a| matches!(a, IRExpr::Keyword { name, .. } if name == crate::ir::LOG_TEXT_ARG),
            )
        {
            return Ok(());
        }
        let positional: Vec<&IRExpr> = arguments
            .iter()
            .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
            .collect();
        let at = usize::from(method_name == "log");
        let Some(fmt) = positional.get(at).and_then(|m| const_str(m)) else {
            return Ok(());
        };
        let count = positional.len().saturating_sub(at + 1);
        if count == 0 {
            return Ok(());
        }
        let placeholder = |k: usize| v(&format!("{}{k}", crate::ir::LOG_ARG_PREFIX));
        let keyed = fmt.contains("%(");
        let operand = if keyed && count == 1 {
            crate::ir::PercentOperand::One(placeholder(0))
        } else {
            crate::ir::PercentOperand::Tuple((0..count).map(placeholder).collect())
        };
        let fmt = fmt.to_string();
        let added = match crate::ir::percent_format(&fmt, operand, layout) {
            Ok(text) => IRExpr::Keyword {
                name: crate::ir::LOG_TEXT_ARG.to_string(),
                value: Box::new(text),
            },
            Err(err) => {
                let why = err.to_string();
                layout.add_string(&why);
                IRExpr::Keyword {
                    name: crate::ir::LOG_TEXT_ERROR_ARG.to_string(),
                    value: Box::new(text(&why)),
                }
            }
        };
        arguments.push(added);
        Ok(())
    });
}

/// Make the program's names for a class's bases the module's classes.
fn rename_bases(class: &mut IRClass, b: &Bindings) {
    for base in &mut class.bases {
        let member = match base.split_once('.') {
            Some((m, rest)) if b.modules.contains(&m.to_string()) => Some(rest.to_string()),
            _ => b.names.get(base.as_str()).cloned(),
        };
        if let Some(member) = member.filter(|m| CLASSES.contains(&m.as_str())) {
            *base = class_name(&member);
        }
    }
}

fn rename_type(ty: &mut IRType, b: &Bindings) {
    match ty {
        IRType::Class(c) => {
            if let Some(member) = b
                .names
                .get(c.as_str())
                .filter(|m| CLASSES.contains(&m.as_str()))
            {
                *c = class_name(member);
            }
        }
        IRType::List(e) | IRType::Set(e) | IRType::VarTuple(e) | IRType::Optional(e) => {
            rename_type(e, b)
        }
        IRType::Dict(k, v) => {
            rename_type(k, b);
            rename_type(v, b);
        }
        IRType::Tuple(ms) | IRType::Union(ms) => ms.iter_mut().for_each(|m| rename_type(m, b)),
        _ => {}
    }
}

fn rename_body_types(body: &mut IRBody, b: &Bindings) {
    for stmt in &mut body.statements {
        if let IRStatement::Assign {
            var_type: Some(t), ..
        } = stmt
        {
            rename_type(t, b);
        }
        for nested in bodies_mut(stmt) {
            rename_body_types(nested, b);
        }
    }
}

fn rename_function(f: &mut IRFunction, b: &Bindings) {
    f.params
        .iter_mut()
        .for_each(|p| rename_type(&mut p.param_type, b));
    rename_type(&mut f.return_type, b);
    rename_body_types(&mut f.body, b);
}

/// Make the `logging` classes when the module imports `logging`, and make
/// its names for them name them.
pub fn lower_logging(module: &mut IRModule) -> Result<()> {
    let b = Bindings::of(module);
    if b.modules.is_empty() && b.names.is_empty() {
        return Ok(());
    }
    // The function each logging call is in, while the names are the
    // program's own.
    for f in &mut module.functions {
        // The `__main__` block is module code.
        let name = match f.name.as_str() {
            crate::ir::MAIN_BLOCK_FN => "<module>".to_string(),
            name => name.to_string(),
        };
        tag_functions(&mut f.body, &name);
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            let name = m.name.clone();
            tag_functions(&mut m.body, &name);
        }
        let class = c.name.clone();
        for var in &mut c.class_vars {
            expr_body(&mut var.value, |body| tag_functions(body, &class));
        }
    }
    for var in &mut module.variables {
        expr_body(&mut var.value, |body| tag_functions(body, "<module>"));
    }

    let mut formats = Formats::default();
    let mut rewrite = |e: &mut IRExpr| rewrite_expr(e, &b, &mut formats);
    for f in &mut module.functions {
        crate::ir::visit_body(&mut f.body, &mut rewrite)?;
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            crate::ir::visit_body(&mut m.body, &mut rewrite)?;
        }
        for var in &mut c.class_vars {
            expr_body(&mut var.value, |body| {
                crate::ir::visit_body(body, &mut rewrite)
            })?;
        }
    }
    for var in &mut module.variables {
        expr_body(&mut var.value, |body| {
            crate::ir::visit_body(body, &mut rewrite)
        })?;
    }
    for f in &mut module.functions {
        expand_basic_config(&mut f.body, &mut formats)?;
        rename_function(f, &b);
    }
    for c in &mut module.classes {
        rename_bases(c, &b);
        for m in &mut c.methods {
            expand_basic_config(&mut m.body, &mut formats)?;
            rename_function(m, &b);
        }
        // `super().__init__(sys.stdout)` in a StreamHandler of the program's.
        if c.bases
            .iter()
            .any(|base| *base == class_name("StreamHandler"))
        {
            for m in &mut c.methods {
                crate::ir::visit_body(&mut m.body, &mut |e: &mut IRExpr| {
                    if let IRExpr::MethodCall {
                        object,
                        method_name,
                        arguments,
                    } = e
                    {
                        let on_super = matches!(object.as_ref(),
                            IRExpr::FunctionCall { function_name, .. } if function_name == "super");
                        if on_super && method_name == "__init__" {
                            let fd = stream_fd(
                                argument(arguments, 0, "stream").as_ref(),
                                "StreamHandler()",
                            )?;
                            *arguments = vec![int(fd)];
                        }
                    }
                    Ok(())
                })?;
            }
        }
    }
    for var in &mut module.variables {
        if let Some(t) = &mut var.var_type {
            rename_type(t, &b);
        }
    }

    let mut layout = std::mem::take(&mut module.memory_layout);
    for f in &mut module.functions {
        format_messages(&mut f.body, &mut layout);
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            format_messages(&mut m.body, &mut layout);
        }
    }
    module.memory_layout = layout;

    let constants = strings(module);
    let mut classes = classes::all();
    classes.append(&mut module.classes);
    module.classes = classes;
    let defined: std::collections::HashSet<String> =
        module.functions.iter().map(|f| f.name.clone()).collect();
    module.functions.extend(
        crate::ir::datetime::strftime_functions()
            .into_iter()
            .filter(|f| !defined.contains(&f.name)),
    );
    module.functions.extend(classes::functions());
    module
        .functions
        .extend(format_functions(&formats, &constants));

    // The strings the classes and functions use are interned like the
    // program's.
    let mut strings = Vec::new();
    let mut collect = |e: &mut IRExpr| {
        if let IRExpr::Const(IRConstant::String(s)) = e {
            strings.push(s.clone());
        }
        Ok(())
    };
    for f in &mut module.functions {
        let _ = crate::ir::visit_body(&mut f.body, &mut collect);
        for p in &mut f.params {
            if let Some(d) = &mut p.default_value {
                expr_body(d, |body| {
                    let _ = crate::ir::visit_body(body, &mut collect);
                });
            }
        }
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            let _ = crate::ir::visit_body(&mut m.body, &mut collect);
            for p in &mut m.params {
                if let Some(d) = &mut p.default_value {
                    expr_body(d, |body| {
                        let _ = crate::ir::visit_body(body, &mut collect);
                    });
                }
            }
        }
        for var in &mut c.class_vars {
            expr_body(&mut var.value, |body| {
                let _ = crate::ir::visit_body(body, &mut collect);
            });
        }
    }
    for s in strings {
        module.memory_layout.add_string(&s);
    }
    Ok(())
}
