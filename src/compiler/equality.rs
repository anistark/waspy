//! Comparing and hashing values by what they hold, not where they live.
//!
//! Python's `==` on two tuples, two lists, or two strings compares contents;
//! `in`, set membership, and dict keys all rest on the same `==`, and a set
//! or dict also needs a hash that agrees with it. Code generation used to
//! compare the words it had on hand, which for a collection is its pointer, so
//! `(a, 2) == (1, 2)` was False, a set of tuples never found or de-duplicated
//! its members, and a tuple dict key never matched an equal one. Strings
//! already compared by content, but hashed by offset, so a string built at
//! runtime missed an equal member already in a set. String ordering answered
//! a constant.
//!
//! Everything here works on values held in locals (a word: an int, a bool, a
//! string's offset, a collection's or instance's pointer) or on slot addresses
//! (a float is only ever compared from its slot, since the locals used are
//! i32). All state lives in held slots (see `HELD_LOCALS`), because comparing
//! nested collections is nested codegen and the scratch run is not safe across
//! it. The generated sequences call no user code except a non-raising `__eq__`:
//! the unwinding check after a call that can raise assumes it is not inside
//! blocks the caller opened, so a raising `__eq__` reached from inside a
//! collection is refused rather than miscompiled.

use crate::compiler::context::{CompilationContext, COLLECTION_DATA, COLLECTION_SLOT};
use crate::compiler::expression::{emit_str_content_eq, emit_user_call, emit_virtual_compare};
use crate::ir::{IRCompareOp, IRType, STRING_LEN_PREFIX};
use wasm_encoder::{BlockType, Function, Instruction, MemArg};

fn mem(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

fn byte(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 0,
        memory_index: 0,
    }
}

/// Why two values of `ty` cannot be compared for equality here, if they
/// cannot. `None` means every `emit_*_eq` below handles the type.
pub(crate) fn eq_unsupported(ctx: &CompilationContext, ty: &IRType) -> Option<String> {
    match ty {
        IRType::Int | IRType::Bool | IRType::None | IRType::Float => None,
        IRType::String | IRType::Bytes => None,
        IRType::Class(name) => {
            // An `__eq__` that can raise cannot be called from inside the
            // blocks these sequences open (see the module note).
            // With virtual dispatch, a subclass's override may be the one
            // called, so every implementation reachable from here counts.
            let raises = match ctx.virtual_call(name, "__eq__") {
                Some((column, _)) => ctx
                    .virtual_implementations(column)
                    .any(|f| ctx.can_raise.contains(&f)),
                None => ctx
                    .get_class_info(name)
                    .and_then(|ci| ci.methods.get("__eq__").copied())
                    .is_some_and(|idx| ctx.can_raise.contains(&idx)),
            };
            raises.then(|| format!("an instance of '{name}', whose __eq__ can raise"))
        }
        IRType::List(elem) => match elem.as_ref() {
            IRType::Unknown | IRType::Any => {
                Some("a list whose element type is not known".to_string())
            }
            e => eq_unsupported(ctx, e),
        },
        IRType::Tuple(members) if members.is_empty() => {
            Some("a tuple whose member types are not known".to_string())
        }
        IRType::Tuple(members) => members.iter().find_map(|m| eq_unsupported(ctx, m)),
        IRType::Dict(_, _) => Some("a dict".to_string()),
        IRType::Set(_) => Some("a set".to_string()),
        other => Some(format!(
            "a value of type '{}'",
            crate::type_to_string(other)
        )),
    }
}

/// Why a value of `ty` cannot be a set member or dict key, if it cannot.
///
/// An untyped value is a single word everywhere in code generation (most
/// often an int whose producer carried no type) and keeps hashing by that
/// word, as it always has.
pub(crate) fn hash_unsupported(ctx: &CompilationContext, ty: &IRType) -> Option<String> {
    match ty {
        IRType::Unknown | IRType::Any => None,
        IRType::Int | IRType::Bool | IRType::None | IRType::Float => None,
        IRType::String | IRType::Bytes => None,
        IRType::List(_) => Some("unhashable type: 'list'".to_string()),
        IRType::Dict(_, _) => Some("unhashable type: 'dict'".to_string()),
        IRType::Set(_) => Some("unhashable type: 'set'".to_string()),
        IRType::Tuple(members) if members.is_empty() => {
            Some("a tuple whose member types are not known".to_string())
        }
        IRType::Tuple(members) => members.iter().find_map(|m| hash_unsupported(ctx, m)),
        // A class hashes by identity, which agrees with its equality only when
        // it does not define `__eq__`. With one, CPython sets `__hash__` to
        // None unless the class defines it too, and calling a user `__hash__`
        // is not supported here.
        IRType::Class(name) => ctx
            .get_class_info(name)
            .and_then(|ci| ci.methods.get("__eq__"))
            .map(|_| format!("an instance of '{name}', which defines __eq__")),
        other => Some(format!(
            "a value of type '{}'",
            crate::type_to_string(other)
        )),
    }
}

/// Why two values of `ty` cannot be ordered with `<` and friends, if they
/// cannot.
pub(crate) fn order_unsupported(ctx: &CompilationContext, ty: &IRType) -> Option<String> {
    match ty {
        IRType::Int | IRType::Bool | IRType::Float => None,
        IRType::String | IRType::Bytes => None,
        IRType::List(elem) => match elem.as_ref() {
            IRType::Unknown | IRType::Any => {
                Some("a list whose element type is not known".to_string())
            }
            e => order_unsupported(ctx, e).or_else(|| eq_unsupported(ctx, e)),
        },
        IRType::Tuple(members) if members.is_empty() => {
            Some("a tuple whose member types are not known".to_string())
        }
        IRType::Tuple(members) => members
            .iter()
            .find_map(|m| order_unsupported(ctx, m).or_else(|| eq_unsupported(ctx, m))),
        other => Some(format!(
            "a value of type '{}'",
            crate::type_to_string(other)
        )),
    }
}

/// Take a held slot, or report that comparisons nest too deeply here. The
/// callers only reach this after a static check, so running out means a type
/// nested beyond `HELD_LOCALS`, which is reported rather than miscompiled.
pub(crate) fn hold(ctx: &CompilationContext) -> u32 {
    match ctx.hold() {
        Some(slot) => slot,
        None => {
            ctx.report(
                "a comparison between collections nests more deeply than the compiler reserves \
                 room for",
            );
            // Take the level anyway, so the caller's `release` still pairs
            // with it, and hand back a real local so the module validates
            // while the error is collected; the answer is never used.
            ctx.held_depth.set(ctx.held_depth.get() + 1);
            ctx.temp_local
        }
    }
}

pub(crate) fn release(ctx: &CompilationContext, n: u32) {
    for _ in 0..n {
        ctx.release_held();
    }
}

/// Push the address of element `index_local` (a local) or `const_index` of the
/// sequence whose pointer is in `seq`.
fn push_element_addr(func: &mut Function, seq: u32, index_local: Option<u32>, const_index: u32) {
    func.instruction(&Instruction::LocalGet(seq));
    func.instruction(&Instruction::I32Load(mem(COLLECTION_DATA as u64)));
    match index_local {
        Some(i) => {
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
        }
        None if const_index > 0 => {
            func.instruction(&Instruction::I32Const(
                (const_index * COLLECTION_SLOT) as i32,
            ));
            func.instruction(&Instruction::I32Add);
        }
        None => {}
    }
}

/// Push whether the values of type `ty` in word locals `a` and `b` are equal.
/// `ty` must pass [`eq_unsupported`] and must not be a float (floats are
/// compared from their slots by [`emit_slots_eq`]).
pub(crate) fn emit_values_eq(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    a: u32,
    b: u32,
) {
    match ty {
        IRType::String | IRType::Bytes => emit_str_content_eq(func, ctx, a, b),
        IRType::Class(name) => {
            let eq = ctx
                .get_class_info(name)
                .and_then(|ci| ci.methods.get("__eq__").copied());
            match eq {
                Some(eq_idx) => {
                    func.instruction(&Instruction::LocalGet(a));
                    func.instruction(&Instruction::LocalGet(b));
                    match ctx.virtual_call(name, "__eq__") {
                        Some((column, type_index)) => {
                            emit_virtual_compare(func, ctx, column, type_index)
                        }
                        None => emit_user_call(func, ctx, eq_idx),
                    }
                }
                // No `__eq__`: identity, which is Python's default.
                None => {
                    func.instruction(&Instruction::LocalGet(a));
                    func.instruction(&Instruction::LocalGet(b));
                    func.instruction(&Instruction::I32Eq);
                }
            }
        }
        IRType::Tuple(members) => emit_tuple_eq(func, ctx, members, a, b),
        IRType::List(elem) => emit_list_eq(func, ctx, elem, a, b),
        _ => {
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&Instruction::I32Eq);
        }
    }
}

/// Push whether the values of type `ty` in the slots at addresses `sa` and
/// `sb` (locals) are equal.
pub(crate) fn emit_slots_eq(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    sa: u32,
    sb: u32,
) {
    if matches!(ty, IRType::Float) {
        func.instruction(&Instruction::LocalGet(sa));
        func.instruction(&Instruction::F64Load(mem(0)));
        func.instruction(&Instruction::LocalGet(sb));
        func.instruction(&Instruction::F64Load(mem(0)));
        func.instruction(&Instruction::F64Eq);
        return;
    }
    let a = hold(ctx);
    let b = hold(ctx);
    func.instruction(&Instruction::LocalGet(sa));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalSet(a));
    func.instruction(&Instruction::LocalGet(sb));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalSet(b));
    emit_values_eq(func, ctx, ty, a, b);
    release(ctx, 2);
}

/// Two tuples of the same member types: equal lengths and every member equal,
/// stopping at the first that is not (a member's `__eq__` is not called past
/// it, as in CPython).
fn emit_tuple_eq(
    func: &mut Function,
    ctx: &CompilationContext,
    members: &[IRType],
    a: u32,
    b: u32,
) {
    let r = hold(ctx);
    let sa = hold(ctx);
    let sb = hold(ctx);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(r));
    func.instruction(&Instruction::Block(BlockType::Empty));
    // Lengths, which differ only for tuples built at different arities.
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(r));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);
    for (i, member) in members.iter().enumerate() {
        push_element_addr(func, a, None, i as u32);
        func.instruction(&Instruction::LocalSet(sa));
        push_element_addr(func, b, None, i as u32);
        func.instruction(&Instruction::LocalSet(sb));
        emit_slots_eq(func, ctx, member, sa, sb);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(r));
        func.instruction(&Instruction::Br(1));
        func.instruction(&Instruction::End);
    }
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(r));
    release(ctx, 3);
}

/// Two lists: equal lengths and every element equal, stopping at the first
/// that is not.
fn emit_list_eq(func: &mut Function, ctx: &CompilationContext, elem: &IRType, a: u32, b: u32) {
    let r = hold(ctx);
    let n = hold(ctx);
    let i = hold(ctx);
    let sa = hold(ctx);
    let sb = hold(ctx);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(r));
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalTee(n));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(r));
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    push_element_addr(func, a, Some(i), 0);
    func.instruction(&Instruction::LocalSet(sa));
    push_element_addr(func, b, Some(i), 0);
    func.instruction(&Instruction::LocalSet(sb));
    emit_slots_eq(func, ctx, elem, sa, sb);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(r));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(r));
    release(ctx, 5);
}

/// The i32 comparison instruction for an ordering operator.
fn i32_order(op: &IRCompareOp) -> Instruction<'static> {
    match op {
        IRCompareOp::Lt => Instruction::I32LtS,
        IRCompareOp::LtE => Instruction::I32LeS,
        IRCompareOp::Gt => Instruction::I32GtS,
        _ => Instruction::I32GeS,
    }
}

/// Push whether the values of type `ty` in word locals `a` and `b` satisfy the
/// ordering `op` (`<`, `<=`, `>`, `>=`). `ty` must pass [`order_unsupported`]
/// and must not be a float.
pub(crate) fn emit_values_order(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    a: u32,
    b: u32,
    op: &IRCompareOp,
) {
    match ty {
        IRType::String | IRType::Bytes => {
            emit_str_cmp(func, ctx, a, b);
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&i32_order(op));
        }
        IRType::Tuple(members) => emit_tuple_order(func, ctx, members, a, b, op),
        IRType::List(elem) => emit_list_order(func, ctx, elem, a, b, op),
        _ => {
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&i32_order(op));
        }
    }
}

fn emit_slots_order(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    sa: u32,
    sb: u32,
    op: &IRCompareOp,
) {
    if matches!(ty, IRType::Float) {
        func.instruction(&Instruction::LocalGet(sa));
        func.instruction(&Instruction::F64Load(mem(0)));
        func.instruction(&Instruction::LocalGet(sb));
        func.instruction(&Instruction::F64Load(mem(0)));
        func.instruction(&match op {
            IRCompareOp::Lt => Instruction::F64Lt,
            IRCompareOp::LtE => Instruction::F64Le,
            IRCompareOp::Gt => Instruction::F64Gt,
            _ => Instruction::F64Ge,
        });
        return;
    }
    let a = hold(ctx);
    let b = hold(ctx);
    func.instruction(&Instruction::LocalGet(sa));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalSet(a));
    func.instruction(&Instruction::LocalGet(sb));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalSet(b));
    emit_values_order(func, ctx, ty, a, b, op);
    release(ctx, 2);
}

/// CPython's sequence ordering: find the first index whose elements are not
/// equal and order by those; if one sequence runs out first, the shorter is
/// the smaller. Tuples of the same member types have the same length, so the
/// fall-through case is `op(n, n)`.
fn emit_tuple_order(
    func: &mut Function,
    ctx: &CompilationContext,
    members: &[IRType],
    a: u32,
    b: u32,
    op: &IRCompareOp,
) {
    let res = hold(ctx);
    let sa = hold(ctx);
    let sb = hold(ctx);
    func.instruction(&Instruction::Block(BlockType::Empty));
    for (i, member) in members.iter().enumerate() {
        push_element_addr(func, a, None, i as u32);
        func.instruction(&Instruction::LocalSet(sa));
        push_element_addr(func, b, None, i as u32);
        func.instruction(&Instruction::LocalSet(sb));
        emit_slots_eq(func, ctx, member, sa, sb);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        emit_slots_order(func, ctx, member, sa, sb, op);
        func.instruction(&Instruction::LocalSet(res));
        func.instruction(&Instruction::Br(1));
        func.instruction(&Instruction::End);
    }
    // Every member equal: only `<=` and `>=` hold.
    let all_equal = matches!(op, IRCompareOp::LtE | IRCompareOp::GtE) as i32;
    func.instruction(&Instruction::I32Const(all_equal));
    func.instruction(&Instruction::LocalSet(res));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(res));
    release(ctx, 3);
}

fn emit_list_order(
    func: &mut Function,
    ctx: &CompilationContext,
    elem: &IRType,
    a: u32,
    b: u32,
    op: &IRCompareOp,
) {
    let res = hold(ctx);
    let la = hold(ctx);
    let lb = hold(ctx);
    let n = hold(ctx);
    let i = hold(ctx);
    let sa = hold(ctx);
    let sb = hold(ctx);
    func.instruction(&Instruction::LocalGet(a));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalSet(la));
    func.instruction(&Instruction::LocalGet(b));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalSet(lb));
    // n = min(la, lb)
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(n));
    // Default when every compared element is equal: order by length.
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&i32_order(op));
    func.instruction(&Instruction::LocalSet(res));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    push_element_addr(func, a, Some(i), 0);
    func.instruction(&Instruction::LocalSet(sa));
    push_element_addr(func, b, Some(i), 0);
    func.instruction(&Instruction::LocalSet(sb));
    emit_slots_eq(func, ctx, elem, sa, sb);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_slots_order(func, ctx, elem, sa, sb, op);
    func.instruction(&Instruction::LocalSet(res));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(res));
    release(ctx, 7);
}

/// Push -1, 0, or 1 as the string at offset `a` sorts before, with, or after
/// the one at offset `b`. Byte-wise, which for UTF-8 is exactly code-point
/// order, the order CPython compares `str` in.
pub(crate) fn emit_str_cmp(func: &mut Function, ctx: &CompilationContext, a: u32, b: u32) {
    let la = hold(ctx);
    let lb = hold(ctx);
    let n = hold(ctx);
    let i = hold(ctx);
    let res = hold(ctx);
    let ca = hold(ctx);
    let cb = hold(ctx);
    for (off, len) in [(a, la), (b, lb)] {
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
        func.instruction(&Instruction::I32Sub);
        func.instruction(&Instruction::I32Load(mem(0)));
        func.instruction(&Instruction::LocalSet(len));
    }
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(n));
    // res = sign(la - lb): the answer if the shorter is a prefix of the longer.
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::LocalGet(la));
    func.instruction(&Instruction::LocalGet(lb));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(res));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    for (off, c) in [(a, ca), (b, cb)] {
        func.instruction(&Instruction::LocalGet(off));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::I32Load8U(byte(0)));
        func.instruction(&Instruction::LocalSet(c));
    }
    func.instruction(&Instruction::LocalGet(ca));
    func.instruction(&Instruction::LocalGet(cb));
    func.instruction(&Instruction::I32Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(ca));
    func.instruction(&Instruction::LocalGet(cb));
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::LocalGet(ca));
    func.instruction(&Instruction::LocalGet(cb));
    func.instruction(&Instruction::I32LtU);
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(res));
    func.instruction(&Instruction::Br(2));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(res));
    release(ctx, 7);
}

/// Push the hash of the value of type `ty` in word local `v`. Equal values
/// hash equal: a string by its bytes (FNV-1a), a tuple by its members
/// (CPython's multiply-and-xor combine), anything else by its word. `ty` must
/// pass [`hash_unsupported`] and must not be a float.
pub(crate) fn emit_value_hash(func: &mut Function, ctx: &CompilationContext, ty: &IRType, v: u32) {
    match ty {
        IRType::String | IRType::Bytes => {
            let h = hold(ctx);
            let n = hold(ctx);
            let i = hold(ctx);
            func.instruction(&Instruction::I32Const(0x811c_9dc5_u32 as i32));
            func.instruction(&Instruction::LocalSet(h));
            func.instruction(&Instruction::LocalGet(v));
            func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
            func.instruction(&Instruction::I32Sub);
            func.instruction(&Instruction::I32Load(mem(0)));
            func.instruction(&Instruction::LocalSet(n));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(n));
            func.instruction(&Instruction::I32GeU);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(h));
            func.instruction(&Instruction::LocalGet(v));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::I32Load8U(byte(0)));
            func.instruction(&Instruction::I32Xor);
            func.instruction(&Instruction::I32Const(0x0100_0193));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::LocalSet(h));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::LocalGet(h));
            release(ctx, 3);
        }
        IRType::Tuple(members) => {
            let h = hold(ctx);
            let s = hold(ctx);
            func.instruction(&Instruction::I32Const(0x0034_5678));
            func.instruction(&Instruction::LocalSet(h));
            for (i, member) in members.iter().enumerate() {
                push_element_addr(func, v, None, i as u32);
                func.instruction(&Instruction::LocalSet(s));
                func.instruction(&Instruction::LocalGet(h));
                emit_slot_hash(func, ctx, member, s);
                func.instruction(&Instruction::I32Xor);
                func.instruction(&Instruction::I32Const(1_000_003));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::LocalSet(h));
            }
            func.instruction(&Instruction::LocalGet(h));
            release(ctx, 2);
        }
        _ => {
            func.instruction(&Instruction::LocalGet(v));
        }
    }
}

/// Push the hash of the value of type `ty` in the slot at address `s`.
pub(crate) fn emit_slot_hash(func: &mut Function, ctx: &CompilationContext, ty: &IRType, s: u32) {
    if matches!(ty, IRType::Float) {
        // high32(bits) ^ low32(bits), the fold sets already use for floats.
        for high in [true, false] {
            func.instruction(&Instruction::LocalGet(s));
            func.instruction(&Instruction::F64Load(mem(0)));
            func.instruction(&Instruction::I64ReinterpretF64);
            if high {
                func.instruction(&Instruction::I64Const(32));
                func.instruction(&Instruction::I64ShrU);
            }
            func.instruction(&Instruction::I32WrapI64);
        }
        func.instruction(&Instruction::I32Xor);
        return;
    }
    let v = hold(ctx);
    func.instruction(&Instruction::LocalGet(s));
    func.instruction(&Instruction::I32Load(mem(0)));
    func.instruction(&Instruction::LocalSet(v));
    emit_value_hash(func, ctx, ty, v);
    release(ctx, 1);
}
