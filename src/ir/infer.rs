//! Types as the IR shows them, before code generation: what a pass that
//! synthesizes code (`itertools`, `collections`) can learn about a value from
//! literals, annotations, the declared results of the module's functions, and
//! what generators yield. Anything else is `Unknown`.

use super::build::known;
use crate::ir::{
    IRBody, IRComprehensionKind, IRConstant, IRExpr, IRModule, IROp, IRStatement, IRType, IRUnaryOp,
};
use std::collections::HashMap;

pub(crate) struct Types {
    /// The module's functions' declared results.
    pub(crate) returns: HashMap<String, IRType>,
    /// What each generator function (the module's and the synthesized ones)
    /// yields.
    pub(crate) yields: HashMap<String, IRType>,
    /// The module's classes, whose calls make instances.
    pub(crate) classes: std::collections::HashSet<String>,
    /// Each class's bases, for a method or field it inherits.
    pub(crate) bases: HashMap<String, Vec<String>>,
    /// Each class's methods' declared results, keyed `(class, method)`.
    pub(crate) methods: HashMap<(String, String), IRType>,
    /// Each class's fields whose type a method states: an annotated
    /// `self.x: T = ...`, or `self.x = p` of an annotated parameter `p`.
    pub(crate) fields: HashMap<(String, String), IRType>,
    /// `import js`, and the names imported from `js`: JavaScript values.
    pub(crate) js_module: bool,
    pub(crate) js_names: std::collections::HashSet<String>,
}

impl Types {
    pub(crate) fn of(&self, expr: &IRExpr, env: &HashMap<String, IRType>) -> IRType {
        if let Some(js) = self.js_of(expr, env) {
            return js;
        }
        match expr {
            IRExpr::Const(IRConstant::Int(_)) => IRType::Int,
            IRExpr::Const(IRConstant::Float(_)) => IRType::Float,
            IRExpr::Const(IRConstant::String(_)) => IRType::String,
            IRExpr::Const(IRConstant::Bool(_)) => IRType::Bool,
            IRExpr::Const(IRConstant::Bytes(_)) => IRType::Bytes,
            IRExpr::Const(IRConstant::None) => IRType::None,
            IRExpr::Variable(n) | IRExpr::Param(n) => {
                env.get(n).cloned().unwrap_or(IRType::Unknown)
            }
            IRExpr::ListLiteral(items) => IRType::List(Box::new(self.common(items, env))),
            IRExpr::SetLiteral(items) => IRType::Set(Box::new(self.common(items, env))),
            IRExpr::TupleLiteral(items) => {
                IRType::Tuple(items.iter().map(|i| self.of(i, env)).collect())
            }
            IRExpr::DictLiteral(entries) => {
                let keys: Vec<IRExpr> = entries.iter().map(|(k, _)| k.clone()).collect();
                let values: Vec<IRExpr> = entries.iter().map(|(_, v)| v.clone()).collect();
                IRType::Dict(
                    Box::new(self.common(&keys, env)),
                    Box::new(self.common(&values, env)),
                )
            }
            IRExpr::RangeCall { .. } => IRType::Range,
            IRExpr::Comprehension {
                kind,
                element,
                generators,
                ..
            } => {
                let mut inner = env.clone();
                for g in generators {
                    let elem = self.element(&self.of(&g.iterable, &inner));
                    match g.targets.as_slice() {
                        [t] => {
                            inner.insert(t.clone(), elem);
                        }
                        targets => {
                            if let IRType::Tuple(ms) = elem {
                                for (t, m) in targets.iter().zip(ms) {
                                    inner.insert(t.clone(), m);
                                }
                            }
                        }
                    }
                }
                let e = self.of(element, &inner);
                match kind {
                    IRComprehensionKind::Set => IRType::Set(Box::new(e)),
                    _ => IRType::List(Box::new(e)),
                }
            }
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } => {
                if self.yields.contains_key(function_name) {
                    return IRType::Class(format!("__Gen_{function_name}"));
                }
                if function_name == crate::ir::AS_TUPLE_FN {
                    return match arguments.first().map(|a| self.of(a, env)) {
                        Some(IRType::List(e)) => IRType::VarTuple(e),
                        _ => IRType::Unknown,
                    };
                }
                if let Some(r) = self.returns.get(function_name) {
                    return r.clone();
                }
                if self.classes.contains(function_name) {
                    return IRType::Class(function_name.clone());
                }
                match function_name.as_str() {
                    crate::ir::ANY_GEN_FN | crate::ir::ALL_GEN_FN => IRType::Bool,
                    "str" | "repr" | "chr" | "input" => IRType::String,
                    "bytes" => IRType::Bytes,
                    "int" | "len" | "ord" => IRType::Int,
                    "float" => IRType::Float,
                    "sorted" | "__sorted_desc" => match arguments.first() {
                        Some(a) => IRType::List(Box::new(self.element(&self.of(a, env)))),
                        None => IRType::Unknown,
                    },
                    _ => IRType::Unknown,
                }
            }
            // `C().<from>(src)` is the `C` it fills.
            IRExpr::MethodCall {
                object,
                method_name,
                ..
            } if method_name == crate::ir::FROM_METHOD => self.of(object, env),
            IRExpr::MethodCall {
                object,
                method_name,
                ..
            } => match (self.of(object, env), method_name.as_str()) {
                (IRType::String, "split" | "splitlines") => IRType::List(Box::new(IRType::String)),
                (
                    IRType::String,
                    "upper" | "lower" | "strip" | "lstrip" | "rstrip" | "replace" | "title"
                    | "join" | "capitalize",
                ) => IRType::String,
                (IRType::Dict(k, _), "keys") => IRType::List(k),
                (IRType::Dict(_, val), "values") => IRType::List(val),
                (IRType::Dict(k, val), "items") => {
                    IRType::List(Box::new(IRType::Tuple(vec![*k, *val])))
                }
                (IRType::Class(class), method) => self
                    .member(&class, |c| {
                        self.methods.get(&(c.to_string(), method.to_string()))
                    })
                    .unwrap_or(IRType::Unknown),
                _ => IRType::Unknown,
            },
            IRExpr::Attribute { object, attribute } => match self.of(object, env) {
                IRType::Class(class) => self
                    .member(&class, |c| {
                        self.fields.get(&(c.to_string(), attribute.to_string()))
                    })
                    .unwrap_or(IRType::Unknown),
                _ => IRType::Unknown,
            },
            IRExpr::BinaryOp { left, right, op } => {
                let (l, r) = (self.of(left, env), self.of(right, env));
                match op {
                    IROp::Div => IRType::Float,
                    _ if l == IRType::Float || r == IRType::Float => IRType::Float,
                    _ if l == r => l,
                    _ => IRType::Unknown,
                }
            }
            IRExpr::UnaryOp { operand, op } => match op {
                IRUnaryOp::Not => IRType::Bool,
                _ => self.of(operand, env),
            },
            _ => IRType::Unknown,
        }
    }

    /// A JavaScript value, when `expr` is one: a global of `js`, a name
    /// imported from it, a module loaded at run time, or an attribute, a
    /// call, or an item of a JavaScript value.
    fn js_of(&self, expr: &IRExpr, env: &HashMap<String, IRType>) -> Option<IRType> {
        let js_module = |e: &IRExpr| {
            self.js_module
                && matches!(e, IRExpr::Variable(m) if m == "js" && !env.contains_key("js"))
        };
        let js_name = |n: &str| self.js_names.contains(n) && !env.contains_key(n);
        let proxy = |e: &IRExpr| self.of(e, env) == IRType::JsProxy;
        let is_js = match expr {
            IRExpr::Variable(n) => js_name(n),
            IRExpr::Attribute { object, .. } => js_module(object) || proxy(object),
            IRExpr::MethodCall {
                object,
                method_name,
                ..
            } => (js_module(object) || proxy(object)) && method_name != "to_py",
            IRExpr::FunctionCall { function_name, .. } => {
                js_name(function_name)
                    || function_name == "__js_import"
                    || env.get(function_name) == Some(&IRType::JsProxy)
            }
            IRExpr::Indexing { container, .. } => proxy(container),
            _ => false,
        };
        is_js.then_some(IRType::JsProxy)
    }

    /// What `look` finds on `class` or the nearest of its bases that has it.
    fn member<'a>(
        &'a self,
        class: &str,
        look: impl Fn(&str) -> Option<&'a IRType>,
    ) -> Option<IRType> {
        let mut pending = vec![class.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(c) = pending.pop() {
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(t) = look(&c) {
                return Some(t.clone());
            }
            if let Some(bases) = self.bases.get(&c) {
                pending.extend(bases.iter().rev().cloned());
            }
        }
        None
    }

    pub(crate) fn of_in(&self, env: &HashMap<String, IRType>, expr: &IRExpr) -> IRType {
        self.of(expr, env)
    }

    pub(crate) fn common(&self, items: &[IRExpr], env: &HashMap<String, IRType>) -> IRType {
        let mut types = items.iter().map(|i| self.of(i, env));
        let Some(first) = types.next() else {
            return IRType::Unknown;
        };
        if types.all(|t| t == first) {
            first
        } else {
            IRType::Unknown
        }
    }

    /// What iterating a value of `ty` yields.
    pub(crate) fn element(&self, ty: &IRType) -> IRType {
        match ty {
            IRType::List(e) | IRType::Set(e) | IRType::VarTuple(e) | IRType::Generator(e) => {
                (**e).clone()
            }
            IRType::Tuple(ms) => match ms.first() {
                Some(first) if ms.iter().all(|m| m == first) => first.clone(),
                _ => IRType::Unknown,
            },
            IRType::String => IRType::String,
            IRType::Bytes | IRType::Range => IRType::Int,
            IRType::Dict(k, _) => (**k).clone(),
            IRType::Class(c) => c
                .strip_prefix("__Gen_")
                .and_then(|f| self.yields.get(f))
                .cloned()
                .unwrap_or(IRType::Unknown),
            _ => IRType::Unknown,
        }
    }

    /// Whether a value of `ty` is an iterator (it is consumed by iterating).
    pub(crate) fn is_iterator(&self, ty: &IRType) -> bool {
        matches!(ty, IRType::Class(c) if c.starts_with("__Gen_"))
    }
}

/// Record what a statement binds, after its expressions are rewritten.
pub(crate) fn bind(stmt: &IRStatement, types: &Types, env: &mut HashMap<String, IRType>) {
    match stmt {
        IRStatement::Assign {
            target,
            value,
            var_type,
        } => {
            let ty = var_type
                .clone()
                .filter(known)
                .unwrap_or_else(|| types.of(value, env));
            env.entry(target.clone()).or_insert(ty);
        }
        IRStatement::For {
            target, iterable, ..
        } => {
            let elem = types.element(&types.of(iterable, env));
            env.entry(target.clone()).or_insert(elem);
        }
        IRStatement::TupleUnpack { targets, value, .. } => {
            if let IRType::Tuple(ms) = types.of(value, env) {
                for (t, m) in targets.iter().zip(ms) {
                    env.entry(t.clone()).or_insert(m);
                }
            }
        }
        _ => {}
    }
}

impl Types {
    /// The module's declared results, generators' yields, and classes.
    pub(crate) fn of_module(module: &IRModule) -> Types {
        Types {
            returns: module
                .functions
                .iter()
                .filter(|f| !matches!(f.return_type, IRType::Unknown))
                .map(|f| (f.name.clone(), f.return_type.clone()))
                .collect(),
            yields: module
                .functions
                .iter()
                .filter(|f| crate::ir::body_has_yield(&f.body))
                .map(|f| {
                    let y = match &f.return_type {
                        IRType::Generator(t) if known(t) => (**t).clone(),
                        _ => crate::ir::generators_yielded_type(&f.body, &f.params),
                    };
                    (f.name.clone(), y)
                })
                .collect(),
            classes: module.classes.iter().map(|c| c.name.clone()).collect(),
            bases: module
                .classes
                .iter()
                .map(|c| (c.name.clone(), c.bases.clone()))
                .collect(),
            methods: module
                .classes
                .iter()
                .flat_map(|c| {
                    c.methods
                        .iter()
                        .filter(|m| fully_known(&m.return_type))
                        .map(|m| ((c.name.clone(), m.name.clone()), m.return_type.clone()))
                })
                .collect(),
            js_module: module
                .imports
                .iter()
                .any(|i| i.module == "js" && !i.is_from_import),
            js_names: module
                .imports
                .iter()
                .filter(|i| i.module == "js" && i.is_from_import)
                .filter_map(|i| i.alias.clone().or_else(|| i.name.clone()))
                .collect(),
            fields: module
                .classes
                .iter()
                .flat_map(|c| {
                    let mut out = Vec::new();
                    for m in &c.methods {
                        let params: HashMap<&str, &IRType> = m
                            .params
                            .iter()
                            .map(|p| (p.name.as_str(), &p.param_type))
                            .collect();
                        stated_fields(&m.body, &params, &mut out);
                    }
                    out.into_iter()
                        .map(|(f, t)| ((c.name.clone(), f), t))
                        .collect::<Vec<_>>()
                })
                .collect(),
        }
    }
}

/// Whether a type is known all the way down: no part of it is `Unknown` or
/// `Any`, so it can be written as a place's annotation.
pub(crate) fn fully_known(ty: &IRType) -> bool {
    match ty {
        IRType::Unknown | IRType::Any => false,
        IRType::List(e)
        | IRType::Set(e)
        | IRType::VarTuple(e)
        | IRType::Optional(e)
        | IRType::Generator(e)
        | IRType::Coroutine(e) => fully_known(e),
        IRType::Dict(k, v) => fully_known(k) && fully_known(v),
        IRType::Tuple(ms) | IRType::Union(ms) => ms.iter().all(fully_known),
        IRType::Callable {
            params,
            return_type,
        } => params.iter().all(fully_known) && fully_known(return_type),
        _ => true,
    }
}

/// The fields a method body states the type of: `self.x: T = ...`, and
/// `self.x = p` for a parameter `p` annotated with a type known all the way
/// down. The first statement of a field is the one kept.
fn stated_fields(body: &IRBody, params: &HashMap<&str, &IRType>, out: &mut Vec<(String, IRType)>) {
    for stmt in &body.statements {
        if let IRStatement::AttributeAssign {
            object,
            attribute,
            value,
            annotation,
        } = stmt
        {
            if matches!(object, IRExpr::Variable(n) | IRExpr::Param(n) if n == "self") {
                let ty = match (annotation, value) {
                    (Some(t), _) if fully_known(t) => Some(t.clone()),
                    (None, IRExpr::Variable(p)) => params
                        .get(p.as_str())
                        .filter(|t| fully_known(t))
                        .map(|t| (*t).clone()),
                    _ => None,
                };
                if let Some(ty) = ty {
                    if !out.iter().any(|(f, _)| f == attribute) {
                        out.push((attribute.clone(), ty));
                    }
                }
            }
        }
        for nested in nested_bodies(stmt) {
            stated_fields(nested, params, out);
        }
    }
}

/// Visit every expression of a body, subexpressions included, in source
/// order, with what the statements before it bind. Nested bodies share the
/// environment, and the first binding of a name is the one kept.
pub(crate) fn walk_body(
    body: &IRBody,
    types: &Types,
    env: &mut HashMap<String, IRType>,
    f: &mut impl FnMut(&IRExpr, &HashMap<String, IRType>),
) {
    for stmt in &body.statements {
        walk_stmt(stmt, types, env, f);
    }
}

fn walk_stmt(
    stmt: &IRStatement,
    types: &Types,
    env: &mut HashMap<String, IRType>,
    f: &mut impl FnMut(&IRExpr, &HashMap<String, IRType>),
) {
    let mut exprs: Vec<&IRExpr> = Vec::new();
    let mut bodies: Vec<&IRBody> = Vec::new();
    match stmt {
        IRStatement::Return(Some(e)) | IRStatement::Yield { value: Some(e) } => exprs.push(e),
        IRStatement::Assign { value, .. }
        | IRStatement::AugAssign { value, .. }
        | IRStatement::TupleUnpack { value, .. }
        | IRStatement::Expression(value)
        | IRStatement::DynamicImport {
            module_name: value, ..
        } => exprs.push(value),
        IRStatement::Raise { exception, cause } => {
            exprs.extend(exception.iter());
            exprs.extend(cause.iter());
        }
        IRStatement::If {
            condition,
            then_body,
            else_body,
        } => {
            exprs.push(condition);
            bodies.push(then_body);
            bodies.extend(else_body.iter().map(|b| b.as_ref()));
        }
        IRStatement::While { condition, body } => {
            exprs.push(condition);
            bodies.push(body);
        }
        IRStatement::For {
            iterable,
            body,
            else_body,
            ..
        } => {
            exprs.push(iterable);
            bodies.push(body);
            bodies.extend(else_body.iter().map(|b| b.as_ref()));
        }
        IRStatement::With {
            context_expr, body, ..
        } => {
            exprs.push(context_expr);
            bodies.push(body);
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            bodies.push(try_body);
            bodies.extend(except_handlers.iter().map(|h| &h.body));
            bodies.extend(finally_body.iter().map(|b| b.as_ref()));
        }
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => {
            exprs.push(object);
            exprs.push(value);
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            exprs.push(container);
            exprs.push(index);
            exprs.push(value);
        }
        IRStatement::FunctionDef(func) => bodies.push(&func.body),
        _ => {}
    }
    for e in exprs {
        walk_expr(e, env, f);
    }
    bind(stmt, types, env);
    for b in bodies {
        walk_body(b, types, env, f);
    }
}

fn walk_expr(
    expr: &IRExpr,
    env: &HashMap<String, IRType>,
    f: &mut impl FnMut(&IRExpr, &HashMap<String, IRType>),
) {
    f(expr, env);
    let mut go = |e: &IRExpr| walk_expr(e, env, f);
    match expr {
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            go(left);
            go(right);
        }
        IRExpr::UnaryOp { operand, .. } => go(operand),
        IRExpr::FunctionCall { arguments, .. } => arguments.iter().for_each(go),
        IRExpr::MethodCall {
            object, arguments, ..
        } => {
            go(object);
            arguments.iter().for_each(go);
        }
        IRExpr::Keyword { value, .. } | IRExpr::CellStore { value, .. } => go(value),
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => {
            go(condition);
            go(body);
            go(orelse);
        }
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            items.iter().for_each(go)
        }
        IRExpr::DictLiteral(pairs) => {
            for (k, v) in pairs {
                go(k);
                go(v);
            }
        }
        IRExpr::Indexing { container, index } => {
            go(container);
            go(index);
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            go(container);
            for part in [start, end, step].into_iter().flatten() {
                go(part);
            }
        }
        IRExpr::Attribute { object, .. } => go(object),
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            for g in generators {
                go(&g.iterable);
                g.conditions.iter().for_each(&mut go);
            }
            go(element);
            if let Some(v) = value {
                go(v);
            }
        }
        IRExpr::DynamicImportExpr { module_name } => go(module_name),
        IRExpr::RangeCall { start, stop, step } => {
            go(stop);
            for part in [start, step].into_iter().flatten() {
                go(part);
            }
        }
        IRExpr::Lambda { body, .. } => go(body),
        _ => {}
    }
}

/// The bodies a statement holds.
pub(crate) fn nested_bodies(stmt: &IRStatement) -> Vec<&IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            let mut v = vec![then_body.as_ref()];
            v.extend(else_body.iter().map(|b| b.as_ref()));
            v
        }
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body.as_ref()],
        IRStatement::For {
            body, else_body, ..
        } => {
            let mut v = vec![body.as_ref()];
            v.extend(else_body.iter().map(|b| b.as_ref()));
            v
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            let mut v = vec![try_body.as_ref()];
            v.extend(except_handlers.iter().map(|h| &h.body));
            v.extend(finally_body.iter().map(|b| b.as_ref()));
            v
        }
        _ => Vec::new(),
    }
}
