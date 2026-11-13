//! Coroutines (#7): `async def` and `await`, lowered onto the generator
//! machinery.
//!
//! A coroutine is a generator in all but name: its body runs in steps,
//! suspending wherever it has to wait. So `async def f(...) -> T` becomes a
//! state class `__Coro_f`, built as a generator's is
//! (`generators::build_state_class`). Its `_aw_next()` resumes the body and
//! answers what the coroutine now waits on: the future it is blocked on, or
//! None to let the event loop run once (`asyncio.sleep(0)`). Finishing is
//! the generator's StopIteration, with what `return` gave kept in
//! `__coro_result`, and an exception raised in the body comes out of the call
//! that resumed it. `f(...)` makes one, suspended before its first statement,
//! as CPython's call of a coroutine function does.
//!
//! `await x` is CPython's `yield from x.__await__()`: it resumes `x` until
//! `x` finishes, passing up whatever `x` waits on, and then gives `x`'s
//! result. Every awaitable here (a coroutine, a task, a future) has the same
//! three methods for it, `_aw_next()`, `_aw_throw(e)`, and `_aw_result()`, so
//! the suspension is written once and the methods are picked by the
//! awaitable's class at code generation:
//!
//! ```text
//! __aw = x
//! while True:
//!     if <an exception was thrown in>: __sig = __aw._aw_throw(<it>)
//!     else: __sig = __aw._aw_next()
//!     if <x finished>: break
//!     yield __sig
//! r = __aw._aw_result()
//! ```
//!
//! A future's `_aw_next()` answers itself until it is done, so a task
//! awaiting one parks on it; a coroutine's runs it inline, as CPython does.
//! An exception thrown into a suspended coroutine (a task's cancellation) is
//! passed down the chain of awaits to the innermost, where it is raised, and
//! unwinds outwards through each coroutine's handlers.
//!
//! An `await` inside an expression is hoisted out into a statement of its
//! own, and what CPython evaluates before it in that statement is computed
//! into a temporary first, so the order of evaluation is CPython's. An
//! `await` CPython might not evaluate (the right of `and`/`or`, an arm of a
//! conditional expression) or would evaluate repeatedly (a comprehension) is
//! refused.

use super::build::*;
use crate::ir::asyncio::{self as aio, Api};
use crate::ir::infer::{bind, fully_known, Types};
use crate::ir::{
    IRBody, IRClass, IRConstant, IRExpr, IRFunction, IRModule, IRParam, IRStatement, IRType,
};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

/// `await x`, as the converter lowers it: a call of this intrinsic, which
/// `lower_coroutines` makes a suspension of.
pub const AWAIT_FN: &str = "__waspy_await";

/// The prefix of the functions coroutine methods are lifted to.
pub const CORO_METHOD_PREFIX: &str = "__coromethod_";

/// The prefix of a coroutine function's state class.
pub const CORO_CLASS_PREFIX: &str = "__Coro_";

/// The local a coroutine's `return` stores its value in.
const RESULT: &str = "__coro_result";
/// The fields a task's (or an awaiting coroutine's) `_aw_throw` sets: the
/// exception to raise where the coroutine is suspended, and that there is
/// one.
pub(crate) const THROWN: &str = "__coro_thrown";
pub(crate) const THROW: &str = "__coro_throw";

/// The state class of a coroutine function.
pub(crate) fn state_class(function: &str) -> String {
    format!("{CORO_CLASS_PREFIX}{function}")
}

fn refuse(message: impl Into<String>) -> anyhow::Error {
    refused(message.into())
}

/// Whether a function is a coroutine function, and what it resolves to.
fn coroutine_result(f: &IRFunction) -> Option<&IRType> {
    match &f.return_type {
        IRType::Coroutine(t) => Some(t),
        _ => None,
    }
}

pub fn lower_coroutines(module: &mut IRModule) -> Result<()> {
    let api = Api::of(module);
    let any_coroutine = module
        .functions
        .iter()
        .any(|f| coroutine_result(f).is_some())
        || module
            .classes
            .iter()
            .any(|c| c.methods.iter().any(|m| coroutine_result(m).is_some()));
    if !any_coroutine && !api.used() {
        return Ok(());
    }
    lift_coroutine_methods(module)?;
    if api.used() {
        module.functions.extend(aio::coroutine_functions());
    }

    // What each coroutine function resolves to.
    let mut results: BTreeMap<String, IRType> = BTreeMap::new();
    for f in &module.functions {
        let Some(declared) = coroutine_result(f) else {
            continue;
        };
        let result = match declared {
            IRType::Unknown | IRType::Any if returns_value(&f.body) => {
                return Err(refuse(format!(
                    "the coroutine '{}' returns a value but declares no return type. Hint: \
                     annotate it, for example '-> int'",
                    display_name(&f.name)
                )));
            }
            IRType::Unknown | IRType::Any => IRType::None,
            t => t.clone(),
        };
        if crate::ir::collections::spec::encode(&result).is_none() || !fully_known(&result) {
            return Err(refuse(format!(
                "the coroutine '{}' returns '{}', which a task cannot hold here. Hint: return \
                 a value of a type an annotation can name",
                display_name(&f.name),
                crate::type_to_string(&result)
            )));
        }
        results.insert(f.name.clone(), result);
    }
    if results.is_empty() && !api.used() {
        return Ok(());
    }

    // The runtime, specialized for every result a coroutine can have (and a
    // promise's, which settles with a JS value, in a module using the
    // JavaScript bridge).
    let bridge = crate::ir::uses_js_bridge(module);
    let mut kinds: Vec<IRType> = results.values().cloned().collect();
    kinds.push(IRType::None);
    if bridge {
        kinds.push(IRType::JsProxy);
    }
    let mut seen = HashSet::new();
    kinds.retain(|t| seen.insert(crate::ir::collections::spec::encode(t)));
    let runtime = aio::runtime_classes(&kinds, bridge);
    for name in aio::runtime_strings() {
        module.memory_layout.add_string(name);
    }
    module
        .memory_layout
        .add_string(crate::ir::generators::IGNORED_GENERATOR_EXIT);
    module.classes.splice(0..0, runtime);
    module.functions.extend(aio::runtime_functions(bridge));
    if bridge {
        module.functions.extend(aio::bridge_functions());
        module.memory_layout.add_string(
            "a JavaScript promise is awaited inside asyncio.run(), which cannot hand control back \
             to JavaScript to settle it. Hint: export the coroutine and await it from JavaScript",
        );
    }

    // A call of a coroutine function makes its state class's instance.
    for f in &mut module.functions {
        if results.contains_key(&f.name) {
            f.return_type = IRType::Class(state_class(&f.name));
        }
    }
    let types = coroutine_types(module, &results);
    let modules: HashSet<String> = module
        .imports
        .iter()
        .filter(|i| !i.is_from_import)
        .map(|i| i.alias.clone().unwrap_or_else(|| i.module.clone()))
        .collect();

    // Every body: the asyncio calls, and in a coroutine the awaits.
    let mut counter = 0u32;
    let mut lowerer = Lowerer {
        api: &api,
        types: &types,
        modules: &modules,
        counter: &mut counter,
    };
    for f in &mut module.functions {
        let mut env = params_env(&f.params);
        let locals = local_names(f);
        if results.contains_key(&f.name) {
            rename_self(f);
            let result = results[&f.name].clone();
            let body = std::mem::replace(&mut f.body, IRBody { statements: vec![] });
            f.body = lowerer.coroutine_body(body, &mut env, &locals, &result)?;
        } else {
            lowerer.plain_body(&mut f.body, &mut env)?;
        }
    }
    for class in &mut module.classes {
        for m in &mut class.methods {
            let mut env = params_env(&m.params);
            lowerer.plain_body(&mut m.body, &mut env)?;
        }
    }

    // Each coroutine function becomes its state class's constructor.
    let types = coroutine_types(module, &results);
    let mut state_classes = Vec::new();
    for f in &mut module.functions {
        let Some(result) = results.get(&f.name) else {
            continue;
        };
        let class_name = state_class(&f.name);
        f.return_type = IRType::Generator(Box::new(aio::waited_on()));
        let mut class =
            crate::ir::generators::build_state_class(f, &class_name, &mut counter, &types)?;
        finish_state_class(&mut class, result);
        state_classes.push(class);
        let arguments = f
            .params
            .iter()
            .map(|p| IRExpr::Variable(p.name.clone()))
            .collect();
        f.body = IRBody {
            statements: vec![ret_val(call(&class_name, arguments))],
        };
        f.return_type = IRType::Class(class_name);
    }
    module.classes.extend(state_classes);
    Ok(())
}

/// The types the module shows, with each coroutine function making its state
/// class, a subclass of the runtime's coroutine class for its result.
fn coroutine_types(module: &IRModule, results: &BTreeMap<String, IRType>) -> Types {
    let mut types = Types::of_module(module);
    for (f, t) in results {
        // A coroutine's suspensions are not a generator's.
        types.yields.remove(f);
        let class = state_class(f);
        types
            .returns
            .insert(f.clone(), IRType::Class(class.clone()));
        types.classes.insert(class.clone());
        types.bases.insert(class, vec![aio::coro_class(t)]);
    }
    types
}

/// A coroutine function's name as the program wrote it.
fn display_name(name: &str) -> String {
    match name.strip_prefix(CORO_METHOD_PREFIX) {
        Some(rest) => rest.replacen('_', ".", 1),
        None => name.to_string(),
    }
}

/// Whether a body returns a value anywhere.
fn returns_value(body: &IRBody) -> bool {
    body.statements.iter().any(|s| match s {
        IRStatement::Return(Some(e)) => !matches!(e, IRExpr::Const(IRConstant::None)),
        other => crate::ir::infer::nested_bodies(other)
            .into_iter()
            .any(returns_value),
    })
}

fn params_env(params: &[IRParam]) -> HashMap<String, IRType> {
    params
        .iter()
        .map(|p| (p.name.clone(), p.param_type.clone()))
        .collect()
}

/// The names a function binds: its parameters and every name it assigns.
fn local_names(f: &IRFunction) -> HashSet<String> {
    let mut out: HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
    fn walk(body: &IRBody, out: &mut HashSet<String>) {
        for s in &body.statements {
            match s {
                IRStatement::Assign { target, .. }
                | IRStatement::AugAssign { target, .. }
                | IRStatement::For { target, .. } => {
                    out.insert(target.clone());
                }
                IRStatement::TupleUnpack { targets, .. } => out.extend(targets.iter().cloned()),
                IRStatement::TryExcept {
                    except_handlers, ..
                } => out.extend(except_handlers.iter().filter_map(|h| h.name.clone())),
                _ => {}
            }
            for nested in crate::ir::infer::nested_bodies(s) {
                walk(nested, out);
            }
        }
    }
    walk(&f.body, &mut out);
    out
}

/// A coroutine's body runs in its state class's methods, where `self` is the
/// state: a parameter or local the program named `self` is renamed apart.
fn rename_self(f: &mut IRFunction) {
    if !local_names(f).contains("self") {
        return;
    }
    let map = HashMap::from([("self".to_string(), "__coro_self".to_string())]);
    for p in &mut f.params {
        if p.name == "self" {
            p.name = "__coro_self".to_string();
        }
    }
    crate::ir::rename_body(&mut f.body, &map);
    fn targets(body: &mut IRBody) {
        for s in &mut body.statements {
            match s {
                IRStatement::Assign { target, .. }
                | IRStatement::AugAssign { target, .. }
                | IRStatement::For { target, .. }
                    if target == "self" =>
                {
                    *target = "__coro_self".to_string();
                }
                _ => {}
            }
            for nested in sub_bodies_mut(s) {
                targets(nested);
            }
        }
    }
    targets(&mut f.body);
}

fn sub_bodies_mut(stmt: &mut IRStatement) -> Vec<&mut IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => std::iter::once(then_body.as_mut())
            .chain(else_body.as_deref_mut())
            .collect(),
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body],
        IRStatement::For {
            body, else_body, ..
        } => std::iter::once(body.as_mut())
            .chain(else_body.as_deref_mut())
            .collect(),
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => std::iter::once(try_body.as_mut())
            .chain(except_handlers.iter_mut().map(|h| &mut h.body))
            .chain(finally_body.as_deref_mut())
            .collect(),
        _ => Vec::new(),
    }
}

/// An `async def` method becomes a coroutine function taking the receiver
/// first, and the method returns the coroutine it makes, as a generator
/// method does. Which coroutine a call makes is decided by the receiver's
/// static class, so one another class of its hierarchy also defines is
/// refused, as is `super()` in one.
fn lift_coroutine_methods(module: &mut IRModule) -> Result<()> {
    let ancestors = |start: &str, classes: &[IRClass]| {
        let mut out = HashSet::new();
        let mut pending = vec![start.to_string()];
        while let Some(c) = pending.pop() {
            if out.insert(c.clone()) {
                if let Some(class) = classes.iter().find(|k| k.name == c) {
                    pending.extend(class.bases.iter().cloned());
                }
            }
        }
        out
    };
    let mut functions = Vec::new();
    for c in 0..module.classes.len() {
        let class_name = module.classes[c].name.clone();
        for m in 0..module.classes[c].methods.len() {
            let method = module.classes[c].methods[m].clone();
            let IRType::Coroutine(result) = &method.return_type else {
                continue;
            };
            let refuse_method = |why: &str| {
                refuse(format!(
                    "the coroutine method '{class_name}.{}' {why}",
                    method.name
                ))
            };
            if let Some(other) = module.classes.iter().find(|k| {
                k.name != class_name
                    && k.methods.iter().any(|x| x.name == method.name)
                    && (ancestors(&k.name, &module.classes).contains(&class_name)
                        || ancestors(&class_name, &module.classes).contains(&k.name))
            }) {
                return Err(refuse_method(&format!(
                    "is also defined by '{}' in the same hierarchy, so which coroutine a call \
                     makes would depend on the instance",
                    other.name
                )));
            }
            let mut uses_super = false;
            let mut probe = method.body.clone();
            let _ = crate::ir::visit_body(&mut probe, &mut |e: &mut IRExpr| {
                if matches!(e, IRExpr::FunctionCall { function_name, .. } if function_name == "super")
                {
                    uses_super = true;
                }
                Ok(())
            });
            if uses_super {
                return Err(refuse_method(
                    "calls super(), which is not supported in a coroutine",
                ));
            }
            let Some(receiver) = method.params.first().map(|p| p.name.clone()) else {
                return Err(refuse_method("takes no parameters, so it has no instance"));
            };
            let function = format!("{CORO_METHOD_PREFIX}{class_name}_{}", method.name);
            let renamed = "__coro_self".to_string();
            let mut body = method.body.clone();
            crate::ir::rename_body(&mut body, &HashMap::from([(receiver, renamed.clone())]));
            let mut params = method.params.clone();
            params[0].name = renamed;
            params[0].param_type = IRType::Class(class_name.clone());
            let arguments = method
                .params
                .iter()
                .map(|p| IRExpr::Variable(p.name.clone()))
                .collect();
            functions.push(IRFunction {
                name: function.clone(),
                params,
                body,
                return_type: IRType::Coroutine(result.clone()),
                decorators: Vec::new(),
            });
            let target = &mut module.classes[c].methods[m];
            target.body = IRBody {
                statements: vec![ret_val(call(&function, arguments))],
            };
            target.return_type = IRType::Class(state_class(&function));
        }
    }
    module.functions.extend(functions);
    Ok(())
}

/// The state class `build_state_class` made, as a coroutine: a subclass of
/// the runtime's coroutine class for its result, with the awaitable
/// protocol's methods.
fn finish_state_class(class: &mut IRClass, result: &IRType) {
    let me = || param("self", &IRType::Class(class.name.clone()));
    class.bases = vec![aio::coro_class(result)];
    let next = format!("{}::__next__", class.name);
    class.methods.push(method(
        "_aw_next",
        vec![me()],
        aio::waited_on(),
        vec![ret_val(call(&next, vec![v("self")]))],
    ));
    let mut read = Vec::new();
    if *result == IRType::None {
        read.push(ret_val(none()));
    } else {
        // The field is declared here too, for a coroutine that never
        // returns a value (it only raises).
        read.push(if_(
            boolean(false),
            vec![set_my_typed(RESULT, my(RESULT), result)],
        ));
        read.push(ret_val(my(RESULT)));
    }
    class
        .methods
        .push(method("_aw_result", vec![me()], result.clone(), read));
}

/// Rewrites the asyncio calls in a body, and in a coroutine's the awaits.
struct Lowerer<'a> {
    api: &'a Api,
    types: &'a Types,
    /// The modules the program imports, which name no value to evaluate.
    modules: &'a HashSet<String>,
    counter: &'a mut u32,
}

/// What a statement's hoisting needs to know of the coroutine it is in.
struct Scope<'a> {
    /// Names local to the coroutine: nothing else can rebind one while it
    /// waits, so one read before an `await` needs no temporary.
    locals: &'a HashSet<String>,
}

impl Lowerer<'_> {
    fn fresh(&mut self, stem: &str) -> String {
        *self.counter += 1;
        format!("__{stem}{}", self.counter)
    }

    fn ty(&self, expr: &IRExpr, env: &HashMap<String, IRType>) -> IRType {
        self.types.of(expr, env)
    }

    /// A body that is not a coroutine's: its asyncio calls, and a call of a
    /// coroutine function whose coroutine is dropped unawaited.
    fn plain_body(&mut self, body: &mut IRBody, env: &mut HashMap<String, IRType>) -> Result<()> {
        for stmt in &mut body.statements {
            self.check_dropped(stmt, env)?;
            let mut err = None;
            each_expr(stmt, &mut |e| {
                if err.is_none() {
                    if let Err(x) = self.api.rewrite(e, false) {
                        err = Some(x);
                    }
                }
            });
            if let Some(x) = err {
                return Err(x);
            }
            bind(stmt, self.types, env);
            for nested in sub_bodies_mut(stmt) {
                self.plain_body(nested, env)?;
            }
        }
        Ok(())
    }

    /// `f()` as a statement, for a coroutine function `f`: CPython warns that
    /// the coroutine was never awaited, and runs none of it.
    fn check_dropped(&self, stmt: &IRStatement, env: &HashMap<String, IRType>) -> Result<()> {
        if let IRStatement::Expression(e) = stmt {
            if let IRType::Class(c) = self.ty(e, env) {
                if let Some(f) = c.strip_prefix(CORO_CLASS_PREFIX) {
                    return Err(refuse(format!(
                        "the coroutine '{}' is never awaited, so none of it runs (CPython warns \
                         'coroutine was never awaited'). Hint: 'await' it, or pass it to \
                         asyncio.run() or asyncio.create_task()",
                        display_name(f)
                    )));
                }
            }
        }
        Ok(())
    }

    /// A coroutine's body, with every `await` a suspension.
    fn coroutine_body(
        &mut self,
        body: IRBody,
        env: &mut HashMap<String, IRType>,
        locals: &HashSet<String>,
        result: &IRType,
    ) -> Result<IRBody> {
        let scope = Scope { locals };
        let mut statements = vec![throw_check()];
        statements.extend(self.statements(body.statements, env, &scope, result)?);
        Ok(IRBody { statements })
    }

    fn statements(
        &mut self,
        stmts: Vec<IRStatement>,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
        result: &IRType,
    ) -> Result<Vec<IRStatement>> {
        let mut out = Vec::new();
        for stmt in stmts {
            self.check_dropped(&stmt, env)?;
            out.extend(self.statement(stmt, env, scope, result)?);
        }
        Ok(out)
    }

    fn statement(
        &mut self,
        stmt: IRStatement,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
        result: &IRType,
    ) -> Result<Vec<IRStatement>> {
        let mut pre = Vec::new();
        let rewritten = match stmt {
            // `a, b = await asyncio.gather(x, y)`: each target takes its
            // task's result, whatever their types.
            IRStatement::TupleUnpack {
                targets,
                value,
                starred: None,
            } if self.gather_of(&value).is_some_and(|args| {
                args.len() == targets.len() && !args.iter().any(is_starred)
            }) =>
            {
                let args = self.gather_of(&value).expect("checked").to_vec();
                let tasks = self.gather_tasks(args, &mut pre, env, scope)?;
                for (target, task) in targets.iter().zip(&tasks) {
                    pre.extend(self.read_result(target, v(task), env));
                }
                return self.expand(pre, env, scope, result);
            }
            // `x = await y`: the suspension binds `x` itself.
            IRStatement::Assign {
                target,
                value:
                    IRExpr::FunctionCall {
                        function_name,
                        mut arguments,
                    },
                var_type,
            } if function_name == AWAIT_FN && arguments.len() == 1 => {
                let x = &mut arguments[0];
                if self.gather_of_expr(x).is_none() && self.wait_for_of(x)?.is_none() {
                    self.hoist(x, &mut pre, scope, env)?;
                }
                if let Some(t) = var_type.filter(fully_known) {
                    env.entry(target.clone()).or_insert(t);
                }
                pre.push(IRStatement::Assign {
                    target,
                    value: IRExpr::FunctionCall {
                        function_name,
                        arguments,
                    },
                    var_type: None,
                });
                return self.expand(pre, env, scope, result);
            }
            IRStatement::Assign {
                target,
                mut value,
                var_type,
            } => {
                self.hoist(&mut value, &mut pre, scope, env)?;
                IRStatement::Assign {
                    target,
                    value,
                    var_type,
                }
            }
            IRStatement::AugAssign {
                target,
                mut value,
                op,
            } => {
                self.hoist(&mut value, &mut pre, scope, env)?;
                IRStatement::AugAssign { target, value, op }
            }
            IRStatement::Expression(mut e) => {
                self.hoist(&mut e, &mut pre, scope, env)?;
                match e {
                    // The value of a lone `await` is not used.
                    IRExpr::Variable(ref n) if n.starts_with("__aw_r") && !pre.is_empty() => {
                        return self.expand(pre, env, scope, result);
                    }
                    other => IRStatement::Expression(other),
                }
            }
            IRStatement::Return(value) => {
                let mut value = value;
                if let Some(e) = &mut value {
                    self.hoist(e, &mut pre, scope, env)?;
                }
                pre.push(IRStatement::Return(None));
                match value {
                    Some(IRExpr::Const(IRConstant::None)) | None => {}
                    Some(e) => {
                        let at = pre.len() - 1;
                        pre.insert(at, assign_typed(RESULT, e, result));
                    }
                }
                return self.expand(pre, env, scope, result);
            }
            IRStatement::AttributeAssign {
                mut object,
                attribute,
                mut value,
                annotation,
            } => {
                // CPython evaluates the value, then the object.
                let mut parts = vec![value, object];
                self.hoist_seq(&mut parts, &mut pre, scope, env)?;
                object = parts.pop().expect("two");
                value = parts.pop().expect("two");
                IRStatement::AttributeAssign {
                    object,
                    attribute,
                    value,
                    annotation,
                }
            }
            IRStatement::AttributeAugAssign {
                object,
                attribute,
                value,
                op,
            } => {
                if contains_await(&value) {
                    // `o.a += await x`: CPython reads `o.a` before it waits.
                    let mut object = object;
                    self.hoist(&mut object, &mut pre, scope, env)?;
                    let o = self.spill(object, &mut pre, env);
                    let current = self.fresh("aw_v");
                    pre.push(assign(&current, attr(o.clone(), &attribute)));
                    let mut value = value;
                    self.hoist(&mut value, &mut pre, scope, env)?;
                    IRStatement::AttributeAssign {
                        object: o,
                        attribute,
                        value: bin(v(&current), op, value),
                        annotation: None,
                    }
                } else {
                    let mut object = object;
                    self.hoist(&mut object, &mut pre, scope, env)?;
                    IRStatement::AttributeAugAssign {
                        object,
                        attribute,
                        value,
                        op,
                    }
                }
            }
            IRStatement::IndexAssign {
                container,
                index,
                value,
            } => {
                // The value first, then the container and the index.
                let mut parts = vec![value, container, index];
                self.hoist_seq(&mut parts, &mut pre, scope, env)?;
                let index = parts.pop().expect("three");
                let container = parts.pop().expect("three");
                let value = parts.pop().expect("three");
                IRStatement::IndexAssign {
                    container,
                    index,
                    value,
                }
            }
            IRStatement::TupleUnpack {
                targets,
                mut value,
                starred,
            } => {
                self.hoist(&mut value, &mut pre, scope, env)?;
                IRStatement::TupleUnpack {
                    targets,
                    value,
                    starred,
                }
            }
            IRStatement::Raise { exception, cause } => {
                let mut parts: Vec<IRExpr> =
                    exception.iter().chain(cause.iter()).cloned().collect();
                self.hoist_seq(&mut parts, &mut pre, scope, env)?;
                let mut parts = parts.into_iter();
                let exception = exception.as_ref().map(|_| parts.next().expect("counted"));
                let cause = cause.as_ref().map(|_| parts.next().expect("counted"));
                IRStatement::Raise { exception, cause }
            }
            IRStatement::If {
                mut condition,
                then_body,
                else_body,
            } => {
                self.hoist(&mut condition, &mut pre, scope, env)?;
                let then_body = body(self.statements(then_body.statements, env, scope, result)?);
                let else_body = match else_body {
                    Some(b) => Some(body(self.statements(b.statements, env, scope, result)?)),
                    None => None,
                };
                IRStatement::If {
                    condition,
                    then_body,
                    else_body,
                }
            }
            IRStatement::While {
                mut condition,
                body: loop_body,
            } => {
                let mut cond_pre = Vec::new();
                self.hoist(&mut condition, &mut cond_pre, scope, env)?;
                let mut statements = loop_body.statements;
                if !cond_pre.is_empty() {
                    // The condition waits: it is worked out at the top of
                    // every round.
                    let mut head = cond_pre;
                    head.push(if_(not(condition), vec![IRStatement::Break]));
                    head.extend(statements);
                    statements = head;
                    condition = boolean(true);
                }
                let statements = self.statements(statements, env, scope, result)?;
                IRStatement::While {
                    condition,
                    body: body(statements),
                }
            }
            IRStatement::For {
                target,
                mut iterable,
                body: loop_body,
                else_body,
            } => {
                self.hoist(&mut iterable, &mut pre, scope, env)?;
                let element = self.types.element(&self.ty(&iterable, env));
                env.entry(target.clone()).or_insert(element);
                let loop_body = body(self.statements(loop_body.statements, env, scope, result)?);
                let else_body = match else_body {
                    Some(b) => Some(body(self.statements(b.statements, env, scope, result)?)),
                    None => None,
                };
                IRStatement::For {
                    target,
                    iterable,
                    body: loop_body,
                    else_body,
                }
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                let try_body = body(self.statements(try_body.statements, env, scope, result)?);
                let mut handlers = Vec::new();
                for h in except_handlers {
                    if let Some(name) = &h.name {
                        let ty = match h.exception_types.as_slice() {
                            [one] => IRType::Class(one.clone()),
                            _ => IRType::Class("BaseException".to_string()),
                        };
                        env.entry(name.clone()).or_insert(ty);
                    }
                    handlers.push(crate::ir::IRExceptHandler {
                        exception_types: h.exception_types,
                        name: h.name,
                        body: IRBody {
                            statements: self.statements(h.body.statements, env, scope, result)?,
                        },
                    });
                }
                let finally_body = match finally_body {
                    Some(b) => Some(body(self.statements(b.statements, env, scope, result)?)),
                    None => None,
                };
                IRStatement::TryExcept {
                    try_body,
                    except_handlers: handlers,
                    finally_body,
                }
            }
            IRStatement::With {
                context_expr,
                optional_vars,
                body: with_body,
            } => {
                let mut context_expr = context_expr;
                self.hoist(&mut context_expr, &mut pre, scope, env)?;
                IRStatement::With {
                    context_expr,
                    optional_vars,
                    body: body(self.statements(with_body.statements, env, scope, result)?),
                }
            }
            other => {
                let mut other = other;
                let mut found = false;
                each_expr(&mut other, &mut |e| found |= contains_await(e));
                if found {
                    return Err(refuse("'await' in this statement is not supported"));
                }
                other
            }
        };
        pre.push(rewritten);
        self.expand(pre, env, scope, result)
    }

    /// The statements `hoist` left, with each `__aw_r = await x` made a
    /// suspension and the asyncio calls rewritten.
    fn expand(
        &mut self,
        stmts: Vec<IRStatement>,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
        result: &IRType,
    ) -> Result<Vec<IRStatement>> {
        let mut out = Vec::new();
        for stmt in stmts {
            match stmt {
                IRStatement::Assign { target, value, .. } if awaited(&value).is_some() => {
                    let x = awaited(&value).cloned().expect("checked");
                    out.extend(self.suspension(&target, x, env, scope, result)?);
                }
                mut other => {
                    let mut err = None;
                    each_expr(&mut other, &mut |e| {
                        if err.is_none() {
                            if let Err(x) = self.api.rewrite(e, false) {
                                err = Some(x);
                            }
                        }
                    });
                    if let Some(x) = err {
                        return Err(x);
                    }
                    bind(&other, self.types, env);
                    out.push(other);
                }
            }
        }
        Ok(out)
    }

    /// `target = await x`.
    fn suspension(
        &mut self,
        target: &str,
        x: IRExpr,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
        result: &IRType,
    ) -> Result<Vec<IRStatement>> {
        if let Some(args) = self.gather_of_expr(&x) {
            let args = args.to_vec();
            return self.gather(target, args, env, scope, result);
        }
        if let Some((aw, timeout)) = self.wait_for_of(&x)? {
            return self.wait_for(target, aw, timeout, env, scope, result);
        }
        let mut x = x;
        self.api.rewrite_tree(&mut x, true)?;
        let mut ty = self.ty(&x, env);
        // A JavaScript value (a promise, or any thenable): a future it
        // settles, as Pyodide awaits one.
        if ty == IRType::JsProxy {
            x = call(&aio::fname("promise"), vec![x]);
            ty = IRType::Class(aio::future_class(&IRType::JsProxy));
        }
        if let IRType::Class(c) = &ty {
            if !aio::is_awaitable_class(c) && !c.starts_with(CORO_CLASS_PREFIX) {
                return Err(refuse(format!(
                    "object of type '{c}' can't be used in 'await' expression"
                )));
            }
        } else if fully_known(&ty) {
            return Err(refuse(format!(
                "object of type '{}' can't be used in 'await' expression",
                crate::type_to_string(&ty)
            )));
        }
        let aw = self.fresh("aw_x");
        let sig = self.fresh("aw_s");
        let mut out = vec![assign_typed(&aw, x, &ty)];
        env.insert(aw.clone(), ty.clone());
        out.push(while_(
            boolean(true),
            vec![
                expr_stmt(call(crate::ir::STOP_CHECK_FN, vec![])),
                if_else(
                    my(THROWN),
                    vec![
                        set_attr(v("self"), THROWN, boolean(false)),
                        assign_typed(
                            &sig,
                            mcall(v(&aw), "_aw_throw", vec![my(THROW)]),
                            &aio::waited_on(),
                        ),
                    ],
                    vec![assign_typed(
                        &sig,
                        mcall(v(&aw), "_aw_next", vec![]),
                        &aio::waited_on(),
                    )],
                ),
                if_(
                    call(crate::ir::STOP_CHECK_FN, vec![]),
                    vec![IRStatement::Break],
                ),
                yield_(v(&sig)),
            ],
        ));
        let read = mcall(v(&aw), "_aw_result", vec![]);
        let rty = self.ty(&read, env);
        if rty == IRType::None {
            out.push(expr_stmt(read));
            if !target.starts_with("__aw_r") {
                out.push(assign_typed(target, none(), &IRType::None));
            }
        } else {
            // A declared type for the target is the type it holds.
            let declared = env.get(target).cloned().filter(fully_known);
            out.push(assign_typed(
                target,
                read,
                declared.as_ref().unwrap_or(&rty),
            ));
            env.entry(target.to_string()).or_insert(rty);
        }
        Ok(out)
    }

    /// `target = fut.result()`, for a future whose result may be None (a
    /// call of which leaves no value).
    fn read_result(
        &self,
        target: &str,
        fut: IRExpr,
        env: &HashMap<String, IRType>,
    ) -> Vec<IRStatement> {
        let r = mcall(fut, "result", vec![]);
        let ty = self.ty(&r, env);
        if ty == IRType::None {
            vec![expr_stmt(r), assign_typed(target, none(), &IRType::None)]
        } else {
            vec![assign_typed(target, r, &ty)]
        }
    }

    /// `asyncio.gather(...)`'s arguments, when `value` is `await` of one.
    fn gather_of<'e>(&self, value: &'e IRExpr) -> Option<&'e [IRExpr]> {
        self.gather_of_expr(awaited(value)?)
    }

    fn gather_of_expr<'e>(&self, x: &'e IRExpr) -> Option<&'e [IRExpr]> {
        match self.api.call_of(x)? {
            (n, args) if n == "gather" => Some(args),
            _ => None,
        }
    }

    /// Each argument of a `gather()` evaluated, then made a task (or kept,
    /// as a future), in order; the names of the tasks.
    fn gather_tasks(
        &mut self,
        args: Vec<IRExpr>,
        pre: &mut Vec<IRStatement>,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
    ) -> Result<Vec<String>> {
        let mut values = Vec::new();
        for (k, mut arg) in args.into_iter().enumerate() {
            if let IRExpr::Keyword { name, .. } = &arg {
                return Err(refuse(format!(
                    "asyncio.gather() with '{}' is not supported",
                    if name == crate::ir::STARRED_ARG {
                        "*".to_string()
                    } else {
                        format!("{name}=")
                    }
                )));
            }
            self.hoist(&mut arg, pre, scope, env)?;
            self.api.rewrite_tree(&mut arg, false)?;
            let name = self.fresh(&format!("ga{k}_"));
            let ty = self.ty(&arg, env);
            pre.push(assign_typed(&name, arg, &ty));
            env.insert(name.clone(), ty);
            values.push(name);
        }
        let mut tasks = Vec::new();
        for value in values {
            let name = self.fresh("gt");
            let task = mcall(v(&value), "_ensure", vec![]);
            let ty = self.ty(&task, env);
            pre.push(assign_typed(&name, task, &ty));
            env.insert(name.clone(), ty);
            tasks.push(name);
        }
        let outer = self.fresh("gg");
        pre.push(assign_typed(
            &outer,
            call(&aio::name("_Gathering"), vec![]),
            &IRType::Class(aio::name("_Gathering")),
        ));
        for task in &tasks {
            pre.push(expr_stmt(mcall(v(&outer), "_add", vec![v(task)])));
        }
        pre.push(expr_stmt(mcall(v(&outer), "_start", vec![])));
        let done = self.fresh("aw_r");
        pre.push(assign(&done, call(AWAIT_FN, vec![v(&outer)])));
        Ok(tasks)
    }

    /// `target = await asyncio.gather(...)`.
    fn gather(
        &mut self,
        target: &str,
        args: Vec<IRExpr>,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
        result: &IRType,
    ) -> Result<Vec<IRStatement>> {
        let mut pre = Vec::new();
        if let [IRExpr::Keyword { name, value }] = args.as_slice() {
            if name == crate::ir::STARRED_ARG {
                // `gather(*xs)`: a task for each element, in order.
                let mut xs = (**value).clone();
                self.hoist(&mut xs, &mut pre, scope, env)?;
                self.api.rewrite_tree(&mut xs, false)?;
                let elem = self.types.element(&self.ty(&xs, env));
                let probe = self.fresh("gx");
                let mut inner = env.clone();
                inner.insert(probe.clone(), elem);
                let task_ty = self.ty(&mcall(v(&probe), "_ensure", vec![]), &inner);
                let list = self.fresh("gl");
                let list_ty = IRType::List(Box::new(task_ty.clone()));
                pre.push(assign_typed(&list, IRExpr::ListLiteral(vec![]), &list_ty));
                pre.push(for_(
                    &probe,
                    xs,
                    vec![expr_stmt(mcall(
                        v(&list),
                        "append",
                        vec![mcall(v(&probe), "_ensure", vec![])],
                    ))],
                ));
                let outer = self.fresh("gg");
                pre.push(assign_typed(
                    &outer,
                    call(&aio::name("_Gathering"), vec![]),
                    &IRType::Class(aio::name("_Gathering")),
                ));
                let t = self.fresh("gt");
                pre.push(for_(
                    &t,
                    v(&list),
                    vec![expr_stmt(mcall(v(&outer), "_add", vec![v(&t)]))],
                ));
                pre.push(expr_stmt(mcall(v(&outer), "_start", vec![])));
                let done = self.fresh("aw_r");
                pre.push(assign(&done, call(AWAIT_FN, vec![v(&outer)])));
                let c = self.fresh("gc");
                let mut inner = env.clone();
                inner.insert(c.clone(), task_ty);
                let r = mcall(v(&c), "result", vec![]);
                let rty = self.ty(&r, &inner);
                let results = IRExpr::Comprehension {
                    kind: crate::ir::IRComprehensionKind::List,
                    element: Box::new(r),
                    value: None,
                    generators: vec![crate::ir::IRGenerator {
                        targets: vec![c],
                        iterable: v(&list),
                        conditions: vec![],
                    }],
                };
                pre.push(assign_typed(target, results, &IRType::List(Box::new(rty))));
                return self.expand(pre, env, scope, result);
            }
        }
        if args.is_empty() {
            return Err(refuse(
                "asyncio.gather() with no arguments is not supported, since nothing gives its \
                 list a type",
            ));
        }
        let tasks = self.gather_tasks(args, &mut pre, env, scope)?;
        let results: Vec<IRExpr> = tasks
            .iter()
            .map(|t| {
                let r = mcall(v(t), "result", vec![]);
                if self.ty(&r, env) == IRType::None {
                    none()
                } else {
                    r
                }
            })
            .collect();
        let ty = self.ty(&IRExpr::ListLiteral(results.clone()), env);
        pre.push(assign_typed(target, IRExpr::ListLiteral(results), &ty));
        self.expand(pre, env, scope, result)
    }

    /// `asyncio.wait_for(aw, timeout)`'s two arguments, when `x` is one.
    fn wait_for_of(&self, x: &IRExpr) -> Result<Option<(IRExpr, IRExpr)>> {
        let Some((n, args)) = self.api.call_of(x) else {
            return Ok(None);
        };
        if n != "wait_for" {
            return Ok(None);
        }
        let mut aw = None;
        let mut timeout = None;
        for (k, a) in args.iter().enumerate() {
            match a {
                IRExpr::Keyword { name, value } if name == "timeout" => {
                    timeout = Some((**value).clone())
                }
                IRExpr::Keyword { name, value } if name == "fut" => aw = Some((**value).clone()),
                IRExpr::Keyword { name, .. } => {
                    return Err(refuse(format!(
                        "asyncio.wait_for() got an unexpected keyword argument '{name}'"
                    )))
                }
                other if k == 0 => aw = Some(other.clone()),
                other if k == 1 => timeout = Some(other.clone()),
                _ => {
                    return Err(refuse(
                        "asyncio.wait_for() takes 2 positional arguments but more were given",
                    ))
                }
            }
        }
        match (aw, timeout) {
            (Some(aw), Some(t)) => Ok(Some((aw, t))),
            _ => Err(refuse(
                "asyncio.wait_for() needs an awaitable and a timeout",
            )),
        }
    }

    /// `target = await asyncio.wait_for(aw, timeout)`, as CPython 3.13 runs
    /// it: `aw` awaited in place under a timeout that cancels the waiting
    /// task when it expires, which `wait_for` then turns into TimeoutError;
    /// a timeout of zero or less makes `aw` a task, cancels it if it is not
    /// done, and waits for that.
    fn wait_for(
        &mut self,
        target: &str,
        aw: IRExpr,
        timeout: IRExpr,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
        result: &IRType,
    ) -> Result<Vec<IRStatement>> {
        let mut pre = Vec::new();
        let mut parts = vec![aw, timeout];
        self.hoist_seq(&mut parts, &mut pre, scope, env)?;
        let mut timeout = parts.pop().expect("two");
        let mut aw = parts.pop().expect("two");
        self.api.rewrite_tree(&mut aw, false)?;
        self.api.rewrite_tree(&mut timeout, false)?;
        let x = self.fresh("wx");
        let xty = self.ty(&aw, env);
        pre.push(assign_typed(&x, aw, &xty));
        env.insert(x.clone(), xty);
        if matches!(timeout, IRExpr::Const(IRConstant::None)) {
            pre.push(assign(target, call(AWAIT_FN, vec![v(&x)])));
            return self.expand(pre, env, scope, result);
        }
        let t = self.fresh("wt");
        pre.push(assign_typed(
            &t,
            call("float", vec![timeout.clone()]),
            &IRType::Float,
        ));
        env.insert(t.clone(), IRType::Float);
        let constant_positive = match &timeout {
            IRExpr::Const(IRConstant::Int(n)) => Some(*n > 0),
            IRExpr::Const(IRConstant::Float(f)) => Some(*f > 0.0),
            _ => None,
        };
        let e = self.fresh("we");
        // The timeout: `aw` awaited in place, the task cancelled at the
        // deadline.
        let to = self.fresh("wto");
        let mut timed = vec![
            assign_typed(
                &to,
                call(
                    &aio::name("_Timeout"),
                    vec![bin(
                        call(aio::CLOCK_FN, vec![]),
                        crate::ir::IROp::Add,
                        call(aio::NS_FN, vec![v(&t)]),
                    )],
                ),
                &IRType::Class(aio::name("_Timeout")),
            ),
            expr_stmt(mcall(v(&to), "_enter", vec![])),
            IRStatement::TryExcept {
                try_body: body(vec![assign(target, call(AWAIT_FN, vec![v(&x)]))]),
                except_handlers: vec![crate::ir::IRExceptHandler {
                    exception_types: vec!["BaseException".to_string()],
                    name: Some(e.clone()),
                    body: IRBody {
                        statements: vec![
                            expr_stmt(mcall(v(&to), "_exit_failed", vec![v(&e)])),
                            IRStatement::Raise {
                                exception: None,
                                cause: None,
                            },
                        ],
                    },
                }],
                finally_body: None,
            },
            expr_stmt(mcall(v(&to), "_exit_ok", vec![])),
        ];
        if constant_positive == Some(true) {
            pre.append(&mut timed);
            return self.expand_nested(pre, env, scope, result);
        }
        // Zero or less: a task, cancelled unless it is done.
        let f = self.fresh("wf");
        let fut = mcall(v(&x), "_ensure", vec![]);
        let fty = self.ty(&fut, env);
        env.insert(f.clone(), fty.clone());
        let cancelled = self.fresh("we");
        let expired = vec![
            assign_typed(&f, fut, &fty),
            if_else(
                not(mcall(v(&f), "done", vec![])),
                vec![
                    expr_stmt(call(
                        AWAIT_FN,
                        vec![call(&aio::fname("cancel_and_wait"), vec![v(&f)])],
                    )),
                    IRStatement::TryExcept {
                        try_body: body(self.read_result(target, v(&f), env)),
                        except_handlers: vec![crate::ir::IRExceptHandler {
                            exception_types: vec!["CancelledError".to_string()],
                            name: Some(cancelled.clone()),
                            body: IRBody {
                                statements: vec![IRStatement::Raise {
                                    exception: Some(call("TimeoutError", vec![])),
                                    cause: Some(v(&cancelled)),
                                }],
                            },
                        }],
                        finally_body: None,
                    },
                ],
                self.read_result(target, v(&f), env),
            ),
        ];
        if constant_positive == Some(false) {
            pre.extend(expired);
        } else {
            pre.push(if_else(
                cmp(v(&t), crate::ir::IRCompareOp::LtE, float(0.0)),
                expired,
                timed,
            ));
        }
        self.expand_nested(pre, env, scope, result)
    }

    /// Statements that may hold awaits at any depth, as written by this pass.
    fn expand_nested(
        &mut self,
        stmts: Vec<IRStatement>,
        env: &mut HashMap<String, IRType>,
        scope: &Scope,
        result: &IRType,
    ) -> Result<Vec<IRStatement>> {
        self.statements(stmts, env, scope, result)
    }

    /// Move each `await` in `e` (and what CPython evaluates before it) into
    /// statements ahead of it, leaving `e` free of awaits.
    fn hoist(
        &mut self,
        e: &mut IRExpr,
        pre: &mut Vec<IRStatement>,
        scope: &Scope,
        env: &mut HashMap<String, IRType>,
    ) -> Result<()> {
        if !contains_await(e) {
            return Ok(());
        }
        match e {
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if function_name == AWAIT_FN => {
                let [x] = arguments.as_mut_slice() else {
                    return Err(refuse("'await' takes one operand"));
                };
                // `await asyncio.gather(...)` / `wait_for(...)` keep their
                // arguments for the suspension to take apart.
                if self.gather_of_expr(x).is_none() && self.wait_for_of(x)?.is_none() {
                    self.hoist(x, pre, scope, env)?;
                }
                let r = self.fresh("aw_r");
                pre.push(assign(&r, std::mem::replace(e, v(&r))));
                Ok(())
            }
            IRExpr::BoolOp { left, right, .. } => {
                if contains_await(right) {
                    return Err(refuse(
                        "'await' in the right operand of 'and'/'or' is not supported, since \
                         CPython may not evaluate it. Hint: use an 'if' statement",
                    ));
                }
                self.hoist(left, pre, scope, env)
            }
            IRExpr::Conditional {
                condition,
                body,
                orelse,
            } => {
                if contains_await(body) || contains_await(orelse) {
                    return Err(refuse(
                        "'await' in an arm of a conditional expression is not supported, since \
                         CPython evaluates only one arm. Hint: use an 'if' statement",
                    ));
                }
                self.hoist(condition, pre, scope, env)
            }
            IRExpr::Comprehension { .. } | IRExpr::Lambda { .. } => Err(refuse(
                "'await' inside a comprehension or a lambda is not supported. Hint: write the \
                 loop out",
            )),
            other => {
                let mut kids = children(other);
                self.hoist_refs(&mut kids, pre, scope, env)
            }
        }
    }

    /// Hoist the awaits in sibling expressions evaluated in this order: the
    /// ones before the last await are kept in temporaries unless nothing can
    /// change them while it waits.
    fn hoist_refs(
        &mut self,
        kids: &mut [&mut IRExpr],
        pre: &mut Vec<IRStatement>,
        scope: &Scope,
        env: &mut HashMap<String, IRType>,
    ) -> Result<()> {
        let Some(last) = kids.iter().rposition(|k| contains_await(k)) else {
            return Ok(());
        };
        for (k, kid) in kids.iter_mut().enumerate() {
            if k > last {
                break;
            }
            self.hoist(kid, pre, scope, env)?;
            if k < last && !self.stable(kid, scope) {
                let value = std::mem::replace(*kid, none());
                **kid = self.spill(value, pre, env);
            }
        }
        Ok(())
    }

    fn hoist_seq(
        &mut self,
        parts: &mut [IRExpr],
        pre: &mut Vec<IRStatement>,
        scope: &Scope,
        env: &mut HashMap<String, IRType>,
    ) -> Result<()> {
        let mut refs: Vec<&mut IRExpr> = parts.iter_mut().collect();
        self.hoist_refs(&mut refs, pre, scope, env)
    }

    /// Whether nothing an `await` runs can change what `e` evaluates to: a
    /// constant, a local of this coroutine, or a module's name.
    fn stable(&self, e: &IRExpr, scope: &Scope) -> bool {
        match e {
            IRExpr::Const(_) => true,
            IRExpr::Variable(n) | IRExpr::Param(n) => {
                scope.locals.contains(n) || self.modules.contains(n) || n.starts_with("__aw")
            }
            _ => false,
        }
    }

    /// `e` computed into a temporary, typed when its type is known.
    fn spill(
        &mut self,
        e: IRExpr,
        pre: &mut Vec<IRStatement>,
        env: &mut HashMap<String, IRType>,
    ) -> IRExpr {
        if matches!(e, IRExpr::Variable(ref n) if n.starts_with("__aw")) {
            return e;
        }
        let name = self.fresh("aw_t");
        let ty = self.ty(&e, env);
        pre.push(assign_typed(&name, e, &ty));
        env.insert(name.clone(), ty);
        v(&name)
    }
}

/// What `await` is applied to, when `value` is an `await`.
fn awaited(value: &IRExpr) -> Option<&IRExpr> {
    match value {
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if function_name == AWAIT_FN => arguments.first(),
        _ => None,
    }
}

fn is_starred(e: &IRExpr) -> bool {
    matches!(e, IRExpr::Keyword { name, .. } if name == crate::ir::STARRED_ARG)
}

/// Raise, at the top of a coroutine, what was thrown into it before it
/// started.
fn throw_check() -> IRStatement {
    if_(
        my(THROWN),
        vec![
            set_attr(v("self"), THROWN, boolean(false)),
            IRStatement::Raise {
                exception: Some(my(THROW)),
                cause: None,
            },
        ],
    )
}

/// `yield None` in a coroutine: let the event loop run once, and raise what
/// is thrown in meanwhile.
pub(crate) fn bare_yield() -> Vec<IRStatement> {
    vec![
        IRStatement::Yield {
            value: Some(none()),
        },
        throw_check(),
    ]
}

/// Whether an expression holds an `await`.
pub(crate) fn contains_await(e: &IRExpr) -> bool {
    let mut found = false;
    let mut probe = e.clone();
    visit_expr(&mut probe, &mut |x| {
        if matches!(x, IRExpr::FunctionCall { function_name, .. } if function_name == AWAIT_FN) {
            found = true;
        }
    });
    found
}

pub(crate) fn visit_expr(e: &mut IRExpr, f: &mut impl FnMut(&mut IRExpr)) {
    f(e);
    for kid in children(e) {
        visit_expr(kid, f);
    }
    match e {
        IRExpr::BoolOp { left, right, .. } => {
            visit_expr(left, f);
            visit_expr(right, f);
        }
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => {
            visit_expr(condition, f);
            visit_expr(body, f);
            visit_expr(orelse, f);
        }
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            visit_expr(element, f);
            if let Some(v) = value {
                visit_expr(v, f);
            }
            for g in generators {
                visit_expr(&mut g.iterable, f);
                for c in &mut g.conditions {
                    visit_expr(c, f);
                }
            }
        }
        IRExpr::Lambda { body, .. } => visit_expr(body, f),
        _ => {}
    }
}

/// An expression's operands in the order CPython evaluates them, but for
/// those of `and`/`or`, a conditional expression, a comprehension, and a
/// lambda, which are evaluated conditionally or more than once.
fn children(e: &mut IRExpr) -> Vec<&mut IRExpr> {
    match e {
        IRExpr::BinaryOp { left, right, .. } | IRExpr::CompareOp { left, right, .. } => {
            vec![left, right]
        }
        IRExpr::UnaryOp { operand, .. } => vec![operand],
        IRExpr::FunctionCall { arguments, .. } => arguments.iter_mut().collect(),
        IRExpr::MethodCall {
            object, arguments, ..
        } => std::iter::once(object.as_mut())
            .chain(arguments.iter_mut())
            .collect(),
        IRExpr::Keyword { value, .. } => vec![value],
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            items.iter_mut().collect()
        }
        IRExpr::DictLiteral(entries) => entries.iter_mut().flat_map(|(k, v)| [k, v]).collect(),
        IRExpr::Indexing { container, index } => vec![container, index],
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => std::iter::once(container.as_mut())
            .chain(start.as_deref_mut())
            .chain(end.as_deref_mut())
            .chain(step.as_deref_mut())
            .collect(),
        IRExpr::Attribute { object, .. } => vec![object],
        IRExpr::DynamicImportExpr { module_name } => vec![module_name],
        IRExpr::RangeCall { start, stop, step } => start
            .as_deref_mut()
            .into_iter()
            .chain(std::iter::once(stop.as_mut()))
            .chain(step.as_deref_mut())
            .collect(),
        IRExpr::CellStore { value, .. } => vec![value],
        _ => Vec::new(),
    }
}

/// Apply `f` to each expression a statement holds directly (not those of
/// its nested bodies), outermost first.
fn each_expr(stmt: &mut IRStatement, f: &mut impl FnMut(&mut IRExpr)) {
    let mut visit = |e: &mut IRExpr| visit_expr(e, f);
    match stmt {
        IRStatement::Return(Some(e))
        | IRStatement::Expression(e)
        | IRStatement::Assign { value: e, .. }
        | IRStatement::AugAssign { value: e, .. }
        | IRStatement::TupleUnpack { value: e, .. }
        | IRStatement::Yield { value: Some(e) }
        | IRStatement::DynamicImport { module_name: e, .. } => visit(e),
        IRStatement::If { condition, .. } | IRStatement::While { condition, .. } => {
            visit(condition)
        }
        IRStatement::For { iterable, .. } => visit(iterable),
        IRStatement::With { context_expr, .. } => visit(context_expr),
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => {
            visit(object);
            visit(value);
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            visit(container);
            visit(index);
            visit(value);
        }
        IRStatement::Raise { exception, cause } => {
            if let Some(e) = exception {
                visit(e);
            }
            if let Some(c) = cause {
                visit(c);
            }
        }
        _ => {}
    }
}
