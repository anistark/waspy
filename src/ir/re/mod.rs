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
//! compiled. A bytes pattern has classes of its own (`classes::Text`), as its
//! subject, groups, and replacements are bytes.

mod classes;
mod compile;
mod parse;
mod vm;

use crate::ir::build::*;
use crate::ir::{IRBody, IRConstant, IRExpr, IRModule, IROp, IRStatement, IRType, IRVariable};
use anyhow::Result;
use classes::Text;
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

/// A pattern as written: a str, or bytes.
#[derive(Clone, PartialEq)]
enum Source {
    Str(String),
    Bytes(Vec<u8>),
}

impl Source {
    fn text(&self) -> Text {
        match self {
            Source::Str(_) => Text::Str,
            Source::Bytes(_) => Text::Bytes,
        }
    }
    /// What the parser reads: the str, or the bytes' latin-1 decoding.
    fn decoded(&self) -> String {
        match self {
            Source::Str(s) => s.clone(),
            Source::Bytes(b) => b.iter().map(|&c| c as char).collect(),
        }
    }
    fn value(&self) -> IRExpr {
        match self {
            Source::Str(s) => text(s),
            Source::Bytes(b) => classes::bytes_const(b),
        }
    }
}

/// The patterns compiled, each once.
struct Cache {
    entries: Vec<(Source, u32)>,
    compiled: Vec<IRVariable>,
}

impl Cache {
    /// The cached pattern object for a pattern and its flags.
    fn pattern(&mut self, pattern: &Source, flags: u32, call: &str) -> Result<IRExpr> {
        let key = (pattern.clone(), flags);
        let k = match self.entries.iter().position(|e| *e == key) {
            Some(k) => k,
            None => {
                let t = pattern.text();
                let parsed = parse::parse_pattern(&pattern.decoded(), flags, t == Text::Str)
                    .map_err(|e| re_error(call, &e))?;
                let program = compile::compile(&parsed).map_err(|e| re_error(call, &e))?;
                let class = t.short(match parsed.groups - 1 {
                    0 => classes::PATTERN0,
                    1 => classes::PATTERN1,
                    _ => classes::PATTERN_N,
                });
                let value = call_class(
                    &class,
                    vec![
                        pattern.value(),
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
                    var_type: Some(classes::ty(&class)),
                });
                self.entries.push(key);
                self.entries.len() - 1
            }
        };
        Ok(attr(v(&class_name("_Cache")), &format!("p{k}")))
    }

    /// The text a cached pattern matches, by its class variable.
    fn text_of(&self, expr: &IRExpr) -> Option<Text> {
        let IRExpr::Attribute { object, attribute } = expr else {
            return None;
        };
        if !matches!(object.as_ref(), IRExpr::Variable(c) if *c == class_name("_Cache")) {
            return None;
        }
        let k: usize = attribute.strip_prefix('p')?.parse().ok()?;
        Some(self.entries.get(k)?.0.text())
    }

    fn uses_bytes(&self) -> bool {
        self.entries.iter().any(|(p, _)| p.text() == Text::Bytes)
    }
}

fn call_class(short: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&class_name(short), args)
}

fn re_error(call: &str, message: &str) -> anyhow::Error {
    if let Some(why) = message.strip_prefix("unsupported: ") {
        return refused(format!("re.{call}(): {why}"));
    }
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
    /// Module constants that are strings or bytes.
    strings: HashMap<String, Source>,
    /// Module variables holding a compiled pattern, and the text it matches.
    patterns: HashMap<String, Text>,
    /// Whether `re.escape` is given bytes.
    escapes_bytes: bool,
    /// The parameters of the function being rewritten, by their types.
    params: HashMap<String, IRType>,
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
            Some(IRExpr::Const(IRConstant::String(s))) => Some(Source::Str(s.clone())),
            Some(IRExpr::Const(IRConstant::Bytes(b))) => Some(Source::Bytes(b.clone())),
            Some(IRExpr::Variable(n)) => self.strings.get(n).cloned(),
            _ => None,
        };
        if let Some(text) = text {
            return self.cache.pattern(&text, flags, call);
        }
        if let Some(IRExpr::Variable(n)) = &e {
            if self.patterns.contains_key(n) {
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

    /// CPython's TypeError for a str subject of a bytes pattern, or the
    /// reverse, where the subject is written out.
    fn check_subject(&self, pattern: &IRExpr, string: &IRExpr, call: &str) -> Result<()> {
        let t = self.cache.text_of(pattern).or_else(|| match pattern {
            IRExpr::Variable(n) => self.patterns.get(n).copied(),
            _ => None,
        });
        let why = match (t, string) {
            (Some(Text::Bytes), IRExpr::Const(IRConstant::String(_))) => {
                "cannot use a bytes pattern on a string-like object"
            }
            (Some(Text::Str), IRExpr::Const(IRConstant::Bytes(_))) => {
                "cannot use a string pattern on a bytes-like object"
            }
            _ => return Ok(()),
        };
        Err(refused(format!("re.{call}() raises TypeError: {why}")))
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
                self.check_subject(&p, &string, name)?;
                mcall(p, name, vec![string])
            }
            "split" => {
                if pos > 2 {
                    return Err(deprecated("maxsplit"));
                }
                let flags = self.flags(keyword(&arguments, "flags").as_ref(), name)?;
                let p = self.pattern(argument(&arguments, 0, "pattern"), flags, name)?;
                let string = argument(&arguments, 1, "string").unwrap_or_else(|| text(""));
                self.check_subject(&p, &string, name)?;
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
                self.check_subject(&p, &string, name)?;
                let count = keyword(&arguments, "count").unwrap_or_else(|| int(0));
                mcall(p, name, vec![repl, string, count])
            }
            // `re.escape(b)` of bytes answers bytes.
            "escape" => {
                let bytes = match positional(&arguments).first() {
                    Some(IRExpr::Const(IRConstant::Bytes(_))) => true,
                    Some(IRExpr::Variable(n)) => {
                        matches!(self.strings.get(n), Some(Source::Bytes(_)))
                            || matches!(self.params.get(n), Some(IRType::Bytes))
                    }
                    _ => false,
                };
                if bytes {
                    self.escapes_bytes = true;
                    call(&fname("b_escape"), arguments)
                } else {
                    call(&fname("escape"), arguments)
                }
            }
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
        // Match (of bytes, for a bytes pattern).
        if let IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } = expr
        {
            let t = self
                .cache
                .text_of(object)
                .or_else(|| match object.as_ref() {
                    IRExpr::Variable(n) => self.patterns.get(n).copied(),
                    _ => None,
                })
                .unwrap_or(Text::Str);
            if matches!(method_name.as_str(), "sub" | "subn") {
                if let Some(first) = arguments.first_mut() {
                    let function = match first {
                        IRExpr::Lambda { params, .. } => {
                            if let Some(p) = params.first_mut() {
                                if matches!(p.param_type, IRType::Unknown | IRType::Any) {
                                    p.param_type = t.cty("Match");
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

/// `Pattern` and `Match` imported from `re` as the classes, and
/// `Pattern[bytes]` / `Match[bytes]` (as `re.Pattern[bytes]`) as a bytes
/// pattern's.
fn rename_type(ty: &mut IRType, b: &Bindings) {
    match ty {
        IRType::Class(c) => {
            let (name, of) = match c.split_once('[') {
                Some((name, of)) => (name, of.trim_end_matches(']')),
                None => (c.as_str(), "str"),
            };
            let member = match name.strip_prefix("re.") {
                Some(m) => Some(m.to_string()),
                None => b.names.get(name).cloned(),
            }
            .filter(|m| matches!(m.as_str(), "Match" | "Pattern"));
            if let Some(member) = member {
                let t = if of == "bytes" {
                    Text::Bytes
                } else {
                    Text::Str
                };
                *c = t.class(&member);
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
            IRExpr::Const(IRConstant::String(s)) => {
                Some((var.name.clone(), Source::Str(s.clone())))
            }
            IRExpr::Const(IRConstant::Bytes(b)) => {
                Some((var.name.clone(), Source::Bytes(b.clone())))
            }
            _ => None,
        })
        .collect();
    let functions = module.functions.iter().map(|f| f.name.clone()).collect();
    let mut pass = Pass {
        b: &b,
        strings,
        patterns: HashMap::new(),
        escapes_bytes: false,
        params: HashMap::new(),
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
        if let Some(t) = pass.cache.text_of(&var.value) {
            pass.patterns.insert(var.name.clone(), t);
        }
        if let Some(t) = &mut var.var_type {
            rename_type(t, &b);
        }
    }
    for f in &mut module.functions {
        pass.params = f
            .params
            .iter()
            .map(|p| (p.name.clone(), p.param_type.clone()))
            .collect();
        pass.body(&mut f.body)?;
        f.params
            .iter_mut()
            .for_each(|p| rename_type(&mut p.param_type, &b));
        rename_type(&mut f.return_type, &b);
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            pass.params = m
                .params
                .iter()
                .map(|p| (p.name.clone(), p.param_type.clone()))
                .collect();
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
    // A program with a bytes pattern (or one that escapes bytes) has the
    // bytes classes as well; the bytes template parses as a str one.
    let mut texts = vec![Text::Str];
    if pass.cache.uses_bytes() || pass.escapes_bytes {
        texts.push(Text::Bytes);
    }
    let cache = std::mem::take(&mut pass.cache.compiled);
    let mut classes = classes::all(cache, pass.replaces_by_function, &texts);
    classes.append(&mut module.classes);
    module.classes = classes;
    module.functions.extend(vm::functions());
    module.functions.extend(classes::functions(&texts));

    let mut strings = Vec::new();
    let mut bytes = Vec::new();
    let mut collect = |e: &mut IRExpr| {
        match e {
            IRExpr::Const(IRConstant::String(s)) => strings.push(s.clone()),
            IRExpr::Const(IRConstant::Bytes(b)) => bytes.push(b.clone()),
            _ => {}
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
    for b in bytes {
        module.memory_layout.add_bytes(&b);
    }
    Ok(())
}
