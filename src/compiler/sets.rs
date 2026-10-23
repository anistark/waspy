//! Sets, laid out and probed the way CPython's are (`Objects/setobject.c`).
//!
//! A set's iteration order is its table order, so it is part of what a
//! program answers: `for x in s`, `list(s)`, `str(s)`, and `s.pop()` all see
//! it. CPython does not promise an order, but for ints, bools, floats, and
//! tuples of them the order is a pure function of the values inserted and the
//! table's history, and code that iterates a small int set relies on it in
//! practice. So this follows CPython's algorithm step for step: the 64-bit
//! hashes of those types, a table of at least 8 buckets that grows when five
//! times its fill reaches three times its mask, nine linear probes before each
//! perturbed jump, tombstones for removed members, the reinsertion order of a
//! resize, and the order in which each operation visits its operands. A
//! string's hash is CPython's SipHash under a per-process random key, so a set
//! of strings has no reproducible order there; here its order is fixed and
//! unspecified.
//!
//! The header is `[count][cap][fill][data][finger]`: the live members, the
//! bucket count, the buckets that are live or tombstones, the bucket block
//! (behind a pointer, so a resize never moves the set itself), and where
//! `pop()` resumes. A bucket is `[state:i32][_:i32][value:8 bytes]`.
//!
//! Every routine here keeps its working values in held slots, so the element
//! comparisons it runs (string contents, tuple members, an instance's
//! `__eq__`) may use the scratch run.

use crate::compiler::context::CompilationContext;
use crate::compiler::expression::{emit_slot_eq_needle, mem_off, slot_arg};
use crate::ir::{IRType, STRING_LEN_PREFIX};
use wasm_encoder::{BlockType, Function, Instruction, MemArg};

pub(crate) const SET_HEADER: u32 = 24;
pub(crate) const SET_CAP: u32 = 4;
pub(crate) const SET_FILL: u32 = 8;
pub(crate) const SET_DATA: u32 = 12;
pub(crate) const SET_FINGER: u32 = 16;
pub(crate) const SET_BUCKET: u32 = 16;
pub(crate) const SET_BUCKET_VALUE: u32 = 8;
pub(crate) const SET_LIVE: i32 = 1;
pub(crate) const SET_DEAD: i32 = 2;
/// `PySet_MINSIZE`: the table a new set starts with.
const SET_MIN_SIZE: i32 = 8;
/// `LINEAR_PROBES`: buckets tried after the first before a perturbed jump.
const LINEAR_PROBES: i32 = 9;
/// `PERTURB_SHIFT`.
const PERTURB_SHIFT: i64 = 5;

/// A value held across nested emission: an i32 word, or an f64 for a float.
#[derive(Clone, Copy)]
pub(crate) enum Held {
    Word(u32),
    F64(u32),
}

impl Held {
    pub(crate) fn of(ctx: &CompilationContext, ty: &IRType) -> Held {
        if matches!(ty, IRType::Float) {
            match ctx.hold_f64() {
                Some(slot) => Held::F64(slot),
                None => {
                    ctx.report(
                        "a set operation nests more deeply than the compiler reserves room for",
                    );
                    ctx.held_f64_depth.set(ctx.held_f64_depth.get() + 1);
                    Held::F64(ctx.temp_local_f64)
                }
            }
        } else {
            Held::Word(word(ctx))
        }
    }

    pub(crate) fn release(self, ctx: &CompilationContext) {
        match self {
            Held::Word(_) => ctx.release_held(),
            Held::F64(_) => ctx.release_held_f64(),
        }
    }

    pub(crate) fn get(self, func: &mut Function) {
        let (Held::Word(local) | Held::F64(local)) = self;
        func.instruction(&Instruction::LocalGet(local));
    }

    pub(crate) fn set(self, func: &mut Function) {
        let (Held::Word(local) | Held::F64(local)) = self;
        func.instruction(&Instruction::LocalSet(local));
    }

    /// Load the value from the slot whose address is on the stack.
    pub(crate) fn load(self, func: &mut Function) {
        match self {
            Held::Word(local) => {
                func.instruction(&Instruction::I32Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(local));
            }
            Held::F64(local) => {
                func.instruction(&Instruction::F64Load(slot_arg()));
                func.instruction(&Instruction::LocalSet(local));
            }
        }
    }

    /// Store the value into the slot whose address is on the stack.
    pub(crate) fn store(self, func: &mut Function) {
        match self {
            Held::Word(local) => {
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::I32Store(slot_arg()));
            }
            Held::F64(local) => {
                func.instruction(&Instruction::LocalGet(local));
                func.instruction(&Instruction::F64Store(slot_arg()));
            }
        }
    }
}

/// A held word.
fn word(ctx: &CompilationContext) -> u32 {
    crate::compiler::equality::hold(ctx)
}

fn release(ctx: &CompilationContext, n: u32) {
    for _ in 0..n {
        ctx.release_held();
    }
}

fn at(offset: u32) -> MemArg {
    mem_off(offset as u64)
}

fn wide(offset: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align: 3,
        memory_index: 0,
    }
}

/// Why a set of `ty` cannot be built here, if it cannot: CPython's unhashable
/// types, and the members this compiler cannot compare.
pub(crate) fn member_unsupported(ctx: &CompilationContext, ty: &IRType) -> Option<String> {
    crate::compiler::equality::hash_unsupported(ctx, ty)
}

// ---------------------------------------------------------------------------
// Hashing
// ---------------------------------------------------------------------------

/// Push the 64-bit hash of the value of `ty` in `v`, CPython's for an int, a
/// bool, a float, and a tuple of those.
pub(crate) fn emit_hash(func: &mut Function, ctx: &CompilationContext, ty: &IRType, v: Held) {
    match (ty, v) {
        (IRType::Float, Held::F64(local)) => emit_float_hash(func, ctx, local),
        (IRType::String | IRType::Bytes, Held::Word(local)) => emit_text_hash(func, ctx, local),
        (IRType::Tuple(members), Held::Word(local)) => emit_tuple_hash(func, ctx, members, local),
        (IRType::VarTuple(member), Held::Word(local)) => {
            emit_var_tuple_hash(func, ctx, member, local)
        }
        // A named tuple hashes as the tuple of its fields.
        (IRType::Class(class), Held::Word(local))
            if crate::compiler::dunders::is_named_tuple(ctx, class) =>
        {
            func.instruction(&Instruction::LocalGet(local));
            let members = match crate::compiler::dunders::emit_receiver_call(
                func,
                ctx,
                class,
                crate::ir::VIEW_METHOD,
            ) {
                Some(IRType::Tuple(members)) => members,
                _ => Vec::new(),
            };
            let t = word(ctx);
            func.instruction(&Instruction::LocalSet(t));
            emit_tuple_hash(func, ctx, &members, t);
            release(ctx, 1);
        }
        (_, v) => {
            // An int is its own hash, except that -1 is -2 (-1 is CPython's
            // error return). A bool is 0 or 1, None its word, and an instance
            // without `__eq__` its identity.
            v.get(func);
            func.instruction(&Instruction::I64ExtendI32S);
            emit_not_minus_one(func, ctx, -2);
        }
    }
}

/// Replace a hash of -1 on the stack with `instead`.
fn emit_not_minus_one(func: &mut Function, ctx: &CompilationContext, instead: i64) {
    let t = ctx.hold_i64();
    func.instruction(&Instruction::LocalSet(t));
    func.instruction(&Instruction::I64Const(instead));
    func.instruction(&Instruction::LocalGet(t));
    func.instruction(&Instruction::LocalGet(t));
    func.instruction(&Instruction::I64Const(-1));
    func.instruction(&Instruction::I64Eq);
    func.instruction(&Instruction::Select);
    ctx.release_held_i64();
}

/// `_Py_HashDouble`: the hash of the rational number the float is, reduced
/// modulo the Mersenne prime 2**61 - 1, so an integral float hashes as the
/// int it equals. An infinity is +-314159; a NaN hashes by identity in
/// CPython, which a value has none of, so it is 0 here.
fn emit_float_hash(func: &mut Function, ctx: &CompilationContext, v: u32) {
    const MODULUS: i64 = (1 << 61) - 1;
    const BITS: i64 = 61;
    let x = ctx.hold_i64();
    let y = ctx.hold_i64();
    let bits = ctx.hold_i64();
    let e = word(ctx);
    let sign = word(ctx);
    let m = match ctx.hold_f64() {
        Some(slot) => slot,
        None => {
            ctx.report("a float hash nests more deeply than the compiler reserves room for");
            ctx.held_f64_depth.set(ctx.held_f64_depth.get() + 1);
            ctx.temp_local_f64_3
        }
    };
    let out = ctx.hold_i64();

    func.instruction(&Instruction::Block(BlockType::Empty));
    // NaN: 0.
    func.instruction(&Instruction::LocalGet(v));
    func.instruction(&Instruction::LocalGet(v));
    func.instruction(&Instruction::F64Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I64Const(0));
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);
    // An infinity: +-314159.
    func.instruction(&Instruction::LocalGet(v));
    func.instruction(&Instruction::F64Abs);
    func.instruction(&Instruction::F64Const(f64::INFINITY.into()));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I64Const(-314159));
    func.instruction(&Instruction::I64Const(314159));
    func.instruction(&Instruction::LocalGet(v));
    func.instruction(&Instruction::F64Const(0.0_f64.into()));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // frexp: v = m * 2**e with 0.5 <= |m| < 1. A subnormal is scaled into the
    // normal range first.
    func.instruction(&Instruction::LocalGet(v));
    func.instruction(&Instruction::LocalSet(m));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(e));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::F64Const(0.0_f64.into()));
    func.instruction(&Instruction::F64Ne);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::I64ReinterpretF64);
    func.instruction(&Instruction::I64Const(52));
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::I64Const(0x7ff));
    func.instruction(&Instruction::I64And);
    func.instruction(&Instruction::I64Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::F64Const(18014398509481984.0_f64.into())); // 2**54
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalSet(m));
    func.instruction(&Instruction::I32Const(-54));
    func.instruction(&Instruction::LocalSet(e));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::I64ReinterpretF64);
    func.instruction(&Instruction::LocalSet(bits));
    // e += exponent - 1022
    func.instruction(&Instruction::LocalGet(e));
    func.instruction(&Instruction::LocalGet(bits));
    func.instruction(&Instruction::I64Const(52));
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::I64Const(0x7ff));
    func.instruction(&Instruction::I64And);
    func.instruction(&Instruction::I32WrapI64);
    func.instruction(&Instruction::I32Const(1022));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(e));
    // m = |mantissa| with the exponent field set for [0.5, 1)
    func.instruction(&Instruction::LocalGet(bits));
    func.instruction(&Instruction::I64Const(0x000f_ffff_ffff_ffff));
    func.instruction(&Instruction::I64And);
    func.instruction(&Instruction::I64Const(1022 << 52));
    func.instruction(&Instruction::I64Or);
    func.instruction(&Instruction::F64ReinterpretI64);
    func.instruction(&Instruction::LocalSet(m));
    func.instruction(&Instruction::End);
    // sign
    func.instruction(&Instruction::LocalGet(v));
    func.instruction(&Instruction::F64Const(0.0_f64.into()));
    func.instruction(&Instruction::F64Lt);
    func.instruction(&Instruction::LocalSet(sign));

    // x = 0; while m: x = rotl28(x) mod; m *= 2**28; e -= 28; y = int(m);
    // m -= y; x += y; x %= MODULUS (by one subtraction)
    func.instruction(&Instruction::I64Const(0));
    func.instruction(&Instruction::LocalSet(x));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::F64Const(0.0_f64.into()));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::I64Const(28));
    func.instruction(&Instruction::I64Shl);
    func.instruction(&Instruction::I64Const(MODULUS));
    func.instruction(&Instruction::I64And);
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::I64Const(BITS - 28));
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::I64Or);
    func.instruction(&Instruction::LocalSet(x));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::F64Const(268435456.0_f64.into()));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::LocalSet(m));
    func.instruction(&Instruction::LocalGet(e));
    func.instruction(&Instruction::I32Const(28));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(e));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::I64TruncF64U);
    func.instruction(&Instruction::LocalSet(y));
    func.instruction(&Instruction::LocalGet(m));
    func.instruction(&Instruction::LocalGet(y));
    func.instruction(&Instruction::F64ConvertI64U);
    func.instruction(&Instruction::F64Sub);
    func.instruction(&Instruction::LocalSet(m));
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::LocalGet(y));
    func.instruction(&Instruction::I64Add);
    func.instruction(&Instruction::LocalTee(x));
    func.instruction(&Instruction::I64Const(MODULUS));
    func.instruction(&Instruction::I64GeU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::I64Const(MODULUS));
    func.instruction(&Instruction::I64Sub);
    func.instruction(&Instruction::LocalSet(x));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // e = e >= 0 ? e % 61 : 61 - 1 - ((-1 - e) % 61)
    func.instruction(&Instruction::LocalGet(e));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::If(BlockType::Result(
        wasm_encoder::ValType::I32,
    )));
    func.instruction(&Instruction::LocalGet(e));
    func.instruction(&Instruction::I32Const(BITS as i32));
    func.instruction(&Instruction::I32RemS);
    func.instruction(&Instruction::Else);
    func.instruction(&Instruction::I32Const(BITS as i32 - 1));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalGet(e));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(BITS as i32));
    func.instruction(&Instruction::I32RemS);
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalSet(e));
    // x = ((x << e) & MOD) | (x >> (61 - e))
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::LocalGet(e));
    func.instruction(&Instruction::I64ExtendI32U);
    func.instruction(&Instruction::I64Shl);
    func.instruction(&Instruction::I64Const(MODULUS));
    func.instruction(&Instruction::I64And);
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::I64Const(BITS));
    func.instruction(&Instruction::LocalGet(e));
    func.instruction(&Instruction::I64ExtendI32U);
    func.instruction(&Instruction::I64Sub);
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::I64Or);
    func.instruction(&Instruction::LocalSet(x));
    // x = x * sign
    func.instruction(&Instruction::I64Const(0));
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::I64Sub);
    func.instruction(&Instruction::LocalGet(x));
    func.instruction(&Instruction::LocalGet(sign));
    func.instruction(&Instruction::Select);
    emit_not_minus_one(func, ctx, -2);
    func.instruction(&Instruction::LocalSet(out));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(out));

    ctx.release_held_i64(); // out
    ctx.release_held_f64(); // m
    release(ctx, 2); // sign, e
    ctx.release_held_i64(); // bits
    ctx.release_held_i64(); // y
    ctx.release_held_i64(); // x
}

/// FNV-1a over a string's bytes. CPython's string hash is keyed per process,
/// so no fixed hash reproduces its order; any consistent one will do.
fn emit_text_hash(func: &mut Function, ctx: &CompilationContext, v: u32) {
    let h = word(ctx);
    let n = word(ctx);
    let i = word(ctx);
    func.instruction(&Instruction::I32Const(0x811c_9dc5_u32 as i32));
    func.instruction(&Instruction::LocalSet(h));
    func.instruction(&Instruction::LocalGet(v));
    func.instruction(&Instruction::I32Const(STRING_LEN_PREFIX as i32));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Load(at(0)));
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
    func.instruction(&Instruction::I32Load8U(MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }));
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
    func.instruction(&Instruction::I64ExtendI32U);
    release(ctx, 3);
}

const XXPRIME_1: u64 = 11400714785074694791;
const XXPRIME_2: u64 = 14029467366897019727;
const XXPRIME_5: u64 = 2870177450012600261;

/// `acc = rotl31(acc + lane * XXPRIME_2) * XXPRIME_1`, with the lane on the
/// stack and the accumulator in `acc`: one step of CPython's tuple hash.
fn emit_tuple_lane(func: &mut Function, acc: u32) {
    func.instruction(&Instruction::I64Const(XXPRIME_2 as i64));
    func.instruction(&Instruction::I64Mul);
    func.instruction(&Instruction::LocalGet(acc));
    func.instruction(&Instruction::I64Add);
    func.instruction(&Instruction::I64Const(31));
    func.instruction(&Instruction::I64Rotl);
    func.instruction(&Instruction::I64Const(XXPRIME_1 as i64));
    func.instruction(&Instruction::I64Mul);
    func.instruction(&Instruction::LocalSet(acc));
}

/// Finish CPython's tuple hash: mix in the length (in `len`, an i32 local, or
/// the constant `const_len`), and map -1 to 1546275796.
fn emit_tuple_finish(
    func: &mut Function,
    ctx: &CompilationContext,
    acc: u32,
    len: Option<u32>,
    const_len: i64,
) {
    func.instruction(&Instruction::LocalGet(acc));
    match len {
        Some(local) => {
            func.instruction(&Instruction::LocalGet(local));
            func.instruction(&Instruction::I64ExtendI32U);
        }
        None => {
            func.instruction(&Instruction::I64Const(const_len));
        }
    }
    func.instruction(&Instruction::I64Const((XXPRIME_5 ^ 3527539) as i64));
    func.instruction(&Instruction::I64Xor);
    func.instruction(&Instruction::I64Add);
    emit_not_minus_one(func, ctx, 1546275796);
}

/// The member at `index` (an i32 local, or a constant) of the tuple in `t`,
/// into a held value.
fn load_member(func: &mut Function, t: u32, index: Option<u32>, const_index: u32, into: Held) {
    func.instruction(&Instruction::LocalGet(t));
    func.instruction(&Instruction::I32Load(at(
        crate::compiler::context::COLLECTION_DATA,
    )));
    match index {
        Some(i) => {
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(
                crate::compiler::context::COLLECTION_SLOT as i32,
            ));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
        }
        None => {
            func.instruction(&Instruction::I32Const(
                (const_index * crate::compiler::context::COLLECTION_SLOT) as i32,
            ));
            func.instruction(&Instruction::I32Add);
        }
    }
    into.load(func);
}

/// CPython's tuple hash (xxHash's mixing over the members' hashes).
fn emit_tuple_hash(func: &mut Function, ctx: &CompilationContext, members: &[IRType], t: u32) {
    let acc = ctx.hold_i64();
    func.instruction(&Instruction::I64Const(XXPRIME_5 as i64));
    func.instruction(&Instruction::LocalSet(acc));
    for (i, member) in members.iter().enumerate() {
        let m = Held::of(ctx, member);
        load_member(func, t, None, i as u32, m);
        emit_hash(func, ctx, member, m);
        emit_tuple_lane(func, acc);
        m.release(ctx);
    }
    emit_tuple_finish(func, ctx, acc, None, members.len() as i64);
    ctx.release_held_i64();
}

/// The same hash over a tuple of any length.
fn emit_var_tuple_hash(func: &mut Function, ctx: &CompilationContext, member: &IRType, t: u32) {
    let acc = ctx.hold_i64();
    let n = word(ctx);
    let i = word(ctx);
    func.instruction(&Instruction::I64Const(XXPRIME_5 as i64));
    func.instruction(&Instruction::LocalSet(acc));
    func.instruction(&Instruction::LocalGet(t));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::LocalSet(n));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(n));
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::BrIf(1));
    let m = Held::of(ctx, member);
    load_member(func, t, Some(i), 0, m);
    emit_hash(func, ctx, member, m);
    emit_tuple_lane(func, acc);
    m.release(ctx);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    emit_tuple_finish(func, ctx, acc, Some(n), 0);
    release(ctx, 2);
    ctx.release_held_i64();
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// Push the address of bucket `index` (an i32 local) of the bucket block in
/// `data` (an i32 local).
fn push_bucket(func: &mut Function, data: u32, index: u32) {
    func.instruction(&Instruction::LocalGet(data));
    func.instruction(&Instruction::LocalGet(index));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
}

/// Allocate a zeroed bucket block of `size` (an i32 local) buckets and push
/// its address.
fn emit_new_buckets(func: &mut Function, ctx: &CompilationContext, size: u32) {
    let block = word(ctx);
    func.instruction(&Instruction::LocalGet(size));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalTee(block));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(size));
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::MemoryFill(0));
    func.instruction(&Instruction::LocalGet(block));
    release(ctx, 1);
}

/// Push a new, empty set: `PySet_MINSIZE` buckets, nothing live.
pub(crate) fn emit_new_set(func: &mut Function, ctx: &CompilationContext) {
    let set = word(ctx);
    let size = word(ctx);
    func.instruction(&Instruction::I32Const(SET_HEADER as i32));
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(set));
    // The header's words start at zero: count, fill, finger.
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32Const(SET_HEADER as i32));
    func.instruction(&Instruction::MemoryFill(0));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Const(SET_MIN_SIZE));
    func.instruction(&Instruction::I32Store(at(SET_CAP)));
    func.instruction(&Instruction::I32Const(SET_MIN_SIZE));
    func.instruction(&Instruction::LocalSet(size));
    func.instruction(&Instruction::LocalGet(set));
    emit_new_buckets(func, ctx, size);
    func.instruction(&Instruction::I32Store(at(SET_DATA)));
    func.instruction(&Instruction::LocalGet(set));
    release(ctx, 2);
}

/// Push whether the value of `ty` in `v` equals the member of the bucket whose
/// address is in `bucket`.
fn emit_bucket_eq(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    bucket: u32,
    v: Held,
) {
    func.instruction(&Instruction::LocalGet(bucket));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    match v {
        Held::F64(local) => {
            func.instruction(&Instruction::F64Load(slot_arg()));
            func.instruction(&Instruction::LocalGet(local));
            func.instruction(&Instruction::F64Eq);
        }
        Held::Word(local) => emit_slot_eq_needle(func, ctx, ty, local),
    }
}

/// `i = (i * 5 + 1 + (perturb >>= 5)) & mask`: the jump after a run of
/// linear probes.
fn emit_perturb(func: &mut Function, i: u32, perturb: u32, mask: u32) {
    func.instruction(&Instruction::LocalGet(perturb));
    func.instruction(&Instruction::I64Const(PERTURB_SHIFT));
    func.instruction(&Instruction::I64ShrU);
    func.instruction(&Instruction::LocalSet(perturb));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(5));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(perturb));
    func.instruction(&Instruction::I32WrapI64);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(i));
}

/// Look the value of `ty` in `v`, whose hash is in the i64 local `hash`, up in
/// the set in `set`: `set_lookkey` and the search half of `set_add_entry`.
/// Leaves the index of the bucket holding an equal member in `found`, or -1,
/// and, given `free`, the bucket an insert takes there: the last tombstone
/// the probe passed, or else the empty bucket it stopped at.
#[allow(clippy::too_many_arguments)]
fn emit_find(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    set: u32,
    v: Held,
    hash: u32,
    found: u32,
    free: Option<u32>,
) {
    let data = word(ctx);
    let mask = word(ctx);
    let i = word(ctx);
    let j = word(ctx);
    let probes = word(ctx);
    let bucket = word(ctx);
    let freeslot = word(ctx);
    let perturb = ctx.hold_i64();

    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_DATA)));
    func.instruction(&Instruction::LocalSet(data));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_CAP)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(mask));
    func.instruction(&Instruction::LocalGet(hash));
    func.instruction(&Instruction::I32WrapI64);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::LocalGet(hash));
    func.instruction(&Instruction::LocalSet(perturb));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::I32Const(-1));
    func.instruction(&Instruction::LocalSet(freeslot));

    func.instruction(&Instruction::Block(BlockType::Empty)); // done
    func.instruction(&Instruction::Loop(BlockType::Empty)); // outer
                                                            // probes = i + LINEAR_PROBES <= mask ? LINEAR_PROBES : 0
    func.instruction(&Instruction::I32Const(LINEAR_PROBES));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(LINEAR_PROBES));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32LeS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(probes));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Loop(BlockType::Empty)); // inner, a do-while
    push_bucket(func, data, j);
    func.instruction(&Instruction::LocalTee(bucket));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    if let Some(free) = free {
        // An insert takes the first tombstone it passed, else this bucket.
        func.instruction(&Instruction::LocalGet(freeslot));
        func.instruction(&Instruction::LocalGet(j));
        func.instruction(&Instruction::LocalGet(freeslot));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32GeS);
        func.instruction(&Instruction::Select);
        func.instruction(&Instruction::LocalSet(free));
    }
    func.instruction(&Instruction::Br(3));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(bucket));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    emit_bucket_eq(func, ctx, ty, bucket, v);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalSet(found));
    func.instruction(&Instruction::Br(4));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::Else);
    // A tombstone. CPython 3.13 inserts into the last one the probe passed
    // before the empty bucket (it keeps overwriting `freeslot`), which is
    // what decides where a re-added member lands.
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalSet(freeslot));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    // while (probes--)
    func.instruction(&Instruction::LocalGet(probes));
    func.instruction(&Instruction::LocalGet(probes));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(probes));
    func.instruction(&Instruction::BrIf(0));
    func.instruction(&Instruction::End); // inner
    emit_perturb(func, i, perturb, mask);
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End); // outer
    func.instruction(&Instruction::End); // done

    ctx.release_held_i64();
    release(ctx, 7);
}

/// `set_insert_clean`: put a member known to be absent into the bucket block
/// `data` of `mask + 1` buckets, which holds no tombstones.
fn emit_insert_clean(
    func: &mut Function,
    ty: &IRType,
    ctx: &CompilationContext,
    data: u32,
    mask: u32,
    v: Held,
    hash: u32,
) {
    let _ = ty;
    let i = word(ctx);
    let k = word(ctx);
    let slot = word(ctx);
    let perturb = ctx.hold_i64();
    func.instruction(&Instruction::LocalGet(hash));
    func.instruction(&Instruction::I32WrapI64);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::LocalGet(hash));
    func.instruction(&Instruction::LocalSet(perturb));
    func.instruction(&Instruction::Block(BlockType::Empty)); // found
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalSet(slot));
    push_bucket(func, data, slot);
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));
    // The next LINEAR_PROBES buckets, when they are in the table.
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(LINEAR_PROBES));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32LeS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(k));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(slot));
    push_bucket(func, data, slot);
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(3));
    func.instruction(&Instruction::LocalGet(k));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalTee(k));
    func.instruction(&Instruction::I32Const(LINEAR_PROBES));
    func.instruction(&Instruction::I32LeS);
    func.instruction(&Instruction::BrIf(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    emit_perturb(func, i, perturb, mask);
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    // The bucket in `slot` takes the member.
    push_bucket(func, data, slot);
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Store(at(0)));
    push_bucket(func, data, slot);
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    v.store(func);
    ctx.release_held_i64();
    release(ctx, 3);
}

/// `set_table_resize`: rebuild the set in `set` into the smallest table of at
/// least 8 buckets with more than `minused` (an i32 local) of them, putting
/// the live members back in the order the old table holds them.
fn emit_resize(func: &mut Function, ctx: &CompilationContext, ty: &IRType, set: u32, minused: u32) {
    let size = word(ctx);
    let newdata = word(ctx);
    let newmask = word(ctx);
    let olddata = word(ctx);
    let oldcap = word(ctx);
    let j = word(ctx);
    let bucket = word(ctx);
    let hash = ctx.hold_i64();
    let v = Held::of(ctx, ty);

    func.instruction(&Instruction::I32Const(SET_MIN_SIZE));
    func.instruction(&Instruction::LocalSet(size));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(size));
    func.instruction(&Instruction::LocalGet(minused));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(size));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Shl);
    func.instruction(&Instruction::LocalSet(size));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    emit_new_buckets(func, ctx, size);
    func.instruction(&Instruction::LocalSet(newdata));
    func.instruction(&Instruction::LocalGet(size));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(newmask));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_DATA)));
    func.instruction(&Instruction::LocalSet(olddata));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_CAP)));
    func.instruction(&Instruction::LocalSet(oldcap));

    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(oldcap));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    push_bucket(func, olddata, j);
    func.instruction(&Instruction::LocalTee(bucket));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(bucket));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    v.load(func);
    emit_hash(func, ctx, ty, v);
    func.instruction(&Instruction::LocalSet(hash));
    emit_insert_clean(func, ty, ctx, newdata, newmask, v, hash);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(size));
    func.instruction(&Instruction::I32Store(at(SET_CAP)));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(newdata));
    func.instruction(&Instruction::I32Store(at(SET_DATA)));
    // The tombstones are gone: fill is the count.
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Store(at(SET_FILL)));

    v.release(ctx);
    ctx.release_held_i64();
    release(ctx, 7);
}

/// `set_add_entry`: add the value of `ty` in `v` to the set in `set` unless
/// an equal member is there, growing the table as CPython's does. Pushes
/// whether it was added.
pub(crate) fn emit_add(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    set: u32,
    v: Held,
) {
    let hash = ctx.hold_i64();
    emit_hash(func, ctx, ty, v);
    func.instruction(&Instruction::LocalSet(hash));
    emit_add_hashed(func, ctx, ty, set, v, hash);
    ctx.release_held_i64();
}

fn emit_add_hashed(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    set: u32,
    v: Held,
    hash: u32,
) {
    let found = word(ctx);
    let free = word(ctx);
    let bucket = word(ctx);
    let was_empty = word(ctx);
    let minused = word(ctx);
    let added = word(ctx);
    emit_find(func, ctx, ty, set, v, hash, found, Some(free));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(added));
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(added));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_DATA)));
    func.instruction(&Instruction::LocalSet(bucket));
    push_bucket(func, bucket, free);
    func.instruction(&Instruction::LocalTee(bucket));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::LocalSet(was_empty));
    func.instruction(&Instruction::LocalGet(bucket));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Store(at(0)));
    func.instruction(&Instruction::LocalGet(bucket));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    v.store(func);
    // used += 1
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(at(0)));
    // A bucket that was empty (not a reused tombstone) raises the fill, and
    // the table grows once five times the fill reaches three times the mask.
    func.instruction(&Instruction::LocalGet(was_empty));
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_FILL)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(at(SET_FILL)));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_FILL)));
    func.instruction(&Instruction::I32Const(5));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_CAP)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(3));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32GeU);
    func.instruction(&Instruction::If(BlockType::Empty));
    // used > 50000 ? used * 2 : used * 4
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::LocalTee(minused));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Shl);
    func.instruction(&Instruction::LocalGet(minused));
    func.instruction(&Instruction::I32Const(2));
    func.instruction(&Instruction::I32Shl);
    func.instruction(&Instruction::LocalGet(minused));
    func.instruction(&Instruction::I32Const(50000));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(minused));
    emit_resize(func, ctx, ty, set, minused);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(added));
    release(ctx, 6);
}

/// Push whether the set in `set` holds a member equal to the value in `v`.
pub(crate) fn emit_contains(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    set: u32,
    v: Held,
) {
    let hash = ctx.hold_i64();
    let found = word(ctx);
    emit_hash(func, ctx, ty, v);
    func.instruction(&Instruction::LocalSet(hash));
    emit_find(func, ctx, ty, set, v, hash, found, None);
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GeS);
    release(ctx, 1);
    ctx.release_held_i64();
}

/// `set_discard_entry`: remove a member equal to the value in `v`, leaving a
/// tombstone. Pushes whether one was there.
pub(crate) fn emit_discard(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    set: u32,
    v: Held,
) {
    let hash = ctx.hold_i64();
    let found = word(ctx);
    let data = word(ctx);
    emit_hash(func, ctx, ty, v);
    func.instruction(&Instruction::LocalSet(hash));
    emit_find(func, ctx, ty, set, v, hash, found, None);
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_DATA)));
    func.instruction(&Instruction::LocalSet(data));
    push_bucket(func, data, found);
    func.instruction(&Instruction::I32Const(SET_DEAD));
    func.instruction(&Instruction::I32Store(at(0)));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Store(at(0)));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::LocalGet(found));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32GeS);
    release(ctx, 2);
    ctx.release_held_i64();
}

/// Run `body` once per live member of the set in `set`, in table order, with
/// the member loaded into `v`. The body may emit anything; it runs two block
/// frames deeper than the caller (a block and a loop, counted in the
/// expression block depth) plus the `if` that skips empty buckets.
pub(crate) fn emit_for_each(
    func: &mut Function,
    ctx: &CompilationContext,
    set: u32,
    v: Held,
    mut body: impl FnMut(&mut Function),
) {
    let j = word(ctx);
    let bucket = word(ctx);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    ctx.enter_expr_blocks(3);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_CAP)));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_DATA)));
    func.instruction(&Instruction::LocalSet(bucket));
    push_bucket(func, bucket, j);
    func.instruction(&Instruction::LocalTee(bucket));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(bucket));
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    v.load(func);
    body(func);
    func.instruction(&Instruction::End);
    ctx.exit_expr_blocks(3);
    func.instruction(&Instruction::LocalGet(j));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(j));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    release(ctx, 2);
}

/// `set_merge`: add every member of the set in `src` to the set in `dst`,
/// growing `dst` once up front, and copying the table outright or inserting
/// cleanly where `dst` is empty, as CPython does.
pub(crate) fn emit_merge(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    dst: u32,
    src: u32,
) {
    let minused = word(ctx);
    let data = word(ctx);
    let mask = word(ctx);
    let hash = ctx.hold_i64();
    let v = Held::of(ctx, ty);
    let load = |func: &mut Function, set: u32, field: u32| {
        func.instruction(&Instruction::LocalGet(set));
        func.instruction(&Instruction::I32Load(at(field)));
    };

    func.instruction(&Instruction::Block(BlockType::Empty));
    // Nothing to add.
    func.instruction(&Instruction::LocalGet(dst));
    func.instruction(&Instruction::LocalGet(src));
    func.instruction(&Instruction::I32Eq);
    load(func, src, 0);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::I32Or);
    func.instruction(&Instruction::BrIf(0));
    // One resize for the whole merge: (fill + other.used) * 5 >= mask * 3.
    load(func, dst, SET_FILL);
    load(func, src, 0);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(5));
    func.instruction(&Instruction::I32Mul);
    load(func, dst, SET_CAP);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(3));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::If(BlockType::Empty));
    load(func, dst, 0);
    load(func, src, 0);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Shl);
    func.instruction(&Instruction::LocalSet(minused));
    emit_resize(func, ctx, ty, dst, minused);
    func.instruction(&Instruction::End);
    // Empty, the same size, and no tombstones in the source: copy the table.
    load(func, dst, SET_FILL);
    func.instruction(&Instruction::I32Eqz);
    load(func, dst, SET_CAP);
    load(func, src, SET_CAP);
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32And);
    load(func, src, SET_FILL);
    load(func, src, 0);
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::If(BlockType::Empty));
    load(func, dst, SET_DATA);
    load(func, src, SET_DATA);
    load(func, src, SET_CAP);
    func.instruction(&Instruction::I32Const(SET_BUCKET as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    for field in [0, SET_FILL] {
        func.instruction(&Instruction::LocalGet(dst));
        load(func, src, field);
        func.instruction(&Instruction::I32Store(at(field)));
    }
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);
    // Empty: insert cleanly in the source's table order.
    load(func, dst, SET_FILL);
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    for field in [0, SET_FILL] {
        func.instruction(&Instruction::LocalGet(dst));
        load(func, src, 0);
        func.instruction(&Instruction::I32Store(at(field)));
    }
    load(func, dst, SET_DATA);
    func.instruction(&Instruction::LocalSet(data));
    load(func, dst, SET_CAP);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(mask));
    emit_for_each(func, ctx, src, v, |func| {
        emit_hash(func, ctx, ty, v);
        func.instruction(&Instruction::LocalSet(hash));
        emit_insert_clean(func, ty, ctx, data, mask, v, hash);
    });
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);
    // Otherwise the members may overlap: insert each normally.
    emit_for_each(func, ctx, src, v, |func| {
        emit_add(func, ctx, ty, dst, v);
        func.instruction(&Instruction::Drop);
    });
    func.instruction(&Instruction::End);

    v.release(ctx);
    ctx.release_held_i64();
    release(ctx, 3);
}

/// Push a copy of the set in `src`: a new set merged with it.
pub(crate) fn emit_copy(func: &mut Function, ctx: &CompilationContext, ty: &IRType, src: u32) {
    let out = word(ctx);
    emit_new_set(func, ctx);
    func.instruction(&Instruction::LocalSet(out));
    emit_merge(func, ctx, ty, out, src);
    func.instruction(&Instruction::LocalGet(out));
    release(ctx, 1);
}

/// `a | b`, `a & b`, `a - b`, `a ^ b` on the sets in `a` and `b`, built the
/// way CPython builds each, which decides the result's order. Pushes the new
/// set.
pub(crate) fn emit_operator(
    func: &mut Function,
    ctx: &CompilationContext,
    op: &crate::ir::IROp,
    ty: &IRType,
    a: u32,
    b: u32,
) {
    use crate::ir::IROp;
    let out = word(ctx);
    let v = Held::of(ctx, ty);
    match op {
        // set_or: a copy of a, updated with b.
        IROp::BitOr => {
            emit_copy(func, ctx, ty, a);
            func.instruction(&Instruction::LocalSet(out));
            emit_merge(func, ctx, ty, out, b);
        }
        // set_intersection: iterate the smaller (b on a tie), keeping what the
        // other holds.
        IROp::BitAnd => {
            let small = word(ctx);
            let big = word(ctx);
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::I32GtS);
            func.instruction(&Instruction::Select);
            func.instruction(&Instruction::LocalSet(small));
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::I32GtS);
            func.instruction(&Instruction::Select);
            func.instruction(&Instruction::LocalSet(big));
            emit_new_set(func, ctx);
            func.instruction(&Instruction::LocalSet(out));
            emit_for_each(func, ctx, small, v, |func| {
                emit_contains(func, ctx, ty, big, v);
                func.instruction(&Instruction::If(BlockType::Empty));
                emit_add(func, ctx, ty, out, v);
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::End);
            });
            release(ctx, 2);
        }
        // set_difference: a copy of a with b's members discarded when a is
        // more than four times b's size; otherwise a's members b lacks.
        IROp::Sub => {
            func.instruction(&Instruction::LocalGet(a));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::I32Const(2));
            func.instruction(&Instruction::I32ShrS);
            func.instruction(&Instruction::LocalGet(b));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::I32GtS);
            func.instruction(&Instruction::If(BlockType::Empty));
            emit_copy(func, ctx, ty, a);
            func.instruction(&Instruction::LocalSet(out));
            emit_difference_update(func, ctx, ty, out, &Operand::Set(b));
            func.instruction(&Instruction::Else);
            emit_new_set(func, ctx);
            func.instruction(&Instruction::LocalSet(out));
            emit_for_each(func, ctx, a, v, |func| {
                emit_contains(func, ctx, ty, b, v);
                func.instruction(&Instruction::I32Eqz);
                func.instruction(&Instruction::If(BlockType::Empty));
                emit_add(func, ctx, ty, out, v);
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::End);
            });
            func.instruction(&Instruction::End);
        }
        // set_symmetric_difference: a copy of b, then each member of a
        // discarded if there or added if not.
        _ => {
            emit_copy(func, ctx, ty, b);
            func.instruction(&Instruction::LocalSet(out));
            emit_symmetric_update(func, ctx, ty, out, a);
        }
    }
    func.instruction(&Instruction::LocalGet(out));
    v.release(ctx);
    release(ctx, 1);
}

/// `set_symmetric_difference_update`: each member of `src` is discarded from
/// `dst` if it is there and added if not.
pub(crate) fn emit_symmetric_update(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    dst: u32,
    src: u32,
) {
    let v = Held::of(ctx, ty);
    emit_for_each(func, ctx, src, v, |func| {
        emit_discard(func, ctx, ty, dst, v);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        emit_add(func, ctx, ty, dst, v);
        func.instruction(&Instruction::Drop);
        func.instruction(&Instruction::End);
    });
    v.release(ctx);
}

/// Make the set in `dst` hold what the set in `src` holds, by taking over its
/// table: `set_swap_bodies`, as `intersection_update` uses it. The finger
/// `pop()` resumes from is not part of the body and stays.
pub(crate) fn emit_take_table(func: &mut Function, dst: u32, src: u32) {
    for field in [0, SET_CAP, SET_FILL, SET_DATA] {
        func.instruction(&Instruction::LocalGet(dst));
        func.instruction(&Instruction::LocalGet(src));
        func.instruction(&Instruction::I32Load(at(field)));
        func.instruction(&Instruction::I32Store(at(field)));
    }
}

/// `set_clear_internal`: back to an empty table of the minimum size.
pub(crate) fn emit_clear(func: &mut Function, ctx: &CompilationContext, set: u32) {
    let size = word(ctx);
    for field in [0, SET_FILL, SET_FINGER] {
        func.instruction(&Instruction::LocalGet(set));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::I32Store(at(field)));
    }
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Const(SET_MIN_SIZE));
    func.instruction(&Instruction::I32Store(at(SET_CAP)));
    func.instruction(&Instruction::I32Const(SET_MIN_SIZE));
    func.instruction(&Instruction::LocalSet(size));
    func.instruction(&Instruction::LocalGet(set));
    emit_new_buckets(func, ctx, size);
    func.instruction(&Instruction::I32Store(at(SET_DATA)));
    release(ctx, 1);
}

/// `set_pop`: remove and push the member at or after the finger, in table
/// order, wrapping around; the finger then rests just past it. The caller has
/// checked the set is not empty.
pub(crate) fn emit_pop(func: &mut Function, ctx: &CompilationContext, set: u32, v: Held) {
    let i = word(ctx);
    let mask = word(ctx);
    let data = word(ctx);
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_CAP)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(mask));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_DATA)));
    func.instruction(&Instruction::LocalSet(data));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_FINGER)));
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    push_bucket(func, data, i);
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(SET_LIVE));
    func.instruction(&Instruction::I32Eq);
    func.instruction(&Instruction::BrIf(1));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalGet(mask));
    func.instruction(&Instruction::I32And);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    push_bucket(func, data, i);
    func.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
    func.instruction(&Instruction::I32Add);
    v.load(func);
    push_bucket(func, data, i);
    func.instruction(&Instruction::I32Const(SET_DEAD));
    func.instruction(&Instruction::I32Store(at(0)));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Store(at(0)));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::I32Store(at(SET_FINGER)));
    release(ctx, 3);
}

/// Emit `items` into a new set, as a set display builds one. A display of more
/// than two constants is a frozenset constant merged into an empty set in
/// CPython (the compiler folds it), so it is built that way: the frozenset by
/// adding each, rebuilt from its own order as CPython's constant merging
/// rebuilds it, then the merge, whose table size and order can differ from
/// adding each straight into the result.
pub(crate) fn emit_display(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &crate::ir::MemoryLayout,
    items: &[crate::ir::IRExpr],
    ty: &IRType,
) {
    let set = word(ctx);
    let v = Held::of(ctx, ty);
    let constant = items.len() > 2 && items.iter().all(is_constant);
    emit_new_set(func, ctx);
    func.instruction(&Instruction::LocalSet(set));
    for item in items {
        let t = crate::compiler::expression::emit_collection_element(
            item,
            func,
            ctx,
            memory_layout,
            Some(ty),
            "a set display",
        );
        crate::compiler::expression::narrow_element_to_word(func, &t);
        v.set(func);
        emit_add(func, ctx, ty, set, v);
        func.instruction(&Instruction::Drop);
    }
    if constant {
        // The compiler's frozenset, which merging the code's constants builds
        // once more from the first one's order, then SET_UPDATE into a new set.
        let folded = word(ctx);
        let again = word(ctx);
        func.instruction(&Instruction::LocalGet(set));
        func.instruction(&Instruction::LocalSet(folded));
        emit_new_set(func, ctx);
        func.instruction(&Instruction::LocalSet(again));
        emit_for_each(func, ctx, folded, v, |func| {
            emit_add(func, ctx, ty, again, v);
            func.instruction(&Instruction::Drop);
        });
        emit_new_set(func, ctx);
        func.instruction(&Instruction::LocalSet(set));
        emit_merge(func, ctx, ty, set, again);
        release(ctx, 2);
    }
    func.instruction(&Instruction::LocalGet(set));
    v.release(ctx);
    release(ctx, 1);
}

/// A literal CPython's compiler folds into a constant.
fn is_constant(expr: &crate::ir::IRExpr) -> bool {
    use crate::ir::{IRExpr, IRUnaryOp};
    match expr {
        IRExpr::Const(_) => true,
        IRExpr::UnaryOp {
            operand,
            op: IRUnaryOp::Neg | IRUnaryOp::UAdd,
        } => matches!(operand.as_ref(), IRExpr::Const(_)),
        IRExpr::TupleLiteral(items) => items.iter().all(is_constant),
        _ => false,
    }
}

/// Load the bucket value at `offset` past the address on the stack, as a
/// held word or f64, wide for an f64.
#[allow(dead_code)]
fn load_wide(func: &mut Function, v: Held) {
    match v {
        Held::F64(local) => {
            func.instruction(&Instruction::F64Load(wide(0)));
            func.instruction(&Instruction::LocalSet(local));
        }
        Held::Word(local) => {
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::LocalSet(local));
        }
    }
}

/// Replace the set pointer on the stack with a new list of its members in
/// table order, which is the order iterating it visits them. Every construct
/// that walks a set walks this snapshot, as CPython's iterator would walk the
/// table (a set that changes size under iteration is a RuntimeError there).
pub(crate) fn emit_members_as_list(func: &mut Function, ctx: &CompilationContext, ty: &IRType) {
    use crate::compiler::context::{COLLECTION_CAP, COLLECTION_HEADER, COLLECTION_SLOT};
    let set = word(ctx);
    let list = word(ctx);
    let i = word(ctx);
    func.instruction(&Instruction::LocalSet(set));
    func.instruction(&Instruction::I32Const(COLLECTION_HEADER as i32));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::Call(ctx.alloc_func_index));
    func.instruction(&Instruction::LocalSet(list));
    crate::compiler::expression::store_runtime_data_ptr(func, list);
    for offset in [0, COLLECTION_CAP] {
        func.instruction(&Instruction::LocalGet(list));
        func.instruction(&Instruction::LocalGet(set));
        func.instruction(&Instruction::I32Load(at(0)));
        func.instruction(&Instruction::I32Store(at(offset)));
    }
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    let v = Held::of(ctx, ty);
    emit_for_each(func, ctx, set, v, |func| {
        func.instruction(&Instruction::LocalGet(list));
        func.instruction(&Instruction::I32Load(at(
            crate::compiler::context::COLLECTION_DATA,
        )));
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
        func.instruction(&Instruction::I32Mul);
        func.instruction(&Instruction::I32Add);
        v.store(func);
        func.instruction(&Instruction::LocalGet(i));
        func.instruction(&Instruction::I32Const(1));
        func.instruction(&Instruction::I32Add);
        func.instruction(&Instruction::LocalSet(i));
    });
    v.release(ctx);
    func.instruction(&Instruction::LocalGet(list));
    release(ctx, 3);
}

/// A set method's argument: another set, or a sequence whose members it takes
/// in order (a list, a tuple, a range), held in `ptr`.
#[derive(Clone)]
pub(crate) enum Operand {
    Set(u32),
    Sequence(u32),
}

/// Run `body` per member of `operand` (in its iteration order) with the member
/// in `v`.
fn emit_for_each_of(
    func: &mut Function,
    ctx: &CompilationContext,
    operand: &Operand,
    v: Held,
    body: impl FnMut(&mut Function),
) {
    match operand {
        Operand::Set(set) => emit_for_each(func, ctx, *set, v, body),
        Operand::Sequence(seq) => emit_for_each_element(func, ctx, *seq, v, body),
    }
}

/// Run `body` per element of the list-layout sequence in `seq`.
fn emit_for_each_element(
    func: &mut Function,
    ctx: &CompilationContext,
    seq: u32,
    v: Held,
    mut body: impl FnMut(&mut Function),
) {
    let i = word(ctx);
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));
    ctx.enter_expr_blocks(2);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::LocalGet(seq));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32GeS);
    func.instruction(&Instruction::BrIf(1));
    load_member(func, seq, Some(i), 0, v);
    body(func);
    ctx.exit_expr_blocks(2);
    func.instruction(&Instruction::LocalGet(i));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Add);
    func.instruction(&Instruction::LocalSet(i));
    func.instruction(&Instruction::Br(0));
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);
    release(ctx, 1);
}

/// `set_update_internal`: add the members of `other` to the set in `dst`.
fn emit_update(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    dst: u32,
    other: &Operand,
) {
    match other {
        Operand::Set(src) => emit_merge(func, ctx, ty, dst, *src),
        Operand::Sequence(_) => {
            let v = Held::of(ctx, ty);
            emit_for_each_of(func, ctx, other, v, |func| {
                emit_add(func, ctx, ty, dst, v);
                func.instruction(&Instruction::Drop);
            });
            v.release(ctx);
        }
    }
}

/// A new set of the members of `other`, as `set(other)` builds it.
fn emit_set_of(func: &mut Function, ctx: &CompilationContext, ty: &IRType, other: &Operand) {
    let out = word(ctx);
    emit_new_set(func, ctx);
    func.instruction(&Instruction::LocalSet(out));
    emit_update(func, ctx, ty, out, other);
    func.instruction(&Instruction::LocalGet(out));
    release(ctx, 1);
}

/// `a.intersection(other)` into a new set: `set_intersection`, with a
/// sequence iterated in order and kept where `a` holds the element.
fn emit_intersection(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    a: u32,
    other: &Operand,
) {
    match other {
        Operand::Set(b) => emit_operator(func, ctx, &crate::ir::IROp::BitAnd, ty, a, *b),
        Operand::Sequence(_) => {
            let out = word(ctx);
            let v = Held::of(ctx, ty);
            emit_new_set(func, ctx);
            func.instruction(&Instruction::LocalSet(out));
            emit_for_each_of(func, ctx, other, v, |func| {
                emit_contains(func, ctx, ty, a, v);
                func.instruction(&Instruction::If(BlockType::Empty));
                emit_add(func, ctx, ty, out, v);
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::End);
            });
            func.instruction(&Instruction::LocalGet(out));
            v.release(ctx);
            release(ctx, 1);
        }
    }
}

/// `set_difference_update_internal`: discard every member of `other` from the
/// set in `dst`, then rebuild the table if more than a quarter of it is
/// tombstones.
fn emit_difference_update(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    dst: u32,
    other: &Operand,
) {
    let v = Held::of(ctx, ty);
    emit_for_each_of(func, ctx, other, v, |func| {
        emit_discard(func, ctx, ty, dst, v);
        func.instruction(&Instruction::Drop);
    });
    v.release(ctx);
    emit_drop_tombstones(func, ctx, ty, dst);
}

/// Rebuild the table of the set in `set` when its tombstones (`fill - used`)
/// are more than a quarter of its mask, as `difference_update` does.
fn emit_drop_tombstones(func: &mut Function, ctx: &CompilationContext, ty: &IRType, set: u32) {
    let minused = word(ctx);
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_FILL)));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(SET_CAP)));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::I32Const(2));
    func.instruction(&Instruction::I32ShrU);
    func.instruction(&Instruction::I32GtU);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalGet(set));
    func.instruction(&Instruction::I32Load(at(0)));
    func.instruction(&Instruction::LocalTee(minused));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Shl);
    func.instruction(&Instruction::LocalGet(minused));
    func.instruction(&Instruction::I32Const(2));
    func.instruction(&Instruction::I32Shl);
    func.instruction(&Instruction::LocalGet(minused));
    func.instruction(&Instruction::I32Const(50000));
    func.instruction(&Instruction::I32GtS);
    func.instruction(&Instruction::Select);
    func.instruction(&Instruction::LocalSet(minused));
    emit_resize(func, ctx, ty, set, minused);
    func.instruction(&Instruction::End);
    release(ctx, 1);
}

/// Push whether every member of the set in `inner` is in the set in `outer`.
fn emit_subset(func: &mut Function, ctx: &CompilationContext, ty: &IRType, inner: u32, outer: u32) {
    let result = word(ctx);
    let v = Held::of(ctx, ty);
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(result));
    emit_for_each(func, ctx, inner, v, |func| {
        emit_contains(func, ctx, ty, outer, v);
        func.instruction(&Instruction::I32Eqz);
        func.instruction(&Instruction::If(BlockType::Empty));
        func.instruction(&Instruction::I32Const(0));
        func.instruction(&Instruction::LocalSet(result));
        func.instruction(&Instruction::End);
    });
    func.instruction(&Instruction::LocalGet(result));
    v.release(ctx);
    release(ctx, 1);
}

/// `a OP b` for two sets in `a` and `b`, OP one of `== != < <= > >=`: subset
/// and equality by membership, as CPython compares sets.
pub(crate) fn emit_comparison(
    func: &mut Function,
    ctx: &CompilationContext,
    op: &crate::ir::IRCompareOp,
    ty: &IRType,
    a: u32,
    b: u32,
) {
    use crate::ir::IRCompareOp;
    let (inner, outer, size) = match op {
        IRCompareOp::LtE => (a, b, None),
        IRCompareOp::Lt => (a, b, Some(Instruction::I32LtS)),
        IRCompareOp::GtE => (b, a, None),
        IRCompareOp::Gt => (b, a, Some(Instruction::I32LtS)),
        _ => (a, b, Some(Instruction::I32Eq)),
    };
    emit_subset(func, ctx, ty, inner, outer);
    if let Some(size) = size {
        func.instruction(&Instruction::LocalGet(inner));
        func.instruction(&Instruction::I32Load(at(0)));
        func.instruction(&Instruction::LocalGet(outer));
        func.instruction(&Instruction::I32Load(at(0)));
        func.instruction(&size);
        func.instruction(&Instruction::I32And);
    }
    if matches!(op, IRCompareOp::NotEq) {
        func.instruction(&Instruction::I32Eqz);
    }
}

/// Every method of a set, with the receiver's pointer on the stack. A method
/// taking other collections takes a set or a list, tuple, or range, as
/// CPython takes any iterable; each is evaluated in order before any work, as
/// arguments are.
pub(crate) fn emit_method(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &crate::ir::MemoryLayout,
    method_name: &str,
    arguments: &[crate::ir::IRExpr],
    set_type: &IRType,
) -> IRType {
    use crate::compiler::expression::{emit_collection_element, emit_expr, narrow_element_to_word};
    use crate::ir::IROp;
    let member = match set_type {
        IRType::Set(e) => e.as_ref().clone(),
        _ => IRType::Unknown,
    };
    let declared = (!matches!(member, IRType::Unknown | IRType::Any)).then_some(&member);
    let arity = |ok: bool, takes: &str| -> bool {
        if !ok {
            ctx.report(format!(
                "set.{method_name}() takes {takes}, not {} argument(s)",
                arguments.len()
            ));
        }
        ok
    };
    let set = word(ctx);
    func.instruction(&Instruction::LocalSet(set));
    let refuse = |func: &mut Function| -> IRType {
        func.instruction(&Instruction::I32Const(0));
        release(ctx, 1);
        IRType::Unknown
    };

    let result = match method_name {
        "add" | "remove" | "discard" => {
            if !arity(arguments.len() == 1, "exactly one argument") {
                return refuse(func);
            }
            let t = emit_collection_element(
                &arguments[0],
                func,
                ctx,
                memory_layout,
                declared,
                &format!("set.{method_name}()"),
            );
            narrow_element_to_word(func, &t);
            let ty = declared.cloned().unwrap_or(t);
            if let Some(why) = member_unsupported(ctx, &ty) {
                ctx.report(format!("{why} cannot be a set member"));
            }
            let v = Held::of(ctx, &ty);
            v.set(func);
            match method_name {
                "add" => {
                    emit_add(func, ctx, &ty, set, v);
                    func.instruction(&Instruction::Drop);
                }
                "remove" => {
                    emit_discard(func, ctx, &ty, set, v);
                    func.instruction(&Instruction::I32Eqz);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    let key = match v {
                        Held::Word(slot) | Held::F64(slot) => {
                            crate::compiler::exceptions::ExcArg::Value(slot, ty.clone())
                        }
                    };
                    crate::compiler::function::emit_raise(func, ctx, "KeyError", key, 1);
                    func.instruction(&Instruction::End);
                }
                _ => {
                    emit_discard(func, ctx, &ty, set, v);
                    func.instruction(&Instruction::Drop);
                }
            }
            v.release(ctx);
            IRType::None
        }
        "copy" | "clear" | "pop" => {
            if !arity(arguments.is_empty(), "no arguments") {
                return refuse(func);
            }
            match method_name {
                "copy" => {
                    emit_copy(func, ctx, &member, set);
                    set_type.clone()
                }
                "clear" => {
                    emit_clear(func, ctx, set);
                    IRType::None
                }
                _ => {
                    let v = Held::of(ctx, &member);
                    func.instruction(&Instruction::LocalGet(set));
                    func.instruction(&Instruction::I32Load(at(0)));
                    func.instruction(&Instruction::I32Eqz);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    crate::compiler::function::emit_raise(
                        func,
                        ctx,
                        "KeyError",
                        crate::compiler::exceptions::ExcArg::Text("pop from an empty set"),
                        1,
                    );
                    func.instruction(&Instruction::Else);
                    emit_pop(func, ctx, set, v);
                    func.instruction(&Instruction::End);
                    v.get(func);
                    v.release(ctx);
                    if matches!(member, IRType::String | IRType::Bytes) {
                        crate::compiler::expression::recover_str_pair(func, ctx);
                    }
                    member.clone()
                }
            }
        }
        "union"
        | "intersection"
        | "difference"
        | "symmetric_difference"
        | "update"
        | "intersection_update"
        | "difference_update"
        | "symmetric_difference_update"
        | "issubset"
        | "issuperset"
        | "isdisjoint" => {
            let single = matches!(
                method_name,
                "symmetric_difference"
                    | "symmetric_difference_update"
                    | "issubset"
                    | "issuperset"
                    | "isdisjoint"
            );
            let in_place = method_name.ends_with("_update") || method_name == "update";
            if single && !arity(arguments.len() == 1, "exactly one argument") {
                return refuse(func);
            }
            // Each operand: a set, or a sequence of the receiver's member type.
            let mut operands = Vec::new();
            for arg in arguments {
                let t = emit_expr(arg, func, ctx, memory_layout, None);
                let (element, operand) = match &t {
                    IRType::Set(e) => {
                        let slot = word(ctx);
                        func.instruction(&Instruction::LocalSet(slot));
                        (e.as_ref().clone(), Operand::Set(slot))
                    }
                    IRType::List(e) | IRType::VarTuple(e) => {
                        let slot = word(ctx);
                        func.instruction(&Instruction::LocalSet(slot));
                        (e.as_ref().clone(), Operand::Sequence(slot))
                    }
                    IRType::Tuple(members) if members.windows(2).all(|w| w[0] == w[1]) => {
                        let slot = word(ctx);
                        func.instruction(&Instruction::LocalSet(slot));
                        (
                            members.first().cloned().unwrap_or(IRType::Unknown),
                            Operand::Sequence(slot),
                        )
                    }
                    other => {
                        if matches!(other, IRType::String | IRType::Bytes) {
                            func.instruction(&Instruction::Drop);
                        }
                        func.instruction(&Instruction::Drop);
                        ctx.report(format!(
                            "set.{method_name}() of a '{}' is not supported. Hint: pass a set, \
                             a list, or a tuple",
                            crate::type_to_string(other)
                        ));
                        let slot = word(ctx);
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(slot));
                        (member.clone(), Operand::Set(slot))
                    }
                };
                let open = |t: &IRType| matches!(t, IRType::Unknown | IRType::Any);
                if !open(&element) && !open(&member) && element != member {
                    ctx.report(format!(
                        "set.{method_name}() of '{}' elements on a set of '{}': the two hash \
                         and compare differently here",
                        crate::type_to_string(&element),
                        crate::type_to_string(&member)
                    ));
                }
                operands.push(operand);
            }
            let ty = if matches!(member, IRType::Unknown | IRType::Any) {
                match operands.first() {
                    Some(_) => member.clone(),
                    None => member.clone(),
                }
            } else {
                member.clone()
            };
            let answer = match method_name {
                "issubset" | "issuperset" => {
                    // A non-set argument is made a set first.
                    let other = word(ctx);
                    match &operands[0] {
                        Operand::Set(s) => {
                            func.instruction(&Instruction::LocalGet(*s));
                        }
                        seq => emit_set_of(func, ctx, &ty, seq),
                    }
                    func.instruction(&Instruction::LocalSet(other));
                    let (inner, outer) = if method_name == "issubset" {
                        (set, other)
                    } else {
                        (other, set)
                    };
                    emit_subset(func, ctx, &ty, inner, outer);
                    release(ctx, 1);
                    IRType::Bool
                }
                "isdisjoint" => {
                    let result = word(ctx);
                    let v = Held::of(ctx, &ty);
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::LocalSet(result));
                    emit_for_each_of(func, ctx, &operands[0], v, |func| {
                        emit_contains(func, ctx, &ty, set, v);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(result));
                        func.instruction(&Instruction::End);
                    });
                    func.instruction(&Instruction::LocalGet(result));
                    v.release(ctx);
                    release(ctx, 1);
                    IRType::Bool
                }
                "update" => {
                    for other in &operands {
                        emit_update(func, ctx, &ty, set, other);
                    }
                    IRType::None
                }
                "difference_update" => {
                    for other in &operands {
                        emit_difference_update(func, ctx, &ty, set, other);
                    }
                    IRType::None
                }
                "symmetric_difference_update" => {
                    let other = word(ctx);
                    match &operands[0] {
                        Operand::Set(s) => {
                            func.instruction(&Instruction::LocalGet(*s));
                        }
                        seq => emit_set_of(func, ctx, &ty, seq),
                    }
                    func.instruction(&Instruction::LocalSet(other));
                    emit_symmetric_update(func, ctx, &ty, set, other);
                    release(ctx, 1);
                    IRType::None
                }
                "intersection_update" | "intersection" => {
                    // Each operand intersects the running result in turn; with
                    // none, it is a copy.
                    let current = word(ctx);
                    if operands.is_empty() {
                        emit_copy(func, ctx, &ty, set);
                    } else {
                        func.instruction(&Instruction::LocalGet(set));
                        func.instruction(&Instruction::LocalSet(current));
                        for other in &operands {
                            emit_intersection(func, ctx, &ty, current, other);
                            func.instruction(&Instruction::LocalSet(current));
                        }
                        func.instruction(&Instruction::LocalGet(current));
                    }
                    release(ctx, 1);
                    if method_name == "intersection_update" {
                        let tmp = word(ctx);
                        func.instruction(&Instruction::LocalSet(tmp));
                        emit_take_table(func, set, tmp);
                        release(ctx, 1);
                        IRType::None
                    } else {
                        set_type.clone()
                    }
                }
                "union" => {
                    let out = word(ctx);
                    emit_copy(func, ctx, &ty, set);
                    func.instruction(&Instruction::LocalSet(out));
                    for other in &operands {
                        emit_update(func, ctx, &ty, out, other);
                    }
                    func.instruction(&Instruction::LocalGet(out));
                    release(ctx, 1);
                    set_type.clone()
                }
                "difference" => {
                    match operands.as_slice() {
                        [Operand::Set(b)] => {
                            emit_operator(func, ctx, &IROp::Sub, &ty, set, *b);
                        }
                        _ => {
                            let out = word(ctx);
                            emit_copy(func, ctx, &ty, set);
                            func.instruction(&Instruction::LocalSet(out));
                            for other in &operands {
                                emit_difference_update(func, ctx, &ty, out, other);
                            }
                            func.instruction(&Instruction::LocalGet(out));
                            release(ctx, 1);
                        }
                    }
                    set_type.clone()
                }
                // symmetric_difference
                _ => {
                    match &operands[0] {
                        Operand::Set(b) => {
                            emit_operator(func, ctx, &IROp::BitXor, &ty, set, *b);
                        }
                        seq => {
                            let other = word(ctx);
                            emit_set_of(func, ctx, &ty, seq);
                            func.instruction(&Instruction::LocalSet(other));
                            emit_symmetric_update(func, ctx, &ty, other, set);
                            func.instruction(&Instruction::LocalGet(other));
                            release(ctx, 1);
                        }
                    }
                    set_type.clone()
                }
            };
            let _ = in_place;
            release(ctx, operands.len() as u32);
            answer
        }
        _ => {
            ctx.report(format!(
                "'{method_name}' is not a set method this compiler implements"
            ));
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    };
    release(ctx, 1);
    result
}

/// `set(x)` with the value of `x` (of `ty`) on the stack: a copy of a set, the
/// elements of a list, tuple, or range added in order, or a dict's keys after
/// the one resize CPython makes up front for a dict. Pushes the new set and
/// returns its type.
pub(crate) fn emit_set_from(func: &mut Function, ctx: &CompilationContext, ty: &IRType) -> IRType {
    use crate::compiler::context::{COLLECTION_DATA, DICT_ENTRY};
    let src = word(ctx);
    let out = word(ctx);
    let member = match ty {
        IRType::Set(e) | IRType::List(e) | IRType::VarTuple(e) => e.as_ref().clone(),
        IRType::Dict(k, _) => k.as_ref().clone(),
        IRType::Tuple(members) if members.windows(2).all(|w| w[0] == w[1]) => {
            members.first().cloned().unwrap_or(IRType::Unknown)
        }
        IRType::Range => IRType::Int,
        other => {
            if matches!(other, IRType::String | IRType::Bytes) {
                func.instruction(&Instruction::Drop);
            }
            func.instruction(&Instruction::Drop);
            ctx.report(format!(
                "set() of a '{}' is not supported yet. Hint: pass a set, a list, a tuple, a \
                 range, or a dict",
                crate::type_to_string(other)
            ));
            emit_new_set(func, ctx);
            release(ctx, 2);
            return IRType::Set(Box::new(IRType::Unknown));
        }
    };
    if let Some(why) = member_unsupported(ctx, &member) {
        ctx.report(format!("{why} cannot be a set member"));
    }
    func.instruction(&Instruction::LocalSet(src));
    emit_new_set(func, ctx);
    func.instruction(&Instruction::LocalSet(out));
    match ty {
        IRType::Set(_) => emit_merge(func, ctx, &member, out, src),
        IRType::Dict(..) => {
            // (fill + len(d)) * 5 >= mask * 3 on an empty set: resize to
            // (used + len(d)) * 2 first, then add each key in dict order.
            let minused = word(ctx);
            let i = word(ctx);
            let v = Held::of(ctx, &member);
            func.instruction(&Instruction::LocalGet(src));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::I32Const(5));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Const((SET_MIN_SIZE - 1) * 3));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::If(BlockType::Empty));
            func.instruction(&Instruction::LocalGet(src));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Shl);
            func.instruction(&Instruction::LocalSet(minused));
            emit_resize(func, ctx, &member, out, minused);
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            ctx.enter_expr_blocks(2);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(src));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(src));
            func.instruction(&Instruction::I32Load(at(COLLECTION_DATA)));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            v.load(func);
            emit_add(func, ctx, &member, out, v);
            func.instruction(&Instruction::Drop);
            ctx.exit_expr_blocks(2);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            v.release(ctx);
            release(ctx, 2);
        }
        IRType::Range => {
            let i = word(ctx);
            let n = word(ctx);
            let v = Held::of(ctx, &member);
            crate::compiler::expression::emit_range_count(func, ctx, src);
            func.instruction(&Instruction::LocalSet(n));
            func.instruction(&Instruction::I32Const(0));
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Block(BlockType::Empty));
            func.instruction(&Instruction::Loop(BlockType::Empty));
            ctx.enter_expr_blocks(2);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(n));
            func.instruction(&Instruction::I32GeS);
            func.instruction(&Instruction::BrIf(1));
            func.instruction(&Instruction::LocalGet(src));
            func.instruction(&Instruction::I32Load(at(0)));
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::LocalGet(src));
            func.instruction(&Instruction::I32Load(at(8)));
            func.instruction(&Instruction::I32Mul);
            func.instruction(&Instruction::I32Add);
            v.set(func);
            emit_add(func, ctx, &member, out, v);
            func.instruction(&Instruction::Drop);
            ctx.exit_expr_blocks(2);
            func.instruction(&Instruction::LocalGet(i));
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Add);
            func.instruction(&Instruction::LocalSet(i));
            func.instruction(&Instruction::Br(0));
            func.instruction(&Instruction::End);
            func.instruction(&Instruction::End);
            v.release(ctx);
            release(ctx, 2);
        }
        _ => emit_update(func, ctx, &member, out, &Operand::Sequence(src)),
    }
    func.instruction(&Instruction::LocalGet(out));
    release(ctx, 2);
    IRType::Set(Box::new(member))
}
