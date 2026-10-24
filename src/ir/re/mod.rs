//! `re` at run time.
//!
//! A pattern is parsed and compiled with the program (`parse.rs`, a port of
//! CPython's `re/_parser.py`, and `compile.rs`), so one CPython would reject
//! is reported when compiling, with `re.error`'s message. Each distinct
//! pattern becomes one `re.Pattern`, a class variable of `re._Cache` (as
//! `re.compile` caches, `re.compile(p) is re.compile(p)`), whose program
//! the matcher (`vm.rs`) runs over a subject's characters. `Pattern` and
//! `Match` are classes written out in IR (`classes.rs`).
//!
//! A pattern is written as a literal, a module constant, or a compiled
//! pattern; one computed at run time is refused, as its program could not be
//! compiled.

mod classes;
mod compile;
mod parse;
mod vm;

use crate::ir::build::*;
use crate::ir::{IRBody, IRConstant, IRExpr, IRModule, IROp, IRStatement, IRType, IRVariable};
use anyhow::Result;
use std::collections::HashMap;

/// The prefix of the functions the pass makes; they are internal.
pub const RE_FN_PREFIX: &str = "__re_";
/// `sub` and `subn` with a function for the replacement.
pub(crate) const SUB_FN: &str = "_sub_fn";
pub(crate) const SUBN_FN: &str = "_subn_fn";

pub(crate) fn class_name(short: &str) -> String {
    format!("re.{short}")
}

fn fname(name: &str) -> String {
    format!("{RE_FN_PREFIX}{name}")
}

const FLAGS: &[(&str, u32)] = &[
    ("NOFLAG", 0),
    ("I", 2),
    ("IGNORECASE", 2),
    ("L", 4),
    ("LOCALE", 4),
    ("M", 8),
    ("MULTILINE", 8),
    ("S", 16),
    ("DOTALL", 16),
    ("U", 32),
    ("UNICODE", 32),
    ("X", 64),
    ("VERBOSE", 64),
    ("DEBUG", 128),
    ("A", 256),
    ("ASCII", 256),
];

struct Bindings {
    modules: Vec<String>,
    names: HashMap<String, String>,
}

impl Bindings {
    fn of(module: &IRModule) -> Bindings {
        let mut modules = Vec::new();
        let mut names = HashMap::new();
        for imp in &module.imports {
            if imp.module != "re" {
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
                    modules.push(imp.alias.clone().unwrap_or_else(|| "re".to_string()))
                }
                _ => {}
            }
        }
        Bindings { modules, names }
    }

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

/// The patterns compiled, each once.
struct Cache {
    entries: Vec<(String, u32)>,
    compiled: Vec<IRVariable>,
}

impl Cache {
    /// The cached pattern object for a pattern and its flags.
    fn pattern(&mut self, pattern: &str, flags: u32, call: &str) -> Result<IRExpr> {
        let key = (pattern.to_string(), flags);
        let k = match self.entries.iter().position(|e| *e == key) {
            Some(k) => k,
            None => {
                let parsed =
                    parse::parse_pattern(pattern, flags).map_err(|e| re_error(call, &e))?;
                let program = compile::compile(&parsed).map_err(|e| re_error(call, &e))?;
                let class = match parsed.groups - 1 {
                    0 => classes::PATTERN0,
                    1 => classes::PATTERN1,
                    _ => classes::PATTERN_N,
                };
                let value = call_class(
                    class,
                    vec![
                        text(pattern),
                        int(parsed.flags as i32),
                        int(parsed.groups as i32),
                        IRExpr::ListLiteral(program.code.iter().map(|&c| int(c)).collect()),
                        int(program.repeats as i32),
                        int(program.first.map_or(-1, |c| c as i32)),
                        boolean(program.needs.lower),
                        boolean(program.needs.upper),
                        IRExpr::ListLiteral(
                            parsed.groupdict.iter().map(|(n, _)| text(n)).collect(),
                        ),
                        IRExpr::ListLiteral(
                            parsed
                                .groupdict
                                .iter()
                                .map(|(_, g)| int(*g as i32))
                                .collect(),
                        ),
                    ],
                );
                self.compiled.push(IRVariable {
                    name: format!("p{}", self.entries.len()),
                    value,
                    var_type: Some(classes::ty(class)),
                });
                self.entries.push(key);
                self.entries.len() - 1
            }
        };
        Ok(attr(v(&class_name("_Cache")), &format!("p{k}")))
    }
}

fn call_class(short: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&class_name(short), args)
}

fn re_error(call: &str, message: &str) -> anyhow::Error {
    let (class, text) = match message.split_once(": ") {
        Some((c @ ("ValueError" | "OverflowError" | "IndexError"), rest)) => (c, rest),
        _ => ("re.error", message),
    };
    refused(format!("re.{call}() raises {class}: {text}"))
}

fn positional(arguments: &[IRExpr]) -> Vec<&IRExpr> {
    arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect()
}

fn keyword(arguments: &[IRExpr], name: &str) -> Option<IRExpr> {
    arguments.iter().find_map(|a| match a {
        IRExpr::Keyword { name: n, value } if n == name => Some((**value).clone()),
        _ => None,
    })
}

fn argument(arguments: &[IRExpr], n: usize, name: &str) -> Option<IRExpr> {
    positional(arguments)
        .get(n)
        .map(|e| (*e).clone())
        .or_else(|| keyword(arguments, name))
}

struct Pass<'a> {
    b: &'a Bindings,
    /// Module constants that are strings.
    strings: HashMap<String, String>,
    /// Module variables holding a compiled pattern.
    patterns: Vec<String>,
    cache: Cache,
    /// The module's functions, which `sub` can be given.
    functions: Vec<String>,
    /// Whether `sub` or `subn` is given a function.
    replaces_by_function: bool,
}

impl Pass<'_> {
    fn flags(&self, e: Option<&IRExpr>, call: &str) -> Result<u32> {
        let Some(e) = e else { return Ok(0) };
        match e {
            IRExpr::Const(IRConstant::Int(n)) if *n >= 0 => Ok(*n as u32),
            IRExpr::BinaryOp {
                left,
                right,
                op: IROp::BitOr,
            } => Ok(self.flags(Some(left), call)? | self.flags(Some(right), call)?),
            other => match self.b.member(other) {
                Some(name) => match FLAGS.iter().find(|(n, _)| *n == name) {
                    Some(&(_, bit)) => Ok(bit),
                    None => Err(refused(format!(
                        "re.{call}()'s flags are re's flags, written out"
                    ))),
                },
                None => Err(refused(format!(
                    "re.{call}()'s flags must be constants (re.IGNORECASE, re.I | re.M), as the \
                     pattern is compiled with the program"
                ))),
            },
        }
    }

    /// The pattern a call names: a cached pattern for a literal or a module
    /// constant, or the compiled pattern a module variable holds.
    fn pattern(&mut self, e: Option<IRExpr>, flags: u32, call: &str) -> Result<IRExpr> {
        let text = match &e {
            Some(IRExpr::Const(IRConstant::String(s))) => Some(s.clone()),
            Some(IRExpr::Variable(n)) => self.strings.get(n).cloned(),
            _ => None,
        };
        if let Some(text) = text {
            return self.cache.pattern(&text, flags, call);
        }
        if let Some(IRExpr::Variable(n)) = &e {
            if self.patterns.contains(n) {
                if flags != 0 {
                    return Err(refused(format!(
                        "re.{call}() with flags and a compiled pattern raises ValueError: cannot \
                         process flags argument with a compiled pattern"
                    )));
                }
                return Ok(v(n));
            }
        }
        Err(refused(format!(
            "re.{call}() is supported with a pattern written as a string literal or a module \
             constant (or a pattern compiled from one), as patterns are compiled with the \
             program"
        )))
    }

    fn module_call(&mut self, name: &str, arguments: Vec<IRExpr>) -> Result<Option<IRExpr>> {
        let pos = positional(&arguments).len();
        let deprecated = |what: &str| {
            refused(format!(
                "re.{name}() with {what} passed by position, for which CPython 3.13 prints a \
                 DeprecationWarning. Hint: pass it by name"
            ))
        };
        Ok(Some(match name {
            "compile" => {
                let flags = self.flags(argument(&arguments, 1, "flags").as_ref(), name)?;
                self.pattern(argument(&arguments, 0, "pattern"), flags, name)?
            }
            "search" | "match" | "fullmatch" | "findall" | "finditer" => {
                let flags = self.flags(argument(&arguments, 2, "flags").as_ref(), name)?;
                let p = self.pattern(argument(&arguments, 0, "pattern"), flags, name)?;
                let Some(string) = argument(&arguments, 1, "string") else {
                    return Err(refused(format!(
                        "re.{name}() missing required argument: 'string'"
                    )));
                };
                mcall(p, name, vec![string])
            }
            "split" => {
                if pos > 2 {
                    return Err(deprecated("maxsplit"));
                }
                let flags = self.flags(keyword(&arguments, "flags").as_ref(), name)?;
                let p = self.pattern(argument(&arguments, 0, "pattern"), flags, name)?;
                let string = argument(&arguments, 1, "string").unwrap_or_else(|| text(""));
                let maxsplit = keyword(&arguments, "maxsplit").unwrap_or_else(|| int(0));
                mcall(p, "split", vec![string, maxsplit])
            }
            "sub" | "subn" => {
                if pos > 3 {
                    return Err(deprecated("count"));
                }
                let flags = self.flags(keyword(&arguments, "flags").as_ref(), name)?;
                let p = self.pattern(argument(&arguments, 0, "pattern"), flags, name)?;
                let repl = argument(&arguments, 1, "repl").unwrap_or_else(|| text(""));
                let string = argument(&arguments, 2, "string").unwrap_or_else(|| text(""));
                let count = keyword(&arguments, "count").unwrap_or_else(|| int(0));
                mcall(p, name, vec![repl, string, count])
            }
            "escape" => call(&fname("escape"), arguments),
            "purge" => none(),
            _ => return Ok(None),
        }))
    }

    fn rewrite(&mut self, expr: &mut IRExpr) -> Result<()> {
        let called = match expr {
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } => match object.as_ref() {
                IRExpr::Variable(m) if self.b.modules.contains(m) => {
                    Some((method_name.clone(), arguments.clone()))
                }
                _ => None,
            },
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } => self
                .b
                .names
                .get(function_name)
                .map(|n| (n.clone(), arguments.clone())),
            _ => None,
        };
        if let Some((name, arguments)) = called {
            if let Some(done) = self.module_call(&name, arguments)? {
                *expr = done;
            }
        } else if let Some(member) = self.b.member(expr) {
            if let Some(&(_, bit)) = FLAGS.iter().find(|(n, _)| *n == member) {
                *expr = int(bit as i32);
            }
        }
        // `sub` with a function: its own method, the lambda's parameter a
        // Match.
        if let IRExpr::MethodCall {
            method_name,
            arguments,
            ..
        } = expr
        {
            if matches!(method_name.as_str(), "sub" | "subn") {
                if let Some(first) = arguments.first_mut() {
                    let function = match first {
                        IRExpr::Lambda { params, .. } => {
                            if let Some(p) = params.first_mut() {
                                if matches!(p.param_type, IRType::Unknown | IRType::Any) {
                                    p.param_type = IRType::Class(class_name("Match"));
                                }
                            }
                            true
                        }
                        IRExpr::Variable(n) => self.functions.contains(n),
                        _ => false,
                    };
                    if function {
                        *method_name = if method_name == "sub" {
                            SUB_FN
                        } else {
                            SUBN_FN
                        }
                        .to_string();
                        self.replaces_by_function = true;
                    }
                }
            }
        }
        Ok(())
    }
}

fn rename_type(ty: &mut IRType, b: &Bindings) {
    match ty {
        IRType::Class(c) => {
            if let Some(member) = b
                .names
                .get(c.as_str())
                .filter(|m| matches!(m.as_str(), "Match" | "Pattern"))
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

impl Pass<'_> {
    fn body(&mut self, body: &mut IRBody) -> Result<()> {
        crate::ir::visit_body(body, &mut |e: &mut IRExpr| self.rewrite(e))
    }
}

/// Compile the program's patterns and make `re`'s classes when the module
/// imports `re`, its calls into the patterns' methods.
pub fn lower_re(module: &mut IRModule) -> Result<()> {
    let b = Bindings::of(module);
    if b.modules.is_empty() && b.names.is_empty() {
        return Ok(());
    }
    let strings = module
        .variables
        .iter()
        .filter_map(|var| match &var.value {
            IRExpr::Const(IRConstant::String(s)) => Some((var.name.clone(), s.clone())),
            _ => None,
        })
        .collect();
    let functions = module.functions.iter().map(|f| f.name.clone()).collect();
    let mut pass = Pass {
        b: &b,
        strings,
        patterns: Vec::new(),
        cache: Cache {
            entries: Vec::new(),
            compiled: Vec::new(),
        },
        functions,
        replaces_by_function: false,
    };
    // Module variables first, in order, so a later call can name a pattern
    // one of them compiled.
    for var in &mut module.variables {
        expr_body(&mut var.value, |body| pass.body(body))?;
        if matches!(&var.value, IRExpr::Attribute { object, .. }
            if matches!(object.as_ref(), IRExpr::Variable(c) if *c == class_name("_Cache")))
        {
            pass.patterns.push(var.name.clone());
        }
        if let Some(t) = &mut var.var_type {
            rename_type(t, &b);
        }
    }
    for f in &mut module.functions {
        pass.body(&mut f.body)?;
        f.params
            .iter_mut()
            .for_each(|p| rename_type(&mut p.param_type, &b));
        rename_type(&mut f.return_type, &b);
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            pass.body(&mut m.body)?;
            m.params
                .iter_mut()
                .for_each(|p| rename_type(&mut p.param_type, &b));
            rename_type(&mut m.return_type, &b);
        }
        for var in &mut c.class_vars {
            expr_body(&mut var.value, |body| pass.body(body))?;
        }
    }
    let cache = std::mem::take(&mut pass.cache.compiled);
    let mut classes = classes::all(cache, pass.replaces_by_function);
    classes.append(&mut module.classes);
    module.classes = classes;
    module.functions.extend(vm::functions());
    module.functions.extend(classes::functions());

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
