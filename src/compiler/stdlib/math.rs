//! `math`'s functions, as CPython's `mathmodule.c` defines them over libm.
//!
//! An argument is converted to a float the way `PyFloat_AsDouble` does (an
//! int or a bool widens), the routine runs (a WASM instruction or a `libm`
//! helper), and the result is checked the way CPython's wrappers check it:
//! a NaN from a non-NaN argument is `ValueError("math domain error")`, an
//! infinity from a finite one is `OverflowError("math range error")` where
//! the function can overflow and a domain error where it cannot.

use crate::compiler::context::CompilationContext;
use crate::compiler::exceptions::ExcArg;
use crate::compiler::expression::emit_expr;
use crate::compiler::function::emit_raise;
use crate::compiler::libm::{Libm, VEC};
use crate::compiler::runtime::Helper;
use crate::ir::{IRExpr, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction as I, MemArg};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

const DOMAIN: &str = "math domain error";
const RANGE: &str = "math range error";

/// What a one-argument float function computes.
#[derive(Clone, Copy)]
enum Op {
    Routine(Libm),
    Sqrt,
    Fabs,
}

/// How a result is checked, as `math_1`'s `can_overflow` says.
#[derive(Clone, Copy, PartialEq)]
enum Check {
    /// Never an error (`fabs`).
    None,
    /// An infinity from a finite argument is a domain error (a singularity).
    Domain,
    /// An infinity from a finite argument overflowed.
    Overflow,
}

/// Emit `math.<name>(arguments)`, or `None` when `math` has no such function
/// (the caller refuses it).
pub(crate) fn emit_call(
    name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> Option<IRType> {
    let one = |op: Op, check: Check| (op, check);
    let unary = match name {
        "sqrt" => Some(one(Op::Sqrt, Check::Domain)),
        "fabs" => Some(one(Op::Fabs, Check::None)),
        "sin" => Some(one(Op::Routine(Libm::Sin), Check::Domain)),
        "cos" => Some(one(Op::Routine(Libm::Cos), Check::Domain)),
        "tan" => Some(one(Op::Routine(Libm::Tan), Check::Domain)),
        "asin" => Some(one(Op::Routine(Libm::Asin), Check::Domain)),
        "acos" => Some(one(Op::Routine(Libm::Acos), Check::Domain)),
        "atan" => Some(one(Op::Routine(Libm::Atan), Check::Domain)),
        "sinh" => Some(one(Op::Routine(Libm::Sinh), Check::Overflow)),
        "cosh" => Some(one(Op::Routine(Libm::Cosh), Check::Overflow)),
        "tanh" => Some(one(Op::Routine(Libm::Tanh), Check::Domain)),
        "exp" => Some(one(Op::Routine(Libm::Exp), Check::Overflow)),
        _ => None,
    };
    if let Some((op, check)) = unary {
        if !arity(name, arguments, 1, 1, func, ctx) {
            return Some(IRType::Float);
        }
        let x = ctx.hold_f64()?;
        if !emit_float_arg(name, &arguments[0], func, ctx, memory_layout) {
            ctx.release_held_f64();
            return Some(IRType::Float);
        }
        emit!(func, I::LocalTee(x));
        match op {
            Op::Sqrt => emit!(func, I::F64Sqrt),
            Op::Fabs => emit!(func, I::F64Abs),
            Op::Routine(routine) => emit!(func, I::Call(ctx.helper(Helper::Libm(routine)))),
        }
        check_result(func, ctx, &[x], check);
        ctx.release_held_f64();
        return Some(IRType::Float);
    }
    match name {
        "log" | "log10" | "log2" => {
            if !arity(
                name,
                arguments,
                1,
                if name == "log" { 2 } else { 1 },
                func,
                ctx,
            ) {
                return Some(IRType::Float);
            }
            let routine = match name {
                "log" => Libm::Log,
                "log10" => Libm::Log10,
                _ => Libm::Log2,
            };
            emit_log(routine, &arguments[0], name, func, ctx, memory_layout)?;
            if let Some(base) = arguments.get(1) {
                // log(x, base) is log(x) / log(base), a float division.
                let num = ctx.hold_f64()?;
                emit!(func, I::LocalSet(num));
                emit_log(routine, base, name, func, ctx, memory_layout)?;
                let den = ctx.temp_local_f64;
                emit!(
                    func,
                    I::LocalTee(den),
                    I::F64Const(0.0.into()),
                    I::F64Eq,
                    I::If(BlockType::Empty)
                );
                emit_raise(
                    func,
                    ctx,
                    "ZeroDivisionError",
                    ExcArg::Text("float division by zero"),
                    1,
                );
                emit!(func, I::End, I::LocalGet(num), I::LocalGet(den), I::F64Div);
                ctx.release_held_f64();
            }
            Some(IRType::Float)
        }
        "atan2" | "pow" | "fmod" | "remainder" | "copysign" => {
            if !arity(name, arguments, 2, 2, func, ctx) {
                return Some(IRType::Float);
            }
            let x = ctx.hold_f64()?;
            let Some(y) = ctx.hold_f64() else {
                ctx.release_held_f64();
                return None;
            };
            let ok = emit_float_arg(name, &arguments[0], func, ctx, memory_layout);
            emit!(func, I::LocalSet(x));
            let ok = ok && emit_float_arg(name, &arguments[1], func, ctx, memory_layout);
            emit!(func, I::LocalSet(y));
            if ok {
                match name {
                    "atan2" => emit_atan2(func, ctx, x, y),
                    "pow" => emit_pow(func, ctx, x, y),
                    "fmod" => emit_fmod(func, ctx, x, y),
                    "remainder" => {
                        emit!(
                            func,
                            I::LocalGet(x),
                            I::LocalGet(y),
                            I::Call(ctx.helper(Helper::Libm(Libm::Remainder)))
                        );
                        check_result(func, ctx, &[x, y], Check::Overflow);
                    }
                    _ => emit!(func, I::LocalGet(x), I::LocalGet(y), I::F64Copysign),
                }
            } else {
                emit!(func, I::F64Const(0.0.into()));
            }
            ctx.release_held_f64();
            ctx.release_held_f64();
            Some(IRType::Float)
        }
        "hypot" => {
            if arguments
                .iter()
                .any(|a| matches!(a, IRExpr::Keyword { .. }))
            {
                return refuse(func, ctx, "math.hypot() takes no keyword arguments");
            }
            // Every coordinate is held while the next is emitted (one may be
            // a hypot itself), then all are stored where the routine reads.
            let mut held = Vec::new();
            let mut ok = true;
            for arg in arguments {
                let Some(slot) = ctx.hold_f64() else {
                    ok = false;
                    ctx.report("math.hypot() of more coordinates than the compiler holds at once");
                    break;
                };
                held.push(slot);
                ok &= emit_float_arg("hypot", arg, func, ctx, memory_layout);
                emit!(func, I::F64Abs, I::LocalSet(slot));
            }
            for (k, slot) in held.iter().enumerate() {
                emit!(
                    func,
                    I::I32Const(VEC + 8 * k as i32),
                    I::LocalGet(*slot),
                    I::F64Store(MemArg {
                        offset: 0,
                        align: 3,
                        memory_index: 0
                    })
                );
            }
            for _ in &held {
                ctx.release_held_f64();
            }
            if ok {
                emit!(
                    func,
                    I::I32Const(arguments.len() as i32),
                    I::Call(ctx.helper(Helper::Libm(Libm::VectorNorm)))
                );
            } else {
                emit!(func, I::F64Const(0.0.into()));
            }
            Some(IRType::Float)
        }
        "degrees" | "radians" => {
            if !arity(name, arguments, 1, 1, func, ctx)
                || !emit_float_arg(name, &arguments[0], func, ctx, memory_layout)
            {
                return Some(IRType::Float);
            }
            let factor = if name == "degrees" {
                180.0 / std::f64::consts::PI
            } else {
                std::f64::consts::PI / 180.0
            };
            emit!(func, I::F64Const(factor.into()), I::F64Mul);
            Some(IRType::Float)
        }
        "floor" | "ceil" | "trunc" => {
            if !arity(name, arguments, 1, 1, func, ctx) {
                return Some(IRType::Int);
            }
            let ty = emit_expr(&arguments[0], func, ctx, memory_layout, None);
            match ty {
                IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any => {}
                IRType::Float => {
                    emit!(
                        func,
                        match name {
                            "floor" => I::F64Floor,
                            "ceil" => I::F64Ceil,
                            _ => I::F64Trunc,
                        }
                    );
                    emit_float_to_int(func, ctx);
                }
                other => {
                    drop_value(func, &other);
                    ctx.report(format!(
                        "math.{name}() takes a number, got {}",
                        crate::type_to_string(&other)
                    ));
                    emit!(func, I::I32Const(0));
                }
            }
            Some(IRType::Int)
        }
        "isnan" | "isinf" | "isfinite" => {
            if !arity(name, arguments, 1, 1, func, ctx)
                || !emit_float_arg(name, &arguments[0], func, ctx, memory_layout)
            {
                return Some(IRType::Bool);
            }
            let x = ctx.temp_local_f64;
            emit!(func, I::LocalTee(x));
            match name {
                // x != x
                "isnan" => emit!(func, I::LocalGet(x), I::F64Ne),
                // |x| == inf
                "isinf" => emit!(func, I::F64Abs, I::F64Const(f64::INFINITY.into()), I::F64Eq),
                // x - x == 0
                _ => emit!(
                    func,
                    I::LocalGet(x),
                    I::F64Sub,
                    I::F64Const(0.0.into()),
                    I::F64Eq
                ),
            }
            Some(IRType::Bool)
        }
        "factorial" => {
            if !arity(name, arguments, 1, 1, func, ctx)
                || !emit_int_arg(name, &arguments[0], func, ctx, memory_layout)
            {
                return Some(IRType::Int);
            }
            let (n, acc) = (ctx.temp_local, ctx.temp_local + 1);
            emit!(
                func,
                I::LocalTee(n),
                I::I32Const(0),
                I::I32LtS,
                I::If(BlockType::Empty)
            );
            emit_raise(
                func,
                ctx,
                "ValueError",
                ExcArg::Text("factorial() not defined for negative values"),
                1,
            );
            // acc = 1; while n > 1: acc *= n; n -= 1 (wrapping, as int does)
            emit!(
                func,
                I::End,
                I::I32Const(1),
                I::LocalSet(acc),
                I::Block(BlockType::Empty),
                I::Loop(BlockType::Empty),
                I::LocalGet(n),
                I::I32Const(1),
                I::I32LeS,
                I::BrIf(1),
                I::LocalGet(acc),
                I::LocalGet(n),
                I::I32Mul,
                I::LocalSet(acc),
                I::LocalGet(n),
                I::I32Const(1),
                I::I32Sub,
                I::LocalSet(n),
                I::Br(0),
                I::End,
                I::End,
                I::LocalGet(acc)
            );
            Some(IRType::Int)
        }
        "gcd" => {
            if arguments
                .iter()
                .any(|a| matches!(a, IRExpr::Keyword { .. }))
            {
                return refuse(func, ctx, "math.gcd() takes no keyword arguments");
            }
            // gcd() is 0; each further argument folds in: g = gcd(g, |a|).
            let Some(g) = ctx.hold() else {
                return refuse(
                    func,
                    ctx,
                    "math.gcd() nests more deeply than the compiler holds",
                );
            };
            emit!(func, I::I32Const(0), I::LocalSet(g));
            let (a, t) = (ctx.temp_local, ctx.temp_local + 1);
            let mut ok = true;
            for arg in arguments {
                ok &= emit_int_arg("gcd", arg, func, ctx, memory_layout);
                // a = |arg|, wrapping for the most negative int.
                emit!(
                    func,
                    I::LocalTee(a),
                    I::LocalGet(a),
                    I::I32Const(31),
                    I::I32ShrS,
                    I::I32Xor,
                    I::LocalGet(a),
                    I::I32Const(31),
                    I::I32ShrS,
                    I::I32Sub,
                    I::LocalSet(a),
                    // while a != 0: g, a = a, g % a (unsigned, so a wrapped
                    // most negative int reads as 2**31)
                    I::Block(BlockType::Empty),
                    I::Loop(BlockType::Empty),
                    I::LocalGet(a),
                    I::I32Eqz,
                    I::BrIf(1),
                    I::LocalGet(g),
                    I::LocalGet(a),
                    I::I32RemU,
                    I::LocalSet(t),
                    I::LocalGet(a),
                    I::LocalSet(g),
                    I::LocalGet(t),
                    I::LocalSet(a),
                    I::Br(0),
                    I::End,
                    I::End
                );
            }
            emit!(func, I::LocalGet(g));
            ctx.release_held();
            if !ok {
                emit!(func, I::Drop, I::I32Const(0));
            }
            Some(IRType::Int)
        }
        _ => None,
    }
}

/// Report a refusal and push a float placeholder, so the module still
/// validates and every offending call is reported in one run.
fn refuse(func: &mut Function, ctx: &CompilationContext, message: &str) -> Option<IRType> {
    ctx.report(message.to_string());
    emit!(func, I::F64Const(0.0.into()));
    Some(IRType::Float)
}

fn drop_value(func: &mut Function, ty: &IRType) {
    emit!(func, I::Drop);
    if matches!(ty, IRType::String | IRType::Bytes) {
        emit!(func, I::Drop);
    }
}

/// Check the argument count (and that none is a keyword), reporting and
/// pushing a placeholder when it is wrong.
fn arity(
    name: &str,
    arguments: &[IRExpr],
    min: usize,
    max: usize,
    func: &mut Function,
    ctx: &CompilationContext,
) -> bool {
    if arguments
        .iter()
        .any(|a| matches!(a, IRExpr::Keyword { .. }))
    {
        ctx.report(format!("math.{name}() takes no keyword arguments"));
    } else if arguments.len() < min || arguments.len() > max {
        let expected = if min == max {
            format!("{min}")
        } else {
            format!("{min} to {max}")
        };
        ctx.report(format!(
            "math.{name}() takes {expected} argument{}, got {}",
            if max == 1 { "" } else { "s" },
            arguments.len()
        ));
    } else {
        return true;
    }
    emit!(func, I::F64Const(0.0.into()));
    false
}

/// Emit an argument as an f64, as `PyFloat_AsDouble` converts it. A value
/// that is not a number is refused (CPython's TypeError), leaving a
/// placeholder f64.
fn emit_float_arg(
    name: &str,
    arg: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> bool {
    let ty = emit_expr(arg, func, ctx, memory_layout, Some(&IRType::Float));
    match ty {
        IRType::Float => true,
        IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any => {
            emit!(func, I::F64ConvertI32S);
            true
        }
        other => {
            drop_value(func, &other);
            ctx.report(format!(
                "math.{name}() takes a number, got {}",
                crate::type_to_string(&other)
            ));
            emit!(func, I::F64Const(0.0.into()));
            false
        }
    }
}

/// Emit an argument that must be an int (`factorial`, `gcd`). A float is
/// CPython's TypeError, refused here like any other non-int.
fn emit_int_arg(
    name: &str,
    arg: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> bool {
    let ty = emit_expr(arg, func, ctx, memory_layout, Some(&IRType::Int));
    match ty {
        IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any => true,
        other => {
            drop_value(func, &other);
            ctx.report(format!(
                "math.{name}() takes an int, got {} ('{}' object cannot be interpreted as an \
                 integer)",
                crate::type_to_string(&other),
                crate::type_to_string(&other)
            ));
            emit!(func, I::I32Const(0));
            false
        }
    }
}

/// With the result on the stack and the arguments in `args`, raise as
/// CPython's `math_1` / `math_2` do, leaving the result.
fn check_result(func: &mut Function, ctx: &CompilationContext, args: &[u32], check: Check) {
    if check == Check::None {
        return;
    }
    let r = ctx.temp_local_f64_3;
    // nan(r) and no argument is nan: a domain error.
    emit!(func, I::LocalTee(r), I::LocalGet(r), I::F64Ne);
    for &x in args {
        emit!(func, I::LocalGet(x), I::LocalGet(x), I::F64Eq, I::I32And);
    }
    emit!(func, I::If(BlockType::Empty));
    emit_raise(func, ctx, "ValueError", ExcArg::Text(DOMAIN), 1);
    emit!(func, I::End);
    // inf(r) and every argument finite: overflow, or a singularity.
    emit!(
        func,
        I::LocalGet(r),
        I::F64Abs,
        I::F64Const(f64::INFINITY.into()),
        I::F64Eq
    );
    for &x in args {
        emit!(
            func,
            I::LocalGet(x),
            I::LocalGet(x),
            I::F64Sub,
            I::F64Const(0.0.into()),
            I::F64Eq,
            I::I32And
        );
    }
    emit!(func, I::If(BlockType::Empty));
    if check == Check::Overflow {
        emit_raise(func, ctx, "OverflowError", ExcArg::Text(RANGE), 1);
    } else {
        emit_raise(func, ctx, "ValueError", ExcArg::Text(DOMAIN), 1);
    }
    emit!(func, I::End, I::LocalGet(r));
}

/// `log`, `log10`, or `log2` of an argument, with CPython's `loghelper` and
/// `m_log`: zero, a negative, and -inf are domain errors; nan is nan.
fn emit_log(
    routine: Libm,
    arg: &IRExpr,
    name: &str,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> Option<()> {
    if !emit_float_arg(name, arg, func, ctx, memory_layout) {
        return Some(());
    }
    let x = ctx.temp_local_f64;
    emit!(
        func,
        I::LocalTee(x),
        I::F64Const(0.0.into()),
        I::F64Le,
        I::If(BlockType::Empty)
    );
    emit_raise(func, ctx, "ValueError", ExcArg::Text(DOMAIN), 1);
    emit!(
        func,
        I::End,
        I::LocalGet(x),
        I::Call(ctx.helper(Helper::Libm(routine)))
    );
    Some(())
}

/// CPython's `m_atan2`: the special values first, then libm.
fn emit_atan2(func: &mut Function, ctx: &CompilationContext, y: u32, x: u32) {
    // m_atan2 only differs from musl's atan2 in the NaN it returns, which
    // no Python value can tell apart, and in handling the infinities and
    // zeros itself, where both agree.
    emit!(
        func,
        I::LocalGet(y),
        I::LocalGet(x),
        I::Call(ctx.helper(Helper::Libm(Libm::Atan2)))
    );
}

/// CPython's `math_pow_impl`.
fn emit_pow(func: &mut Function, ctx: &CompilationContext, x: u32, y: u32) {
    // The IEEE specials are CPython's (they agree with musl's pow); a NaN
    // from finite arguments is a domain error, as is an infinity from a zero
    // base; any other infinity from finite arguments overflowed.
    let r = ctx.temp_local_f64_3;
    emit!(
        func,
        I::LocalGet(x),
        I::LocalGet(y),
        I::Call(ctx.helper(Helper::Libm(Libm::Pow))),
        I::LocalSet(r)
    );
    let finite = |func: &mut Function, v: u32| {
        emit!(
            func,
            I::LocalGet(v),
            I::LocalGet(v),
            I::F64Sub,
            I::F64Const(0.0.into()),
            I::F64Eq
        );
    };
    // both finite and r not finite
    finite(func, x);
    finite(func, y);
    emit!(func, I::I32And);
    finite(func, r);
    emit!(func, I::I32Eqz, I::I32And, I::If(BlockType::Empty));
    // nan(r) or x == 0: domain; else range.
    emit!(
        func,
        I::LocalGet(r),
        I::LocalGet(r),
        I::F64Ne,
        I::LocalGet(x),
        I::F64Const(0.0.into()),
        I::F64Eq,
        I::I32Or,
        I::If(BlockType::Empty)
    );
    emit_raise(func, ctx, "ValueError", ExcArg::Text(DOMAIN), 2);
    emit!(func, I::End);
    emit_raise(func, ctx, "OverflowError", ExcArg::Text(RANGE), 1);
    emit!(func, I::End, I::LocalGet(r));
}

/// CPython's `math_fmod_impl`: `fmod(x, ±inf)` is x for finite x, and a NaN
/// from non-NaN arguments is a domain error.
fn emit_fmod(func: &mut Function, ctx: &CompilationContext, x: u32, y: u32) {
    let r = ctx.temp_local_f64_3;
    emit!(
        func,
        I::LocalGet(x),
        I::LocalGet(y),
        I::Call(ctx.helper(Helper::Libm(Libm::Fmod))),
        I::LocalSet(r),
        // y infinite and x finite: x
        I::LocalGet(x),
        I::LocalGet(r),
        I::LocalGet(y),
        I::F64Abs,
        I::F64Const(f64::INFINITY.into()),
        I::F64Eq,
        I::LocalGet(x),
        I::LocalGet(x),
        I::F64Sub,
        I::F64Const(0.0.into()),
        I::F64Eq,
        I::I32And,
        I::Select,
        I::LocalTee(r),
        I::LocalGet(r),
        I::F64Ne,
        I::LocalGet(x),
        I::LocalGet(x),
        I::F64Eq,
        I::I32And,
        I::LocalGet(y),
        I::LocalGet(y),
        I::F64Eq,
        I::I32And,
        I::If(BlockType::Empty)
    );
    emit_raise(func, ctx, "ValueError", ExcArg::Text(DOMAIN), 1);
    emit!(func, I::End, I::LocalGet(r));
}

/// An integral f64 on the stack to an int: NaN is CPython's ValueError and
/// an infinity its OverflowError. A finite value outside the int range has
/// no int to be (see Numbers), and traps.
fn emit_float_to_int(func: &mut Function, ctx: &CompilationContext) {
    let v = ctx.temp_local_f64;
    emit!(
        func,
        I::LocalTee(v),
        I::LocalGet(v),
        I::F64Ne,
        I::If(BlockType::Empty)
    );
    emit_raise(
        func,
        ctx,
        "ValueError",
        ExcArg::Text("cannot convert float NaN to integer"),
        1,
    );
    emit!(
        func,
        I::End,
        I::LocalGet(v),
        I::F64Abs,
        I::F64Const(f64::INFINITY.into()),
        I::F64Eq,
        I::If(BlockType::Empty)
    );
    emit_raise(
        func,
        ctx,
        "OverflowError",
        ExcArg::Text("cannot convert float infinity to integer"),
        1,
    );
    emit!(func, I::End, I::LocalGet(v), I::I32TruncF64S);
}

/// What `math.<name>()` answers.
pub(crate) fn result_type(name: &str) -> Option<IRType> {
    Some(match name {
        "floor" | "ceil" | "trunc" | "factorial" | "gcd" => IRType::Int,
        "isnan" | "isinf" | "isfinite" => IRType::Bool,
        "sqrt" | "fabs" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sinh" | "cosh"
        | "tanh" | "exp" | "log" | "log10" | "log2" | "atan2" | "pow" | "fmod" | "remainder"
        | "copysign" | "hypot" | "degrees" | "radians" => IRType::Float,
        _ => return None,
    })
}
