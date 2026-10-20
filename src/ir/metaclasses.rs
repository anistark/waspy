//! Metaclasses, applied at compile time.
//!
//! A metaclass is a class deriving from `type`; `class C(metaclass=M)` (or a
//! base's metaclass, inherited) has M make C. Classes are not created at run
//! time here, so M's methods are specialized for each class it makes, with
//! the class itself (`cls`), its name, and its bases bound statically:
//!
//! - `M.__new__(mcs, name, bases, namespace)` and `M.__init__(cls, name,
//!   bases, namespace)` run once, when the module is initialized, at the
//!   place the class statement stands. `__new__` has the shape
//!   `cls = super().__new__(mcs, name, bases, namespace)`, statements, then
//!   `return cls`. An attribute they give the class under a constant name
//!   (`cls.registry = []`) is a class variable of the class, defined there.
//! - `M.__call__(cls, *args, **kwargs)` replaces the class's instantiation:
//!   every `C(...)` calls it, specialized as a decorator's wrapper is (with
//!   C's `__init__` parameters), and `super().__call__(*args, **kwargs)`
//!   inside it builds the instance.
//! - Any other method of M is a method of the class (`C.describe()`), and a
//!   class variable of M is read through the class (`C.registry`) when the
//!   class has none of its own, as CPython looks a class attribute up.
//!
//! Refused, since they would need a class created at run time: the
//! `namespace` argument (attributes under computed names), `__prepare__`, a
//! metaclass called directly (`M("X", (), {})`), a `super()` from a
//! metaclass method reaching another metaclass's, and decorated metaclass
//! methods.

use crate::ir::{
    IRBody, IRConstant, IRExpr, IRFunction, IRModule, IRParam, IRStatement, IRType, IRVariable,
    ParamStar,
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// The prefix of the functions a metaclass is specialized into.
pub const METACLASS_FN_PREFIX: &str = "__metaclass_";

fn refuse(message: String) -> anyhow::Error {
    crate::core::errors::unsupported_feature(message, None).into()
}

/// The classes of the module by name, with their bases and members.
struct Classes<'a> {
    by_name: HashMap<&'a str, &'a crate::ir::IRClass>,
}

impl<'a> Classes<'a> {
    fn new(module: &'a IRModule) -> Self {
        Classes {
            by_name: module
                .classes
                .iter()
                .map(|c| (c.name.as_str(), c))
                .collect(),
        }
    }

    /// The class and its bases, depth first in base order (the order a
    /// lookup in a single-inheritance chain takes, and the C3 order for the
    /// hierarchies a metaclass is usually part of).
    fn chain(&self, class: &str) -> Vec<&'a crate::ir::IRClass> {
        let mut out: Vec<&crate::ir::IRClass> = Vec::new();
        let mut pending = vec![class.to_string()];
        while let Some(c) = pending.pop() {
            let Some(cls) = self.by_name.get(c.as_str()) else {
                continue;
            };
            if out.iter().any(|k| k.name == cls.name) {
                continue;
            }
            out.push(cls);
            pending.extend(cls.bases.iter().rev().cloned());
        }
        out
    }

    fn is_metaclass(&self, class: &str) -> bool {
        let mut pending = vec![class.to_string()];
        let mut seen = HashSet::new();
        while let Some(c) = pending.pop() {
            if c == "type" {
                return true;
            }
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(cls) = self.by_name.get(c.as_str()) {
                pending.extend(cls.bases.iter().cloned());
            }
        }
        false
    }

    fn is_subclass(&self, sub: &str, base: &str) -> bool {
        self.chain(sub).iter().any(|c| c.name == base)
    }

    /// The method `name` as `class` sees it, with the class defining it.
    fn method(&self, class: &str, name: &str) -> Option<(String, IRFunction)> {
        self.chain(class).into_iter().find_map(|c| {
            c.methods
                .iter()
                .find(|m| m.name == name)
                .map(|m| (c.name.clone(), m.clone()))
        })
    }

    /// Whether `class` has the class attribute `name` along its bases.
    fn has_var(&self, class: &str, name: &str) -> bool {
        self.chain(class).iter().any(|c| {
            c.class_vars.iter().any(|v| v.name == name) || c.deferred_vars.iter().any(|v| v == name)
        })
    }

    /// Whether `class` itself (not a base) has the class attribute `name`.
    fn has_own_var(&self, class: &str, name: &str) -> bool {
        self.by_name.get(class).is_some_and(|c| {
            c.class_vars.iter().any(|v| v.name == name) || c.deferred_vars.iter().any(|v| v == name)
        })
    }

    /// The class past `class` along its bases that has `name`, a class
    /// variable or a metaclass's attribute.
    fn inherited_owner(&self, class: &str, name: &str) -> Option<String> {
        self.chain(class).into_iter().skip(1).find_map(|c| {
            (c.class_vars.iter().any(|v| v.name == name)
                || c.deferred_vars.iter().any(|v| v == name))
            .then(|| c.name.clone())
        })
    }

    /// The class along `class`'s bases whose body defines `name`.
    fn var_owner(&self, class: &str, name: &str) -> Option<String> {
        self.chain(class)
            .into_iter()
            .find(|c| c.class_vars.iter().any(|v| v.name == name))
            .map(|c| c.name.clone())
    }
}

/// Apply every metaclass in the module.
pub fn apply_metaclasses(module: &mut IRModule) -> Result<()> {
    let metas: HashSet<String> = {
        let classes = Classes::new(module);
        module
            .classes
            .iter()
            .filter(|c| classes.is_metaclass(&c.name))
            .map(|c| c.name.clone())
            .collect()
    };
    for class in &mut module.classes {
        // `metaclass=type` is what every class has.
        if class.metaclass.as_deref() == Some("type") {
            class.metaclass = None;
        }
        if let Some(meta) = &class.metaclass {
            if !metas.contains(meta) {
                return Err(refuse(format!(
                    "class '{}' names '{meta}' as its metaclass, which is not a class this \
                     module defines deriving from 'type'",
                    class.name
                )));
            }
        }
        // A metaclass's base 'type' is not a class here.
        class.bases.retain(|b| b != "type");
    }
    if metas.is_empty() {
        return Ok(());
    }

    // Each class's metaclass: its own, or its bases'; the most derived of
    // them, which CPython requires every other to be a base of.
    let mut effective: Vec<(String, String)> = Vec::new();
    {
        let classes = Classes::new(module);
        let mut of: HashMap<String, String> = HashMap::new();
        for class in &module.classes {
            if metas.contains(&class.name) {
                continue;
            }
            let mut candidates: Vec<String> = class.metaclass.iter().cloned().collect();
            candidates.extend(class.bases.iter().filter_map(|b| of.get(b).cloned()));
            let Some(winner) = candidates
                .iter()
                .find(|w| candidates.iter().all(|c| classes.is_subclass(w, c)))
                .cloned()
            else {
                if candidates.is_empty() {
                    continue;
                }
                return Err(crate::core::errors::type_error(
                    format!(
                        "class '{}' raises TypeError: metaclass conflict: the metaclass of a \
                         derived class must be a (non-strict) subclass of the metaclasses of \
                         all its bases",
                        class.name
                    ),
                    None,
                )
                .into());
            };
            for special in ["__prepare__", "__instancecheck__", "__subclasscheck__"] {
                if classes.method(&winner, special).is_some() {
                    return Err(refuse(format!(
                        "metaclass '{winner}' defines {special}, which is not supported"
                    )));
                }
            }
            of.insert(class.name.clone(), winner.clone());
            effective.push((class.name.clone(), winner));
        }
    }

    for meta in &metas {
        let classes = Classes::new(module);
        if let Some(m) = classes
            .by_name
            .get(meta.as_str())
            .and_then(|c| c.methods.iter().find(|m| !m.decorators.is_empty()))
        {
            return Err(refuse(format!(
                "the metaclass method '{meta}.{}' has a decorator, which is not supported",
                m.name
            )));
        }
    }

    let mut inserts: Vec<(usize, IRVariable)> = Vec::new();
    for (class, meta) in &effective {
        create_class(module, class, meta, &mut inserts)?;
        specialize_call(module, class, meta)?;
    }
    inserts.sort_by_key(|(position, _)| *position);
    for (offset, (position, var)) in inserts.into_iter().enumerate() {
        let at = (position + offset).min(module.variables.len());
        module.variables.insert(at, var);
    }
    let of: HashMap<String, String> = effective.into_iter().collect();
    redirect_through_metaclass(module, &of)?;

    // The metaclass's methods have all been specialized; what is left of it
    // is its class variables. Making a class with it at run time is refused.
    for class in &mut module.classes {
        if metas.contains(&class.name) {
            class.methods.clear();
        }
    }
    let mut problem = None;
    each_body(module, &mut |body| {
        let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
            if let IRExpr::FunctionCall { function_name, .. } = e {
                if metas.contains(function_name) {
                    problem.get_or_insert(format!(
                        "calling the metaclass '{function_name}' creates a class at run time, \
                         which is not supported"
                    ));
                }
            }
            Ok(())
        });
    });
    match problem {
        Some(p) => Err(refuse(p)),
        None => Ok(()),
    }
}

/// Apply `f` to every body in the module, and to its definitions' values.
fn each_body(module: &mut IRModule, f: &mut impl FnMut(&mut IRBody)) {
    for func in &mut module.functions {
        f(&mut func.body);
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            f(&mut method.body);
        }
    }
    let values = |vars: &mut Vec<IRVariable>, f: &mut dyn FnMut(&mut IRBody)| {
        for v in vars {
            let mut body = IRBody {
                statements: vec![IRStatement::Expression(v.value.clone())],
            };
            f(&mut body);
            if let Some(IRStatement::Expression(value)) = body.statements.pop() {
                v.value = value;
            }
        }
    };
    values(&mut module.variables, f);
    for class in &mut module.classes {
        values(&mut class.class_vars, f);
    }
}

/// `method`'s body with its parameters bound: the first to `first`, the
/// others by `bound`. A parameter not bound may not be used (the namespace),
/// nor may any be rebound.
fn bind_params(method: &IRFunction, meta: &str, first: IRExpr, bound: &[IRExpr]) -> Result<IRBody> {
    let mut map: HashMap<String, Option<IRExpr>> = HashMap::new();
    for (k, p) in method.params.iter().enumerate() {
        let value = if k == 0 {
            Some(first.clone())
        } else {
            bound.get(k - 1).cloned()
        };
        map.insert(p.name.clone(), value);
    }
    let mut body = method.body.clone();
    let mut rebound = None;
    crate::ir::walk_targets(&mut body, &mut |n| {
        if map.contains_key(n) {
            rebound.get_or_insert(n.clone());
        }
    });
    if let Some(n) = rebound {
        return Err(refuse(format!(
            "the metaclass method '{meta}.{}' rebinds its parameter '{n}', which is not \
             supported",
            method.name
        )));
    }
    let mut unbound = None;
    crate::ir::visit_body(&mut body, &mut |e: &mut IRExpr| {
        if let IRExpr::Variable(n) | IRExpr::Param(n) = e {
            match map.get(n.as_str()) {
                Some(Some(value)) => *e = value.clone(),
                Some(None) => {
                    unbound.get_or_insert(n.clone());
                }
                None => {}
            }
        }
        Ok(())
    })?;
    if let Some(n) = unbound {
        return Err(refuse(format!(
            "the metaclass method '{meta}.{}' uses '{n}', the class namespace, which is not \
             supported: it would give the class attributes under names known only at run time",
            method.name
        )));
    }
    Ok(body)
}

/// Whether `expr` is `super().<method>(...)` (or `type.<method>(...)`).
fn is_super_call(expr: &IRExpr, method: &str) -> bool {
    match expr {
        IRExpr::MethodCall {
            object,
            method_name,
            ..
        } if method_name == method => match &**object {
            IRExpr::FunctionCall { function_name, .. } => function_name == "super",
            IRExpr::Variable(n) => n == "type",
            _ => false,
        },
        _ => false,
    }
}

/// A `super()` call in a metaclass method has to reach `type`'s.
fn check_super_reaches_type(
    classes: &Classes,
    meta: &str,
    owner: &str,
    method: &str,
) -> Result<()> {
    let chain = classes.chain(meta);
    let after = chain.iter().skip_while(|c| c.name != owner).skip(1);
    for c in after {
        if c.methods.iter().any(|m| m.name == method) {
            return Err(refuse(format!(
                "super().{method}() in '{owner}.{method}' reaches '{}.{method}', and one \
                 metaclass's method calling another's is not supported",
                c.name
            )));
        }
    }
    Ok(())
}

/// Run `meta`'s `__new__` and `__init__` for `class`, at its position.
fn create_class(
    module: &mut IRModule,
    class: &str,
    meta: &str,
    inserts: &mut Vec<(usize, IRVariable)>,
) -> Result<()> {
    let (position, bases, new, init) = {
        let classes = Classes::new(module);
        let cls = classes.by_name[class];
        (
            cls.position,
            cls.bases
                .iter()
                .filter(|b| classes.by_name.contains_key(b.as_str()))
                .cloned()
                .collect::<Vec<_>>(),
            classes.method(meta, "__new__"),
            classes.method(meta, "__init__"),
        )
    };
    module.memory_layout.add_string(class);
    let name = IRExpr::Const(IRConstant::String(class.to_string()));
    let bases = IRExpr::TupleLiteral(bases.iter().map(|b| IRExpr::Variable(b.clone())).collect());
    let this = IRExpr::Variable(class.to_string());
    let mut statements = Vec::new();
    if let Some((owner, new)) = new {
        check_super_reaches_type(&Classes::new(module), meta, &owner, "__new__")?;
        let shape = || {
            refuse(format!(
                "'{owner}.__new__' must make the class with 'cls = super().__new__(mcs, name, \
                 bases, namespace)' and end with 'return cls'"
            ))
        };
        // `return super().__new__(...)` at the end makes the class last.
        let mut new = new;
        if let Some(IRStatement::Return(Some(value))) = new.body.statements.last() {
            if is_super_call(value, "__new__") {
                let value = value.clone();
                new.body.statements.pop();
                new.body.statements.push(IRStatement::Assign {
                    target: "__metaclass_made".to_string(),
                    value,
                    var_type: None,
                });
                new.body
                    .statements
                    .push(IRStatement::Return(Some(IRExpr::Variable(
                        "__metaclass_made".to_string(),
                    ))));
            }
        }
        // The made class's name, bound below to the class itself.
        let made = new
            .body
            .statements
            .iter()
            .find_map(|s| match s {
                IRStatement::Assign { target, value, .. } if is_super_call(value, "__new__") => {
                    Some(target.clone())
                }
                _ => None,
            })
            .ok_or_else(shape)?;
        let mut new = new.clone();
        let Some(IRStatement::Return(Some(IRExpr::Variable(r)))) = new.body.statements.pop() else {
            return Err(shape());
        };
        if r != made {
            return Err(shape());
        }
        new.body.statements.retain(|s| {
            !matches!(s, IRStatement::Assign { target, value, .. } if *target == made && is_super_call(value, "__new__"))
        });
        let mut body = bind_params(
            &new,
            meta,
            IRExpr::Variable(meta.to_string()),
            &[name.clone(), bases.clone()],
        )?;
        crate::ir::rename_body(&mut body, &HashMap::from([(made, class.to_string())]));
        statements.extend(body.statements);
    }
    if let Some((owner, init)) = init {
        check_super_reaches_type(&Classes::new(module), meta, &owner, "__init__")?;
        let mut init = init.clone();
        init.body
            .statements
            .retain(|s| !matches!(s, IRStatement::Expression(e) if is_super_call(e, "__init__")));
        let body = bind_params(&init, meta, this.clone(), &[name, bases])?;
        statements.extend(body.statements);
    }
    if statements
        .iter()
        .any(|s| matches!(s, IRStatement::Return(_)))
    {
        return Err(refuse(format!(
            "a 'return' in the metaclass '{meta}' making '{class}' is not supported"
        )));
    }
    place_creation(module, class, meta, statements, position, inserts)
}

/// The statements making `class`, as module definitions at `position`: an
/// attribute's first assignment through the class defines it (a class
/// variable of its own), and the rest runs in a function the module
/// initializer calls there.
fn place_creation(
    module: &mut IRModule,
    class: &str,
    meta: &str,
    statements: Vec<IRStatement>,
    position: usize,
    inserts: &mut Vec<(usize, IRVariable)>,
) -> Result<()> {
    let globals: Vec<String> = statements
        .iter()
        .filter_map(|s| match s {
            IRStatement::Global(names) => Some(names.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    let mut chunk: Vec<IRStatement> = Vec::new();
    let mut runs = 0usize;
    let mut defined: Vec<String> = Vec::new();
    let mut flush = |chunk: &mut Vec<IRStatement>,
                     inserts: &mut Vec<(usize, IRVariable)>,
                     module: &mut IRModule| {
        if chunk.is_empty() {
            return;
        }
        let run = format!("{METACLASS_FN_PREFIX}{class}_create{runs}");
        runs += 1;
        let mut body = vec![IRStatement::Global(globals.clone())];
        body.append(chunk);
        body.push(IRStatement::Return(Some(IRExpr::Const(IRConstant::Int(0)))));
        module.functions.push(IRFunction {
            name: run.clone(),
            params: Vec::new(),
            body: IRBody { statements: body },
            return_type: IRType::Int,
            decorators: Vec::new(),
        });
        inserts.push((
            position,
            IRVariable {
                name: format!("{run}_done"),
                value: IRExpr::FunctionCall {
                    function_name: run,
                    arguments: Vec::new(),
                },
                var_type: None,
            },
        ));
    };
    let is_this = |e: &IRExpr| matches!(e, IRExpr::Variable(n) if n == class);
    for stmt in statements {
        if matches!(stmt, IRStatement::Global(_)) {
            continue;
        }
        // An attribute the class does not have itself: assigning it through
        // the class gives the class its own, shadowing a base's.
        let new_attribute = |attribute: &str, module: &IRModule, defined: &[String]| {
            !defined.iter().any(|d| d == attribute)
                && !Classes::new(module).has_own_var(class, attribute)
        };
        match stmt {
            IRStatement::AttributeAssign {
                object,
                attribute,
                value,
                annotation,
            } if is_this(&object) && new_attribute(&attribute, module, &defined) => {
                if reads_attribute(&chunk, class, &attribute) {
                    return Err(refuse(format!(
                        "the metaclass reads '{class}.{attribute}' before giving it a value, \
                         which is not supported"
                    )));
                }
                flush(&mut chunk, inserts, module);
                defined.push(attribute.clone());
                inserts.push((
                    position,
                    IRVariable {
                        name: format!("{class}.{attribute}"),
                        value,
                        var_type: annotation,
                    },
                ));
            }
            // `cls.n += 1` where `n` is the metaclass's: the class's own `n`,
            // from the metaclass's.
            IRStatement::AttributeAugAssign {
                object,
                attribute,
                value,
                op,
            } if is_this(&object) && new_attribute(&attribute, module, &defined) => {
                // The value it starts from is the one the class sees now,
                // a base's or the metaclass's.
                let classes = Classes::new(module);
                let from = classes
                    .inherited_owner(class, &attribute)
                    .or_else(|| classes.var_owner(meta, &attribute))
                    .ok_or_else(|| {
                        refuse(format!(
                            "the metaclass augments '{class}.{attribute}', which has no value \
                             yet"
                        ))
                    })?;
                flush(&mut chunk, inserts, module);
                defined.push(attribute.clone());
                inserts.push((
                    position,
                    IRVariable {
                        name: format!("{class}.{attribute}"),
                        value: IRExpr::BinaryOp {
                            left: Box::new(IRExpr::Attribute {
                                object: Box::new(IRExpr::Variable(from)),
                                attribute: attribute.clone(),
                            }),
                            right: Box::new(value),
                            op,
                        },
                        var_type: None,
                    },
                ));
            }
            other => {
                // A new attribute first given a value inside a branch or a
                // loop would exist only on some paths.
                let mut nested = None;
                let mut probe = IRBody {
                    statements: vec![other.clone()],
                };
                find_attribute_writes(&mut probe, class, &mut |a| {
                    if !defined.iter().any(|d| d == a)
                        && !Classes::new(module).has_own_var(class, a)
                    {
                        nested.get_or_insert(a.to_string());
                    }
                });
                if let Some(a) = nested {
                    return Err(refuse(format!(
                        "the metaclass gives '{class}' the attribute '{a}' inside a branch or a \
                         loop, which is not supported. Hint: give it a value first, outside"
                    )));
                }
                chunk.push(other);
            }
        }
    }
    flush(&mut chunk, inserts, module);
    if let Some(cls) = module.classes.iter_mut().find(|c| c.name == class) {
        cls.deferred_vars.extend(defined);
    }
    Ok(())
}

/// Whether `statements` read `class.attribute`.
fn reads_attribute(statements: &[IRStatement], class: &str, attribute: &str) -> bool {
    let mut body = IRBody {
        statements: statements.to_vec(),
    };
    let mut found = false;
    let _ = crate::ir::visit_body(&mut body, &mut |e: &mut IRExpr| {
        if let IRExpr::Attribute {
            object,
            attribute: a,
        } = e
        {
            if a == attribute && matches!(&**object, IRExpr::Variable(n) if n == class) {
                found = true;
            }
        }
        Ok(())
    });
    found
}

/// Every attribute a body assigns through `class`, at any depth.
fn find_attribute_writes(body: &mut IRBody, class: &str, f: &mut impl FnMut(&str)) {
    for stmt in &mut body.statements {
        if let IRStatement::AttributeAssign {
            object: IRExpr::Variable(n),
            attribute,
            ..
        }
        | IRStatement::AttributeAugAssign {
            object: IRExpr::Variable(n),
            attribute,
            ..
        } = stmt
        {
            if n == class {
                f(attribute);
            }
        }
        let nested: Vec<&mut IRBody> = match stmt {
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => std::iter::once(then_body.as_mut())
                .chain(else_body.as_deref_mut())
                .collect(),
            IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body],
            IRStatement::For { body, .. } => vec![body],
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => std::iter::once(try_body.as_mut())
                .chain(except_handlers.iter_mut().map(|h| &mut h.body))
                .chain(finally_body.as_deref_mut())
                .collect(),
            _ => Vec::new(),
        };
        for b in nested {
            find_attribute_writes(b, class, f);
        }
    }
}

/// `meta.__call__` for `class`: every `class(...)` calls it, and its
/// `super().__call__(...)` builds the instance.
fn specialize_call(module: &mut IRModule, class: &str, meta: &str) -> Result<()> {
    let (call, init) = {
        let classes = Classes::new(module);
        let call = classes.method(meta, "__call__");
        let init = classes.method(class, "__init__");
        (call, init)
    };
    let Some((owner, call)) = call else {
        return Ok(());
    };
    check_super_reaches_type(&Classes::new(module), meta, &owner, "__call__")?;
    // The instantiation it wraps, with the class's `__init__` parameters.
    let original = IRFunction {
        name: class.to_string(),
        params: init
            .map(|(_, f)| f.params.into_iter().skip(1).collect())
            .unwrap_or_default(),
        body: IRBody {
            statements: Vec::new(),
        },
        return_type: IRType::Class(class.to_string()),
        decorators: Vec::new(),
    };
    let make = "__metaclass_make";
    let mut body = bind_params(
        &IRFunction {
            params: call.params[..1].to_vec(),
            ..call.clone()
        },
        meta,
        IRExpr::Variable(class.to_string()),
        &[],
    )?;
    crate::ir::visit_body(&mut body, &mut |e: &mut IRExpr| {
        if is_super_call(e, "__call__") {
            if let IRExpr::MethodCall { arguments, .. } = e {
                *e = IRExpr::FunctionCall {
                    function_name: make.to_string(),
                    arguments: std::mem::take(arguments),
                };
            }
        }
        Ok(())
    })?;
    let mut wrapper = IRFunction {
        name: format!("{METACLASS_FN_PREFIX}{class}_call"),
        params: call.params[1..].to_vec(),
        body,
        return_type: call.return_type.clone(),
        decorators: Vec::new(),
    };
    let star =
        crate::ir::decorate::specialize_params(&mut wrapper, &original, class, &HashMap::new())?;
    crate::ir::decorate::substitute_fn(
        &mut wrapper.body,
        make,
        class,
        class,
        &original,
        star.as_ref().map(|(a, k)| (a.as_str(), k.as_deref())),
        &mut module.memory_layout,
    )?;
    if wrapper.return_type == IRType::Unknown {
        wrapper.return_type = IRType::Class(class.to_string());
    }
    // Every instantiation goes through it, the one inside it excepted.
    let name = wrapper.name.clone();
    each_body(module, &mut |body| {
        let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
            if let IRExpr::FunctionCall { function_name, .. } = e {
                if function_name == class {
                    *function_name = name.clone();
                }
            }
            Ok(())
        });
    });
    module.functions.push(wrapper);
    Ok(())
}

/// Reads of a class attribute and calls of a method that only the class's
/// metaclass has: `C.registry` is the metaclass's variable, and
/// `C.describe()` the metaclass's method, specialized for C.
fn redirect_through_metaclass(module: &mut IRModule, of: &HashMap<String, String>) -> Result<()> {
    let mut made: HashSet<String> = HashSet::new();
    loop {
        let mut wanted: Vec<(String, String)> = Vec::new();
        {
            let snapshot = module.clone();
            let classes = Classes::new(&snapshot);
            each_body(module, &mut |body| {
                let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
                    match e {
                        IRExpr::Attribute { object, attribute } => {
                            if let IRExpr::Variable(c) = &**object {
                                if let Some(meta) = of.get(c) {
                                    if !classes.has_var(c, attribute) {
                                        if let Some(owner) = classes.var_owner(meta, attribute) {
                                            **object = IRExpr::Variable(owner);
                                        }
                                    }
                                }
                            }
                        }
                        IRExpr::MethodCall {
                            object,
                            method_name,
                            arguments,
                        } => {
                            if let IRExpr::Variable(c) = &**object {
                                if let Some(meta) = of.get(c) {
                                    if classes.method(c, method_name).is_none()
                                        && classes.method(meta, method_name).is_some()
                                    {
                                        let function =
                                            format!("{METACLASS_FN_PREFIX}{c}_{method_name}");
                                        wanted.push((c.clone(), method_name.clone()));
                                        *e = IRExpr::FunctionCall {
                                            function_name: function,
                                            arguments: std::mem::take(arguments),
                                        };
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    Ok(())
                });
            });
        }
        let mut progressed = false;
        for (class, method) in wanted {
            let function = format!("{METACLASS_FN_PREFIX}{class}_{method}");
            if !made.insert(function.clone()) {
                continue;
            }
            progressed = true;
            let meta = &of[&class];
            let (_, m) = Classes::new(module)
                .method(meta, &method)
                .expect("found above");
            let mut params: Vec<IRParam> = m.params.iter().skip(1).cloned().collect();
            if params.iter().any(|p| p.star != ParamStar::None) {
                return Err(refuse(format!(
                    "the metaclass method '{meta}.{method}' takes star parameters, which is not \
                     supported"
                )));
            }
            let first = IRExpr::Variable(class.clone());
            let rest: Vec<IRExpr> = params
                .iter()
                .map(|p| IRExpr::Variable(p.name.clone()))
                .collect();
            let body = bind_params(&m, meta, first, &rest)?;
            for p in &mut params {
                p.keyword_only = false;
            }
            module.functions.push(IRFunction {
                name: function,
                params,
                body,
                return_type: m.return_type.clone(),
                decorators: Vec::new(),
            });
        }
        if !progressed {
            return Ok(());
        }
    }
}
