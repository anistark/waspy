//! `float()` of a string, correctly rounded, as runtime helpers (see
//! `compiler::runtime`).
//!
//! The digits are read under CPython's grammar for `float()` (surrounding
//! whitespace, a sign, `inf`/`infinity`/`nan` in any case, `_` between
//! digits, an exponent). The value they spell is then rounded the way Gay's
//! `strtod` with `bigcomp` rounds it: a float approximation within a few
//! units in the last place, corrected one step at a time by comparing the
//! input's digits with the exact decimal expansion of the midpoint between
//! two neighbouring floats. A midpoint has at most 767 significant digits,
//! so digits past 30000 only matter as "something nonzero follows", which a
//! sticky digit keeps. A string CPython rejects raises `ValueError`: the
//! helper leaves it pending and answers 0.0, and the call site's check
//! unwinds.

use crate::compiler::context::CompilationContext;
use crate::compiler::runtime::{
    byte, function, w32, Helper, BIG_R, BIG_S, BIG_T, BIG_Z, TEXT, TEXT_BYTES,
};
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

/// The most digits kept; one more slot holds the sticky digit.
const CAP: i32 = TEXT_BYTES - 2;
const MAX_BITS: i64 = 0x7fef_ffff_ffff_ffff;

/// Whether CPython's `str.strip()` would remove the byte: the ASCII
/// whitespace and the four separators `\x1c`-`\x1f`.
fn is_space(f: &mut Function, b: u32) {
    emit!(
        f,
        I::LocalGet(b),
        I::I32Const(32),
        I::I32Eq,
        I::LocalGet(b),
        I::I32Const(9),
        I::I32Sub,
        I::I32Const(4),
        I::I32LeU,
        I::I32Or,
        I::LocalGet(b),
        I::I32Const(0x1c),
        I::I32Sub,
        I::I32Const(3),
        I::I32LeU,
        I::I32Or
    );
}

/// Push the byte at `s + at` (locals).
fn at(f: &mut Function, s: u32, at: u32) {
    emit!(
        f,
        I::LocalGet(s),
        I::LocalGet(at),
        I::I32Add,
        I::I32Load8U(byte())
    );
}

fn is_digit(f: &mut Function, b: u32) {
    emit!(
        f,
        I::LocalGet(b),
        I::I32Const(b'0' as i32),
        I::I32Sub,
        I::I32Const(9),
        I::I32LeU
    );
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

/// `(s) -> f64`: `float(s)`.
pub(crate) fn build_parse_float(ctx: &CompilationContext) -> Function {
    use ValType::{F64, I32, I64};
    let mid = ctx.helper(Helper::MidCmp);
    let mut locals = vec![I32; 18];
    locals.extend([I64, F64, F64, I64, I32]);
    let mut f = function(&locals);
    let s = 0;
    let (len, pos, end, neg, ch, n, k, seen_digit, seen_dot, prev_digit, sticky, expv, eneg) =
        (1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13);
    let (edigits, sig, m, p, t) = (14, 15, 16, 17, 18);
    let (d, x, y, bits, c) = (19, 20, 21, 22, 23);
    let _ = len;
    let failure = Fail::new(ctx);
    let fail = |f: &mut Function| value_error(f, failure);

    // Trim.
    emit!(
        f,
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalSet(end),
        I::I32Const(0),
        I::LocalSet(pos)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(pos), I::LocalGet(end), I::I32GeU, I::BrIf(1));
    at(&mut f, s, pos);
    emit!(f, I::LocalSet(ch));
    is_space(&mut f, ch);
    emit!(f, I::I32Eqz, I::BrIf(1));
    incr(&mut f, pos, 1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(end), I::LocalGet(pos), I::I32LeU, I::BrIf(1));
    emit!(
        f,
        I::LocalGet(s),
        I::LocalGet(end),
        I::I32Add,
        I::I32Const(1),
        I::I32Sub,
        I::I32Load8U(byte()),
        I::LocalSet(ch)
    );
    is_space(&mut f, ch);
    emit!(f, I::I32Eqz, I::BrIf(1));
    incr(&mut f, end, -1);
    emit!(f, I::Br(0), I::End, I::End);

    // The sign.
    emit!(
        f,
        I::LocalGet(pos),
        I::LocalGet(end),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    at(&mut f, s, pos);
    emit!(
        f,
        I::LocalTee(ch),
        I::I32Const(b'-' as i32),
        I::I32Eq,
        I::LocalGet(ch),
        I::I32Const(b'+' as i32),
        I::I32Eq,
        I::I32Or,
        I::If(BlockType::Empty),
        I::LocalGet(ch),
        I::I32Const(b'-' as i32),
        I::I32Eq,
        I::LocalSet(neg)
    );
    incr(&mut f, pos, 1);
    emit!(f, I::End, I::End);

    // inf, infinity, nan, in any case.
    let signed = |f: &mut Function, value: f64| {
        emit!(
            f,
            I::F64Const((-value).into()),
            I::F64Const(value.into()),
            I::LocalGet(neg),
            I::Select,
            I::Return
        );
    };
    for (word, value) in [
        ("inf", f64::INFINITY),
        ("infinity", f64::INFINITY),
        ("nan", f64::NAN),
    ] {
        emit!(
            f,
            I::LocalGet(end),
            I::LocalGet(pos),
            I::I32Sub,
            I::I32Const(word.len() as i32),
            I::I32Eq,
            I::If(BlockType::Empty),
            I::I32Const(1)
        );
        for (j, letter) in word.bytes().enumerate() {
            emit!(
                f,
                I::LocalGet(s),
                I::LocalGet(pos),
                I::I32Add,
                I::I32Load8U(crate::compiler::runtime::mem(j as u64, 0)),
                I::I32Const(0x20),
                I::I32Or,
                I::I32Const(letter as i32),
                I::I32Eq,
                I::I32And
            );
        }
        emit!(f, I::If(BlockType::Empty));
        signed(&mut f, value);
        emit!(f, I::End, I::End);
    }

    // The digits, before and after the point.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(pos), I::LocalGet(end), I::I32GeU, I::BrIf(1));
    at(&mut f, s, pos);
    emit!(f, I::LocalSet(ch));
    is_digit(&mut f, ch);
    emit!(f, I::If(BlockType::Empty));
    emit!(
        f,
        I::I32Const(1),
        I::LocalSet(seen_digit),
        I::I32Const(1),
        I::LocalSet(prev_digit)
    );
    // A leading zero only moves the point.
    emit!(
        f,
        I::LocalGet(sig),
        I::I32Eqz,
        I::LocalGet(ch),
        I::I32Const(b'0' as i32),
        I::I32Eq,
        I::I32And,
        I::If(BlockType::Empty),
        I::LocalGet(seen_dot),
        I::If(BlockType::Empty)
    );
    incr(&mut f, k, -1);
    emit!(f, I::End, I::Else, I::I32Const(1), I::LocalSet(sig));
    emit!(
        f,
        I::LocalGet(n),
        I::I32Const(CAP),
        I::I32LtS,
        I::If(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(n),
        I::I32Const(TEXT),
        I::I32Add,
        I::LocalGet(ch),
        I::I32Const(b'0' as i32),
        I::I32Sub,
        I::I32Store8(byte())
    );
    incr(&mut f, n, 1);
    emit!(
        f,
        I::Else,
        I::LocalGet(ch),
        I::I32Const(b'0' as i32),
        I::I32Ne,
        I::LocalGet(sticky),
        I::I32Or,
        I::LocalSet(sticky),
        I::End,
        I::LocalGet(seen_dot),
        I::I32Eqz,
        I::If(BlockType::Empty)
    );
    incr(&mut f, k, 1);
    emit!(f, I::End, I::End);
    incr(&mut f, pos, 1);
    emit!(f, I::Br(1), I::End);
    // `_` only between two digits.
    emit!(
        f,
        I::LocalGet(ch),
        I::I32Const(b'_' as i32),
        I::I32Eq,
        I::If(BlockType::Empty)
    );
    underscore(&mut f, s, pos, end, prev_digit, ch, failure);
    emit!(f, I::Br(1), I::End);
    emit!(
        f,
        I::LocalGet(ch),
        I::I32Const(b'.' as i32),
        I::I32Eq,
        I::If(BlockType::Empty)
    );
    emit!(f, I::LocalGet(seen_dot), I::If(BlockType::Empty));
    fail(&mut f);
    emit!(
        f,
        I::End,
        I::I32Const(1),
        I::LocalSet(seen_dot),
        I::I32Const(0),
        I::LocalSet(prev_digit)
    );
    incr(&mut f, pos, 1);
    emit!(f, I::Br(1), I::End);
    emit!(f, I::End, I::End);
    emit!(
        f,
        I::LocalGet(seen_digit),
        I::I32Eqz,
        I::If(BlockType::Empty)
    );
    fail(&mut f);
    emit!(f, I::End);

    // The exponent.
    emit!(
        f,
        I::LocalGet(pos),
        I::LocalGet(end),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    at(&mut f, s, pos);
    emit!(
        f,
        I::I32Const(0x20),
        I::I32Or,
        I::I32Const(b'e' as i32),
        I::I32Ne,
        I::If(BlockType::Empty)
    );
    fail(&mut f);
    emit!(f, I::End);
    incr(&mut f, pos, 1);
    emit!(
        f,
        I::LocalGet(pos),
        I::LocalGet(end),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    at(&mut f, s, pos);
    emit!(
        f,
        I::LocalTee(ch),
        I::I32Const(b'-' as i32),
        I::I32Eq,
        I::LocalGet(ch),
        I::I32Const(b'+' as i32),
        I::I32Eq,
        I::I32Or,
        I::If(BlockType::Empty),
        I::LocalGet(ch),
        I::I32Const(b'-' as i32),
        I::I32Eq,
        I::LocalSet(eneg)
    );
    incr(&mut f, pos, 1);
    emit!(f, I::End, I::End);
    emit!(f, I::I32Const(0), I::LocalSet(prev_digit));
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(pos), I::LocalGet(end), I::I32GeU, I::BrIf(1));
    at(&mut f, s, pos);
    emit!(f, I::LocalSet(ch));
    is_digit(&mut f, ch);
    emit!(f, I::If(BlockType::Empty));
    emit!(
        f,
        I::I32Const(1),
        I::LocalSet(edigits),
        I::I32Const(1),
        I::LocalSet(prev_digit),
        I::LocalGet(expv),
        I::I32Const(100_000_000),
        I::I32LtS,
        I::If(BlockType::Empty),
        I::LocalGet(expv),
        I::I32Const(10),
        I::I32Mul,
        I::LocalGet(ch),
        I::I32Add,
        I::I32Const(b'0' as i32),
        I::I32Sub,
        I::LocalSet(expv),
        I::End
    );
    incr(&mut f, pos, 1);
    emit!(f, I::Br(1), I::End);
    emit!(
        f,
        I::LocalGet(ch),
        I::I32Const(b'_' as i32),
        I::I32Eq,
        I::If(BlockType::Empty)
    );
    underscore(&mut f, s, pos, end, prev_digit, ch, failure);
    emit!(f, I::Br(1), I::End);
    fail(&mut f);
    emit!(f, I::End, I::End);
    emit!(f, I::LocalGet(edigits), I::I32Eqz, I::If(BlockType::Empty));
    fail(&mut f);
    emit!(
        f,
        I::End,
        I::LocalGet(k),
        I::I32Const(0),
        I::LocalGet(expv),
        I::I32Sub,
        I::LocalGet(expv),
        I::LocalGet(eneg),
        I::Select,
        I::I32Add,
        I::LocalSet(k),
        I::End
    );

    // A sticky digit stands for the nonzero digits past the cap; otherwise
    // trailing zeros carry nothing.
    emit!(f, I::LocalGet(sticky), I::If(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(n),
        I::I32Const(TEXT),
        I::I32Add,
        I::I32Const(1),
        I::I32Store8(byte())
    );
    incr(&mut f, n, 1);
    emit!(
        f,
        I::Else,
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty)
    );
    emit!(
        f,
        I::LocalGet(n),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(n),
        I::I32Const(TEXT - 1),
        I::I32Add,
        I::I32Load8U(byte()),
        I::BrIf(1)
    );
    incr(&mut f, n, -1);
    emit!(f, I::Br(0), I::End, I::End, I::End);

    // Zero, and values past either end of the float range.
    emit!(
        f,
        I::LocalGet(n),
        I::I32Eqz,
        I::LocalGet(k),
        I::I32Const(-324),
        I::I32LtS,
        I::I32Or,
        I::If(BlockType::Empty)
    );
    signed(&mut f, 0.0);
    emit!(
        f,
        I::End,
        I::LocalGet(k),
        I::I32Const(310),
        I::I32GtS,
        I::If(BlockType::Empty)
    );
    signed(&mut f, f64::INFINITY);
    emit!(f, I::End);

    // An approximation: the first 19 digits, scaled by powers of ten.
    emit!(
        f,
        I::LocalGet(n),
        I::I32Const(19),
        I::LocalGet(n),
        I::I32Const(19),
        I::I32LtS,
        I::Select,
        I::LocalSet(m),
        I::I32Const(0),
        I::LocalSet(t),
        I::I64Const(0),
        I::LocalSet(d)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(t),
        I::LocalGet(m),
        I::I32GeS,
        I::BrIf(1),
        I::LocalGet(d),
        I::I64Const(10),
        I::I64Mul,
        I::LocalGet(t),
        I::I32Const(TEXT),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I64ExtendI32U,
        I::I64Add,
        I::LocalSet(d)
    );
    incr(&mut f, t, 1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(
        f,
        I::LocalGet(d),
        I::F64ConvertI64U,
        I::LocalSet(x),
        I::LocalGet(k),
        I::LocalGet(m),
        I::I32Sub,
        I::LocalSet(p)
    );
    for (cond, op, step) in [(I::I32GeS, I::F64Mul, -22), (I::I32LeS, I::F64Div, 22)] {
        emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
        emit!(
            f,
            I::LocalGet(p),
            I::I32Const(-step),
            cond,
            I::I32Eqz,
            I::BrIf(1),
            I::LocalGet(x),
            I::F64Const(1e22_f64.into()),
            op,
            I::LocalSet(x)
        );
        incr(&mut f, p, step);
        emit!(f, I::Br(0), I::End, I::End);
    }
    // 10**|p| for |p| < 22 is exact.
    emit!(
        f,
        I::F64Const(1.0_f64.into()),
        I::LocalSet(y),
        I::LocalGet(p),
        I::I32Const(0),
        I::LocalGet(p),
        I::I32Sub,
        I::LocalGet(p),
        I::I32Const(0),
        I::I32GeS,
        I::Select,
        I::LocalSet(t)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(t),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(y),
        I::F64Const(10.0_f64.into()),
        I::F64Mul,
        I::LocalSet(y)
    );
    incr(&mut f, t, -1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(
        f,
        I::LocalGet(x),
        I::LocalGet(y),
        I::F64Mul,
        I::LocalGet(x),
        I::LocalGet(y),
        I::F64Div,
        I::LocalGet(p),
        I::I32Const(0),
        I::I32GeS,
        I::Select,
        I::LocalTee(x),
        I::F64Const(f64::INFINITY.into()),
        I::F64Eq,
        I::If(BlockType::Empty),
        I::F64Const(f64::MAX.into()),
        I::LocalSet(x),
        I::End
    );

    // Step to the float the digits round to: up while they reach past the
    // midpoint above (a tie going to the even neighbour), down while they
    // fall short of the one below.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(x),
        I::I64ReinterpretF64,
        I::LocalTee(bits),
        I::LocalGet(n),
        I::LocalGet(k),
        I::Call(mid),
        I::LocalTee(c),
        I::I32Const(0),
        I::I32GtS,
        I::LocalGet(c),
        I::I32Eqz,
        I::LocalGet(bits),
        I::I64Const(1),
        I::I64And,
        I::I32WrapI64,
        I::I32And,
        I::I32Or,
        I::If(BlockType::Empty),
        I::LocalGet(bits),
        I::I64Const(MAX_BITS),
        I::I64Eq,
        I::If(BlockType::Empty),
        I::F64Const(f64::INFINITY.into()),
        I::LocalSet(x),
        I::Br(3),
        I::End,
        I::LocalGet(bits),
        I::I64Const(1),
        I::I64Add,
        I::F64ReinterpretI64,
        I::LocalSet(x),
        I::Br(1),
        I::End
    );
    emit!(
        f,
        I::LocalGet(bits),
        I::I64Eqz,
        I::BrIf(1),
        I::LocalGet(bits),
        I::I64Const(1),
        I::I64Sub,
        I::LocalGet(n),
        I::LocalGet(k),
        I::Call(mid),
        I::LocalTee(c),
        I::I32Const(0),
        I::I32LtS,
        I::LocalGet(c),
        I::I32Eqz,
        I::LocalGet(bits),
        I::I64Const(1),
        I::I64And,
        I::I32WrapI64,
        I::I32And,
        I::I32Or,
        I::If(BlockType::Empty),
        I::LocalGet(bits),
        I::I64Const(1),
        I::I64Sub,
        I::F64ReinterpretI64,
        I::LocalSet(x),
        I::Br(1),
        I::End,
        I::End,
        I::End
    );
    emit!(
        f,
        I::LocalGet(x),
        I::F64Neg,
        I::LocalGet(x),
        I::LocalGet(neg),
        I::Select,
        I::End
    );
    f
}

/// The helpers a `ValueError` naming the string is built from.
#[derive(Clone, Copy)]
struct Fail {
    text: u32,
    repr: u32,
    concat: u32,
    raise: u32,
}

impl Fail {
    fn new(ctx: &CompilationContext) -> Fail {
        Fail {
            text: ctx.helper(Helper::ExcText(
                "could not convert string to float: ".to_string(),
            )),
            repr: ctx.helper(Helper::Repr(crate::ir::IRType::String)),
            concat: ctx.helper(Helper::Concat),
            raise: ctx.helper(Helper::RaiseText("ValueError".to_string())),
        }
    }
}

/// Leave CPython's `ValueError` pending, naming the string (local 0), and
/// answer 0.0.
fn value_error(f: &mut Function, fail: Fail) {
    emit!(
        f,
        I::Call(fail.text),
        I::LocalGet(0),
        I::Call(fail.repr),
        I::Call(fail.concat),
        I::Call(fail.raise),
        I::F64Const(0.0_f64.into()),
        I::Return
    );
}

/// An underscore at `pos`: valid after a digit and before one; consumed.
#[allow(clippy::too_many_arguments)]
fn underscore(
    f: &mut Function,
    s: u32,
    pos: u32,
    end: u32,
    prev_digit: u32,
    ch: u32,
    failure: Fail,
) {
    emit!(
        f,
        I::LocalGet(prev_digit),
        I::I32Eqz,
        I::LocalGet(pos),
        I::I32Const(1),
        I::I32Add,
        I::LocalGet(end),
        I::I32GeU,
        I::I32Or,
        I::If(BlockType::Empty)
    );
    value_error(f, failure);
    emit!(
        f,
        I::End,
        I::LocalGet(s),
        I::LocalGet(pos),
        I::I32Add,
        I::I32Load8U(crate::compiler::runtime::mem(1, 0)),
        I::LocalSet(ch)
    );
    is_digit(f, ch);
    emit!(f, I::I32Eqz, I::If(BlockType::Empty));
    value_error(f, failure);
    emit!(f, I::End);
    emit!(f, I::I32Const(0), I::LocalSet(prev_digit));
    incr(f, pos, 1);
}

/// `(bits: i64, n, k) -> -1 | 0 | 1`: how the decimal `0.d1..dn * 10**k`
/// (digits at `TEXT`) compares with the midpoint between the nonnegative
/// float with these bits and the next float up.
pub(crate) fn build_mid_cmp(ctx: &CompilationContext) -> Function {
    use ValType::{I32, I64};
    let set = ctx.helper(Helper::BigSet);
    let copy = ctx.helper(Helper::BigCopy);
    let mul = ctx.helper(Helper::BigMulSmall);
    let shl = ctx.helper(Helper::BigShl);
    let cmp = ctx.helper(Helper::BigCmp);
    let pow10 = ctx.helper(Helper::BigMulPow10);
    let quorem = ctx.helper(Helper::BigQuoRem);
    let mut f = function(&[I32, I64, I32, I32, I32, I32]);
    let (bits, n, k) = (0, 1, 2);
    let (biased, mant, e, i, d, input) = (3, 4, 5, 6, 7, 8);
    emit!(
        f,
        I::I32Const(BIG_Z),
        I::I64Const(0),
        I::Call(set),
        I::LocalGet(bits),
        I::I64Const(52),
        I::I64ShrU,
        I::I32WrapI64,
        I::LocalTee(biased),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::LocalGet(bits),
        I::LocalSet(mant),
        I::I32Const(-1075),
        I::LocalSet(e),
        I::Else,
        I::LocalGet(bits),
        I::I64Const((1 << 52) - 1),
        I::I64And,
        I::I64Const(1 << 52),
        I::I64Or,
        I::LocalSet(mant),
        I::LocalGet(biased),
        I::I32Const(1076),
        I::I32Sub,
        I::LocalSet(e),
        I::End,
        // R / S = (2M + 1) * 2**e / 10**k
        I::I32Const(BIG_R),
        I::LocalGet(mant),
        I::I64Const(1),
        I::I64Shl,
        I::I64Const(1),
        I::I64Or,
        I::Call(set),
        I::I32Const(BIG_S),
        I::I64Const(1),
        I::Call(set),
        I::LocalGet(e),
        I::I32Const(0),
        I::I32GeS,
        I::If(BlockType::Empty),
        I::I32Const(BIG_R),
        I::LocalGet(e),
        I::Call(shl),
        I::Else,
        I::I32Const(BIG_S),
        I::I32Const(0),
        I::LocalGet(e),
        I::I32Sub,
        I::Call(shl),
        I::End,
        I::LocalGet(k),
        I::I32Const(0),
        I::I32GeS,
        I::If(BlockType::Empty),
        I::I32Const(BIG_S),
        I::LocalGet(k),
        I::Call(pow10),
        I::Else,
        I::I32Const(BIG_R),
        I::I32Const(0),
        I::LocalGet(k),
        I::I32Sub,
        I::Call(pow10),
        I::End
    );
    // The input is in [10**(k-1), 10**k).
    emit!(
        f,
        I::I32Const(BIG_R),
        I::I32Const(BIG_S),
        I::Call(cmp),
        I::I32Const(0),
        I::I32GeS,
        I::If(BlockType::Empty),
        I::I32Const(-1),
        I::Return,
        I::End,
        I::I32Const(BIG_T),
        I::I32Const(BIG_R),
        I::Call(copy),
        I::I32Const(BIG_T),
        I::I32Const(10),
        I::Call(mul),
        I::I32Const(BIG_T),
        I::I32Const(BIG_S),
        I::Call(cmp),
        I::I32Const(0),
        I::I32LtS,
        I::If(BlockType::Empty),
        I::I32Const(1),
        I::Return,
        I::End
    );
    // Digit by digit.
    emit!(f, I::Loop(BlockType::Empty));
    emit!(
        f,
        I::I32Const(BIG_R),
        I::I32Const(10),
        I::Call(mul),
        I::I32Const(BIG_R),
        I::I32Const(BIG_S),
        I::Call(quorem),
        I::LocalSet(d),
        I::I32Const(0),
        I::LocalSet(input),
        I::LocalGet(i),
        I::LocalGet(n),
        I::I32LtS,
        I::If(BlockType::Empty),
        I::LocalGet(i),
        I::I32Const(TEXT),
        I::I32Add,
        I::I32Load8U(byte()),
        I::LocalSet(input),
        I::End,
        I::LocalGet(input),
        I::LocalGet(d),
        I::I32Ne,
        I::If(BlockType::Empty),
        I::I32Const(1),
        I::I32Const(-1),
        I::LocalGet(input),
        I::LocalGet(d),
        I::I32GtU,
        I::Select,
        I::Return,
        I::End
    );
    incr(&mut f, i, 1);
    emit!(
        f,
        I::I32Const(BIG_R),
        I::I32Const(BIG_Z),
        I::Call(cmp),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::LocalGet(i),
        I::LocalGet(n),
        I::I32LtS,
        I::Return,
        I::End,
        I::Br(0),
        I::End,
        I::I32Const(0),
        I::End
    );
    f
}
