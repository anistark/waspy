//! Python's dynamic values: what a place holds when static types run out.
//!
//! A `typing.Any` annotation, a collection literal whose elements differ in
//! type (`{"name": "bob", "age": 30}`, `[1, "a"]`), and JSON parsed into
//! `Any` hold a dynamic value: an instance of the class [`ANY_CLASS`], one
//! subclass per kind of value (None, bool, int, float, str, list, tuple,
//! dict), whose methods answer each operation as CPython's built-in type
//! does. The classes and the functions that box and unbox values are written
//! in the supported subset (`dynamic.py`) and grafted into a program that
//! uses them, so nothing is interpreted (critical rule 6).
//!
//! Code generation does the rest: a value stored where `Any` is declared is
//! boxed by calling [`box_fn`]'s function, a dynamic value stored where a
//! type is declared is unboxed by [`unbox_fn`]'s (a `TypeError` for a value
//! of another kind, as the JavaScript bridge raises), and the operators,
//! `len()`, indexing, iteration, and printing are the class's own methods.
//! A list or dict a dynamic value holds is the very object the program
//! holds, so a change made through one name is seen through the other.

use crate::ir::{IRModule, IRType};

/// The class every dynamic value is an instance of.
pub const ANY_CLASS: &str = "_Any";

/// The prefix of the runtime's functions, kept out of exports and metadata.
pub const ANY_FN_PREFIX: &str = "__any_";

/// The prefix of the runtime's helpers that its methods call.
pub const ANY_HELPER_PREFIX: &str = "_any_";

/// The runtime, in the supported subset.
const SOURCE: &str = include_str!("dynamic.py");

thread_local! {
    /// Set by code generation when it finds a place that holds dynamic
    /// values (a literal mixing types) in a module without the runtime, so
    /// the module is compiled again with it.
    pub static NEEDS_RUNTIME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether `name` is one of the runtime's classes.
pub fn is_any_class(name: &str) -> bool {
    name == ANY_CLASS || name.starts_with("_Any")
}

/// Whether `ty` is a dynamic value.
pub fn is_any(ty: &IRType) -> bool {
    matches!(ty, IRType::Class(name) if is_any_class(name))
}

/// The type of a dynamic value.
pub fn any_type() -> IRType {
    IRType::Class(ANY_CLASS.to_string())
}

/// The function that boxes a value of type `ty` into a dynamic value, when
/// one does.
pub fn box_fn(ty: &IRType) -> Option<&'static str> {
    Some(match ty {
        IRType::None => "__any_none",
        IRType::Bool => "__any_of_bool",
        IRType::Int => "__any_of_int",
        IRType::Float => "__any_of_float",
        IRType::String => "__any_of_str",
        IRType::Optional(inner) => match **inner {
            IRType::String => "__any_of_opt_str",
            IRType::Int => "__any_of_opt_int",
            IRType::Float => "__any_of_opt_float",
            IRType::Bool => "__any_of_opt_bool",
            _ => return None,
        },
        IRType::List(e) if is_any(e) => "__any_of_list",
        IRType::Dict(k, v) if **k == IRType::String && is_any(v) => "__any_of_dict",
        _ => return None,
    })
}

/// The function that unboxes a dynamic value stored where `ty` is declared,
/// when one does.
pub fn unbox_fn(ty: &IRType) -> Option<&'static str> {
    Some(match ty {
        IRType::Bool => "__any_as_bool",
        IRType::Int => "__any_as_int",
        IRType::Float => "__any_as_float",
        IRType::String => "__any_as_str",
        IRType::List(e) if is_any(e) => "__any_as_list",
        IRType::Dict(k, v) if **k == IRType::String && is_any(v) => "__any_as_dict",
        _ => return None,
    })
}

/// Whether `ty` holds a dynamic value anywhere.
pub fn mentions_any(ty: &IRType) -> bool {
    match ty {
        IRType::Class(name) => is_any_class(name),
        IRType::List(e)
        | IRType::Set(e)
        | IRType::VarTuple(e)
        | IRType::Optional(e)
        | IRType::Generator(e)
        | IRType::Coroutine(e) => mentions_any(e),
        IRType::Dict(k, v) => mentions_any(k) || mentions_any(v),
        IRType::Tuple(ts) | IRType::Union(ts) => ts.iter().any(mentions_any),
        _ => false,
    }
}

/// Whether the module names a dynamic value anywhere a type is written, which
/// is what grafting the runtime before code generation answers.
fn names_any(module: &IRModule) -> bool {
    fn mentions(ty: &IRType) -> bool {
        match ty {
            IRType::Class(name) => is_any_class(name),
            IRType::List(e)
            | IRType::Set(e)
            | IRType::VarTuple(e)
            | IRType::Optional(e)
            | IRType::Generator(e)
            | IRType::Coroutine(e) => mentions(e),
            IRType::Dict(k, v) => mentions(k) || mentions(v),
            IRType::Tuple(ts) | IRType::Union(ts) => ts.iter().any(mentions),
            IRType::Callable {
                params,
                return_type,
            } => params.iter().any(mentions) || mentions(return_type),
            _ => false,
        }
    }
    let function = |f: &crate::ir::IRFunction| {
        mentions(&f.return_type)
            || f.params.iter().any(|p| mentions(&p.param_type))
            || body_mentions(&f.body, &mentions)
    };
    module.functions.iter().any(function)
        || module.classes.iter().any(|c| {
            c.methods.iter().any(function)
                || c.class_vars
                    .iter()
                    .any(|v| v.var_type.as_ref().is_some_and(mentions))
        })
        || module
            .variables
            .iter()
            .any(|v| v.var_type.as_ref().is_some_and(mentions))
}

/// Whether a statement of `body` declares a type `mentions` answers for.
fn body_mentions(body: &crate::ir::IRBody, mentions: &dyn Fn(&IRType) -> bool) -> bool {
    use crate::ir::IRStatement;
    body.statements.iter().any(|s| match s {
        IRStatement::Assign {
            var_type: Some(t), ..
        } => mentions(t),
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            body_mentions(then_body, mentions)
                || else_body
                    .as_ref()
                    .is_some_and(|b| body_mentions(b, mentions))
        }
        IRStatement::While { body, .. } | IRStatement::For { body, .. } => {
            body_mentions(body, mentions)
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            body_mentions(try_body, mentions)
                || except_handlers
                    .iter()
                    .any(|h| body_mentions(&h.body, mentions))
                || finally_body
                    .as_ref()
                    .is_some_and(|b| body_mentions(b, mentions))
        }
        _ => false,
    })
}

/// The module with the dynamic-value runtime, when it names `Any` or `force`
/// says code generation found a place that needs it (a literal mixing types),
/// or `None` when neither does or the runtime is already there.
pub fn with_any_runtime(module: &IRModule, force: bool) -> anyhow::Result<Option<IRModule>> {
    if module.classes.iter().any(|c| c.name == ANY_CLASS) || !(force || names_any(module)) {
        return Ok(None);
    }
    let mut module = module.clone();
    crate::ir::embedded::graft(&mut module, SOURCE)?;
    Ok(Some(module))
}
