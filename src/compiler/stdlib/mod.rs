//! Code generation for the standard library's functions.
//!
//! `crate::stdlib` says which modules exist and folds their constants; the
//! calls into them are emitted here, a module per stdlib module, as inline
//! WASM and runtime helpers. A function a module here does not implement
//! answers `None`, and the call site refuses it.

mod functools;
pub(crate) mod json;
mod math;
mod os;
mod random;

pub(crate) use os::{emit_environ, OsFn};

pub(crate) use random::RandomFn;

pub(crate) fn build_random(ctx: &CompilationContext, routine: RandomFn) -> Function {
    random::build(ctx, routine)
}

pub(crate) fn build_os(ctx: &CompilationContext, routine: OsFn) -> Function {
    os::build(ctx, routine)
}

use crate::compiler::context::CompilationContext;
use crate::ir::{IRExpr, IRType, MemoryLayout};
use wasm_encoder::Function;

/// Emit a call to the dotted stdlib function (`math.sqrt`), or `None` when
/// it is not implemented.
pub(crate) fn emit_call(
    qualified: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected: Option<&IRType>,
) -> Option<IRType> {
    let (module, name) = qualified.rsplit_once('.')?;
    match module {
        "json" => json::emit_call(name, arguments, func, ctx, memory_layout, expected),
        "math" => math::emit_call(name, arguments, func, ctx, memory_layout),
        "random" => random::emit_call(name, arguments, func, ctx, memory_layout),
        "os" | "os.path" => os::emit_call(qualified, arguments, func, ctx, memory_layout),
        "functools" => functools::emit_call(name, arguments, func, ctx, memory_layout),
        _ => None,
    }
}

/// The type a call into the standard library answers, as far as can be told
/// before code is generated: `math.sqrt(x)` is a float, `random.choice(xs)`
/// an element of `xs`. `None` when the call is not one, or answers nothing.
pub(crate) fn result_type(call: &IRExpr, ctx: &CompilationContext) -> Option<IRType> {
    let (qualified, arguments) = match call {
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => (
            format!(
                "{}.{method_name}",
                crate::compiler::expression::stdlib_call_name(ctx, object)?
            ),
            arguments,
        ),
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if ctx.get_function_info(function_name).is_none() => {
            let (module, name) = ctx.stdlib_imports.get(function_name)?;
            (format!("{module}.{name}"), arguments)
        }
        _ => return None,
    };
    let (module, name) = qualified.rsplit_once('.')?;
    let ty = match module {
        "math" => math::result_type(name),
        "random" => random::result_type(name, arguments, ctx),
        "os" | "os.path" => os::result_type(&qualified, arguments),
        "functools" => functools::result_type(name, arguments, ctx),
        "json" => json::result_type(name),
        _ => None,
    }?;
    Some(ty).filter(|t| !matches!(t, IRType::None))
}
