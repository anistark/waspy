//! User-written decorators, applied at compile time.
//!
//! `@deco def f(...)` rebinds `f` to `deco(f)`. A decorator is ordinarily a
//! function taking a function and returning a wrapper that forwards its
//! arguments, `def wrapper(*args, **kwargs): ... fn(*args, **kwargs)`, and a
//! wrapper like that has no static signature of its own. So each application
//! is specialized for the function it decorates: the original is kept under
//! an internal name, and the decorator's body is instantiated with `fn`
//! bound to it. The wrapper becomes the new `f`, with `f`'s own parameters,
//! and the forwarding call a direct call. The decorator's own variables,
//! which the wrapper closes over, become module globals private to the
//! application (so a call counter counts per decorated function, as each
//! decoration's closure does in CPython), and its other statements run once,
//! when the module is initialized, at the place the `def` stands.
//!
//! The shapes handled: a decorator `def deco(fn)` returning a nested wrapper
//! or `fn` itself, and a factory `def deco(args)` returning such a decorator
//! (`@repeat(3)`), stacked in any number. Inside the wrapper, `fn(...)`
//! calls the decorated function, `fn.__name__` is its name, and `*args` /
//! `**kwargs` may only be passed on to it. A decorator on a method, one that
//! is a class, or any other shape is refused.

use crate::ir::{
    IRBody, IRConstant, IRExpr, IRFunction, IRModule, IRStatement, IRType, IRUserDecoration,
    IRVariable, ParamStar,
};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// The name a call's `**mapping` argument is kept under until something
/// consumes it.
pub const DOUBLE_STAR_ARG: &str = "**";

/// The prefix of the functions and globals a decoration synthesizes: the
/// undecorated original, intermediate wrappers, and decoration-time code.
/// None of them is exported.
pub const DECORATED_FN_PREFIX: &str = "__decorated_";

/// A call argument standing for "the decorated function's parameters",
/// replaced once the wrapper body has been checked.
const FORWARD_MARK: &str = "\0forward";

/// Whether a module function is the compiler's own, not exported and not
/// listed in the module's signatures.
pub fn is_internal_function(name: &str) -> bool {
    name.starts_with(crate::ir::DRAIN_FN_PREFIX)
        || name.starts_with(DECORATED_FN_PREFIX)
        || name.starts_with(crate::ir::GEN_METHOD_PREFIX)
        || name.starts_with(crate::ir::METACLASS_FN_PREFIX)
}

fn refuse(message: String) -> anyhow::Error {
    crate::core::errors::unsupported_feature(message, None).into()
}

/// Apply every decoration the converter recorded.
pub fn apply_user_decorators(module: &mut IRModule) -> Result<()> {
    let decorations = std::mem::take(&mut module.user_decorations);
    if decorations.is_empty() {
        return Ok(());
    }
    let decorator_names: HashSet<String> = decorations
        .iter()
        .flat_map(|d| d.decorators.iter().map(|(n, _)| n.clone()))
        .collect();
    let templates: HashMap<String, IRFunction> = module
        .functions
        .iter()
        .filter(|f| decorator_names.contains(&f.name))
        .map(|f| (f.name.clone(), f.clone()))
        .collect();
    // Definitions to insert, each at its decoration's position, in order.
    let mut inserts: Vec<(usize, IRVariable)> = Vec::new();
    for decoration in &decorations {
        apply_one(module, decoration, &templates, &mut inserts)?;
    }
    for (offset, (position, var)) in inserts.into_iter().enumerate() {
        let at = (position + offset).min(module.variables.len());
        module.variables.insert(at, var);
    }
    // A function used only as a decorator has done its work. Compiled on its
    // own, its forwarding wrapper would have no signature.
    let still_used: HashSet<String> = decorator_names
        .iter()
        .filter(|name| module_mentions(module, name))
        .cloned()
        .collect();
    module
        .functions
        .retain(|f| !decorator_names.contains(&f.name) || still_used.contains(&f.name));
    Ok(())
}

fn apply_one(
    module: &mut IRModule,
    decoration: &IRUserDecoration,
    templates: &HashMap<String, IRFunction>,
    inserts: &mut Vec<(usize, IRVariable)>,
) -> Result<()> {
    let name = decoration.function.clone();
    let Some(index) = module.functions.iter().position(|f| f.name == name) else {
        return Err(refuse(format!(
            "the decorated function '{name}' is not defined"
        )));
    };
    let original = module.functions[index].clone();
    let undecorated = format!("{DECORATED_FN_PREFIX}{name}_original");
    module.functions[index].name = undecorated.clone();
    let mut target = undecorated;
    let count = decoration.decorators.len();
    // Innermost first: `@a @b def f` is `a(b(f))`.
    for (step, (deco_name, factory_args)) in decoration.decorators.iter().rev().enumerate() {
        let Some(template) = templates.get(deco_name) else {
            return Err(refuse(format!(
                "decorator '@{deco_name}' on function '{name}' is not supported: it is not a \
                 function this module defines"
            )));
        };
        let prefix = format!("{DECORATED_FN_PREFIX}{name}_{step}_");
        let new_name = if step + 1 == count {
            name.clone()
        } else {
            format!("{DECORATED_FN_PREFIX}{name}_{step}")
        };
        let mut hoisted: HashMap<String, String> = HashMap::new();
        // A factory's arguments, bound at decoration time to its parameters.
        let mut factory_bindings: Vec<IRStatement> = Vec::new();
        let decorator = match factory_args {
            Some(args) => {
                if template.params.len() != args.len()
                    || template.params.iter().any(|p| p.star != ParamStar::None)
                {
                    return Err(refuse(format!(
                        "the decorator factory '@{deco_name}(...)' on '{name}' must be called \
                         with exactly its positional parameters"
                    )));
                }
                for (param, arg) in template.params.iter().zip(args) {
                    let global = format!("{prefix}{}", param.name);
                    hoisted.insert(param.name.clone(), global.clone());
                    factory_bindings.push(IRStatement::Assign {
                        target: global,
                        value: arg.clone(),
                        var_type: (param.param_type != IRType::Unknown)
                            .then(|| param.param_type.clone()),
                    });
                }
                match split_returned(&template.body, None, deco_name, &name)? {
                    (pre, Some(mut inner)) => {
                        inner.body.statements.splice(0..0, pre);
                        inner
                    }
                    (_, None) => unreachable!("a factory never returns its parameter"),
                }
            }
            None => template.clone(),
        };
        let [fn_param] = decorator.params.as_slice() else {
            return Err(refuse(format!(
                "decorator '@{deco_name}' on '{name}' must take exactly one parameter, the \
                 function"
            )));
        };
        let fn_name = fn_param.name.clone();
        let (setup, wrapper) = split_returned(&decorator.body, Some(&fn_name), deco_name, &name)?;
        // The decorator's own variables become this application's globals.
        for stmt in &setup {
            for bound in bound_names(stmt) {
                if bound != fn_name {
                    hoisted
                        .entry(bound.clone())
                        .or_insert_with(|| format!("{prefix}{bound}"));
                }
            }
        }
        let globals: Vec<String> = hoisted.values().cloned().collect();

        // Decoration time: a variable's first plain assignment is a module
        // definition, and everything else runs in a function the module
        // initializer calls, both where the `def` stands.
        let mut setup_body = IRBody { statements: setup };
        rename_body(&mut setup_body, &hoisted);
        substitute_fn(
            &mut setup_body,
            &fn_name,
            &target,
            &name,
            &original,
            None,
            &mut module.memory_layout,
        )?;
        let mut statements = factory_bindings;
        statements.append(&mut setup_body.statements);
        let mut chunk: Vec<IRStatement> = Vec::new();
        let mut defined: HashSet<String> = HashSet::new();
        let mut runs = 0usize;
        let mut flush = |chunk: &mut Vec<IRStatement>,
                         inserts: &mut Vec<(usize, IRVariable)>,
                         module: &mut IRModule| {
            if chunk.is_empty() {
                return;
            }
            let run = format!("{prefix}init{runs}");
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
                decoration.position,
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
        for stmt in statements {
            match stmt {
                IRStatement::Assign {
                    target: var,
                    value,
                    var_type,
                } if !defined.contains(&var) && globals.contains(&var) => {
                    flush(&mut chunk, inserts, module);
                    defined.insert(var.clone());
                    inserts.push((
                        decoration.position,
                        IRVariable {
                            name: var,
                            value,
                            var_type,
                        },
                    ));
                }
                other => chunk.push(other),
            }
        }
        flush(&mut chunk, inserts, module);
        if let Some((local, _)) = hoisted.iter().find(|(_, g)| !defined.contains(*g)) {
            return Err(refuse(format!(
                "the variable '{local}' of decorator '@{deco_name}' on '{name}' must be \
                 assigned a value before anything else binds it"
            )));
        }

        // `return fn` leaves the function itself; otherwise the wrapper is
        // the next function of the chain.
        let Some(mut wrapper) = wrapper else {
            continue;
        };
        let star = specialize_params(&mut wrapper, &original, &name, &hoisted)?;
        let mut body = wrapper.body;
        // `nonlocal` named the decorator's variables, which are globals now.
        for stmt in &mut body.statements {
            if let IRStatement::Nonlocal(names) = stmt {
                *stmt = IRStatement::Global(
                    names
                        .iter()
                        .map(|n| hoisted.get(n).cloned().unwrap_or_else(|| n.clone()))
                        .collect(),
                );
            }
        }
        rename_body(&mut body, &hoisted);
        substitute_fn(
            &mut body,
            &fn_name,
            &target,
            &name,
            &original,
            star.as_ref().map(|(a, k)| (a.as_str(), k.as_deref())),
            &mut module.memory_layout,
        )?;
        // An unannotated wrapper gives what the function gives, the usual
        // case; a return of anything else is checked against that type.
        let return_type = match wrapper.return_type {
            IRType::Unknown => original.return_type.clone(),
            declared => declared,
        };
        module.functions.push(IRFunction {
            name: new_name.clone(),
            params: wrapper.params,
            body,
            return_type,
            decorators: Vec::new(),
        });
        target = new_name;
    }
    // When the chain ended on a decorator returning the function itself,
    // `name` is whichever function that was.
    if target != name {
        rename_function(module, &target, &name);
    }
    Ok(())
}

/// A decorator body as `(statements before, the def it returns)`, the def
/// `None` when it returns `fn_name`, its parameter, itself.
fn split_returned(
    body: &IRBody,
    fn_name: Option<&str>,
    deco_name: &str,
    target: &str,
) -> Result<(Vec<IRStatement>, Option<IRFunction>)> {
    let shape = || {
        refuse(format!(
            "decorator '@{deco_name}' on '{target}' must end by returning the function it \
             defines{}",
            if fn_name.is_some() {
                ", or the function it was given"
            } else {
                ""
            }
        ))
    };
    let Some((last, before)) = body.statements.split_last() else {
        return Err(shape());
    };
    let IRStatement::Return(Some(IRExpr::Variable(returned) | IRExpr::Param(returned))) = last
    else {
        return Err(shape());
    };
    if Some(returned.as_str()) == fn_name {
        if before
            .iter()
            .any(|s| matches!(s, IRStatement::FunctionDef(_)))
        {
            return Err(shape());
        }
        return Ok((before.to_vec(), None));
    }
    let mut wrapper = None;
    let mut rest = Vec::new();
    for stmt in before {
        match stmt {
            IRStatement::FunctionDef(def) if def.name == *returned && wrapper.is_none() => {
                wrapper = Some((**def).clone())
            }
            IRStatement::FunctionDef(_) => return Err(shape()),
            other => rest.push(other.clone()),
        }
    }
    match wrapper {
        Some(w) => Ok((rest, Some(w))),
        None => Err(shape()),
    }
}

/// Give the wrapper the decorated function's parameters. Returns the names
/// of its `*args` and `**kwargs` when it forwards through them.
pub(crate) fn specialize_params(
    wrapper: &mut IRFunction,
    original: &IRFunction,
    target: &str,
    hoisted: &HashMap<String, String>,
) -> Result<Option<(String, Option<String>)>> {
    for d in &wrapper.decorators {
        let bare = d.strip_prefix("functools.").unwrap_or(d);
        if !bare.starts_with("wraps") && !bare.starts_with("update_wrapper") {
            return Err(refuse(format!(
                "decorator '@{d}' on the wrapper of '{target}' is not supported"
            )));
        }
    }
    let own: HashSet<String> = wrapper.params.iter().map(|p| p.name.clone()).collect();
    // A name of the wrapper's body that becomes one of the function's
    // parameters would read the argument instead of what it named.
    let mut used = HashSet::new();
    collect_names(&mut wrapper.body, &mut used);
    for p in &original.params {
        if (used.contains(&p.name) && !own.contains(&p.name)) || hoisted.contains_key(&p.name) {
            return Err(refuse(format!(
                "the wrapper decorating '{target}' uses the name '{}', which is also a \
                 parameter of '{target}'. Hint: rename one of them",
                p.name
            )));
        }
    }
    if let Some(p) = wrapper
        .params
        .iter()
        .find(|p| hoisted.contains_key(&p.name))
    {
        return Err(refuse(format!(
            "the wrapper decorating '{target}' has a parameter '{}' that its decorator also \
             binds",
            p.name
        )));
    }
    if wrapper.params.iter().any(|p| p.star != ParamStar::None) {
        let find = |star: ParamStar| {
            wrapper
                .params
                .iter()
                .find(|p| p.star == star)
                .map(|p| p.name.clone())
        };
        let plain = wrapper
            .params
            .iter()
            .filter(|p| p.star == ParamStar::None)
            .count();
        let Some(args) = find(ParamStar::Args).filter(|_| plain == 0) else {
            return Err(refuse(format!(
                "the wrapper decorating '{target}' must take '*args' (and '**kwargs') only, \
                 or the same parameters as '{target}'"
            )));
        };
        let kwargs = find(ParamStar::Kwargs);
        wrapper.params = original.params.clone();
        return Ok(Some((args, kwargs)));
    }
    if wrapper.params.len() != original.params.len() {
        return Err(refuse(format!(
            "the wrapper decorating '{target}' takes {} parameter(s) where '{target}' takes {}",
            wrapper.params.len(),
            original.params.len()
        )));
    }
    // The wrapper's own names, typed and defaulted as the function's (a
    // keyword argument at a call names the function's parameter).
    let renames: HashMap<String, String> = wrapper
        .params
        .iter()
        .zip(&original.params)
        .filter(|(w, o)| w.name != o.name)
        .map(|(w, o)| (w.name.clone(), o.name.clone()))
        .collect();
    rename_body(&mut wrapper.body, &renames);
    wrapper.params = original.params.clone();
    Ok(None)
}

/// Replace the decorator's function parameter `fn_name` inside `body`:
/// `fn(...)` calls `target`, a forwarding `fn(*args, **kwargs)` passes the
/// decorated function's parameters, `fn.__name__` is its name, and `fn` as a
/// value is `target`'s. `*args` and `**kwargs` used any other way are
/// refused, since they would depend on how each call was written.
pub(crate) fn substitute_fn(
    body: &mut IRBody,
    fn_name: &str,
    target: &str,
    display: &str,
    original: &IRFunction,
    star: Option<(&str, Option<&str>)>,
    memory_layout: &mut crate::ir::MemoryLayout,
) -> Result<()> {
    let is = |e: &IRExpr, n: &str| matches!(e, IRExpr::Variable(v) | IRExpr::Param(v) if v == n);
    let mark = || IRExpr::Keyword {
        name: FORWARD_MARK.to_string(),
        value: Box::new(IRExpr::Const(IRConstant::Int(0))),
    };
    memory_layout.add_string(display);
    // First the forwarding calls, recognized whole (the walk reaches a call
    // after its arguments, so this has to come before the check below).
    crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        if let IRExpr::FunctionCall {
            function_name,
            arguments,
        } = e
        {
            if function_name == fn_name {
                let forwarding = match (star, arguments.as_slice()) {
                    (Some((a, _)), [IRExpr::Keyword { name, value }]) => {
                        name == crate::ir::STARRED_ARG && is(value, a)
                    }
                    (
                        Some((a, Some(k))),
                        [IRExpr::Keyword {
                            name: n1,
                            value: v1,
                        }, IRExpr::Keyword {
                            name: n2,
                            value: v2,
                        }],
                    ) => {
                        n1 == crate::ir::STARRED_ARG
                            && n2 == DOUBLE_STAR_ARG
                            && is(v1, a)
                            && is(v2, k)
                    }
                    _ => false,
                };
                if forwarding {
                    *arguments = vec![mark()];
                }
            }
        }
        Ok(())
    })?;
    let mut problem: Option<String> = None;
    crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        match e {
            IRExpr::FunctionCall { function_name, .. } if function_name == fn_name => {
                *function_name = target.to_string();
            }
            IRExpr::Attribute { object, attribute }
                if (is(object, fn_name) || is(object, target))
                    && matches!(attribute.as_str(), "__name__" | "__qualname__") =>
            {
                *e = IRExpr::Const(IRConstant::String(display.to_string()));
            }
            IRExpr::Variable(v) | IRExpr::Param(v) if v == fn_name => {
                *e = IRExpr::Variable(target.to_string());
            }
            IRExpr::Variable(v) | IRExpr::Param(v)
                if star.is_some_and(|(a, k)| v == a || Some(v.as_str()) == k) =>
            {
                problem.get_or_insert(format!(
                    "the wrapper decorating '{display}' uses '{v}' other than passing it on to \
                     the function, which would depend on how each call is written"
                ));
            }
            _ => {}
        }
        Ok(())
    })?;
    if let Some(p) = problem {
        return Err(refuse(p));
    }
    // Then the marks become the function's own parameters, passed through.
    let forwarded: Vec<IRExpr> = original
        .params
        .iter()
        .map(|p| {
            let value = Box::new(IRExpr::Variable(p.name.clone()));
            match p.star {
                ParamStar::Args => IRExpr::Keyword {
                    name: crate::ir::STARRED_ARG.to_string(),
                    value,
                },
                ParamStar::Kwargs => IRExpr::Keyword {
                    name: DOUBLE_STAR_ARG.to_string(),
                    value,
                },
                ParamStar::None if p.keyword_only => IRExpr::Keyword {
                    name: p.name.clone(),
                    value,
                },
                ParamStar::None => *value,
            }
        })
        .collect();
    crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        if let IRExpr::FunctionCall { arguments, .. } = e {
            if matches!(arguments.as_slice(), [IRExpr::Keyword { name, .. }] if name == FORWARD_MARK)
            {
                *arguments = forwarded.clone();
            }
        }
        Ok(())
    })
}

/// The names a statement binds at its own level.
fn bound_names(stmt: &IRStatement) -> Vec<String> {
    match stmt {
        IRStatement::Assign { target, .. } | IRStatement::AugAssign { target, .. } => {
            vec![target.clone()]
        }
        IRStatement::TupleUnpack { targets, .. } => targets.clone(),
        IRStatement::For { target, .. } => vec![target.clone()],
        _ => Vec::new(),
    }
}

/// Apply `f` to every name a body binds, at any depth.
pub(crate) fn walk_targets(body: &mut IRBody, f: &mut impl FnMut(&mut String)) {
    for stmt in &mut body.statements {
        match stmt {
            IRStatement::Assign { target, .. }
            | IRStatement::AugAssign { target, .. }
            | IRStatement::DynamicImport { target, .. } => f(target),
            IRStatement::TupleUnpack { targets: names, .. }
            | IRStatement::Global(names)
            | IRStatement::Nonlocal(names) => names.iter_mut().for_each(&mut *f),
            IRStatement::For {
                target,
                body,
                else_body,
                ..
            } => {
                f(target);
                walk_targets(body, f);
                if let Some(b) = else_body {
                    walk_targets(b, f);
                }
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                walk_targets(then_body, f);
                if let Some(b) = else_body {
                    walk_targets(b, f);
                }
            }
            IRStatement::While { body, .. } => walk_targets(body, f),
            IRStatement::With {
                optional_vars,
                body,
                ..
            } => {
                if let Some(v) = optional_vars {
                    f(v);
                }
                walk_targets(body, f);
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                walk_targets(try_body, f);
                for h in except_handlers {
                    if let Some(n) = &mut h.name {
                        f(n);
                    }
                    walk_targets(&mut h.body, f);
                }
                if let Some(b) = finally_body {
                    walk_targets(b, f);
                }
            }
            IRStatement::FunctionDef(def) => walk_targets(&mut def.body, f),
            _ => {}
        }
    }
}

/// Every name a body reads or binds.
fn collect_names(body: &mut IRBody, names: &mut HashSet<String>) {
    let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        if let IRExpr::Variable(n) | IRExpr::Param(n) = e {
            names.insert(n.clone());
        }
        Ok(())
    });
    walk_targets(body, &mut |n| {
        names.insert(n.clone());
    });
}

/// Rename variables in `body`, references and bindings alike.
pub(crate) fn rename_body(body: &mut IRBody, map: &HashMap<String, String>) {
    if map.is_empty() {
        return;
    }
    let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        if let IRExpr::Variable(n) | IRExpr::Param(n) = e {
            if let Some(new) = map.get(n) {
                *e = IRExpr::Variable(new.clone());
            }
        }
        Ok(())
    });
    walk_targets(body, &mut |n| {
        if let Some(new) = map.get(n) {
            *n = new.clone();
        }
    });
}

/// Rename the function `from` to `to`, and every call of or reference to it.
fn rename_function(module: &mut IRModule, from: &str, to: &str) {
    let mut rename = |e: &mut IRExpr| {
        match e {
            IRExpr::FunctionCall { function_name, .. } if function_name == from => {
                *function_name = to.to_string()
            }
            IRExpr::Variable(v) if v == from => *v = to.to_string(),
            _ => {}
        }
        Ok(())
    };
    for f in &mut module.functions {
        if f.name == from {
            f.name = to.to_string();
        }
        let _ = crate::ir::visit_body(&mut f.body, &mut rename);
    }
    for v in &mut module.variables {
        let mut body = IRBody {
            statements: vec![IRStatement::Expression(v.value.clone())],
        };
        let _ = crate::ir::visit_body(&mut body, &mut rename);
        if let Some(IRStatement::Expression(value)) = body.statements.pop() {
            v.value = value;
        }
    }
}

/// Whether anything in the module other than `name` itself still names it.
fn module_mentions(module: &mut IRModule, name: &str) -> bool {
    let mut found = false;
    let mut look = |e: &mut IRExpr| {
        match e {
            IRExpr::FunctionCall { function_name, .. } if function_name == name => found = true,
            IRExpr::Variable(v) if v == name => found = true,
            _ => {}
        }
        Ok(())
    };
    for f in &mut module.functions {
        if f.name != name {
            let _ = crate::ir::visit_body(&mut f.body, &mut look);
        }
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            let _ = crate::ir::visit_body(&mut m.body, &mut look);
        }
    }
    for v in &mut module.variables {
        let mut body = IRBody {
            statements: vec![IRStatement::Expression(v.value.clone())],
        };
        let _ = crate::ir::visit_body(&mut body, &mut look);
    }
    found
}
