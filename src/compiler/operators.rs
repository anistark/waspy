//! Binary operators whose operands are not numbers: sequence concatenation and
//! repetition, the set operators, and dict merge.
//!
//! The arithmetic path treats every non-float operand as an i32, which is right
//! for ints and bools and wrong for everything else: a collection is a pointer,
//! so `{1, 2} | {2, 3}` or-ed two addresses together and `"ab" * 3` multiplied
//! an offset. [`emit_non_numeric_binop`] runs once both operands are on the
//! stack and either implements the operator or refuses it, the way CPython
//! raises `TypeError` for an operand pair it does not define.
//!
//! Nothing here emits a nested expression, so the scratch locals are safe to
//! hold values across the whole sequence. The ones used sit above the ranges
//! the set insert and slot comparison helpers claim (`temp_local + 1` and
//! `temp_local + 14..=17`).

use crate::compiler::context::{
    CompilationContext, COLLECTION_CAP, COLLECTION_HEADER, COLLECTION_SLOT, DICT_ENTRY,
};
use crate::compiler::expression::emit_expr;
use crate::compiler::expression::{
    byte_arg, emit_data_base, emit_set_base, emit_set_hash, emit_slot_eq_needle,
    emit_stashed_set_insert, mem_off, operator_symbol, slot_arg, store_runtime_data_ptr,
    store_set_data_ptr, SET_BUCKET, SET_BUCKET_VALUE, SET_CAP, SET_HEADER, SET_LIVE, SET_USED,
};
use crate::ir::{
    IRCompareOp, IRConstant, IRExpr, IROp, IRType, IRUnaryOp, MemoryLayout, STRING_LEN_PREFIX,
};
use wasm_encoder::{BlockType, Function, Instruction};

/// The name CPython gives a value of this type in a `TypeError` message.
pub(crate) fn python_type_name(ty: &IRType) -> String {
    match ty {
        IRType::Int => "int".into(),
        IRType::Float => "float".into(),
        IRType::Bool => "bool".into(),
        IRType::String => "str".into(),
        IRType::Bytes => "bytes".into(),
        IRType::List(_) => "list".into(),
        IRType::Tuple(_) => "tuple".into(),
        IRType::Set(_) => "set".into(),
        IRType::Dict(_, _) => "dict".into(),
        IRType::None => "NoneType".into(),
        IRType::Range => "range".into(),
        IRType::Class(name) => name.clone(),
        IRType::Generator(_) => "generator".into(),
        IRType::Callable { .. } => "function".into(),
        other => crate::type_to_string(other),
    }
}

fn is_number(ty: &IRType) -> bool {
    matches!(
        ty,
        IRType::Int | IRType::Bool | IRType::Float | IRType::Unknown | IRType::Any
    )
}

fn is_count(ty: &IRType) -> bool {
    matches!(ty, IRType::Int | IRType::Bool)
}

/// Words a value of this type occupies on the stack.
fn stack_width(ty: &IRType) -> usize {
    if matches!(ty, IRType::String | IRType::Bytes) {
        2
    } else {
        1
    }
}

/// The compile-time value of a repeat count, when it is a literal.
fn const_count(expr: &IRExpr) -> Option<i64> {
    match expr {
        IRExpr::Const(IRConstant::Int(n)) => Some(*n as i64),
        IRExpr::Const(IRConstant::Bool(b)) => Some(*b as i64),
        IRExpr::UnaryOp {
            op: IRUnaryOp::Neg,
            operand,
        } => const_count(operand).map(|n| -n),
        _ => None,
    }
}

/// Element type shared by two collections, treating an unknown side (an empty
/// literal, say) as the other's. `None` when they differ, since slots of
/// different widths cannot be copied into one collection.
fn unify_elements(a: &IRType, b: &IRType) -> Option<IRType> {
    match (a, b) {
        (IRType::Unknown, other) | (other, IRType::Unknown) => Some(other.clone()),
        _ if a == b => Some(a.clone()),
        _ => None,
    }
}

/// Implement or refuse `left op right` when either operand is not a number.
/// Both operands are on the stack. Returns `None` for a pair of numbers, which
/// the caller's arithmetic handles; otherwise the operands have been consumed
/// and the result (or a placeholder of its shape, after a report) pushed.
pub(crate) fn emit_non_numeric_binop(
    func: &mut Function,
    ctx: &CompilationContext,
    op: &IROp,
    left: &IRExpr,
    right: &IRExpr,
    left_ty: &IRType,
    right_ty: &IRType,
) -> Option<IRType> {
    if is_number(left_ty) && is_number(right_ty) {
        return None;
    }
    let result = match (op, left_ty, right_ty) {
        (IROp::Mul, IRType::String | IRType::Bytes, n) if is_count(n) => {
            emit_text_repeat(func, ctx, left_ty);
            Some(left_ty.clone())
        }
        (IROp::Mul, n, IRType::String | IRType::Bytes) if is_count(n) => {
            swap_count_under_text(func, ctx);
            emit_text_repeat(func, ctx, right_ty);
            Some(right_ty.clone())
        }
        (IROp::Add, IRType::List(a), IRType::List(b)) => match unify_elements(a, b) {
            Some(elem) => {
                emit_sequence_concat(func, ctx);
                Some(IRType::List(Box::new(elem)))
            }
            None => {
                return Some(refuse(
                    func,
                    ctx,
                    op,
                    left_ty,
                    right_ty,
                    "the lists hold different element types, which this compiler stores at \
                     different widths",
                ))
            }
        },
        (IROp::Add, IRType::Tuple(a), IRType::Tuple(b)) => {
            emit_sequence_concat(func, ctx);
            Some(IRType::Tuple(a.iter().chain(b.iter()).cloned().collect()))
        }
        (IROp::Mul, IRType::List(_), n) if is_count(n) => {
            emit_sequence_repeat(func, ctx);
            Some(left_ty.clone())
        }
        (IROp::Mul, n, IRType::List(_)) if is_count(n) => {
            swap_words(func, ctx);
            emit_sequence_repeat(func, ctx);
            Some(right_ty.clone())
        }
        (IROp::Mul, IRType::Tuple(members), n) | (IROp::Mul, n, IRType::Tuple(members))
            if is_count(n) =>
        {
            let count_expr = if matches!(left_ty, IRType::Tuple(_)) {
                right
            } else {
                left
            };
            match const_count(count_expr) {
                Some(count) => {
                    if !matches!(left_ty, IRType::Tuple(_)) {
                        swap_words(func, ctx);
                    }
                    emit_sequence_repeat(func, ctx);
                    let times = count.max(0) as usize;
                    Some(IRType::Tuple(
                        (0..times).flat_map(|_| members.iter().cloned()).collect(),
                    ))
                }
                None => {
                    return Some(refuse(
                        func,
                        ctx,
                        op,
                        left_ty,
                        right_ty,
                        "a tuple's length is part of its type here, so the repeat count must \
                         be a constant. Hint: repeat a list instead",
                    ))
                }
            }
        }
        (IROp::BitOr | IROp::BitAnd | IROp::BitXor | IROp::Sub, IRType::Set(a), IRType::Set(b)) => {
            match unify_elements(a, b) {
                Some(elem) => {
                    emit_set_operator(func, ctx, op, &elem);
                    Some(IRType::Set(Box::new(elem)))
                }
                None => {
                    return Some(refuse(
                        func,
                        ctx,
                        op,
                        left_ty,
                        right_ty,
                        "the sets hold different element types, which this compiler hashes \
                         and stores differently",
                    ))
                }
            }
        }
        (IROp::BitOr, IRType::Dict(ka, va), IRType::Dict(kb, vb)) => {
            match (unify_elements(ka, kb), unify_elements(va, vb)) {
                (Some(key), Some(value)) => {
                    emit_dict_merge(func, ctx, &key);
                    Some(IRType::Dict(Box::new(key), Box::new(value)))
                }
                _ => {
                    return Some(refuse(
                        func,
                        ctx,
                        op,
                        left_ty,
                        right_ty,
                        "the dicts hold different key or value types, which this compiler \
                         stores at different widths",
                    ))
                }
            }
        }
        _ => None,
    };
    Some(result.unwrap_or_else(|| refuse(func, ctx, op, left_ty, right_ty, "")))
}

/// Report an operand pair the operator is not defined for (or not supported
/// on here), consuming the operands and leaving an i32 placeholder.
fn refuse(
    func: &mut Function,
    ctx: &CompilationContext,
    op: &IROp,
    left_ty: &IRType,
    right_ty: &IRType,
    why: &str,
) -> IRType {
    let detail = if why.is_empty() {
        String::new()
    } else {
        format!(": {why}")
    };
    ctx.report(format!(
        "unsupported operand type(s) for {}: '{}' and '{}'{detail}",
        operator_symbol(op),
        python_type_name(left_ty),
        python_type_name(right_ty)
    ));
    for _ in 0..stack_width(right_ty) + stack_width(left_ty) {
        func.instruction(&Instruction::Drop);
    }
    func.instruction(&Instruction::I32Const(0));
    IRType::Unknown
}

/// `(a, b)` on the stack becomes `(b, a)`, for two i32 words.
fn swap_words(func: &mut Function, ctx: &CompilationContext) {
    let top = ctx.temp_local + 20;
    let under = ctx.temp_local + 21;
    func.instruction(&Instruction::LocalSet(top));
    func.instruction(&Instruction::LocalSet(under));
    func.instruction(&Instruction::LocalGet(top));
    func.instruction(&Instruction::LocalGet(under));
}

/// `(n, offset, len)` becomes `(offset, len, n)`.
fn swap_count_under_text(func: &mut Function, ctx: &CompilationContext) {
    let len = ctx.temp_local + 20;
    let off = ctx.temp_local + 21;
    let n = ctx.temp_local + 22;
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));
    func.instruction(&Instruction::LocalSet(n));
    func.instruction(&Instruction::LocalGet(off));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalGet(n));
}

/// Push `max(local, 0)`, Python's treatment of a negative repeat count.
fn emit_clamped_count(func: &mut Function, n: u32) {
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(n));
}

/// Copy `count` back-to-back runs of `bytes` bytes from `src` to `dst`.
fn emit_repeated_copy(func: &mut Function, dst: u32, src: u32, bytes: u32, count: u32, i: u32) {
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(count));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(dst));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(bytes));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(src));
    func.instruction(&Instruction::LocalGet(bytes));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
}

/// `s * n` for a str or bytes: `(offset, len, n)` becomes the repeated
/// `(offset, len)`, a fresh length-prefixed blob (NUL-terminated for a str).
fn emit_text_repeat(func: &mut Function, ctx: &CompilationContext, ty: &IRType) {
    let n = ctx.temp_local + 20;
    let len = ctx.temp_local + 21;
    let off = ctx.temp_local + 22;
    let total = ctx.temp_local + 23;
    let data = ctx.temp_local + 24;
    let i = ctx.temp_local + 25;
    let is_string = matches!(ty, IRType::String);

    func.instruction(&Instruction::LocalSet(n));
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::LocalSet(off));
    emit_clamped_count(func, n);

    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(total));

    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Const(
        STRING_LEN_PREFIX as i32 + i32::from(is_string),
    ));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalTee(data));
    func.instruction(&Instruction::LocalGet(total));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(data));

    emit_repeated_copy(func, data, off, len, n, i);

    if is_string {
        func.instruction(&Instruction::LocalGet(data));
        func.instruction(&Instruction::LocalGet(total));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Store8(byte_arg()));
    }
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(total));
}

/// Allocate a list-layout region holding `len` slots (local), with its header
/// written, and leave its pointer in `region`.
fn emit_new_sequence(func: &mut Function, ctx: &CompilationContext, len: u32, region: u32) {
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalTee(region));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(region));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));
    store_runtime_data_ptr(func, region);
}

/// `xs + ys` for two lists or two tuples: `(a, b)` becomes a fresh sequence
/// holding a's elements then b's. Slots are copied as raw bytes, so every
/// element width survives.
fn emit_sequence_concat(func: &mut Function, ctx: &CompilationContext) {
    let b = ctx.temp_local + 20;
    let a = ctx.temp_local + 21;
    let la = ctx.temp_local + 22;
    let total = ctx.temp_local + 23;
    let region = ctx.temp_local + 24;

    func.instruction(&Instruction::LocalSet(b));
    func.instruction(&Instruction::LocalSet(a));
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalTee(la));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(total));
    emit_new_sequence(func, ctx, total, region);

    for (src, offset_slots) in [(a, None), (b, Some(la))] {
        func.instruction(&Instruction::LocalGet(region));
        emit_data_base(func);
        if let Some(skip) = offset_slots {
            func.instruction(&Instruction::LocalGet(skip));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
        }
        func.instruction(&Instruction::LocalGet(src));
        emit_data_base(func);
        func.instruction(&Instruction::LocalGet(src));
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
    }
    func.instruction(&Instruction::LocalGet(region));
}

/// `xs * n` for a list or tuple: `(seq, n)` becomes a fresh sequence holding
/// `n` copies of the elements. Like CPython's, the copy is shallow: a nested
/// collection is shared by every repetition.
fn emit_sequence_repeat(func: &mut Function, ctx: &CompilationContext) {
    let n = ctx.temp_local + 20;
    let src = ctx.temp_local + 21;
    let total = ctx.temp_local + 22;
    let region = ctx.temp_local + 23;
    let bytes = ctx.temp_local + 24;
    let src_data = ctx.temp_local + 25;
    let dst_data = ctx.temp_local + 26;
    let i = ctx.temp_local + 27;

    func.instruction(&Instruction::LocalSet(n));
    func.instruction(&Instruction::LocalSet(src));
    emit_clamped_count(func, n);

    func.instruction(&Instruction::LocalGet(src));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalTee(bytes));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32DivU);
    func.instruction(&Instruction::LocalSet(total));
    emit_new_sequence(func, ctx, total, region);

    func.instruction(&Instruction::LocalGet(src));
    emit_data_base(func);
    func.instruction(&Instruction::LocalSet(src_data));
    func.instruction(&Instruction::LocalGet(region));
    emit_data_base(func);
    func.instruction(&Instruction::LocalSet(dst_data));
    emit_repeated_copy(func, dst_data, src_data, bytes, n, i);
    func.instruction(&Instruction::LocalGet(region));
}

/// The set methods that mirror the operators and comparisons, with one set
/// argument: `a.union(b)` is `a | b`, `a.issubset(b)` is `a <= b`, and
/// `a.isdisjoint(b)` is `not (a & b)`. Entry stack: (a). `None` leaves any
/// other method to the caller. CPython also takes any iterable here; a
/// non-set argument is refused.
pub(crate) fn emit_set_method(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    set_type: &IRType,
) -> Option<IRType> {
    let operator = match method_name {
        "union" => Some(IROp::BitOr),
        "intersection" => Some(IROp::BitAnd),
        "difference" => Some(IROp::Sub),
        "symmetric_difference" => Some(IROp::BitXor),
        "issubset" | "issuperset" | "isdisjoint" => None,
        _ => return None,
    };
    let IRType::Set(a) = set_type else {
        return None;
    };
    let [argument] = arguments else {
        ctx.report(format!(
            "set.{method_name}() takes exactly one argument here, got {}",
            arguments.len()
        ));
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::I32Const(0));
        return Some(IRType::Unknown);
    };
    let arg_type = emit_expr(argument, func, ctx, memory_layout, None);
    let IRType::Set(b) = &arg_type else {
        ctx.report(format!(
            "set.{method_name}() of a {} is not supported: only a set argument is. Hint: build \
             a set from it first",
            python_type_name(&arg_type)
        ));
        for _ in 0..stack_width(&arg_type) + 1 {
            func.instruction(&Instruction::Drop);
        }
        func.instruction(&Instruction::I32Const(0));
        return Some(IRType::Unknown);
    };
    Some(match (operator, method_name) {
        (Some(op), _) => emit_non_numeric_binop(
            func,
            ctx,
            &op,
            &IRExpr::Const(IRConstant::None),
            argument,
            set_type,
            &arg_type,
        )
        .expect("two sets are never a pair of numbers"),
        (None, "issubset") => emit_set_comparison(func, ctx, &IRCompareOp::LtE, a, b),
        (None, "issuperset") => emit_set_comparison(func, ctx, &IRCompareOp::GtE, a, b),
        _ => {
            // Disjoint: the intersection is empty.
            match unify_elements(a, b) {
                Some(elem) => {
                    emit_set_operator(func, ctx, &IROp::BitAnd, &elem);
                    func.instruction(&Instruction::I32Load(slot_arg()));
                    func.instruction(&Instruction::I32Eqz);
                    IRType::Bool
                }
                None => refuse_set_comparison(func, ctx, set_type, &arg_type),
            }
        }
    })
}

/// `a OP b` for two sets on the stack, OP one of `== != < <= > >=`: subset and
/// equality by membership, the way CPython compares sets.
pub(crate) fn emit_set_comparison(
    func: &mut Function,
    ctx: &CompilationContext,
    op: &IRCompareOp,
    a: &IRType,
    b: &IRType,
) -> IRType {
    let Some(elem) = unify_elements(a, b) else {
        return refuse_set_comparison(
            func,
            ctx,
            &IRType::Set(Box::new(a.clone())),
            &IRType::Set(Box::new(b.clone())),
        );
    };
    let l = SetLocals {
        b: ctx.temp_local + 20,
        a: ctx.temp_local + 21,
        result: ctx.temp_local + 22,
        index: ctx.temp_local + 23,
        cap: ctx.temp_local + 24,
        bucket: ctx.temp_local + 25,
        mask: ctx.temp_local + 26,
        hidx: ctx.temp_local + 27,
        probe_bkt: ctx.temp_local + 28,
        found: ctx.temp_local + 29,
        probes: ctx.temp_local + 30,
        other_mask: ctx.temp_local + 31,
    };
    let subset = ctx.temp_local + 32;
    func.instruction(&Instruction::LocalSet(l.b));
    func.instruction(&Instruction::LocalSet(l.a));
    // Which set must lie inside which, and how the sizes must compare.
    let (inner, outer, size) = match op {
        IRCompareOp::LtE => (l.a, l.b, None),
        IRCompareOp::Lt => (l.a, l.b, Some(Instruction::I32LtS)),
        IRCompareOp::GtE => (l.b, l.a, None),
        IRCompareOp::Gt => (l.b, l.a, Some(Instruction::I32LtS)),
        IRCompareOp::Eq | IRCompareOp::NotEq => (l.a, l.b, Some(Instruction::I32Eq)),
        other => {
            ctx.report(format!("'{other:?}' between two sets is not supported"));
            func.instruction(&Instruction::I32Const(0));
            return IRType::Bool;
        }
    };
    // subset = every member of inner is in outer
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(subset));
    emit_for_each_member(func, ctx, &l, inner, &elem, |func| {
        emit_set_contains_needle(func, ctx, &l, outer, &elem);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(subset));
        func.instruction(&Instruction::End);
    });
    func.instruction(&Instruction::LocalGet(subset));
    if let Some(size) = size {
        func.instruction(&Instruction::LocalGet(inner));
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::LocalGet(outer));
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&size);
        func.instruction(&Instruction::I32And);
    }
    if matches!(op, IRCompareOp::NotEq) {
        func.instruction(&Instruction::I32Eqz);
    }
    IRType::Bool
}

fn refuse_set_comparison(
    func: &mut Function,
    ctx: &CompilationContext,
    a: &IRType,
    b: &IRType,
) -> IRType {
    ctx.report(format!(
        "comparing a {} with a {} is not supported: the sets hold different element types, \
         which this compiler hashes and stores differently",
        crate::type_to_string(a),
        crate::type_to_string(b)
    ));
    func.instruction(&Instruction::Drop);
    func.instruction(&Instruction::Drop);
    func.instruction(&Instruction::I32Const(0));
    IRType::Bool
}

/// Locals for the set operator sequence.
struct SetLocals {
    a: u32,
    b: u32,
    result: u32,
    index: u32,
    cap: u32,
    bucket: u32,
    mask: u32,
    hidx: u32,
    probe_bkt: u32,
    found: u32,
    probes: u32,
    other_mask: u32,
}

/// `a | b`, `a & b`, `a - b`, `a ^ b` on two sets: `(a, b)` becomes a fresh
/// set. The result table is sized for every member either operand could
/// contribute, so it never needs to grow while it is filled.
fn emit_set_operator(func: &mut Function, ctx: &CompilationContext, op: &IROp, elem: &IRType) {
    let l = SetLocals {
        b: ctx.temp_local + 20,
        a: ctx.temp_local + 21,
        result: ctx.temp_local + 22,
        index: ctx.temp_local + 23,
        cap: ctx.temp_local + 24,
        bucket: ctx.temp_local + 25,
        mask: ctx.temp_local + 26,
        hidx: ctx.temp_local + 27,
        probe_bkt: ctx.temp_local + 28,
        found: ctx.temp_local + 29,
        probes: ctx.temp_local + 30,
        other_mask: ctx.temp_local + 31,
    };
    func.instruction(&Instruction::LocalSet(l.b));
    func.instruction(&Instruction::LocalSet(l.a));

    // cap = the smallest power of two >= 2 * (members that can be added), at
    // least 2, keeping the load factor at or under 1/2 as every set does.
    func.instruction(&Instruction::LocalGet(l.a));
    func.instruction(&Instruction::I32Load(slot_arg()));
    if matches!(op, IROp::BitOr | IROp::BitXor) {
        func.instruction(&Instruction::LocalGet(l.b));
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::I32Add);
    }
    func.instruction(&Instruction::I32Const(2));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalTee(l.cap));
    func.instruction(&Instruction::I32Const(2));
    func.instruction(&Instruction::LocalGet(l.cap));
    func.instruction(&Instruction::I32Const(2));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(l.cap));
    // 1 << (32 - clz(cap - 1))
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Const(32));
    func.instruction(&Instruction::LocalGet(l.cap));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Clz);
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Shl);
    func.instruction(&Instruction::LocalSet(l.cap));

    // One zeroed block: the header, then the buckets right after it.
    func.instruction(&Instruction::LocalGet(l.cap));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const(SET_HEADER as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalTee(l.index));
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalTee(l.result));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(l.index));
    func.instruction(&Instruction::MemoryFill(0));
    func.instruction(&Instruction::LocalGet(l.result));
    func.instruction(&Instruction::LocalGet(l.cap));
    func.instruction(&Instruction::I32Store(mem_off(SET_CAP as u64)));
    store_set_data_ptr(func, l.result);
    func.instruction(&Instruction::LocalGet(l.cap));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(l.mask));

    // Which members of the source go in: all of them (`None`), or only those
    // the other set does (`Some(true)`) or does not (`Some(false)`) hold.
    let passes: Vec<(u32, u32, Option<bool>)> = match op {
        IROp::BitOr => vec![(l.a, l.b, None), (l.b, l.a, None)],
        IROp::BitAnd => vec![(l.a, l.b, Some(true))],
        IROp::Sub => vec![(l.a, l.b, Some(false))],
        _ => vec![(l.a, l.b, Some(false)), (l.b, l.a, Some(false))],
    };
    for (source, other, filter) in passes {
        emit_for_each_member(func, ctx, &l, source, elem, |func| match filter {
            None => emit_stashed_set_insert(func, ctx, elem, l.result, l.mask, l.hidx, l.probe_bkt),
            Some(want) => {
                emit_set_contains_needle(func, ctx, &l, other, elem);
                if !want {
                    func.instruction(&Instruction::I32Eqz);
                }
                func.instruction(&Instruction::If(BlockType::Empty));
                emit_stashed_set_insert(func, ctx, elem, l.result, l.mask, l.hidx, l.probe_bkt);
                func.instruction(&Instruction::End);
            }
        });
    }
    // `used` counts occupied buckets; with no tombstones it is the count.
    func.instruction(&Instruction::LocalGet(l.result));
    func.instruction(&Instruction::LocalGet(l.result));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Store(mem_off(SET_USED as u64)));
    func.instruction(&Instruction::LocalGet(l.result));
}

/// Run `body` once per live member of the set in `source`, with the member
/// stashed as the search needle (`temp_local_f64` for a float, otherwise
/// `temp_local + 1`), where the insert and probe helpers read it.
fn emit_for_each_member(
    func: &mut Function,
    ctx: &CompilationContext,
    l: &SetLocals,
    source: u32,
    elem: &IRType,
    body: impl FnOnce(&mut Function),
) {
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(l.index));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(l.index));
    func.instruction(&Instruction::LocalGet(source));
    func.instruction(&Instruction::I32Load(mem_off(SET_CAP as u64)));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(source));
    emit_set_base(func);
    func.instruction(&Instruction::LocalGet(l.index));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalTee(l.bucket));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(l.bucket));
    if matches!(elem, IRType::Float) {
        func.instruction(&Instruction::F64Load(mem_off(SET_BUCKET_VALUE as u64)));
        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
    } else {
        func.instruction(&Instruction::I32Load(mem_off(SET_BUCKET_VALUE as u64)));
        func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));
    }
    body(func);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(l.index));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(l.index));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
}

/// Push whether the set in `set` holds the stashed needle: the same probe
/// `in` runs, stopping at the needle or an empty bucket.
fn emit_set_contains_needle(
    func: &mut Function,
    ctx: &CompilationContext,
    l: &SetLocals,
    set: u32,
    elem: &IRType,
) {
    let needle = ctx.temp_local + 1;
    let mask = l.other_mask;
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(mem_off(SET_CAP as u64)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(mask));
    emit_set_hash(func, ctx, elem, needle);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(l.hidx));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(l.probes));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(l.found));

    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(l.probes));
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(set));
    emit_set_base(func);
    func.instruction(&Instruction::LocalGet(l.hidx));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalTee(l.probe_bkt));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(l.probe_bkt));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::LocalGet(l.probe_bkt));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    emit_slot_eq_needle(func, ctx, elem, needle);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(l.found));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(l.hidx));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(l.hidx));
    func.instruction(&Instruction::LocalGet(l.probes));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(l.probes));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(l.found));
}

/// `a | b` on two dicts: `(a, b)` becomes a fresh dict holding a's entries in
/// order, each overwritten by b's value for an equal key, then b's new keys in
/// b's order, which is the order CPython's merge produces.
fn emit_dict_merge(func: &mut Function, ctx: &CompilationContext, key: &IRType) {
    let b = ctx.temp_local + 20;
    let a = ctx.temp_local + 21;
    let result = ctx.temp_local + 22;
    let len = ctx.temp_local + 23;
    let cap = ctx.temp_local + 24;
    let j = ctx.temp_local + 25;
    let entry = ctx.temp_local + 26;
    let k = ctx.temp_local + 27;
    let found = ctx.temp_local + 28;
    let needle = ctx.temp_local + 1;

    func.instruction(&Instruction::LocalSet(b));
    func.instruction(&Instruction::LocalSet(a));
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::LocalTee(len));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(cap));

    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalTee(result));
    func.instruction(&Instruction::LocalGet(cap));
    func.instruction(&Instruction::I32Store(mem_off(COLLECTION_CAP as u64)));
    store_runtime_data_ptr(func, result);

    // a's entries go in as they are: a dict's keys are already distinct.
    func.instruction(&Instruction::LocalGet(result));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(a));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    // for each of b's entries: overwrite the value of an equal key, or append.
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Load(slot_arg()));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));

    func.instruction(&Instruction::LocalGet(b));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalTee(entry));
    if matches!(key, IRType::Float) {
        func.instruction(&Instruction::F64Load(slot_arg()));
        func.instruction(&Instruction::LocalSet(ctx.temp_local_f64));
    } else {
        func.instruction(&Instruction::I32Load(slot_arg()));
        func.instruction(&Instruction::LocalSet(needle));
    }

    // found = index of the equal key in the result, or -1
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(k));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(result));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    emit_slot_eq_needle(func, ctx, key, needle);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(k));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // The destination entry: the matched one, or a new one at the end. The
    // whole entry is copied either way; an equal key is interchangeable with
    // the one it replaces.
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(len));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(result));
    emit_data_base(func);
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(entry));
    func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });

    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(result));
    func.instruction(&Instruction::LocalGet(len));
    func.instruction(&Instruction::I32Store(slot_arg()));
    func.instruction(&Instruction::LocalGet(result));
}
