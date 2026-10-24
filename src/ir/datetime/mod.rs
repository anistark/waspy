//! `datetime`, as classes written out in IR.
//!
//! `timedelta`, `date`, `time`, `datetime`, and `timezone` are classes of the
//! module (`datetime.timedelta`, ...) whose methods are CPython's own
//! algorithms, from `_pydatetime.py`, with the messages, reprs, and formats
//! of the C implementation CPython runs. The pass makes them when the module
//! imports `datetime`, and makes the program's names for them (`date` from
//! `from datetime import date`, `datetime.date` through the module) name
//! these classes.
//!
//! A value has one type here, so an argument CPython takes as an int or a
//! float is a float (`timedelta(hours=1.5)`), the `tzinfo` a datetime holds
//! is a `timezone` (a fixed offset, which is all the module defines), and
//! the local time zone is UTC, as on every WASI host.

mod calendar;
mod classes;
mod text;
mod time;

use crate::ir::build::*;
use crate::ir::{
    IRBody, IRClass, IRCompareOp, IRExpr, IRFunction, IRModule, IROp, IRParam, IRStatement, IRType,
    IRUnaryOp, IRVariable,
};
use anyhow::Result;
use std::collections::HashMap;

/// The prefix of the functions the pass makes; they are internal.
pub const DATETIME_FN_PREFIX: &str = "__datetime_";

pub(crate) const MINYEAR: i32 = 1;
pub(crate) const MAXYEAR: i32 = 9999;
/// The ordinal of 1970-01-01, where timestamps count from.
pub(crate) const EPOCH_ORDINAL: i32 = 719163;
/// The ordinal of 9999-12-31.
pub(crate) const MAX_ORDINAL: i32 = 3652059;

/// The classes, by their name in the module.
pub(crate) const CLASSES: &[&str] = &["timedelta", "date", "time", "datetime", "timezone"];

pub(crate) fn class_name(short: &str) -> String {
    format!("datetime.{short}")
}

pub(crate) fn class_ty(short: &str) -> IRType {
    IRType::Class(class_name(short))
}

fn fname(name: &str) -> String {
    format!("{DATETIME_FN_PREFIX}{name}")
}

/// A call of the pass's function `name`.
pub(crate) fn fcall(name: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&fname(name), args)
}

/// A call of class `short`'s constructor.
pub(crate) fn make(short: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&class_name(short), args)
}

pub(crate) fn function(
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

pub(crate) fn floordiv(a: IRExpr, b: IRExpr) -> IRExpr {
    bin(a, IROp::FloorDiv, b)
}

pub(crate) fn modulo(a: IRExpr, b: IRExpr) -> IRExpr {
    bin(a, IROp::Mod, b)
}

pub(crate) fn neg(a: IRExpr) -> IRExpr {
    IRExpr::UnaryOp {
        operand: Box::new(a),
        op: IRUnaryOp::Neg,
    }
}

pub(crate) fn ifexp(condition: IRExpr, body: IRExpr, orelse: IRExpr) -> IRExpr {
    IRExpr::Conditional {
        condition: Box::new(condition),
        body: Box::new(body),
        orelse: Box::new(orelse),
    }
}

/// `"a" + b + ...`.
pub(crate) fn concat(parts: Vec<IRExpr>) -> IRExpr {
    parts
        .into_iter()
        .reduce(|acc, p| bin(acc, IROp::Add, p))
        .unwrap_or_else(|| text(""))
}

/// `"%0{width}d" % n`.
pub(crate) fn pad(n: IRExpr, width: i32) -> IRExpr {
    fcall("pad", vec![n, int(width)])
}

pub(crate) fn opt(ty: &IRType) -> IRType {
    IRType::Optional(Box::new(ty.clone()))
}

/// The receiver of a method of class `short`.
pub(crate) fn me(short: &str) -> IRParam {
    param("self", &class_ty(short))
}

pub(crate) fn cls(short: &str) -> IRParam {
    param("cls", &class_ty(short))
}

pub(crate) fn classmethod(mut f: IRFunction) -> IRFunction {
    f.decorators = vec!["classmethod".to_string()];
    f
}

/// A class of the module: immutable once made, as CPython's are.
pub(crate) fn class(
    short: &str,
    bases: Vec<String>,
    methods: Vec<IRFunction>,
    mut class_vars: Vec<IRVariable>,
) -> IRClass {
    class_vars.push(IRVariable {
        name: crate::ir::FROZEN_VAR.to_string(),
        value: boolean(true),
        var_type: None,
    });
    IRClass {
        name: class_name(short),
        bases,
        methods,
        class_vars,
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// What the module calls the classes by.
struct Bindings {
    /// `from datetime import date [as d]`: the name, and the class.
    names: HashMap<String, String>,
    /// `import datetime [as dt]`.
    modules: Vec<String>,
}

impl Bindings {
    fn of(module: &IRModule) -> Bindings {
        let mut names = HashMap::new();
        let mut modules = Vec::new();
        for imp in &module.imports {
            if imp.module != "datetime" {
                continue;
            }
            match (&imp.name, imp.is_from_import) {
                (Some(name), true) if CLASSES.contains(&name.as_str()) => {
                    names.insert(
                        imp.alias.clone().unwrap_or_else(|| name.clone()),
                        class_name(name),
                    );
                }
                (None, false) => {
                    modules.push(imp.alias.clone().unwrap_or_else(|| "datetime".to_string()))
                }
                _ => {}
            }
        }
        Bindings { names, modules }
    }

    /// The class an expression names: a bound name, or `datetime.<class>`.
    fn class_named(&self, expr: &IRExpr) -> Option<String> {
        match expr {
            IRExpr::Variable(n) => self.names.get(n).cloned(),
            IRExpr::Attribute { object, attribute } => match object.as_ref() {
                IRExpr::Variable(m)
                    if self.modules.contains(m) && CLASSES.contains(&attribute.as_str()) =>
                {
                    Some(class_name(attribute))
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// Make the program's names for the classes name them, in an expression.
fn rewrite_expr(expr: &mut IRExpr, b: &Bindings) -> Result<()> {
    // `date(...)`, `datetime.date(...)`.
    match expr {
        IRExpr::FunctionCall { function_name, .. } => {
            if let Some(c) = b.names.get(function_name.as_str()) {
                *function_name = c.clone();
            }
        }
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => {
            if let IRExpr::Variable(m) = object.as_ref() {
                if b.modules.contains(m) && CLASSES.contains(&method_name.as_str()) {
                    *expr = IRExpr::FunctionCall {
                        function_name: class_name(method_name),
                        arguments: std::mem::take(arguments),
                    };
                    return rewrite_expr(expr, b);
                }
            }
            if let Some(c) = b.class_named(object) {
                **object = IRExpr::Variable(c);
            }
        }
        IRExpr::Attribute { object, .. } => {
            if let Some(c) = b.class_named(object) {
                **object = IRExpr::Variable(c);
            }
        }
        IRExpr::Variable(n) => {
            if let Some(c) = b.names.get(n.as_str()) {
                *n = c.clone();
            }
        }
        _ => {}
    }
    if let IRExpr::Attribute { object, attribute } = expr {
        if let IRExpr::Variable(m) = object.as_ref() {
            if b.modules.contains(m) && CLASSES.contains(&attribute.as_str()) {
                *expr = IRExpr::Variable(class_name(attribute));
            }
        }
    }
    calls(expr)
}

/// What the classes' calls need beyond their names: a datetime is an
/// instance of `date` (it is a class of its own here), `replace` and
/// `combine` learn whether `tzinfo` was passed, and `strptime`'s format is
/// one this port reads.
fn calls(expr: &mut IRExpr) -> Result<()> {
    let date = class_name("date");
    let datetime = class_name("datetime");
    match expr {
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if function_name == "isinstance" && arguments.len() == 2 => match &mut arguments[1] {
            IRExpr::Variable(t) if *t == date => {
                arguments[1] = IRExpr::TupleLiteral(vec![v(&date), v(&datetime)]);
            }
            IRExpr::TupleLiteral(items)
                if items
                    .iter()
                    .any(|t| matches!(t, IRExpr::Variable(n) if *n == date)) =>
            {
                items.push(v(&datetime));
            }
            _ => {}
        },
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if function_name == "issubclass"
            && matches!(arguments.as_slice(), [IRExpr::Variable(a), IRExpr::Variable(c)] if *a == datetime && *c == date) =>
        {
            *expr = boolean(true);
        }
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => {
            let positional = arguments
                .iter()
                .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
                .count();
            let by_name = arguments
                .iter()
                .any(|a| matches!(a, IRExpr::Keyword { name, .. } if name == "tzinfo"));
            let given = |at_end: bool| {
                if at_end {
                    boolean(true)
                } else {
                    IRExpr::Keyword {
                        name: time::TZINFO_GIVEN.to_string(),
                        value: Box::new(boolean(true)),
                    }
                }
            };
            let on_datetime = matches!(object.as_ref(), IRExpr::Variable(c) if *c == datetime);
            match method_name.as_str() {
                "replace" if positional >= 5 => {
                    return Err(refused(
                        "replace() of a time or datetime with its fields passed by position \
                         is not supported. Hint: pass them by name"
                            .to_string(),
                    ))
                }
                "replace" if by_name => arguments.push(given(false)),
                "combine" if on_datetime && positional == 3 && !by_name => {
                    arguments.push(given(true))
                }
                "combine" if on_datetime && by_name => arguments.push(given(false)),
                "strptime" if on_datetime => check_strptime_format(arguments.get(1))?,
                _ => {}
            }
        }
        _ => {}
    }
    Ok(())
}

/// A `strptime` format this port reads: a constant, whose directives are
/// ones it implements. One CPython reads and it does not (`%G`, `%V`, `%C`,
/// the `%O` and `%E` forms, flags and widths) is refused; one CPython calls
/// a bad directive is left to raise its ValueError.
fn check_strptime_format(format: Option<&IRExpr>) -> Result<()> {
    let Some(IRExpr::Const(crate::ir::IRConstant::String(fmt))) = format else {
        return Err(refused(
            "datetime.strptime() is supported with a constant format string".to_string(),
        ));
    };
    let mut spelled = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            spelled.push(c);
            continue;
        }
        match chars.next() {
            Some(k) if "GVCOE-_0^#123456789".contains(k) => {
                return Err(refused(format!(
                    "the strptime() directive '%{k}' in {fmt:?} is not supported"
                )))
            }
            Some(k) => match time::STRPTIME_COMPOSITES.iter().find(|(c, _)| *c == k) {
                Some((_, s)) => spelled.push_str(s),
                None => {
                    spelled.push('%');
                    spelled.push(k);
                }
            },
            None => spelled.push('%'),
        }
    }
    // CPython's expression names a group per directive, and refuses one named
    // twice.
    let mut seen = std::collections::HashSet::new();
    let mut chars = spelled.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        let Some(k) = chars.next() else { break };
        let group = match k {
            'e' => 'd',
            'k' => 'H',
            'l' => 'I',
            'P' => 'p',
            '%' => continue,
            k => k,
        };
        if time::STRPTIME_DIRECTIVES.contains(k) && !seen.insert(group) {
            return Err(refused(format!(
                "the strptime() format {fmt:?} reads a field twice, which CPython refuses \
                 (re.error: redefinition of group name)"
            )));
        }
    }
    Ok(())
}

/// The type a program's annotation names, made the class's.
fn rewrite_type(ty: &mut IRType, b: &Bindings) {
    match ty {
        IRType::Class(c) => {
            if let Some(full) = b.names.get(c.as_str()) {
                *c = full.clone();
            }
        }
        IRType::List(e)
        | IRType::Set(e)
        | IRType::VarTuple(e)
        | IRType::Optional(e)
        | IRType::Generator(e) => rewrite_type(e, b),
        IRType::Dict(k, v) => {
            rewrite_type(k, b);
            rewrite_type(v, b);
        }
        IRType::Tuple(ms) | IRType::Union(ms) => ms.iter_mut().for_each(|m| rewrite_type(m, b)),
        IRType::Callable {
            params,
            return_type,
        } => {
            params.iter_mut().for_each(|m| rewrite_type(m, b));
            rewrite_type(return_type, b);
        }
        _ => {}
    }
}

fn rewrite_body(body: &mut IRBody, b: &Bindings) -> Result<()> {
    crate::ir::visit_body(body, &mut |e: &mut IRExpr| rewrite_expr(e, b))?;
    fn types(body: &mut IRBody, b: &Bindings) {
        for stmt in &mut body.statements {
            match stmt {
                IRStatement::Assign {
                    var_type: Some(t), ..
                }
                | IRStatement::AttributeAssign {
                    annotation: Some(t),
                    ..
                } => rewrite_type(t, b),
                _ => {}
            }
            for nested in nested_bodies_mut(stmt) {
                types(nested, b);
            }
        }
    }
    types(body, b);
    Ok(())
}

fn nested_bodies_mut(stmt: &mut IRStatement) -> Vec<&mut IRBody> {
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
        _ => Vec::new(),
    }
}

/// The calendar functions and `strftime`, which `logging` renders a
/// record's time with: `(fmt, y, mo, d, hh, mi, ss, us, %z, %Z) -> str`,
/// called as [`fname`]`("strftime")`.
pub(crate) fn strftime_functions() -> Vec<IRFunction> {
    let mut out = calendar::functions();
    out.push(text::strftime());
    out
}

/// The name a function of this pass is called by.
pub(crate) fn function_name(name: &str) -> String {
    fname(name)
}

/// Make the `datetime` classes when the module imports `datetime`, and make
/// its names for them name them.
pub fn lower_datetime(module: &mut IRModule) -> Result<()> {
    let b = Bindings::of(module);
    if b.names.is_empty() && b.modules.is_empty() {
        return Ok(());
    }
    // A name the module defines itself is its own.
    let own: std::collections::HashSet<String> = module
        .classes
        .iter()
        .map(|c| c.name.clone())
        .chain(module.functions.iter().map(|f| f.name.clone()))
        .collect();
    let b = Bindings {
        names: b
            .names
            .into_iter()
            .filter(|(n, _)| !own.contains(n))
            .collect(),
        modules: b.modules,
    };
    for f in &mut module.functions {
        rewrite_body(&mut f.body, &b)?;
        f.params
            .iter_mut()
            .for_each(|p| rewrite_type(&mut p.param_type, &b));
        rewrite_type(&mut f.return_type, &b);
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            rewrite_body(&mut m.body, &b)?;
            m.params
                .iter_mut()
                .for_each(|p| rewrite_type(&mut p.param_type, &b));
            rewrite_type(&mut m.return_type, &b);
        }
        for var in &mut c.class_vars {
            rewrite_expr(&mut var.value, &b)?;
            if let Some(t) = &mut var.var_type {
                rewrite_type(t, &b);
            }
        }
    }
    for var in &mut module.variables {
        let mut body = IRBody {
            statements: vec![IRStatement::Expression(var.value.clone())],
        };
        rewrite_body(&mut body, &b)?;
        if let Some(IRStatement::Expression(e)) = body.statements.pop() {
            var.value = e;
        }
        if let Some(t) = &mut var.var_type {
            rewrite_type(t, &b);
        }
    }
    // `logging` may have made the calendar functions already.
    let defined: std::collections::HashSet<String> =
        module.functions.iter().map(|f| f.name.clone()).collect();
    module.functions.extend(
        calendar::functions()
            .into_iter()
            .chain(text::functions())
            .filter(|f| !defined.contains(&f.name)),
    );
    let mut classes = classes::all(&mut module.memory_layout)?;
    module.classes.append(&mut classes);
    // The strings the classes and functions use are interned like the
    // program's.
    let mut strings = Vec::new();
    let mut collect = |e: &mut IRExpr| {
        if let IRExpr::Const(crate::ir::IRConstant::String(s)) = e {
            strings.push(s.clone());
        }
        Ok(())
    };
    for f in module
        .functions
        .iter_mut()
        .filter(|f| f.name.starts_with(DATETIME_FN_PREFIX))
    {
        let _ = crate::ir::visit_body(&mut f.body, &mut collect);
        for p in &mut f.params {
            if let Some(d) = &mut p.default_value {
                let _ = collect(d);
            }
        }
    }
    for c in module
        .classes
        .iter_mut()
        .filter(|c| c.name.starts_with("datetime."))
    {
        for m in &mut c.methods {
            let _ = crate::ir::visit_body(&mut m.body, &mut collect);
            for p in &mut m.params {
                if let Some(d) = &mut p.default_value {
                    let _ = collect(d);
                }
            }
        }
        for var in &mut c.class_vars {
            let mut body = IRBody {
                statements: vec![IRStatement::Expression(var.value.clone())],
            };
            let _ = crate::ir::visit_body(&mut body, &mut collect);
        }
    }
    for s in strings {
        module.memory_layout.add_string(&s);
    }
    Ok(())
}
