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
use crate::compiler::expression::{
    byte_arg, emit_data_base, emit_slot_eq_needle, mem_off, operator_symbol, slot_arg,
    store_runtime_data_ptr,
};
use crate::ir::{IRCompareOp, IRConstant, IRExpr, IROp, IRType, IRUnaryOp, STRING_LEN_PREFIX};
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
        IRType::Tuple(_) | IRType::VarTuple(_) => "tuple".into(),
        IRType::Set(_) => "set".into(),
        IRType::Dict(_, _) => "dict".into(),
        IRType::None => "NoneType".into(),
        IRType::Range => "range".into(),
        IRType::Class(name) => name.clone(),
        IRType::ClassRef(_) => "type".into(),
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
        // A tuple of any length joins another, or a fixed tuple whose members
        // are its member type; both share the list layout.
        (IROp::Add, IRType::VarTuple(m), IRType::VarTuple(n)) if m == n => {
            emit_sequence_concat(func, ctx);
            Some(left_ty.clone())
        }
        (IROp::Add, IRType::VarTuple(m), IRType::Tuple(ms))
        | (IROp::Add, IRType::Tuple(ms), IRType::VarTuple(m))
            if ms.iter().all(|t| t == m.as_ref()) =>
        {
            emit_sequence_concat(func, ctx);
            Some(IRType::VarTuple(m.clone()))
        }
        (IROp::Mul, IRType::VarTuple(_), n) if is_count(n) => {
            emit_sequence_repeat(func, ctx);
            Some(left_ty.clone())
        }
        (IROp::Mul, n, IRType::VarTuple(_)) if is_count(n) => {
            swap_words(func, ctx);
            emit_sequence_repeat(func, ctx);
            Some(right_ty.clone())
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
    if !matches!(
        op,
        IRCompareOp::Eq
            | IRCompareOp::NotEq
            | IRCompareOp::Lt
            | IRCompareOp::LtE
            | IRCompareOp::Gt
            | IRCompareOp::GtE
    ) {
        ctx.report(format!("'{op:?}' between two sets is not supported"));
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::I32Const(0));
        return IRType::Bool;
    }
    let sb = crate::compiler::equality::hold(ctx);
    let sa = crate::compiler::equality::hold(ctx);
    func.instruction(&Instruction::LocalSet(sb));
    func.instruction(&Instruction::LocalSet(sa));
    crate::compiler::sets::emit_comparison(func, ctx, op, &elem, sa, sb);
    crate::compiler::equality::release(ctx, 2);
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

/// `a | b`, `a & b`, `a - b`, `a ^ b` on two sets: `(a, b)` becomes a fresh
/// set, built the way CPython builds each (see `sets::emit_operator`).
fn emit_set_operator(func: &mut Function, ctx: &CompilationContext, op: &IROp, elem: &IRType) {
    let sb = crate::compiler::equality::hold(ctx);
    let sa = crate::compiler::equality::hold(ctx);
    func.instruction(&Instruction::LocalSet(sb));
    func.instruction(&Instruction::LocalSet(sa));
    crate::compiler::sets::emit_operator(func, ctx, op, elem, sa, sb);
    crate::compiler::equality::release(ctx, 2);
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
