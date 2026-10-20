//! The format specification mini-language and float rendering, as runtime
//! helpers (see `compiler::runtime`).
//!
//! `format(value, spec)`, an f-string's `{value:spec}`, and `str.format`'s
//! fields all end in one helper per value type, which parses the spec at run
//! time (a spec can itself hold replacement fields) and renders the value the
//! way CPython's `format` does: fill, alignment, sign, `z`, `#`, `0`, width,
//! `,`/`_` grouping, precision, and the type letters. Floats are rendered by
//! a port of CPython's `PyOS_double_to_string` over exactly rounded digits,
//! so `f"{x:.2f}"`, `%e`, and `{:g}` agree with CPython digit for digit.
//!
//! A specification CPython rejects with `ValueError` traps here; a constant
//! one is checked when the program is compiled (`spec_error`), so only a spec
//! built at run time can reach the trap.

use crate::compiler::context::CompilationContext;
use crate::compiler::runtime::{
    alloc_string, byte, emit_float_scale, float_locals, function, return_text, w32, Helper, BIG_R,
    BIG_S, BIG_T, BIG_Z, DIGITS, DIG_COUNT, DIG_K, DIG_MODE, DIG_N, SPEC, TEXT,
};
use crate::ir::IRType;
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};

/// Emit a sequence of instructions into a function.
macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

// The fields of a parsed specification, at `SPEC + offset`.
const FILL: i32 = 0; // the fill character's UTF-8 bytes, packed
const FILL_LEN: i32 = 4;
const FILL_SET: i32 = 8;
const ALIGN: i32 = 12;
const SIGN: i32 = 16;
const ZNEG: i32 = 20;
const ALT: i32 = 24;
const ZERO: i32 = 28;
const WIDTH: i32 = 32;
const GROUP: i32 = 36;
const PREC: i32 = 40;
const TYPE: i32 = 44;

/// Flags for `FloatToString`, after CPython's `Py_DTSF_*`.
pub(crate) const FLOAT_ALT: i32 = 1;
pub(crate) const FLOAT_ADD_DOT_0: i32 = 2;
pub(crate) const FLOAT_UPPER: i32 = 4;

fn spec_field(f: &mut Function, field: i32) {
    emit!(f, I::I32Const(SPEC + field), I::I32Load(w32(0)));
}

fn set_spec(f: &mut Function, field: i32, value: i32) {
    emit!(
        f,
        I::I32Const(SPEC + field),
        I::I32Const(value),
        I::I32Store(w32(0))
    );
}

/// `while cond { body }` with `cond` pushing an i32.
fn while_loop(f: &mut Function, cond: impl Fn(&mut Function), body: impl FnOnce(&mut Function)) {
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    cond(f);
    emit!(f, I::I32Eqz, I::BrIf(1));
    body(f);
    emit!(f, I::Br(0), I::End, I::End);
}

fn incr(f: &mut Function, local: u32, by: i32) {
    emit!(
        f,
        I::LocalGet(local),
        I::I32Const(by),
        I::I32Add,
        I::LocalSet(local)
    );
}

/// Store the byte on the stack (pushed by `value`) at the cursor `p`,
/// advancing it.
fn put(f: &mut Function, p: u32, value: impl FnOnce(&mut Function)) {
    emit!(f, I::LocalGet(p));
    value(f);
    emit!(f, I::I32Store8(byte()));
    incr(f, p, 1);
}

fn put_byte(f: &mut Function, p: u32, b: u8) {
    put(f, p, |f| emit!(f, I::I32Const(b as i32)));
}

/// `count` (a local, consumed) zero characters at the cursor `p`.
fn put_zeros(f: &mut Function, p: u32, count: u32) {
    while_loop(
        f,
        |f| emit!(f, I::LocalGet(count), I::I32Const(0), I::I32GtS),
        |f| {
            put_byte(f, p, b'0');
            incr(f, count, -1);
        },
    );
}

/// The digits `[from, to)` of the digit buffer (locals; `from` is consumed)
/// as characters at the cursor `p`.
fn put_digits(f: &mut Function, p: u32, from: u32, to: u32) {
    while_loop(
        f,
        |f| emit!(f, I::LocalGet(from), I::LocalGet(to), I::I32LtS),
        |f| {
            put(f, p, |f| {
                emit!(
                    f,
                    I::LocalGet(from),
                    I::I32Const(DIGITS),
                    I::I32Add,
                    I::I32Load8U(byte()),
                    I::I32Const(b'0' as i32),
                    I::I32Add
                )
            });
            incr(f, from, 1);
        },
    );
}

/// The length of the string at `s` (a local).
fn str_len(f: &mut Function, s: u32) {
    emit!(
        f,
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0))
    );
}

/// Return a new string of the bytes from `TEXT` to the cursor `p`, using
/// `len` and `out` as scratch.
fn return_from_text(f: &mut Function, ctx: &CompilationContext, p: u32, len: u32, out: u32) {
    emit!(
        f,
        I::LocalGet(p),
        I::I32Const(TEXT),
        I::I32Sub,
        I::LocalSet(len)
    );
    alloc_string(f, ctx, len, out);
    emit!(
        f,
        I::LocalGet(out),
        I::I32Const(TEXT),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(out),
        I::Return
    );
}

/// Copy the string at `s` (a local) to the cursor `p`, using `len`.
fn put_string(f: &mut Function, p: u32, s: u32, len: u32) {
    str_len(f, s);
    emit!(
        f,
        I::LocalSet(len),
        I::LocalGet(p),
        I::LocalGet(s),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(p),
        I::LocalGet(len),
        I::I32Add,
        I::LocalSet(p)
    );
}

/// Push the number of code points in the string at `s` (a local), using `i`
/// and `n` as scratch: the bytes that do not continue a sequence.
fn code_points(f: &mut Function, s: u32, i: u32, n: u32, out: u32) {
    emit!(
        f,
        I::I32Const(0),
        I::LocalSet(out),
        I::I32Const(0),
        I::LocalSet(i)
    );
    str_len(f, s);
    emit!(f, I::LocalSet(n));
    while_loop(
        f,
        |f| emit!(f, I::LocalGet(i), I::LocalGet(n), I::I32LtU),
        |f| {
            emit!(
                f,
                I::LocalGet(out),
                I::LocalGet(s),
                I::LocalGet(i),
                I::I32Add,
                I::I32Load8U(byte()),
                I::I32Const(0xc0),
                I::I32And,
                I::I32Const(0x80),
                I::I32Ne,
                I::I32Add,
                I::LocalSet(out)
            );
            incr(f, i, 1);
        },
    );
}

/// The correctly rounded digits of a nonnegative finite float: `DIG_COUNT`
/// significant digits (`DIG_MODE` 2) or digits past the decimal point
/// (`DIG_MODE` 3), rounded half-to-even on the exact value, trailing zeros
/// dropped, as Gay's `dtoa` modes 2 and 3 give them.
pub(crate) fn build_fixed_digits(ctx: &CompilationContext) -> Function {
    let set = ctx.helper(Helper::BigSet);
    let copy = ctx.helper(Helper::BigCopy);
    let mul = ctx.helper(Helper::BigMulSmall);
    let cmp = ctx.helper(Helper::BigCmp);
    let quorem = ctx.helper(Helper::BigQuoRem);
    let mut f = function(&float_locals());
    let (v, bits, k, d, n, mode, want, c, i, count) = (0, 1, 7, 8, 9, 10, 11, 12, 15, 16);
    let digit = |f: &mut Function, index: u32| {
        emit!(f, I::LocalGet(index), I::I32Const(DIGITS), I::I32Add);
    };
    let finish = |f: &mut Function| {
        emit!(
            f,
            I::I32Const(DIG_N),
            I::LocalGet(n),
            I::I32Store(w32(0)),
            I::I32Const(DIG_K),
            I::LocalGet(k),
            I::I32Store(w32(0)),
            I::LocalGet(n),
            I::Return
        );
    };
    // Zero is the digit 0 at the units place.
    let zero = |f: &mut Function| {
        emit!(
            f,
            I::I32Const(DIGITS),
            I::I32Const(0),
            I::I32Store8(byte()),
            I::I32Const(1),
            I::LocalSet(n),
            I::I32Const(1),
            I::LocalSet(k)
        );
    };
    emit!(
        f,
        I::LocalGet(v),
        I::F64Const(0.0_f64.into()),
        I::F64Eq,
        I::If(BlockType::Empty)
    );
    zero(&mut f);
    finish(&mut f);
    emit!(f, I::End);
    emit!(f, I::I32Const(BIG_Z), I::I64Const(0), I::Call(set));
    emit!(f, I::LocalGet(v), I::I64ReinterpretF64, I::LocalSet(bits));
    emit_float_scale(&mut f, ctx);
    emit!(
        f,
        I::I32Const(DIG_MODE),
        I::I32Load(w32(0)),
        I::LocalSet(mode),
        I::I32Const(DIG_COUNT),
        I::I32Load(w32(0)),
        I::LocalSet(count),
        // want = mode == 2 ? count : k + count
        I::LocalGet(count),
        I::LocalGet(k),
        I::LocalGet(count),
        I::I32Add,
        I::LocalGet(mode),
        I::I32Const(2),
        I::I32Eq,
        I::Select,
        I::LocalSet(want)
    );
    // Nothing to keep: the value rounds to 0, or at a want of 0 perhaps to
    // one unit in the last place (R / S is the first digit's value, so that
    // is R > 5S; a tie goes to the even 0).
    emit!(
        f,
        I::LocalGet(want),
        I::I32Const(0),
        I::I32LeS,
        I::If(BlockType::Empty)
    );
    emit!(f, I::LocalGet(want), I::I32Eqz, I::If(BlockType::Empty));
    emit!(
        f,
        I::I32Const(BIG_T),
        I::I32Const(BIG_S),
        I::Call(copy),
        I::I32Const(BIG_T),
        I::I32Const(5),
        I::Call(mul),
        I::I32Const(BIG_R),
        I::I32Const(BIG_T),
        I::Call(cmp),
        I::I32Const(0),
        I::I32GtS,
        I::If(BlockType::Empty),
        I::I32Const(DIGITS),
        I::I32Const(1),
        I::I32Store8(byte()),
        I::I32Const(1),
        I::LocalSet(n),
        I::LocalGet(k),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(k)
    );
    finish(&mut f);
    emit!(f, I::End, I::End);
    zero(&mut f);
    finish(&mut f);
    emit!(f, I::End);

    // `want` digits, or fewer if the value runs out exactly.
    emit!(f, I::I32Const(0), I::LocalSet(n));
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::I32Const(BIG_R),
        I::I32Const(BIG_S),
        I::Call(quorem),
        I::LocalSet(d)
    );
    digit(&mut f, n);
    emit!(f, I::LocalGet(d), I::I32Store8(byte()));
    incr(&mut f, n, 1);
    emit!(f, I::LocalGet(n), I::LocalGet(want), I::I32GeS, I::BrIf(1));
    emit!(
        f,
        I::I32Const(BIG_R),
        I::I32Const(BIG_Z),
        I::Call(cmp),
        I::I32Eqz,
        I::BrIf(1),
        I::I32Const(BIG_R),
        I::I32Const(10),
        I::Call(mul),
        I::Br(0),
        I::End,
        I::End
    );
    // Round half to even on the remainder.
    emit!(
        f,
        I::I32Const(BIG_T),
        I::I32Const(BIG_R),
        I::Call(copy),
        I::I32Const(BIG_T),
        I::I32Const(2),
        I::Call(mul),
        I::I32Const(BIG_T),
        I::I32Const(BIG_S),
        I::Call(cmp),
        I::LocalTee(c),
        I::I32Const(0),
        I::I32GtS,
        I::LocalGet(c),
        I::I32Eqz,
        I::LocalGet(n),
        I::I32Const(DIGITS - 1),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(1),
        I::I32And,
        I::I32And,
        I::I32Or,
        I::If(BlockType::Empty)
    );
    emit!(f, I::LocalGet(n), I::I32Const(1), I::I32Sub, I::LocalSet(i));
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    digit(&mut f, i);
    digit(&mut f, i);
    emit!(
        f,
        I::I32Load8U(byte()),
        I::I32Const(1),
        I::I32Add,
        I::LocalTee(d),
        I::I32Store8(byte())
    );
    emit!(f, I::LocalGet(d), I::I32Const(10), I::I32LtS, I::BrIf(1));
    digit(&mut f, i);
    emit!(f, I::I32Const(0), I::I32Store8(byte()));
    emit!(f, I::LocalGet(i), I::I32Eqz, I::If(BlockType::Empty));
    emit!(
        f,
        I::I32Const(DIGITS),
        I::I32Const(1),
        I::I32Store8(byte()),
        I::LocalGet(k),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(k),
        I::Br(2),
        I::End
    );
    incr(&mut f, i, -1);
    emit!(f, I::Br(0), I::End, I::End, I::End);
    // Trailing zeros carry nothing.
    while_loop(
        &mut f,
        |f| {
            emit!(
                f,
                I::LocalGet(n),
                I::I32Const(1),
                I::I32GtS,
                I::LocalGet(n),
                I::I32Const(DIGITS - 1),
                I::I32Add,
                I::I32Load8U(byte()),
                I::I32Eqz,
                I::I32And
            )
        },
        |f| incr(f, n, -1),
    );
    finish(&mut f);
    emit!(f, I::I32Const(0), I::End);
    f
}

/// CPython's `PyOS_double_to_string` (its `format_float_short`) for a
/// nonnegative value, without the sign: `type` is `e`, `f`, `g`, or `r`
/// (shortest), and `flags` are `FLOAT_ALT`, `FLOAT_ADD_DOT_0`, and
/// `FLOAT_UPPER`.
pub(crate) fn build_float_to_string(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let shortest = ctx.helper(Helper::ShortestDigits);
    let fixed = ctx.helper(Helper::FixedDigits);
    let mut f = function(&[I32; 14]);
    let (v, ty, prec, flags) = (0, 1, 2, 3);
    let (n, k, use_exp, vend, vstart, expo, p, out, len, i, cnt, alt, dot0, upper) =
        (4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17);
    emit!(
        f,
        I::LocalGet(flags),
        I::I32Const(FLOAT_ALT),
        I::I32And,
        I::LocalSet(alt),
        I::LocalGet(flags),
        I::I32Const(FLOAT_ADD_DOT_0),
        I::I32And,
        I::LocalSet(dot0),
        I::LocalGet(flags),
        I::I32Const(FLOAT_UPPER),
        I::I32And,
        I::LocalSet(upper)
    );
    for (test, lower_text, upper_text) in [(0, "nan", "NAN"), (1, "inf", "INF")] {
        if test == 0 {
            emit!(f, I::LocalGet(v), I::LocalGet(v), I::F64Ne);
        } else {
            emit!(
                f,
                I::LocalGet(v),
                I::F64Const(f64::INFINITY.into()),
                I::F64Eq
            );
        }
        emit!(
            f,
            I::If(BlockType::Empty),
            I::LocalGet(upper),
            I::If(BlockType::Empty)
        );
        return_text(&mut f, ctx, upper_text, len, out);
        emit!(f, I::End);
        return_text(&mut f, ctx, lower_text, len, out);
        emit!(f, I::End);
    }

    // The digits.
    let is = |f: &mut Function, local: u32, ch: u8| {
        emit!(f, I::LocalGet(local), I::I32Const(ch as i32), I::I32Eq);
    };
    is(&mut f, ty, b'r');
    emit!(f, I::If(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(v),
        I::F64Const(0.0_f64.into()),
        I::F64Eq,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::I32Const(DIGITS),
        I::I32Const(0),
        I::I32Store8(byte()),
        I::I32Const(DIG_N),
        I::I32Const(1),
        I::I32Store(w32(0)),
        I::I32Const(DIG_K),
        I::I32Const(1),
        I::I32Store(w32(0)),
        I::Else,
        I::LocalGet(v),
        I::Call(shortest),
        I::Drop,
        I::End
    );
    emit!(f, I::Else);
    // 'e': mode 2, prec + 1; 'f': mode 3, prec; 'g': mode 2, max(prec, 1)
    emit!(
        f,
        I::I32Const(DIG_MODE),
        I::I32Const(3),
        I::I32Const(2),
        I::LocalGet(ty),
        I::I32Const(b'f' as i32),
        I::I32Eq,
        I::Select,
        I::I32Store(w32(0)),
        // count: 'e' prec + 1, 'g' max(prec, 1), 'f' prec
        I::I32Const(DIG_COUNT),
        I::LocalGet(prec),
        I::I32Const(1),
        I::I32Add,
        I::LocalGet(prec),
        I::I32Const(1),
        I::LocalGet(prec),
        I::I32Const(1),
        I::I32GeS,
        I::Select,
        I::LocalGet(prec),
        I::LocalGet(ty),
        I::I32Const(b'g' as i32),
        I::I32Eq,
        I::Select,
        I::LocalGet(ty),
        I::I32Const(b'e' as i32),
        I::I32Eq,
        I::Select,
        I::I32Store(w32(0)),
        I::LocalGet(v),
        I::Call(fixed),
        I::Drop,
        I::End
    );
    emit!(
        f,
        I::I32Const(DIG_N),
        I::I32Load(w32(0)),
        I::LocalSet(n),
        I::I32Const(DIG_K),
        I::I32Load(w32(0)),
        I::LocalSet(k),
        I::LocalGet(n),
        I::LocalSet(vend),
        I::I32Const(0),
        I::LocalSet(use_exp)
    );
    // Where the digits end and whether an exponent is used.
    is(&mut f, ty, b'e');
    emit!(
        f,
        I::If(BlockType::Empty),
        I::I32Const(1),
        I::LocalSet(use_exp),
        I::LocalGet(prec),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(vend),
        I::End
    );
    is(&mut f, ty, b'f');
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(k),
        I::LocalGet(prec),
        I::I32Add,
        I::LocalSet(vend),
        I::End
    );
    is(&mut f, ty, b'g');
    emit!(f, I::If(BlockType::Empty));
    // P = max(prec, 1) in cnt
    emit!(
        f,
        I::LocalGet(prec),
        I::I32Const(1),
        I::LocalGet(prec),
        I::I32Const(1),
        I::I32GeS,
        I::Select,
        I::LocalSet(cnt),
        I::LocalGet(k),
        I::I32Const(-4),
        I::I32LeS,
        I::LocalGet(k),
        I::LocalGet(cnt),
        I::LocalGet(dot0),
        I::I32Const(0),
        I::I32Ne,
        I::I32Sub,
        I::I32GtS,
        I::I32Or,
        I::LocalSet(use_exp),
        I::LocalGet(alt),
        I::If(BlockType::Empty),
        I::LocalGet(cnt),
        I::LocalSet(vend),
        I::End,
        I::End
    );
    is(&mut f, ty, b'r');
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(k),
        I::I32Const(-4),
        I::I32LeS,
        I::LocalGet(k),
        I::I32Const(16),
        I::I32GtS,
        I::I32Or,
        I::LocalSet(use_exp),
        I::End
    );
    emit!(
        f,
        I::LocalGet(use_exp),
        I::If(BlockType::Empty),
        I::LocalGet(k),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(expo),
        I::I32Const(1),
        I::LocalSet(k),
        I::End,
        // vstart = k <= 0 ? k - 1 : 0
        I::LocalGet(k),
        I::I32Const(1),
        I::I32Sub,
        I::I32Const(0),
        I::LocalGet(k),
        I::I32Const(0),
        I::I32LeS,
        I::Select,
        I::LocalSet(vstart),
        // vend = max(vend, k + (dot0 && !use_exp))
        I::LocalGet(k),
        I::LocalGet(dot0),
        I::I32Const(0),
        I::I32Ne,
        I::LocalGet(use_exp),
        I::I32Eqz,
        I::I32And,
        I::I32Add,
        I::LocalTee(cnt),
        I::LocalGet(vend),
        I::LocalGet(vend),
        I::LocalGet(cnt),
        I::I32LtS,
        I::Select,
        I::LocalSet(vend)
    );

    // [zeros] digits [zeros], with one decimal point somewhere.
    emit!(f, I::I32Const(TEXT), I::LocalSet(p));
    emit!(
        f,
        I::LocalGet(k),
        I::I32Const(0),
        I::I32LeS,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(k),
        I::LocalGet(vstart),
        I::I32Sub,
        I::LocalSet(cnt)
    );
    put_zeros(&mut f, p, cnt);
    put_byte(&mut f, p, b'.');
    emit!(
        f,
        I::I32Const(0),
        I::LocalGet(k),
        I::I32Sub,
        I::LocalSet(cnt)
    );
    put_zeros(&mut f, p, cnt);
    emit!(
        f,
        I::Else,
        I::I32Const(0),
        I::LocalGet(vstart),
        I::I32Sub,
        I::LocalSet(cnt)
    );
    put_zeros(&mut f, p, cnt);
    emit!(f, I::End);
    emit!(
        f,
        I::LocalGet(k),
        I::I32Const(0),
        I::I32GtS,
        I::LocalGet(k),
        I::LocalGet(n),
        I::I32LeS,
        I::I32And,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::LocalSet(i)
    );
    put_digits(&mut f, p, i, k);
    put_byte(&mut f, p, b'.');
    emit!(f, I::LocalGet(k), I::LocalSet(i));
    put_digits(&mut f, p, i, n);
    emit!(f, I::Else, I::I32Const(0), I::LocalSet(i));
    put_digits(&mut f, p, i, n);
    emit!(f, I::End);
    emit!(
        f,
        I::LocalGet(n),
        I::LocalGet(k),
        I::I32LtS,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(k),
        I::LocalGet(n),
        I::I32Sub,
        I::LocalSet(cnt)
    );
    put_zeros(&mut f, p, cnt);
    put_byte(&mut f, p, b'.');
    emit!(
        f,
        I::LocalGet(vend),
        I::LocalGet(k),
        I::I32Sub,
        I::LocalSet(cnt)
    );
    put_zeros(&mut f, p, cnt);
    emit!(
        f,
        I::Else,
        I::LocalGet(vend),
        I::LocalGet(n),
        I::I32Sub,
        I::LocalSet(cnt)
    );
    put_zeros(&mut f, p, cnt);
    emit!(f, I::End);
    // A trailing point goes, unless '#' keeps it.
    emit!(
        f,
        I::LocalGet(p),
        I::I32Const(1),
        I::I32Sub,
        I::I32Load8U(byte()),
        I::I32Const(b'.' as i32),
        I::I32Eq,
        I::LocalGet(alt),
        I::I32Eqz,
        I::I32And,
        I::If(BlockType::Empty)
    );
    incr(&mut f, p, -1);
    emit!(f, I::End);
    // e+XX, with at least two exponent digits.
    emit!(f, I::LocalGet(use_exp), I::If(BlockType::Empty));
    put(&mut f, p, |f| {
        emit!(
            f,
            I::I32Const(b'E' as i32),
            I::I32Const(b'e' as i32),
            I::LocalGet(upper),
            I::Select
        )
    });
    put(&mut f, p, |f| {
        emit!(
            f,
            I::I32Const(b'-' as i32),
            I::I32Const(b'+' as i32),
            I::LocalGet(expo),
            I::I32Const(0),
            I::I32LtS,
            I::Select
        )
    });
    emit!(
        f,
        I::LocalGet(expo),
        I::I32Const(0),
        I::I32LtS,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::LocalGet(expo),
        I::I32Sub,
        I::LocalSet(expo),
        I::End,
        I::LocalGet(expo),
        I::I32Const(100),
        I::I32GeS,
        I::If(BlockType::Empty)
    );
    put(&mut f, p, |f| {
        emit!(
            f,
            I::LocalGet(expo),
            I::I32Const(100),
            I::I32DivS,
            I::I32Const(b'0' as i32),
            I::I32Add
        )
    });
    emit!(f, I::End);
    put(&mut f, p, |f| {
        emit!(
            f,
            I::LocalGet(expo),
            I::I32Const(10),
            I::I32DivS,
            I::I32Const(10),
            I::I32RemS,
            I::I32Const(b'0' as i32),
            I::I32Add
        )
    });
    put(&mut f, p, |f| {
        emit!(
            f,
            I::LocalGet(expo),
            I::I32Const(10),
            I::I32RemS,
            I::I32Const(b'0' as i32),
            I::I32Add
        )
    });
    emit!(f, I::End);
    return_from_text(&mut f, ctx, p, len, out);
    emit!(f, I::I32Const(0), I::End);
    f
}

/// `repr(v)` of a float: the shortest digits, laid out as CPython's
/// `float_repr` lays them out (fixed notation from 1e-4 up to 1e16,
/// exponent notation outside it, always a `.0` or an exponent).
pub(crate) fn build_float_repr(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let to_string = ctx.helper(Helper::FloatToString);
    let mut f = function(&[I32, I32, I32, I32]);
    let (v, body, out, len, p) = (0, 1, 2, 3, 4);
    emit!(
        f,
        I::LocalGet(v),
        I::F64Abs,
        I::I32Const(b'r' as i32),
        I::I32Const(0),
        I::I32Const(FLOAT_ADD_DOT_0),
        I::Call(to_string),
        I::LocalSet(body),
        // A negative value (NaN included, whose sign CPython drops).
        I::LocalGet(v),
        I::I64ReinterpretF64,
        I::I64Const(0),
        I::I64LtS,
        I::LocalGet(v),
        I::LocalGet(v),
        I::F64Eq,
        I::I32And,
        I::If(BlockType::Empty),
        I::I32Const(TEXT),
        I::LocalSet(p)
    );
    put_byte(&mut f, p, b'-');
    put_string(&mut f, p, body, len);
    return_from_text(&mut f, ctx, p, len, out);
    emit!(f, I::End, I::LocalGet(body), I::End);
    f
}

/// `a + b` for two strings.
pub(crate) fn build_concat(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let mut f = function(&[I32, I32, I32, I32]);
    let (a, b, out, len, p, piece) = (0, 1, 2, 3, 4, 5);
    let _ = piece;
    str_len(&mut f, a);
    str_len(&mut f, b);
    emit!(f, I::I32Add, I::LocalSet(len));
    alloc_string(&mut f, ctx, len, out);
    emit!(f, I::LocalGet(out), I::LocalSet(p));
    put_string(&mut f, p, a, len);
    put_string(&mut f, p, b, len);
    emit!(f, I::LocalGet(out), I::End);
    f
}

/// Parse the specification at `spec` into `SPEC`:
/// `[[fill]align][sign][z][#][0][width][grouping][.precision][type]`.
/// Pushes 0 for a specification CPython rejects outright.
pub(crate) fn build_parse_spec() -> Function {
    use ValType::I32;
    let mut f = function(&[I32; 5]);
    let (s, len, pos, ch, cl, acc) = (0, 1, 2, 3, 4, 5);
    let at = |f: &mut Function, index: u32| {
        emit!(
            f,
            I::LocalGet(s),
            I::LocalGet(index),
            I::I32Add,
            I::I32Load8U(byte())
        );
    };
    let is_align = |f: &mut Function, local: u32| {
        // '<' 60, '=' 61, '>' 62, '^' 94
        emit!(
            f,
            I::LocalGet(local),
            I::I32Const(60),
            I::I32GeU,
            I::LocalGet(local),
            I::I32Const(62),
            I::I32LeU,
            I::I32And,
            I::LocalGet(local),
            I::I32Const(94),
            I::I32Eq,
            I::I32Or
        );
    };
    for (field, value) in [
        (FILL, b' ' as i32),
        (FILL_LEN, 1),
        (FILL_SET, 0),
        (ALIGN, 0),
        (SIGN, 0),
        (ZNEG, 0),
        (ALT, 0),
        (ZERO, 0),
        (WIDTH, -1),
        (GROUP, 0),
        (PREC, -1),
        (TYPE, 0),
    ] {
        set_spec(&mut f, field, value);
    }
    str_len(&mut f, s);
    emit!(
        f,
        I::LocalTee(len),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(1),
        I::Return,
        I::End
    );
    // The fill is any one character, when an alignment follows it.
    emit!(f, I::I32Const(0), I::LocalSet(pos));
    at(&mut f, pos);
    emit!(
        f,
        I::LocalTee(ch),
        // its UTF-8 width
        I::I32Const(1),
        I::I32Const(2),
        I::I32Const(3),
        I::I32Const(4),
        I::LocalGet(ch),
        I::I32Const(0xf0),
        I::I32LtU,
        I::Select,
        I::LocalGet(ch),
        I::I32Const(0xe0),
        I::I32LtU,
        I::Select,
        I::LocalGet(ch),
        I::I32Const(0x80),
        I::I32LtU,
        I::Select,
        I::LocalSet(cl),
        I::Drop
    );
    emit!(
        f,
        I::LocalGet(cl),
        I::LocalGet(len),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    at(&mut f, cl);
    emit!(f, I::LocalSet(ch));
    is_align(&mut f, ch);
    emit!(f, I::If(BlockType::Empty));
    emit!(
        f,
        I::I32Const(SPEC + FILL),
        I::I32Const(0),
        I::I32Store(w32(0)),
        I::I32Const(SPEC + FILL),
        I::LocalGet(s),
        I::LocalGet(cl),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::I32Const(SPEC + FILL_LEN),
        I::LocalGet(cl),
        I::I32Store(w32(0))
    );
    set_spec(&mut f, FILL_SET, 1);
    emit!(
        f,
        I::I32Const(SPEC + ALIGN),
        I::LocalGet(ch),
        I::I32Store(w32(0)),
        I::LocalGet(cl),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(pos),
        I::End,
        I::End
    );
    // Or the alignment alone.
    emit!(f, I::LocalGet(pos), I::I32Eqz, I::If(BlockType::Empty));
    at(&mut f, pos);
    emit!(f, I::LocalSet(ch));
    is_align(&mut f, ch);
    emit!(
        f,
        I::If(BlockType::Empty),
        I::I32Const(SPEC + ALIGN),
        I::LocalGet(ch),
        I::I32Store(w32(0)),
        I::I32Const(1),
        I::LocalSet(pos),
        I::End,
        I::End
    );
    // One-character flags, each where it may appear.
    let flag = |f: &mut Function, test: &dyn Fn(&mut Function), field: i32, store_char: bool| {
        emit!(
            f,
            I::LocalGet(pos),
            I::LocalGet(len),
            I::I32LtU,
            I::If(BlockType::Empty)
        );
        at(f, pos);
        emit!(f, I::LocalSet(ch));
        test(f);
        emit!(f, I::If(BlockType::Empty), I::I32Const(SPEC + field));
        if store_char {
            emit!(f, I::LocalGet(ch));
        } else {
            emit!(f, I::I32Const(1));
        }
        emit!(f, I::I32Store(w32(0)));
        incr(f, pos, 1);
        emit!(f, I::End, I::End);
    };
    let one_of = |chars: &'static [u8]| {
        move |f: &mut Function| {
            emit!(f, I::I32Const(0));
            for &c in chars {
                emit!(f, I::LocalGet(3), I::I32Const(c as i32), I::I32Eq, I::I32Or);
            }
        }
    };
    flag(&mut f, &one_of(b"+- "), SIGN, true);
    flag(&mut f, &one_of(b"z"), ZNEG, false);
    flag(&mut f, &one_of(b"#"), ALT, false);
    // '0' is a flag only when no fill was given; then it fills with zeros.
    spec_field(&mut f, FILL_SET);
    emit!(f, I::I32Eqz, I::If(BlockType::Empty));
    flag(&mut f, &one_of(b"0"), ZERO, false);
    spec_field(&mut f, ZERO);
    emit!(f, I::If(BlockType::Empty));
    set_spec(&mut f, FILL, b'0' as i32);
    emit!(f, I::End, I::End);
    // Digits into `acc`, consuming them; ch says whether there were any.
    let number = |f: &mut Function| {
        emit!(
            f,
            I::I32Const(0),
            I::LocalSet(acc),
            I::I32Const(0),
            I::LocalSet(cl)
        );
        f.instruction(&I::Block(BlockType::Empty));
        f.instruction(&I::Loop(BlockType::Empty));
        emit!(f, I::LocalGet(pos), I::LocalGet(len), I::I32GeU, I::BrIf(1));
        at(f, pos);
        emit!(
            f,
            I::I32Const(b'0' as i32),
            I::I32Sub,
            I::LocalTee(ch),
            I::I32Const(9),
            I::I32GtU,
            I::BrIf(1),
            I::LocalGet(acc),
            I::I32Const(10),
            I::I32Mul,
            I::LocalGet(ch),
            I::I32Add,
            I::LocalSet(acc),
            I::I32Const(1),
            I::LocalSet(cl)
        );
        incr(f, pos, 1);
        emit!(f, I::Br(0), I::End, I::End);
    };
    number(&mut f);
    emit!(
        f,
        I::LocalGet(cl),
        I::If(BlockType::Empty),
        I::I32Const(SPEC + WIDTH),
        I::LocalGet(acc),
        I::I32Store(w32(0)),
        I::End
    );
    flag(&mut f, &one_of(b",_"), GROUP, true);
    // .precision, which needs a digit
    emit!(
        f,
        I::LocalGet(pos),
        I::LocalGet(len),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    at(&mut f, pos);
    emit!(
        f,
        I::I32Const(b'.' as i32),
        I::I32Eq,
        I::If(BlockType::Empty)
    );
    incr(&mut f, pos, 1);
    number(&mut f);
    emit!(
        f,
        I::LocalGet(cl),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::Return,
        I::End,
        I::I32Const(SPEC + PREC),
        I::LocalGet(acc),
        I::I32Store(w32(0)),
        I::End,
        I::End
    );
    // The type, then nothing.
    emit!(
        f,
        I::LocalGet(pos),
        I::LocalGet(len),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    emit!(f, I::I32Const(SPEC + TYPE));
    at(&mut f, pos);
    emit!(f, I::I32Store(w32(0)));
    incr(&mut f, pos, 1);
    emit!(f, I::End);
    emit!(f, I::LocalGet(pos), I::LocalGet(len), I::I32Eq, I::End);
    f
}

/// Fill `prefix` + `body` to the parsed width: `<` fills after, `>` before,
/// `^` both sides (the extra one after), `=` between the prefix (a sign, a
/// base prefix) and the body. The parsed alignment wins over
/// `default_align`. A `prefix` of 0 is none. Widths count code points.
pub(crate) fn build_pad(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let mut f = function(&[I32; 12]);
    let (prefix, body, dalign) = (0, 1, 2);
    let (w, total, pad, align, left, right, out, p, len, i, n, cp) =
        (3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14);
    spec_field(&mut f, WIDTH);
    emit!(f, I::LocalSet(w));
    code_points(&mut f, body, i, n, total);
    emit!(f, I::LocalGet(prefix), I::If(BlockType::Empty));
    code_points(&mut f, prefix, i, n, cp);
    emit!(
        f,
        I::LocalGet(total),
        I::LocalGet(cp),
        I::I32Add,
        I::LocalSet(total),
        I::End
    );
    // Nothing to fill: the prefix and the body as they are.
    emit!(
        f,
        I::LocalGet(w),
        I::LocalGet(total),
        I::I32LeS,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(prefix),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::LocalGet(body),
        I::Return,
        I::End
    );
    emit!(
        f,
        I::LocalGet(prefix),
        I::LocalGet(body),
        I::Call(ctx.helper(Helper::Concat)),
        I::Return,
        I::End
    );
    emit!(
        f,
        I::LocalGet(w),
        I::LocalGet(total),
        I::I32Sub,
        I::LocalSet(pad)
    );
    spec_field(&mut f, ALIGN);
    emit!(
        f,
        I::LocalTee(align),
        I::LocalGet(dalign),
        I::LocalGet(align),
        I::Select,
        I::LocalSet(align),
        I::I32Const(0),
        I::LocalSet(left),
        I::I32Const(0),
        I::LocalSet(right)
    );
    let align_is = |f: &mut Function, c: u8| {
        emit!(f, I::LocalGet(6), I::I32Const(c as i32), I::I32Eq);
    };
    align_is(&mut f, b'<');
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(pad),
        I::LocalSet(right),
        I::End
    );
    align_is(&mut f, b'>');
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(pad),
        I::LocalSet(left),
        I::End
    );
    align_is(&mut f, b'^');
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(pad),
        I::I32Const(1),
        I::I32ShrU,
        I::LocalTee(left),
        I::LocalGet(pad),
        I::LocalGet(left),
        I::I32Sub,
        I::LocalSet(right),
        I::Drop,
        I::End
    );
    // The fill bytes: one fill character per unit of pad.
    let fill = |f: &mut Function, count: u32| {
        while_loop(
            f,
            |f| emit!(f, I::LocalGet(count), I::I32Const(0), I::I32GtS),
            |f| {
                emit!(
                    f,
                    I::LocalGet(10),
                    I::I32Const(SPEC + FILL),
                    I::I32Const(SPEC + FILL_LEN),
                    I::I32Load(w32(0)),
                    I::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0
                    },
                    I::LocalGet(10),
                    I::I32Const(SPEC + FILL_LEN),
                    I::I32Load(w32(0)),
                    I::I32Add,
                    I::LocalSet(10)
                );
                incr(f, count, -1);
            },
        );
    };
    emit!(f, I::I32Const(TEXT), I::LocalSet(p));
    align_is(&mut f, b'=');
    emit!(f, I::If(BlockType::Empty));
    emit!(f, I::LocalGet(prefix), I::If(BlockType::Empty));
    put_string(&mut f, p, prefix, len);
    emit!(f, I::End);
    fill(&mut f, pad);
    put_string(&mut f, p, body, len);
    emit!(f, I::Else);
    fill(&mut f, left);
    emit!(f, I::LocalGet(prefix), I::If(BlockType::Empty));
    put_string(&mut f, p, prefix, len);
    emit!(f, I::End);
    put_string(&mut f, p, body, len);
    fill(&mut f, right);
    emit!(f, I::End);
    return_from_text(&mut f, ctx, p, len, out);
    emit!(f, I::I32Const(0), I::End);
    f
}

/// Group the decimal (or other) digits `digits` with `sep` every `size`
/// digits from the right, first padding with zeros until the grouped text is
/// at least `min_width` long (zero fill counts its own separators, so
/// `f"{1234:08,}"` is `0,001,234` as in CPython).
pub(crate) fn build_group(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let mut f = function(&[I32; 6]);
    let (digits, size) = (0, 2);
    let (n, m, i, p, len, out) = (4, 5, 6, 7, 8, 9);
    str_len(&mut f, digits);
    emit!(f, I::LocalTee(n), I::LocalSet(m));
    // grow m until m + (m - 1) / size >= min_width
    while_loop(
        &mut f,
        |f| {
            emit!(
                f,
                I::LocalGet(5),
                I::LocalGet(5),
                I::I32Const(1),
                I::I32Sub,
                I::LocalGet(2),
                I::I32DivS,
                I::I32Add,
                I::LocalGet(3),
                I::I32LtS
            )
        },
        |f| incr(f, m, 1),
    );
    emit!(
        f,
        I::I32Const(TEXT),
        I::LocalSet(p),
        I::I32Const(0),
        I::LocalSet(i)
    );
    while_loop(
        &mut f,
        |f| emit!(f, I::LocalGet(6), I::LocalGet(5), I::I32LtS),
        |f| {
            // a separator before each group but the first
            emit!(
                f,
                I::LocalGet(i),
                I::I32Const(0),
                I::I32GtS,
                I::LocalGet(m),
                I::LocalGet(i),
                I::I32Sub,
                I::LocalGet(size),
                I::I32RemS,
                I::I32Eqz,
                I::I32And,
                I::If(BlockType::Empty)
            );
            put(f, p, |f| emit!(f, I::LocalGet(1)));
            emit!(f, I::End);
            put(f, p, |f| {
                // a leading zero, or the digit
                emit!(
                    f,
                    I::I32Const(b'0' as i32),
                    I::LocalGet(0),
                    I::LocalGet(6),
                    I::LocalGet(5),
                    I::LocalGet(4),
                    I::I32Sub,
                    I::I32Sub,
                    I::I32Add,
                    I::I32Load8U(byte()),
                    I::LocalGet(6),
                    I::LocalGet(5),
                    I::LocalGet(4),
                    I::I32Sub,
                    I::I32LtS,
                    I::Select
                )
            });
            incr(f, i, 1);
        },
    );
    return_from_text(&mut f, ctx, p, len, out);
    emit!(f, I::I32Const(0), I::End);
    f
}

/// An int under the parsed specification (CPython's `format_long_internal`).
pub(crate) fn build_format_int(ctx: &CompilationContext) -> Function {
    use ValType::{I32, I64};
    let mut f = function(&[
        I32, I32, I32, I32, I32, I32, I64, I32, I32, I32, I32, I32, I32,
    ]);
    let v = 0;
    let (neg, ty, base, prefix, digits, p, a, len, out, sep, size, dalign, i) =
        (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13);
    spec_field(&mut f, TYPE);
    emit!(
        f,
        I::LocalTee(ty),
        I::I32Const(b'd' as i32),
        I::LocalGet(ty),
        I::Select,
        I::LocalSet(ty)
    );
    // A float presentation type formats the int as a float.
    let float_type = |f: &mut Function| {
        emit!(f, I::I32Const(0));
        for c in b"eEfFgG%" {
            emit!(
                f,
                I::LocalGet(2),
                I::I32Const(*c as i32),
                I::I32Eq,
                I::I32Or
            );
        }
    };
    float_type(&mut f);
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(v),
        I::F64ConvertI32S,
        I::Call(ctx.helper(Helper::FormatFloat)),
        I::Return,
        I::End
    );
    // No precision for an int.
    spec_field(&mut f, PREC);
    emit!(
        f,
        I::I32Const(0),
        I::I32GeS,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End
    );
    emit!(
        f,
        I::LocalGet(v),
        I::I32Const(0),
        I::I32LtS,
        I::LocalSet(neg)
    );
    // 'c': the character itself, with no sign or '#'.
    emit!(
        f,
        I::LocalGet(ty),
        I::I32Const(b'c' as i32),
        I::I32Eq,
        I::If(BlockType::Empty)
    );
    spec_field(&mut f, SIGN);
    spec_field(&mut f, ALT);
    emit!(
        f,
        I::I32Or,
        I::LocalGet(v),
        I::I32Const(0),
        I::I32LtS,
        I::I32Or,
        I::LocalGet(v),
        I::I32Const(0x10ffff),
        I::I32GtS,
        I::I32Or,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::I32Const(TEXT),
        I::LocalSet(p)
    );
    crate::compiler::format::put_code_point(&mut f, p, v);
    emit!(
        f,
        I::LocalGet(p),
        I::I32Const(TEXT),
        I::I32Sub,
        I::LocalSet(len)
    );
    alloc_string(&mut f, ctx, len, out);
    emit!(
        f,
        I::LocalGet(out),
        I::I32Const(TEXT),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::I32Const(0),
        I::LocalGet(out),
        I::I32Const(b'>' as i32),
        I::Call(ctx.helper(Helper::Pad)),
        I::Return,
        I::End
    );
    // The base.
    emit!(f, I::I32Const(0), I::LocalSet(base));
    for (c, b) in [
        (b'd', 10),
        (b'n', 10),
        (b'b', 2),
        (b'o', 8),
        (b'x', 16),
        (b'X', 16),
    ] {
        emit!(
            f,
            I::LocalGet(ty),
            I::I32Const(c as i32),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::I32Const(b),
            I::LocalSet(base),
            I::End
        );
    }
    emit!(
        f,
        I::LocalGet(base),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End
    );
    // The digits of |v|, in reverse at the end of TEXT, then turned around.
    emit!(
        f,
        I::LocalGet(v),
        I::I64ExtendI32S,
        I::LocalSet(a),
        I::I64Const(0),
        I::LocalGet(a),
        I::I64Sub,
        I::LocalGet(a),
        I::LocalGet(a),
        I::I64Const(0),
        I::I64LtS,
        I::Select,
        I::LocalSet(a),
        I::I32Const(TEXT),
        I::LocalSet(p)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    put(&mut f, p, |f| {
        emit!(
            f,
            I::LocalGet(7),
            I::LocalGet(3),
            I::I64ExtendI32U,
            I::I64RemU,
            I::I32WrapI64,
            I::LocalTee(13),
            I::I32Const(b'0' as i32),
            I::I32Add,
            I::LocalGet(13),
            I::I32Const(b'A' as i32 - 10),
            I::I32Const(b'a' as i32 - 10),
            I::LocalGet(2),
            I::I32Const(b'X' as i32),
            I::I32Eq,
            I::Select,
            I::I32Add,
            I::LocalGet(13),
            I::I32Const(10),
            I::I32LtU,
            I::Select
        )
    });
    emit!(
        f,
        I::LocalGet(a),
        I::LocalGet(base),
        I::I64ExtendI32U,
        I::I64DivU,
        I::LocalTee(a),
        I::I64Eqz,
        I::BrIf(1),
        I::Br(0),
        I::End,
        I::End
    );
    emit!(
        f,
        I::LocalGet(p),
        I::I32Const(TEXT),
        I::I32Sub,
        I::LocalSet(len)
    );
    alloc_string(&mut f, ctx, len, digits);
    emit!(f, I::I32Const(0), I::LocalSet(i));
    while_loop(
        &mut f,
        |f| emit!(f, I::LocalGet(13), I::LocalGet(8), I::I32LtS),
        |f| {
            emit!(
                f,
                I::LocalGet(5),
                I::LocalGet(13),
                I::I32Add,
                I::I32Const(TEXT - 1),
                I::LocalGet(8),
                I::I32Add,
                I::LocalGet(13),
                I::I32Sub,
                I::I32Load8U(byte()),
                I::I32Store8(byte())
            );
            incr(f, i, 1);
        },
    );
    // Grouping: ',' only in decimal, '_' by threes in decimal and fours in
    // the other bases; neither with 'n'.
    spec_field(&mut f, GROUP);
    emit!(
        f,
        I::LocalTee(sep),
        I::If(BlockType::Empty),
        I::LocalGet(ty),
        I::I32Const(b'n' as i32),
        I::I32Eq,
        I::LocalGet(sep),
        I::I32Const(b',' as i32),
        I::I32Eq,
        I::LocalGet(base),
        I::I32Const(10),
        I::I32Ne,
        I::I32And,
        I::I32Or,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::End,
        I::I32Const(3),
        I::I32Const(4),
        I::LocalGet(base),
        I::I32Const(10),
        I::I32Eq,
        I::Select,
        I::LocalSet(size)
    );
    // The prefix: the sign, then 0b/0o/0x/0X under '#'.
    emit!(f, I::I32Const(TEXT), I::LocalSet(p));
    spec_field(&mut f, SIGN);
    emit!(
        f,
        I::LocalSet(i),
        I::LocalGet(neg),
        I::If(BlockType::Empty),
        I::I32Const(b'-' as i32),
        I::LocalSet(i),
        I::Else,
        I::LocalGet(i),
        I::I32Const(b'-' as i32),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::LocalSet(i),
        I::End,
        I::End,
        I::LocalGet(i),
        I::If(BlockType::Empty)
    );
    put(&mut f, p, |f| emit!(f, I::LocalGet(13)));
    emit!(f, I::End);
    spec_field(&mut f, ALT);
    emit!(
        f,
        I::LocalGet(base),
        I::I32Const(10),
        I::I32Ne,
        I::I32And,
        I::If(BlockType::Empty)
    );
    put_byte(&mut f, p, b'0');
    put(&mut f, p, |f| {
        // base 2 ? 'b' : base 8 ? 'o' : type 'X' ? 'X' : 'x'
        emit!(
            f,
            I::I32Const(b'b' as i32),
            I::I32Const(b'o' as i32),
            I::I32Const(b'X' as i32),
            I::I32Const(b'x' as i32),
            I::LocalGet(2),
            I::I32Const(b'X' as i32),
            I::I32Eq,
            I::Select,
            I::LocalGet(3),
            I::I32Const(8),
            I::I32Eq,
            I::Select,
            I::LocalGet(3),
            I::I32Const(2),
            I::I32Eq,
            I::Select
        )
    });
    emit!(f, I::End);
    emit!(
        f,
        I::LocalGet(p),
        I::I32Const(TEXT),
        I::I32Sub,
        I::LocalSet(len)
    );
    emit!(
        f,
        I::I32Const(0),
        I::LocalSet(prefix),
        I::LocalGet(len),
        I::If(BlockType::Empty)
    );
    alloc_string(&mut f, ctx, len, prefix);
    emit!(
        f,
        I::LocalGet(prefix),
        I::I32Const(TEXT),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::End
    );
    emit_number_finish(&mut f, ctx, prefix, digits, sep, size, dalign, len);
    emit!(f, I::End);
    f
}

/// The alignment a number takes by default: `=` under the `0` flag with no
/// alignment given, else `>`.
fn number_default_align(f: &mut Function, dalign: u32) {
    emit!(f, I::I32Const(b'=' as i32), I::I32Const(b'>' as i32));
    spec_field(f, ZERO);
    spec_field(f, ALIGN);
    emit!(f, I::I32Eqz, I::I32And, I::Select, I::LocalSet(dalign));
}

/// Push whether a number is zero-filled between its sign and digits: `=`
/// alignment (given, or by default in `dalign`) with a `0` fill.
fn zero_filled(f: &mut Function, dalign: u32) {
    spec_field(f, ALIGN);
    emit!(f, I::LocalGet(dalign));
    spec_field(f, ALIGN);
    emit!(f, I::Select, I::I32Const(b'=' as i32), I::I32Eq);
    spec_field(f, FILL);
    emit!(f, I::I32Const(b'0' as i32), I::I32Eq, I::I32And);
}

/// Group a number's digits in `body` (zero-filled to the width less the
/// prefix under `0` fill, as CPython counts the separators it adds) and pad
/// it behind its `prefix`, pushing the result.
#[allow(clippy::too_many_arguments)]
fn emit_number_finish(
    f: &mut Function,
    ctx: &CompilationContext,
    prefix: u32,
    body: u32,
    sep: u32,
    size: u32,
    dalign: u32,
    len: u32,
) {
    number_default_align(f, dalign);
    emit!(
        f,
        I::LocalGet(sep),
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::LocalSet(len)
    );
    zero_filled(f, dalign);
    emit!(f, I::If(BlockType::Empty));
    spec_field(f, WIDTH);
    emit!(
        f,
        I::LocalSet(len),
        I::LocalGet(prefix),
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(len),
        I::LocalGet(prefix),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::I32Sub,
        I::LocalSet(len),
        I::End,
        I::End,
        I::LocalGet(body),
        I::LocalGet(sep),
        I::LocalGet(size),
        I::LocalGet(len),
        I::Call(ctx.helper(Helper::Group)),
        I::LocalSet(body),
        I::End,
        I::LocalGet(prefix),
        I::LocalGet(body),
        I::LocalGet(dalign),
        I::Call(ctx.helper(Helper::Pad))
    );
}

/// Write the code point in `cp` (a local) as UTF-8 at the cursor `p`.
pub(crate) fn put_code_point(f: &mut Function, p: u32, cp: u32) {
    let byte_of = |f: &mut Function, shift: i32, mask: i32, lead: i32| {
        put(f, p, |f| {
            emit!(
                f,
                I::LocalGet(cp),
                I::I32Const(shift),
                I::I32ShrU,
                I::I32Const(mask),
                I::I32And,
                I::I32Const(lead),
                I::I32Or
            )
        });
    };
    emit!(
        f,
        I::LocalGet(cp),
        I::I32Const(0x80),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    put(f, p, |f| emit!(f, I::LocalGet(cp)));
    emit!(
        f,
        I::Else,
        I::LocalGet(cp),
        I::I32Const(0x800),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    byte_of(f, 6, 0x1f, 0xc0);
    byte_of(f, 0, 0x3f, 0x80);
    emit!(
        f,
        I::Else,
        I::LocalGet(cp),
        I::I32Const(0x10000),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    byte_of(f, 12, 0x0f, 0xe0);
    byte_of(f, 6, 0x3f, 0x80);
    byte_of(f, 0, 0x3f, 0x80);
    emit!(f, I::Else);
    byte_of(f, 18, 0x07, 0xf0);
    byte_of(f, 12, 0x3f, 0x80);
    byte_of(f, 6, 0x3f, 0x80);
    byte_of(f, 0, 0x3f, 0x80);
    emit!(f, I::End, I::End, I::End);
}

/// A float under the parsed specification (CPython's
/// `format_float_internal`).
pub(crate) fn build_format_float(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let mut f = function(&[I32; 14]);
    let v = 0;
    let (ty, prec, flags, neg, body, prefix, len, pct, i, intlen, sep, dalign, out, p) =
        (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14);
    spec_field(&mut f, TYPE);
    emit!(f, I::LocalSet(ty));
    // No integer or string presentation for a float.
    emit!(f, I::I32Const(0));
    for c in b"bcdoxXs" {
        emit!(
            f,
            I::LocalGet(ty),
            I::I32Const(*c as i32),
            I::I32Eq,
            I::I32Or
        );
    }
    emit!(f, I::If(BlockType::Empty), I::Unreachable, I::End);
    // 'n' takes no grouping.
    spec_field(&mut f, GROUP);
    emit!(
        f,
        I::I32Const(0),
        I::I32Ne,
        I::LocalGet(ty),
        I::I32Const(b'n' as i32),
        I::I32Eq,
        I::I32And,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End
    );
    emit!(f, I::I32Const(FLOAT_ALT), I::I32Const(0));
    spec_field(&mut f, ALT);
    emit!(f, I::Select, I::LocalSet(flags));
    spec_field(&mut f, PREC);
    emit!(f, I::LocalSet(prec), I::I32Const(0), I::LocalSet(pct));
    // No type: repr's digits, or 'g' given a precision, keeping a '.0'.
    emit!(f, I::LocalGet(ty), I::I32Eqz, I::If(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(flags),
        I::I32Const(FLOAT_ADD_DOT_0),
        I::I32Or,
        I::LocalSet(flags),
        I::I32Const(b'g' as i32),
        I::I32Const(b'r' as i32),
        I::LocalGet(prec),
        I::I32Const(0),
        I::I32GeS,
        I::Select,
        I::LocalSet(ty),
        I::End
    );
    emit!(
        f,
        I::LocalGet(ty),
        I::I32Const(b'n' as i32),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::I32Const(b'g' as i32),
        I::LocalSet(ty),
        I::End,
        I::LocalGet(ty),
        I::I32Const(b'%' as i32),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::I32Const(b'f' as i32),
        I::LocalSet(ty),
        I::LocalGet(v),
        I::F64Const(100.0_f64.into()),
        I::F64Mul,
        I::LocalSet(v),
        I::I32Const(1),
        I::LocalSet(pct),
        I::End
    );
    // Upper case, then the lower-case letter.
    emit!(f, I::I32Const(0));
    for c in b"EFG" {
        emit!(
            f,
            I::LocalGet(ty),
            I::I32Const(*c as i32),
            I::I32Eq,
            I::I32Or
        );
    }
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(flags),
        I::I32Const(FLOAT_UPPER),
        I::I32Or,
        I::LocalSet(flags),
        I::LocalGet(ty),
        I::I32Const(32),
        I::I32Or,
        I::LocalSet(ty),
        I::End
    );
    emit!(f, I::I32Const(0));
    for c in b"efgr" {
        emit!(
            f,
            I::LocalGet(ty),
            I::I32Const(*c as i32),
            I::I32Eq,
            I::I32Or
        );
    }
    emit!(
        f,
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End
    );
    emit!(
        f,
        I::LocalGet(prec),
        I::I32Const(6),
        I::LocalGet(prec),
        I::I32Const(0),
        I::I32GeS,
        I::Select,
        I::LocalSet(prec),
        // negative, NaN aside
        I::LocalGet(v),
        I::I64ReinterpretF64,
        I::I64Const(0),
        I::I64LtS,
        I::LocalGet(v),
        I::LocalGet(v),
        I::F64Eq,
        I::I32And,
        I::LocalSet(neg),
        I::LocalGet(v),
        I::F64Abs,
        I::LocalGet(ty),
        I::LocalGet(prec),
        I::LocalGet(flags),
        I::Call(ctx.helper(Helper::FloatToString)),
        I::LocalSet(body)
    );
    // The integer digits at the front, for grouping; and 'z' drops the sign
    // of a value that rounded to zero.
    emit!(
        f,
        I::I32Const(0),
        I::LocalSet(intlen),
        I::I32Const(1),
        I::LocalSet(i)
    );
    str_len(&mut f, body);
    emit!(f, I::LocalSet(len), I::I32Const(0), I::LocalSet(p));
    while_loop(
        &mut f,
        |f| {
            emit!(f, I::LocalGet(14), I::LocalGet(7), I::I32LtS);
        },
        |f| {
            emit!(
                f,
                I::LocalGet(5),
                I::LocalGet(14),
                I::I32Add,
                I::I32Load8U(byte()),
                I::LocalTee(13),
                I::I32Const(b'e' as i32),
                I::I32Eq,
                I::LocalGet(13),
                I::I32Const(b'E' as i32),
                I::I32Eq,
                I::I32Or,
                I::BrIf(1),
                // anything but a zero or the point before the exponent:
                // not zero (an infinity and a NaN included)
                I::LocalGet(13),
                I::I32Const(b'0' as i32),
                I::I32Ne,
                I::LocalGet(13),
                I::I32Const(b'.' as i32),
                I::I32Ne,
                I::I32And,
                I::If(BlockType::Empty),
                I::I32Const(0),
                I::LocalSet(9),
                I::End,
                I::LocalGet(10),
                I::LocalGet(14),
                I::I32Eq,
                I::LocalGet(13),
                I::I32Const(b'0' as i32),
                I::I32GeU,
                I::LocalGet(13),
                I::I32Const(b'9' as i32),
                I::I32LeU,
                I::I32And,
                I::I32And,
                I::If(BlockType::Empty),
                I::LocalGet(10),
                I::I32Const(1),
                I::I32Add,
                I::LocalSet(10),
                I::End
            );
            incr(f, 14, 1);
        },
    );
    spec_field(&mut f, ZNEG);
    emit!(
        f,
        I::LocalGet(i),
        I::I32And,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::LocalSet(neg),
        I::End
    );
    sign_text(&mut f, ctx, neg, prefix, len);
    // Group the integer digits (zero-filled to the width less the prefix,
    // the fraction, and the '%' under `0` fill), then put the rest back.
    number_default_align(&mut f, dalign);
    spec_field(&mut f, GROUP);
    emit!(f, I::LocalSet(sep));
    emit!(
        f,
        I::LocalGet(sep),
        I::I32Const(0),
        I::I32Ne,
        I::LocalGet(intlen),
        I::I32Const(0),
        I::I32Ne,
        I::I32And,
        I::LocalGet(pct),
        I::I32Or,
        I::If(BlockType::Empty)
    );
    emit!(f, I::I32Const(TEXT), I::LocalSet(p));
    emit!(
        f,
        I::LocalGet(sep),
        I::I32Const(0),
        I::I32Ne,
        I::LocalGet(intlen),
        I::I32Const(0),
        I::I32Ne,
        I::I32And,
        I::If(BlockType::Empty)
    );
    // min width in `i`
    emit!(f, I::I32Const(0), I::LocalSet(i));
    zero_filled(&mut f, dalign);
    emit!(f, I::If(BlockType::Empty));
    spec_field(&mut f, WIDTH);
    str_len(&mut f, body);
    emit!(
        f,
        I::I32Sub,
        I::LocalGet(intlen),
        I::I32Add,
        I::LocalGet(pct),
        I::I32Sub,
        I::LocalSet(i)
    );
    emit!(f, I::LocalGet(prefix), I::If(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(prefix),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::I32Sub,
        I::LocalSet(i),
        I::End,
        I::End
    );
    emit!(f, I::LocalGet(intlen), I::LocalSet(len));
    alloc_string(&mut f, ctx, len, out);
    emit!(
        f,
        I::LocalGet(out),
        I::LocalGet(body),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(out),
        I::LocalGet(sep),
        I::I32Const(3),
        I::LocalGet(i),
        I::Call(ctx.helper(Helper::Group)),
        I::LocalSet(out)
    );
    put_string(&mut f, p, out, len);
    emit!(f, I::Else, I::I32Const(0), I::LocalSet(intlen), I::End);
    // the rest of the body
    str_len(&mut f, body);
    emit!(
        f,
        I::LocalGet(intlen),
        I::I32Sub,
        I::LocalSet(len),
        I::LocalGet(p),
        I::LocalGet(body),
        I::LocalGet(intlen),
        I::I32Add,
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(p),
        I::LocalGet(len),
        I::I32Add,
        I::LocalSet(p),
        I::LocalGet(pct),
        I::If(BlockType::Empty)
    );
    put_byte(&mut f, p, b'%');
    emit!(f, I::End);
    emit!(
        f,
        I::LocalGet(p),
        I::I32Const(TEXT),
        I::I32Sub,
        I::LocalSet(len)
    );
    alloc_string(&mut f, ctx, len, body);
    emit!(
        f,
        I::LocalGet(body),
        I::I32Const(TEXT),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::End
    );
    emit!(
        f,
        I::LocalGet(prefix),
        I::LocalGet(body),
        I::LocalGet(dalign),
        I::Call(ctx.helper(Helper::Pad)),
        I::End
    );
    f
}

/// The sign string of a number into `out` (0 for none), from `neg` and the
/// parsed sign option.
fn sign_text(f: &mut Function, ctx: &CompilationContext, neg: u32, out: u32, len: u32) {
    // ch = neg ? '-' : (sign == '+' || sign == ' ') ? sign : 0
    spec_field(f, SIGN);
    emit!(
        f,
        I::LocalTee(len),
        I::I32Const(0),
        I::LocalGet(len),
        I::I32Const(b'-' as i32),
        I::I32Ne,
        I::Select,
        I::LocalSet(len),
        I::LocalGet(neg),
        I::If(BlockType::Empty),
        I::I32Const(b'-' as i32),
        I::LocalSet(len),
        I::End,
        I::I32Const(0),
        I::LocalSet(out),
        I::LocalGet(len),
        I::If(BlockType::Empty),
        I::I32Const(6),
        I::Call(ctx.alloc_func_index),
        I::LocalTee(out),
        I::I32Const(1),
        I::I32Store(w32(0)),
        I::LocalGet(out),
        I::LocalGet(len),
        I::I32Store8(crate::compiler::runtime::mem(4, 0)),
        I::LocalGet(out),
        I::I32Const(4),
        I::I32Add,
        I::LocalSet(out),
        I::End
    );
}

/// A string under the parsed specification: 's' or no type, no sign, `#`,
/// `=`, or grouping; a precision truncates to that many characters; left
/// aligned by default.
pub(crate) fn build_format_str(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let mut f = function(&[I32; 5]);
    let s = 0;
    let (n, i, cnt, out, len) = (1, 2, 3, 4, 5);
    spec_field(&mut f, TYPE);
    emit!(
        f,
        I::LocalTee(n),
        I::I32Const(0),
        I::I32Ne,
        I::LocalGet(n),
        I::I32Const(b's' as i32),
        I::I32Ne,
        I::I32And
    );
    spec_field(&mut f, SIGN);
    emit!(f, I::I32Or);
    spec_field(&mut f, ALT);
    emit!(f, I::I32Or);
    spec_field(&mut f, GROUP);
    emit!(f, I::I32Or);
    spec_field(&mut f, ALIGN);
    emit!(
        f,
        I::I32Const(b'=' as i32),
        I::I32Eq,
        I::I32Or,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End
    );
    spec_field(&mut f, PREC);
    emit!(
        f,
        I::LocalTee(cnt),
        I::I32Const(0),
        I::I32GeS,
        I::If(BlockType::Empty)
    );
    // the byte offset just past `cnt` code points
    str_len(&mut f, s);
    emit!(f, I::LocalSet(n), I::I32Const(0), I::LocalSet(i));
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(i), I::LocalGet(n), I::I32GeU, I::BrIf(1));
    emit!(
        f,
        I::LocalGet(s),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(0xc0),
        I::I32And,
        I::I32Const(0x80),
        I::I32Ne,
        I::If(BlockType::Empty),
        I::LocalGet(cnt),
        I::I32Eqz,
        I::BrIf(2),
        I::LocalGet(cnt),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(cnt),
        I::End
    );
    incr(&mut f, i, 1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(f, I::LocalGet(i), I::LocalSet(len));
    alloc_string(&mut f, ctx, len, out);
    emit!(
        f,
        I::LocalGet(out),
        I::LocalGet(s),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::I32Const(0),
        I::LocalGet(out),
        I::I32Const(b'<' as i32),
        I::Call(ctx.helper(Helper::Pad)),
        I::Return,
        I::End
    );
    emit!(
        f,
        I::I32Const(0),
        I::LocalGet(s),
        I::I32Const(b'<' as i32),
        I::Call(ctx.helper(Helper::Pad)),
        I::End
    );
    f
}

/// `format(v, spec)` for a value of `ty`.
pub(crate) fn build_format(ctx: &CompilationContext, ty: &IRType) -> Function {
    let mut f = function(&[]);
    let (v, spec) = (0, 1);
    emit!(
        f,
        I::LocalGet(spec),
        I::Call(ctx.helper(Helper::ParseSpec)),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End
    );
    let empty_spec = |f: &mut Function| {
        str_len(f, 1);
        emit!(f, I::I32Eqz);
    };
    match ty {
        IRType::Float => emit!(f, I::LocalGet(v), I::Call(ctx.helper(Helper::FormatFloat))),
        IRType::String => emit!(f, I::LocalGet(v), I::Call(ctx.helper(Helper::FormatStr))),
        IRType::Int | IRType::Unknown | IRType::Any => {
            emit!(f, I::LocalGet(v), I::Call(ctx.helper(Helper::FormatInt)))
        }
        IRType::Bool => {
            empty_spec(&mut f);
            emit!(
                f,
                I::If(BlockType::Result(ValType::I32)),
                I::LocalGet(v),
                I::Call(ctx.helper(Helper::Str(IRType::Bool))),
                I::Else,
                I::LocalGet(v),
                I::Call(ctx.helper(Helper::FormatInt)),
                I::End
            );
        }
        other => {
            // Only the empty spec, which is str().
            empty_spec(&mut f);
            emit!(
                f,
                I::I32Eqz,
                I::If(BlockType::Empty),
                I::Unreachable,
                I::End,
                I::LocalGet(v),
                I::Call(ctx.helper(Helper::Str(other.clone())))
            );
        }
    }
    emit!(f, I::End);
    f
}

/// `ascii()`'s escaping of a string that is already a repr: every non-ASCII
/// character as `\xNN`, `\uNNNN`, or `\UNNNNNNNN`.
pub(crate) fn build_ascii_escape(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    let mut f = function(&[I32; 8]);
    let s = 0;
    let (n, i, b, cp, w, p, len, out) = (1, 2, 3, 4, 5, 6, 7, 8);
    str_len(&mut f, s);
    emit!(
        f,
        I::LocalSet(n),
        I::I32Const(0),
        I::LocalSet(i),
        I::I32Const(TEXT),
        I::LocalSet(p)
    );
    while_loop(
        &mut f,
        |f| emit!(f, I::LocalGet(2), I::LocalGet(1), I::I32LtU),
        |f| {
            emit!(
                f,
                I::LocalGet(s),
                I::LocalGet(i),
                I::I32Add,
                I::I32Load8U(byte()),
                I::LocalTee(b),
                I::I32Const(0x80),
                I::I32LtU,
                I::If(BlockType::Empty)
            );
            put(f, p, |f| emit!(f, I::LocalGet(3)));
            incr(f, i, 1);
            emit!(f, I::Else);
            // decode
            emit!(
                f,
                I::I32Const(2),
                I::I32Const(3),
                I::I32Const(4),
                I::LocalGet(b),
                I::I32Const(0xf0),
                I::I32LtU,
                I::Select,
                I::LocalGet(b),
                I::I32Const(0xe0),
                I::I32LtU,
                I::Select,
                I::LocalSet(w),
                I::LocalGet(b),
                // w == 2 ? 0x1f : w == 3 ? 0x0f : 0x07
                I::I32Const(0x1f),
                I::I32Const(0x0f),
                I::I32Const(0x07),
                I::LocalGet(w),
                I::I32Const(3),
                I::I32Eq,
                I::Select,
                I::LocalGet(w),
                I::I32Const(2),
                I::I32Eq,
                I::Select,
                I::I32And,
                I::LocalSet(cp)
            );
            for k in 1..4 {
                emit!(
                    f,
                    I::LocalGet(w),
                    I::I32Const(k),
                    I::I32GtS,
                    I::If(BlockType::Empty),
                    I::LocalGet(cp),
                    I::I32Const(6),
                    I::I32Shl,
                    I::LocalGet(s),
                    I::LocalGet(i),
                    I::I32Add,
                    I::I32Const(k),
                    I::I32Add,
                    I::I32Load8U(byte()),
                    I::I32Const(0x3f),
                    I::I32And,
                    I::I32Or,
                    I::LocalSet(cp),
                    I::End
                );
            }
            emit!(f, I::LocalGet(i), I::LocalGet(w), I::I32Add, I::LocalSet(i));
            put_byte(f, p, b'\\');
            // x (2 digits), u (4), U (8)
            emit!(
                f,
                I::LocalGet(cp),
                I::I32Const(0x100),
                I::I32LtU,
                I::If(BlockType::Empty)
            );
            put_byte(f, p, b'x');
            hex(f, p, cp, b, 2);
            emit!(
                f,
                I::Else,
                I::LocalGet(cp),
                I::I32Const(0x10000),
                I::I32LtU,
                I::If(BlockType::Empty)
            );
            put_byte(f, p, b'u');
            hex(f, p, cp, b, 4);
            emit!(f, I::Else);
            put_byte(f, p, b'U');
            hex(f, p, cp, b, 8);
            emit!(f, I::End, I::End, I::End);
        },
    );
    return_from_text(&mut f, ctx, p, len, out);
    emit!(f, I::I32Const(0), I::End);
    f
}

/// `digits` lowercase hex digits of `cp` at the cursor `p`, using `t`.
fn hex(f: &mut Function, p: u32, cp: u32, t: u32, digits: i32) {
    for k in (0..digits).rev() {
        put(f, p, |f| {
            emit!(
                f,
                I::LocalGet(cp),
                I::I32Const(k * 4),
                I::I32ShrU,
                I::I32Const(15),
                I::I32And,
                I::LocalTee(t),
                I::I32Const(b'0' as i32),
                I::I32Add,
                I::LocalGet(t),
                I::I32Const(b'a' as i32 - 10),
                I::I32Add,
                I::LocalGet(t),
                I::I32Const(10),
                I::I32LtU,
                I::Select
            )
        });
    }
}

/// Why `format(value, spec)` of a value of `ty` raises in CPython, with its
/// message, for a specification known when compiling. The checks mirror the
/// traps the runtime helpers take for a specification built at run time.
pub fn spec_error(spec: &str, ty: &IRType) -> Option<String> {
    let type_name = crate::compiler::operators::python_type_name(ty);
    let invalid = || format!("Invalid format specifier '{spec}' for object of type '{type_name}'");
    let chars: Vec<char> = spec.chars().collect();
    let mut pos = 0;
    let is_align = |c: char| matches!(c, '<' | '>' | '=' | '^');
    let mut align = None;
    let mut fill_set = false;
    if chars.len() >= 2 && is_align(chars[1]) {
        align = Some(chars[1]);
        fill_set = true;
        pos = 2;
    } else if !chars.is_empty() && is_align(chars[0]) {
        align = Some(chars[0]);
        pos = 1;
    }
    let mut sign = None;
    if pos < chars.len() && matches!(chars[pos], '+' | '-' | ' ') {
        sign = Some(chars[pos]);
        pos += 1;
    }
    let mut z = false;
    if pos < chars.len() && chars[pos] == 'z' {
        z = true;
        pos += 1;
    }
    let mut alt = false;
    if pos < chars.len() && chars[pos] == '#' {
        alt = true;
        pos += 1;
    }
    if !fill_set && pos < chars.len() && chars[pos] == '0' {
        pos += 1;
    }
    while pos < chars.len() && chars[pos].is_ascii_digit() {
        pos += 1;
    }
    let mut group = None;
    if pos < chars.len() && matches!(chars[pos], ',' | '_') {
        group = Some(chars[pos]);
        pos += 1;
    }
    let mut precision = false;
    if pos < chars.len() && chars[pos] == '.' {
        pos += 1;
        let start = pos;
        while pos < chars.len() && chars[pos].is_ascii_digit() {
            pos += 1;
        }
        if pos == start {
            return Some("Format specifier missing precision".to_string());
        }
        precision = true;
    }
    let ty_char = chars.get(pos).copied();
    if ty_char.is_some() {
        pos += 1;
    }
    if pos < chars.len() {
        return Some(invalid());
    }
    let unknown = |c: char| format!("Unknown format code '{c}' for object of type '{type_name}'");
    let grouping = |c: char| group.map(|g| format!("Cannot specify '{g}' with '{c}'."));
    match ty {
        IRType::String => {
            if let Some(c) = ty_char.filter(|c| *c != 's') {
                return Some(unknown(c));
            }
            if sign.is_some() {
                return Some("Sign not allowed in string format specifier".to_string());
            }
            if alt {
                return Some(
                    "Alternate form (#) not allowed in string format specifier".to_string(),
                );
            }
            if z {
                return Some(
                    "Negative zero coercion (z) not allowed in format specifier".to_string(),
                );
            }
            if align == Some('=') {
                return Some("'=' alignment not allowed in string format specifier".to_string());
            }
            grouping('s')
        }
        IRType::Int | IRType::Bool | IRType::Unknown | IRType::Any => {
            if spec.is_empty() {
                return None;
            }
            let c = ty_char.unwrap_or('d');
            if "eEfFgG%".contains(c) {
                return spec_error(spec, &IRType::Float);
            }
            if !"bcdoxXn".contains(c) {
                return Some(unknown(c));
            }
            if precision {
                return Some("Precision not allowed in integer format specifier".to_string());
            }
            if z {
                return Some(
                    "Negative zero coercion (z) not allowed in integer format specifier"
                        .to_string(),
                );
            }
            if c == 'c' {
                if sign.is_some() {
                    return Some("Sign not allowed with integer format specifier 'c'".to_string());
                }
                if alt {
                    return Some(
                        "Alternate form (#) not allowed with integer format specifier 'c'"
                            .to_string(),
                    );
                }
            }
            if c == 'n' || (group == Some(',') && c != 'd') {
                return grouping(c);
            }
            None
        }
        IRType::Float => {
            if let Some(c) = ty_char {
                if !"eEfFgGn%".contains(c) {
                    return Some(unknown(c));
                }
                if c == 'n' {
                    return grouping('n');
                }
            }
            None
        }
        _ if spec.is_empty() => None,
        _ => Some(format!(
            "unsupported format string passed to {type_name}.__format__"
        )),
    }
}
