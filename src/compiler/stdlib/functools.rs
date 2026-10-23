//! `functools`'s functions.
//!
//! The decorators (`@singledispatch`, `@total_ordering`, the caching ones)
//! are lowered in the converter. The calls are emitted here: `reduce` walks
//! a sequence, calling a function value at its signature through the
//! funcref table, as a lambda's call goes.

use crate::compiler::context::{CompilationContext, COLLECTION_DATA};
use crate::compiler::exceptions::ExcArg;
use crate::compiler::expression::{emit_as_sequence, emit_expr, recover_str_pair, wasm_value_type};
use crate::compiler::function::{emit_call_depth_step, emit_post_call_check, emit_raise};
use crate::ir::{IRExpr, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction as I, MemArg, ValType};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

fn slot(offset: u64, ty: &IRType) -> MemArg {
    MemArg {
        offset,
        align: if matches!(ty, IRType::Float) { 3 } else { 2 },
        memory_index: 0,
    }
}

/// Emit `functools.<name>(...)`, or `None` when it is not implemented.
pub(crate) fn emit_call(
    name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> Option<IRType> {
    match name {
        "reduce" => Some(emit_reduce(arguments, func, ctx, memory_layout)),
        _ => None,
    }
}

/// Push the zero value of a type's WASM shape, for a refusal's placeholder.
fn placeholder(func: &mut Function, ty: &IRType) {
    match ty {
        IRType::Float => emit!(func, I::F64Const(0.0.into())),
        IRType::String | IRType::Bytes => emit!(func, I::I32Const(0), I::I32Const(0)),
        IRType::None => {}
        _ => emit!(func, I::I32Const(0)),
    }
}

/// Whether a value of `value` can be passed where `param` is taken, and
/// whether it widens to a float on the way.
fn passes(value: &IRType, param: &IRType) -> Option<bool> {
    let word = |t: &IRType| {
        matches!(
            t,
            IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any
        )
    };
    match (value, param) {
        (IRType::Int | IRType::Bool, IRType::Float) => Some(true),
        // An untyped word is an int, whichever side it is on.
        (v, p) if word(v) && word(p) => Some(false),
        (v, p) if v == p => Some(false),
        _ => None,
    }
}

/// `reduce(function, iterable[, initial])`: CPython's left fold. Without an
/// initial value the first element starts it, and an empty sequence is a
/// `TypeError`.
fn emit_reduce(
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    let refuse = |func: &mut Function, message: String| {
        ctx.report(message);
        emit!(func, I::I32Const(0));
        IRType::Int
    };
    if arguments
        .iter()
        .any(|a| matches!(a, IRExpr::Keyword { .. }))
        || !(2..=3).contains(&arguments.len())
    {
        return refuse(
            func,
            "reduce() takes a function, an iterable, and an optional initial value, by position"
                .to_string(),
        );
    }
    let Some(env) = ctx.hold() else {
        return refuse(func, "reduce() nests too deeply".to_string());
    };
    let release = |n: usize| {
        for _ in 0..n {
            ctx.release_held();
        }
    };
    let fty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
    emit!(func, I::LocalSet(env));
    let (params, ret) = match fty {
        IRType::Callable {
            params,
            return_type,
        } if params.len() == 2 => {
            // A function whose result has no type answers one word, read as
            // an int, as every untyped place is.
            let ret = match *return_type {
                IRType::Unknown | IRType::Any => IRType::Int,
                other => other,
            };
            (params, ret)
        }
        other => {
            release(1);
            return refuse(
                func,
                format!(
                    "reduce() needs a function of two parameters whose types are known, got {}. \
                     Hint: pass a 'def' with annotated parameters, or a lambda over a sequence of \
                     known type",
                    crate::type_to_string(&other)
                ),
            );
        }
    };
    let (p0, p1) = (params[0].clone(), params[1].clone());
    let Some(seq) = ctx.hold() else {
        release(1);
        return refuse(func, "reduce() nests too deeply".to_string());
    };
    let sty = emit_expr(&arguments[1], func, ctx, memory_layout, None);
    let elem = match emit_as_sequence(func, ctx, sty) {
        IRType::List(e) | IRType::VarTuple(e) => *e,
        other => {
            emit!(func, I::Drop);
            if matches!(other, IRType::String | IRType::Bytes) {
                emit!(func, I::Drop);
            }
            release(2);
            return refuse(
                func,
                format!(
                    "reduce() takes a list, tuple, set, dict, or str here, got {}",
                    crate::type_to_string(&other)
                ),
            );
        }
    };
    emit!(func, I::LocalSet(seq));
    // The accumulator is the function's own result, fed back as its first
    // argument, so the two have to be one type.
    let Some(widen_ret) = passes(&ret, &p0) else {
        release(2);
        return refuse(
            func,
            format!(
                "reduce()'s function returns {} but takes {} as its first argument",
                crate::type_to_string(&ret),
                crate::type_to_string(&p0)
            ),
        );
    };
    let Some(widen_elem) = passes(&elem, &p1) else {
        release(2);
        return refuse(
            func,
            format!(
                "reduce()'s function takes {} as its second argument, but the sequence holds {}",
                crate::type_to_string(&p1),
                crate::type_to_string(&elem)
            ),
        );
    };
    let acc_is_float = matches!(p0, IRType::Float);
    let acc = if acc_is_float {
        ctx.hold_f64()
    } else {
        ctx.hold()
    };
    let Some(acc) = acc else {
        release(2);
        return refuse(func, "reduce() nests too deeply".to_string());
    };
    let release_acc = || {
        if acc_is_float {
            ctx.release_held_f64();
        } else {
            ctx.release_held();
        }
        release(2);
    };
    let (i, n) = (ctx.temp_local, ctx.temp_local + 1);
    emit!(
        func,
        I::LocalGet(seq),
        I::I32Load(slot(0, &IRType::Int)),
        I::LocalSet(n)
    );
    // Push element `i` (a local) as the parameter `p` takes it.
    let load_elem = |func: &mut Function, widen: bool| {
        emit!(
            func,
            I::LocalGet(seq),
            I::I32Load(slot(COLLECTION_DATA as u64, &IRType::Int)),
            I::LocalGet(i),
            I::I32Const(8),
            I::I32Mul,
            I::I32Add
        );
        if matches!(elem, IRType::Float) {
            emit!(func, I::F64Load(slot(0, &IRType::Float)));
        } else {
            emit!(func, I::I32Load(slot(0, &IRType::Int)));
            if widen {
                emit!(func, I::F64ConvertI32S);
            }
        }
    };
    match arguments.get(2) {
        Some(initial) => {
            let ity = emit_expr(initial, func, ctx, memory_layout, Some(&p0));
            match passes(&ity, &p0) {
                Some(widen) => {
                    if matches!(ity, IRType::String | IRType::Bytes) {
                        emit!(func, I::Drop);
                    }
                    if widen {
                        emit!(func, I::F64ConvertI32S);
                    }
                }
                None => {
                    ctx.report(format!(
                        "reduce()'s initial value is {} where its function takes {}",
                        crate::type_to_string(&ity),
                        crate::type_to_string(&p0)
                    ));
                    emit!(func, I::Drop);
                    placeholder(
                        func,
                        &if acc_is_float {
                            IRType::Float
                        } else {
                            IRType::Int
                        },
                    );
                }
            }
            emit!(func, I::LocalSet(acc), I::I32Const(0), I::LocalSet(i));
        }
        None => {
            // The first element starts the fold; an empty sequence has none.
            let Some(widen) = passes(&elem, &p0) else {
                release_acc();
                return refuse(
                    func,
                    format!(
                        "reduce() without an initial value starts from the first element, a {}, \
                         where its function takes {}",
                        crate::type_to_string(&elem),
                        crate::type_to_string(&p0)
                    ),
                );
            };
            emit!(func, I::LocalGet(n), I::I32Eqz, I::If(BlockType::Empty));
            emit_raise(
                func,
                ctx,
                "TypeError",
                ExcArg::Text("reduce() of empty iterable with no initial value"),
                1,
            );
            emit!(func, I::End, I::I32Const(0), I::LocalSet(i));
            load_elem(func, widen);
            emit!(func, I::LocalSet(acc), I::I32Const(1), I::LocalSet(i));
        }
    }
    // The call's signature: both parameters, then the environment.
    let key = (
        vec![wasm_value_type(&p0), wasm_value_type(&p1), ValType::I32],
        wasm_value_type(&ret),
    );
    let Some(&type_index) = ctx.signature_types.get(&key) else {
        release_acc();
        return refuse(
            func,
            "no function of this module has the signature reduce() calls with; this is a code \
             generation bug"
                .to_string(),
        );
    };
    let can_raise = !ctx.can_raise.is_empty();
    emit!(
        func,
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(i),
        I::LocalGet(n),
        I::I32GeS,
        I::BrIf(1),
        I::LocalGet(acc)
    );
    load_elem(func, widen_elem);
    emit!(
        func,
        I::LocalGet(env),
        I::LocalGet(env),
        I::I32Load(slot(0, &IRType::Int))
    );
    if can_raise {
        emit_call_depth_step(func, 1);
    }
    emit!(
        func,
        I::CallIndirect {
            type_index,
            table_index: 0,
        }
    );
    if can_raise {
        emit_call_depth_step(func, -1);
        // The loop's two blocks are open around the check.
        ctx.enter_expr_blocks(2);
        emit_post_call_check(func, ctx);
        ctx.exit_expr_blocks(2);
    }
    if widen_ret {
        emit!(func, I::F64ConvertI32S);
    }
    emit!(
        func,
        I::LocalSet(acc),
        I::LocalGet(i),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(i),
        I::Br(0),
        I::End,
        I::End,
        I::LocalGet(acc)
    );
    release_acc();
    let result = if widen_ret { IRType::Float } else { ret };
    if matches!(result, IRType::String | IRType::Bytes) {
        recover_str_pair(func, ctx);
    }
    result
}

/// What `functools.<name>()` answers, before code is generated.
pub(crate) fn result_type(
    name: &str,
    arguments: &[IRExpr],
    ctx: &CompilationContext,
) -> Option<IRType> {
    match name {
        "reduce" => match crate::compiler::function::infer_value_type(arguments.first()?, ctx) {
            IRType::Callable { return_type, .. } => Some(*return_type),
            _ => None,
        },
        _ => None,
    }
}
