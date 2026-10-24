//! Runtime helper functions a module carries only when its code calls them.
//!
//! Code generation asks for a helper with `CompilationContext::helper`, which
//! hands out its function index; once every function is compiled, the module
//! assembler builds each requested helper here (a helper may request others)
//! and appends it. A program that never formats a float carries none of this.
//!
//! The float conversions work on exact multi-word integers ("bignums") in a
//! scratch region of linear memory, so every digit they produce is CPython's:
//! `repr` gives the shortest string that reads back as the same float, chosen
//! and rounded the way David Gay's `dtoa` (mode 0) chooses, which is what
//! CPython calls.

use crate::compiler::context::{
    CompilationContext, COLLECTION_DATA, COLLECTION_HEAP_BASE, COLLECTION_SLOT, DICT_ENTRY,
};
use crate::ir::IRType;
use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

/// The runtime helpers.
#[derive(Clone, Debug, PartialEq)]
pub enum Helper {
    /// `(dst, v: i64)`: a bignum set to `v`.
    BigSet,
    /// `(dst, src)`: a copy.
    BigCopy,
    /// `(a, m)`: `a *= m` for a 32-bit `m`.
    BigMulSmall,
    /// `(a, bits)`: `a <<= bits`.
    BigShl,
    /// `(dst, a, b)`: `dst = a + b`.
    BigAdd,
    /// `(dst, a, b)`: `dst = a - b`, with `a >= b`.
    BigSub,
    /// `(a, b) -> -1 | 0 | 1`.
    BigCmp,
    /// `(a, k)`: `a *= 10**k`.
    BigMulPow10,
    /// `(r, s) -> d`: `d = r // s` for a quotient under 10, leaving `r % s`.
    BigQuoRem,
    /// `(v: f64) -> str`: `repr(v)`, which is also `str(v)`.
    FloatRepr,
    /// `(v) -> str`: `str(v)` for a value of the type (an f64 for a float, a
    /// word otherwise).
    Str(IRType),
    /// `(v) -> str`: `repr(v)`.
    Repr(IRType),
    /// `(s) -> str`: `repr(s)` of a string: quoted, with CPython's escapes.
    StrRepr,
    /// `(b) -> str`: `repr(b)` of bytes, `b'...'`.
    BytesRepr,
    /// `(v: f64) -> n`: the shortest digits of a positive float.
    ShortestDigits,
    /// `(v: f64) -> n`: the correctly rounded digits of a nonnegative float,
    /// in the mode and count at `DIG_MODE` and `DIG_COUNT`.
    FixedDigits,
    /// `(v: f64, type, precision, flags) -> str`: CPython's
    /// `PyOS_double_to_string` for a nonnegative float, without a sign.
    FloatToString,
    /// `(spec) -> ok`: parse a format specification into `SPEC`.
    ParseSpec,
    /// `(prefix, body, default_align) -> str`: fill to the parsed width.
    Pad,
    /// `(digits, separator, size, min_width) -> str`: digit grouping.
    Group,
    /// `(v) -> str`: an int under the parsed specification.
    FormatInt,
    /// `(v: f64) -> str`: a float under the parsed specification.
    FormatFloat,
    /// `(s) -> str`: a string under the parsed specification.
    FormatStr,
    /// `(v, spec) -> str`: `format(v, spec)` for a value of the type.
    Format(IRType),
    /// `(a, b) -> str`: `a + b`.
    Concat,
    /// `(s) -> str`: every non-ASCII character escaped, as `ascii()` does.
    AsciiEscape,
    /// `(s) -> f64`: `float(s)`, correctly rounded.
    ParseFloat,
    /// `(bits: i64, n, k) -> -1 | 0 | 1`: parsed digits against a midpoint.
    MidCmp,
    /// `(cp) -> bool`: the code point is in the class.
    UniHas(crate::core::unicode::Class),
    /// `(cp, out) -> width`: a code point's case mapping, written at `out`.
    UniMap(crate::core::unicode::CaseMap),
    /// `(s, n, i, w) -> cp`: the lowercase of the capital sigma at `i`.
    FinalSigma,
    /// `(s, n, out) -> bytes`: a case transform, written at `out`.
    CaseRun(crate::compiler::unicode::Case),
    /// `(s) -> str`: a case transform of a string.
    Case(crate::compiler::unicode::Case),
    /// `(s) -> bool`: a string predicate.
    StrIs(crate::core::unicode::Predicate),
    /// `(s, i, set, set_n) -> width`: the character at (or before) `i` is
    /// whitespace, or in the set.
    TrimAt {
        set: bool,
        before: bool,
    },
    /// `(s) -> n`: a string's length in code points.
    CharLen,
    /// `(s) -> cp`: the code point of the string's first character.
    FirstCodePoint,
    /// `(cp) -> s`: the one-character string of a code point.
    FromCodePoint,
    /// `(s, i) -> byte`: the byte offset of code point `i`.
    CharOffset,
    /// `(s, byte) -> i`: the code point index of a byte offset.
    CharIndex,
    /// `(s) -> list`: the list of a string's characters.
    StrChars,
    /// `(fd, s) -> ()`: write a string to a file descriptor.
    WriteFd,
    /// `() -> str | 0`: a line of standard input (or all of it).
    ReadLine {
        keep_newline: bool,
        whole: bool,
    },
    /// `(s) -> ()`: raise `UnicodeDecodeError` unless the bytes are UTF-8.
    Utf8Check,
    /// `(position, byte, reason) -> ()`: leave the `UnicodeDecodeError`.
    Utf8Error,
    /// `() -> list`: `sys.argv`.
    Argv,
    /// `() -> list`: `sys.path`.
    SysPath,
    /// `() -> list`: `sys.stdin.readlines()`.
    ReadLines,
    /// `() -> ()`: the WASI command entry, running the `__main__` block.
    Start,
    /// `(dict) -> list`: the list of a dict's keys, in order.
    DictKeys,
    /// `() -> str`: a constant text.
    ExcText(String),
    /// `(message) -> ()`: leave a new `class(message)` pending.
    RaiseText(String),
    /// `() -> ()`: leave a new `class(errno, message)` pending.
    RaiseErrno {
        class: String,
        errno: i32,
        message: String,
    },
    /// `(value) -> ()`: leave a new `class(value)` pending.
    RaiseValue {
        class: String,
        ty: IRType,
    },
    /// `(obj) -> str`: the name of an instance's class.
    ClassName,
    /// A class's `__name__` from its id, or with `repr` its
    /// `<class '__main__.A'>`.
    ClassNameOf {
        repr: bool,
    },
    /// `(path, len, flags) -> fd`, `(fd, buf, len) -> n`, `(fd, buf, len)
    /// -> n`, `(fd) -> status`: file I/O through WASI.
    FileOpen,
    /// `(path, len) -> dirfd`: the preopen a path resolves against.
    ResolvePath,
    FileRead,
    FileWrite,
    FileClose,
    /// A C math library routine (see `libm`).
    Libm(crate::compiler::libm::Libm),
    /// A routine of the `random` module's generator.
    Random(crate::compiler::stdlib::RandomFn),
    /// A routine of `os` or `os.path`.
    Os(crate::compiler::stdlib::OsFn),
    /// A routine of `json`.
    Json(crate::compiler::stdlib::json::JsonFn),
}

/// The parameter and result types of `helper`.
pub fn signature(helper: &Helper) -> (Vec<ValType>, Vec<ValType>) {
    use ValType::{F64, I32, I64};
    match helper {
        Helper::BigSet => (vec![I32, I64], vec![]),
        Helper::BigCopy | Helper::BigMulSmall | Helper::BigShl | Helper::BigMulPow10 => {
            (vec![I32, I32], vec![])
        }
        Helper::BigAdd | Helper::BigSub => (vec![I32, I32, I32], vec![]),
        Helper::BigCmp | Helper::BigQuoRem => (vec![I32, I32], vec![I32]),
        Helper::FloatRepr => (vec![F64], vec![I32]),
        Helper::Str(ty) | Helper::Repr(ty) => (
            vec![if matches!(ty, IRType::Float) {
                F64
            } else {
                I32
            }],
            vec![I32],
        ),
        Helper::StrRepr | Helper::BytesRepr | Helper::AsciiEscape => (vec![I32], vec![I32]),
        Helper::ShortestDigits | Helper::FixedDigits => (vec![F64], vec![I32]),
        Helper::FloatToString => (vec![F64, I32, I32, I32], vec![I32]),
        Helper::ParseSpec | Helper::FormatInt | Helper::FormatStr => (vec![I32], vec![I32]),
        Helper::Pad => (vec![I32, I32, I32], vec![I32]),
        Helper::Group => (vec![I32, I32, I32, I32], vec![I32]),
        Helper::FormatFloat => (vec![F64], vec![I32]),
        Helper::Format(ty) => (
            vec![
                if matches!(ty, IRType::Float) {
                    F64
                } else {
                    I32
                },
                I32,
            ],
            vec![I32],
        ),
        Helper::Concat => (vec![I32, I32], vec![I32]),
        Helper::ParseFloat => (vec![I32], vec![F64]),
        Helper::MidCmp => (vec![I64, I32, I32], vec![I32]),
        Helper::UniHas(_) | Helper::Case(_) | Helper::StrIs(_) => (vec![I32], vec![I32]),
        Helper::UniMap(_) => (vec![I32, I32], vec![I32]),
        Helper::CaseRun(_) => (vec![I32, I32, I32], vec![I32]),
        Helper::FinalSigma | Helper::TrimAt { .. } => (vec![I32, I32, I32, I32], vec![I32]),
        Helper::CharLen | Helper::StrChars | Helper::FirstCodePoint | Helper::FromCodePoint => {
            (vec![I32], vec![I32])
        }
        Helper::WriteFd => (vec![I32, I32], vec![]),
        Helper::Start => (vec![], vec![]),
        Helper::FileOpen | Helper::FileRead | Helper::FileWrite => (vec![I32; 3], vec![I32]),
        Helper::FileClose => (vec![I32], vec![I32]),
        Helper::ResolvePath => (vec![I32, I32], vec![I32]),
        Helper::DictKeys | Helper::ClassName | Helper::ClassNameOf { .. } => (vec![I32], vec![I32]),
        Helper::ExcText(_) => (vec![], vec![I32]),
        Helper::RaiseText(_) => (vec![I32], vec![]),
        Helper::RaiseErrno { .. } => (vec![], vec![]),
        Helper::RaiseValue { ty, .. } => (
            vec![if matches!(ty, IRType::Float) {
                F64
            } else {
                I32
            }],
            vec![],
        ),
        Helper::ReadLine { .. } | Helper::Argv | Helper::SysPath | Helper::ReadLines => {
            (vec![], vec![I32])
        }
        Helper::Utf8Check => (vec![I32], vec![]),
        Helper::Utf8Error => (vec![I32, I32, I32], vec![]),
        Helper::CharOffset | Helper::CharIndex => (vec![I32, I32], vec![I32]),
        Helper::Os(routine) => {
            let (params, result) = routine.signature();
            (
                params.iter().map(|t| t.val()).collect(),
                result.iter().map(|t| t.val()).collect(),
            )
        }
        Helper::Random(routine) => {
            let (params, result) = routine.signature();
            (
                params.iter().map(|t| t.val()).collect(),
                result.iter().map(|t| t.val()).collect(),
            )
        }
        Helper::Json(routine) => {
            let (params, result) = routine.signature();
            (
                params.iter().map(|t| t.val()).collect(),
                result.iter().map(|t| t.val()).collect(),
            )
        }
        Helper::Libm(routine) => {
            let (params, result) = routine.signature();
            (
                params.iter().map(|t| t.val()).collect(),
                result.iter().map(|t| t.val()).collect(),
            )
        }
    }
}

/// Build the body of `helper`.
pub fn build(helper: &Helper, ctx: &CompilationContext) -> Function {
    match helper {
        Helper::BigSet => build_big_set(),
        Helper::BigCopy => build_big_copy(),
        Helper::BigMulSmall => build_big_mul_small(),
        Helper::BigShl => build_big_shl(),
        Helper::BigAdd => build_big_add(),
        Helper::BigSub => build_big_sub(),
        Helper::BigCmp => build_big_cmp(),
        Helper::BigMulPow10 => build_big_mul_pow10(ctx),
        Helper::BigQuoRem => build_big_quorem(ctx),
        Helper::FloatRepr => crate::compiler::format::build_float_repr(ctx),
        Helper::ShortestDigits => build_shortest_digits(ctx),
        Helper::FixedDigits => crate::compiler::format::build_fixed_digits(ctx),
        Helper::FloatToString => crate::compiler::format::build_float_to_string(ctx),
        Helper::ParseSpec => crate::compiler::format::build_parse_spec(),
        Helper::Pad => crate::compiler::format::build_pad(ctx),
        Helper::Group => crate::compiler::format::build_group(ctx),
        Helper::FormatInt => crate::compiler::format::build_format_int(ctx),
        Helper::FormatFloat => crate::compiler::format::build_format_float(ctx),
        Helper::FormatStr => crate::compiler::format::build_format_str(ctx),
        Helper::Format(ty) => crate::compiler::format::build_format(ctx, ty),
        Helper::Concat => crate::compiler::format::build_concat(ctx),
        Helper::AsciiEscape => crate::compiler::format::build_ascii_escape(ctx),
        Helper::ParseFloat => crate::compiler::float_parse::build_parse_float(ctx),
        Helper::MidCmp => crate::compiler::float_parse::build_mid_cmp(ctx),
        Helper::UniHas(class) => crate::compiler::unicode::build_has(*class),
        Helper::UniMap(map) => crate::compiler::unicode::build_map(*map),
        Helper::FinalSigma => crate::compiler::unicode::build_final_sigma(ctx),
        Helper::CaseRun(case) => crate::compiler::unicode::build_case_run(ctx, *case),
        Helper::Case(case) => crate::compiler::unicode::build_case(ctx, *case),
        Helper::StrIs(predicate) => crate::compiler::unicode::build_str_is(ctx, *predicate),
        Helper::CharLen => crate::compiler::unicode::build_char_len(),
        Helper::FirstCodePoint => crate::compiler::unicode::build_first_code_point(),
        Helper::FromCodePoint => crate::compiler::unicode::build_from_code_point(ctx),
        Helper::CharOffset => crate::compiler::unicode::build_char_offset(ctx),
        Helper::CharIndex => crate::compiler::unicode::build_char_index(ctx),
        Helper::StrChars => crate::compiler::unicode::build_str_chars(ctx),
        Helper::WriteFd => crate::compiler::host::build_write_fd(ctx),
        Helper::ReadLine {
            keep_newline,
            whole,
        } => crate::compiler::host::build_read_line(ctx, *keep_newline, *whole),
        Helper::Utf8Check => crate::compiler::host::build_utf8_check(ctx),
        Helper::Utf8Error => crate::compiler::host::build_utf8_error(ctx),
        Helper::Argv => crate::compiler::host::build_argv(ctx),
        Helper::SysPath => crate::compiler::host::build_sys_path(ctx),
        Helper::ReadLines => crate::compiler::host::build_read_lines(ctx),
        Helper::Start => crate::compiler::host::build_start(ctx),
        Helper::DictKeys => build_dict_keys(ctx),
        Helper::ExcText(text) => crate::compiler::exceptions::build_text(ctx, text),
        Helper::RaiseText(class) => crate::compiler::exceptions::build_raise_text(ctx, class),
        Helper::RaiseErrno {
            class,
            errno,
            message,
        } => crate::compiler::exceptions::build_raise_errno(ctx, class, *errno, message),
        Helper::RaiseValue { class, ty } => {
            crate::compiler::exceptions::build_raise_value(ctx, class, ty)
        }
        Helper::ClassName => crate::compiler::exceptions::build_class_name(ctx, None),
        Helper::FileOpen => crate::compiler::host::build_file_open(ctx),
        Helper::ResolvePath => crate::compiler::host::build_resolve_path(ctx),
        Helper::FileRead => crate::compiler::host::build_file_read(ctx),
        Helper::FileWrite => crate::compiler::host::build_file_write(ctx),
        Helper::FileClose => crate::compiler::host::build_file_close(ctx),
        Helper::ClassNameOf { repr } => {
            crate::compiler::exceptions::build_class_name(ctx, Some(*repr))
        }
        Helper::TrimAt { set, before } => {
            crate::compiler::unicode::build_trim_at(ctx, *set, *before)
        }
        Helper::Str(ty) => build_str(ctx, ty, false),
        Helper::Repr(ty) => build_str(ctx, ty, true),
        Helper::StrRepr => build_text_repr(ctx, false),
        Helper::BytesRepr => build_text_repr(ctx, true),
        Helper::Libm(routine) => crate::compiler::libm::build(ctx, *routine),
        Helper::Random(routine) => crate::compiler::stdlib::build_random(ctx, *routine),
        Helper::Os(routine) => crate::compiler::stdlib::build_os(ctx, *routine),
        Helper::Json(routine) => crate::compiler::stdlib::json::build(ctx, routine),
    }
}

/// 32-bit limbs per bignum: 1280 bits, room for `2 * 2**53 * 10**324`, the
/// largest value a float conversion forms.
const LIMBS: i32 = 40;
const BIG_BYTES: i32 = LIMBS * 4;

/// Where the conversions keep their bignums and digits: the unused space
/// between the bytes region and the heap. A conversion runs to completion
/// before another starts (none calls back into user code), so one region
/// serves every call.
const SCRATCH: i32 = 65536;
pub(crate) const BIG_R: i32 = SCRATCH;
pub(crate) const BIG_S: i32 = SCRATCH + BIG_BYTES;
pub(crate) const BIG_MP: i32 = SCRATCH + 2 * BIG_BYTES;
pub(crate) const BIG_MM: i32 = SCRATCH + 3 * BIG_BYTES;
pub(crate) const BIG_T: i32 = SCRATCH + 4 * BIG_BYTES;
pub(crate) const DIGITS: i32 = SCRATCH + 6 * BIG_BYTES;
/// A bignum kept at zero, to test a remainder against.
pub(crate) const BIG_Z: i32 = SCRATCH + 5 * BIG_BYTES;
/// The digit count and decimal-point position a digit generator leaves, and
/// the mode (2: significant digits, 3: digits after the point) and count the
/// fixed-precision one is given.
pub(crate) const DIG_N: i32 = SCRATCH + 3072;
pub(crate) const DIG_K: i32 = SCRATCH + 3076;
pub(crate) const DIG_MODE: i32 = SCRATCH + 3080;
pub(crate) const DIG_COUNT: i32 = SCRATCH + 3084;
/// A parsed format specification (see `format::build_parse_spec`).
pub(crate) const SPEC: i32 = SCRATCH + 3200;
/// Where a formatter assembles text before copying it into a new string.
pub(crate) const TEXT: i32 = SCRATCH + 4096;
pub(crate) const TEXT_BYTES: i32 = 32768;
const _: () = assert!(DIGITS + 1200 < DIG_N);
const _: () = assert!(TEXT + TEXT_BYTES < COLLECTION_HEAP_BASE as i32);

pub(crate) fn mem(offset: u64, align: u32) -> MemArg {
    MemArg {
        offset,
        align,
        memory_index: 0,
    }
}

pub(crate) fn w32(offset: u64) -> MemArg {
    mem(offset, 2)
}

pub(crate) fn byte() -> MemArg {
    mem(0, 0)
}

/// Locals in order after the parameters, coalesced into runs.
pub(crate) fn function(locals: &[ValType]) -> Function {
    let mut runs: Vec<(u32, ValType)> = Vec::new();
    for ty in locals {
        match runs.last_mut() {
            Some((n, last)) if last == ty => *n += 1,
            _ => runs.push((1, *ty)),
        }
    }
    Function::new(runs)
}

/// `for i in 0..LIMBS` (or `LIMBS-1 down to 0` when `down`), with `body`
/// run with the index in local `i`.
fn limb_loop(f: &mut Function, i: u32, down: bool, body: impl FnOnce(&mut Function)) {
    f.instruction(&Instruction::I32Const(if down { LIMBS - 1 } else { 0 }));
    f.instruction(&Instruction::LocalSet(i));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(i));
    if down {
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32LtS);
    } else {
        f.instruction(&Instruction::I32Const(LIMBS));
        f.instruction(&Instruction::I32GeS);
    }
    f.instruction(&Instruction::BrIf(1));
    body(f);
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::I32Const(if down { -1 } else { 1 }));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(i));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
}

/// Push the address of limb `i` (a local) of the bignum in local `a`.
fn limb(f: &mut Function, a: u32, i: u32) {
    f.instruction(&Instruction::LocalGet(a));
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::I32Const(2));
    f.instruction(&Instruction::I32Shl);
    f.instruction(&Instruction::I32Add);
}

fn build_big_set() -> Function {
    // (dst, v)
    let mut f = function(&[]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32Const(BIG_BYTES));
    f.instruction(&Instruction::MemoryFill(0));
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I64Store(mem(0, 2)));
    f.instruction(&Instruction::End);
    f
}

fn build_big_copy() -> Function {
    // (dst, src)
    let mut f = function(&[]);
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Const(BIG_BYTES));
    f.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    f.instruction(&Instruction::End);
    f
}

fn build_big_mul_small() -> Function {
    // (a, m); locals: carry i64 (2), i (3), t i64 (4)
    let mut f = function(&[ValType::I64, ValType::I32, ValType::I64]);
    let (a, m, carry, i, t) = (0, 1, 2, 3, 4);
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::LocalSet(carry));
    limb_loop(&mut f, i, false, |f| {
        limb(f, a, i);
        f.instruction(&Instruction::I64Load32U(w32(0)));
        f.instruction(&Instruction::LocalGet(m));
        f.instruction(&Instruction::I64ExtendI32U);
        f.instruction(&Instruction::I64Mul);
        f.instruction(&Instruction::LocalGet(carry));
        f.instruction(&Instruction::I64Add);
        f.instruction(&Instruction::LocalSet(t));
        limb(f, a, i);
        f.instruction(&Instruction::LocalGet(t));
        f.instruction(&Instruction::I64Store32(w32(0)));
        f.instruction(&Instruction::LocalGet(t));
        f.instruction(&Instruction::I64Const(32));
        f.instruction(&Instruction::I64ShrU);
        f.instruction(&Instruction::LocalSet(carry));
    });
    f.instruction(&Instruction::End);
    f
}

fn build_big_shl() -> Function {
    // (a, bits); locals: words (2), rem (3), i (4), j (5)
    let mut f = function(&[ValType::I32, ValType::I32, ValType::I32, ValType::I32]);
    let (a, bits, words, rem, i, j) = (0, 1, 2, 3, 4, 5);
    f.instruction(&Instruction::LocalGet(bits));
    f.instruction(&Instruction::I32Const(5));
    f.instruction(&Instruction::I32ShrU);
    f.instruction(&Instruction::LocalSet(words));
    f.instruction(&Instruction::LocalGet(bits));
    f.instruction(&Instruction::I32Const(31));
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::LocalSet(rem));
    // Whole limbs: a[i] = i >= words ? a[i - words] : 0, top down.
    f.instruction(&Instruction::LocalGet(words));
    f.instruction(&Instruction::If(BlockType::Empty));
    limb_loop(&mut f, i, true, |f| {
        limb(f, a, i);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::LocalGet(words));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::LocalTee(j));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::I32GeS);
        f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        limb(f, a, j);
        f.instruction(&Instruction::I32Load(w32(0)));
        f.instruction(&Instruction::Else);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::I32Store(w32(0)));
    });
    f.instruction(&Instruction::End);
    // Bits: a[i] = a[i] << rem | a[i-1] >> (32 - rem), top down.
    f.instruction(&Instruction::LocalGet(rem));
    f.instruction(&Instruction::If(BlockType::Empty));
    limb_loop(&mut f, i, true, |f| {
        limb(f, a, i);
        limb(f, a, i);
        f.instruction(&Instruction::I32Load(w32(0)));
        f.instruction(&Instruction::LocalGet(rem));
        f.instruction(&Instruction::I32Shl);
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::LocalSet(j));
        limb(f, a, j);
        f.instruction(&Instruction::I32Load(w32(0)));
        f.instruction(&Instruction::I32Const(32));
        f.instruction(&Instruction::LocalGet(rem));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::I32ShrU);
        f.instruction(&Instruction::Else);
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::I32Or);
        f.instruction(&Instruction::I32Store(w32(0)));
    });
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f
}

fn build_big_add() -> Function {
    // (dst, a, b); locals: carry i64 (3), i (4), t i64 (5)
    let mut f = function(&[ValType::I64, ValType::I32, ValType::I64]);
    let (dst, a, b, carry, i, t) = (0, 1, 2, 3, 4, 5);
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::LocalSet(carry));
    limb_loop(&mut f, i, false, |f| {
        limb(f, a, i);
        f.instruction(&Instruction::I64Load32U(w32(0)));
        limb(f, b, i);
        f.instruction(&Instruction::I64Load32U(w32(0)));
        f.instruction(&Instruction::I64Add);
        f.instruction(&Instruction::LocalGet(carry));
        f.instruction(&Instruction::I64Add);
        f.instruction(&Instruction::LocalSet(t));
        limb(f, dst, i);
        f.instruction(&Instruction::LocalGet(t));
        f.instruction(&Instruction::I64Store32(w32(0)));
        f.instruction(&Instruction::LocalGet(t));
        f.instruction(&Instruction::I64Const(32));
        f.instruction(&Instruction::I64ShrU);
        f.instruction(&Instruction::LocalSet(carry));
    });
    f.instruction(&Instruction::End);
    f
}

fn build_big_sub() -> Function {
    // (dst, a, b); locals: borrow i64 (3), i (4), t i64 (5)
    let mut f = function(&[ValType::I64, ValType::I32, ValType::I64]);
    let (dst, a, b, borrow, i, t) = (0, 1, 2, 3, 4, 5);
    f.instruction(&Instruction::I64Const(0));
    f.instruction(&Instruction::LocalSet(borrow));
    limb_loop(&mut f, i, false, |f| {
        limb(f, a, i);
        f.instruction(&Instruction::I64Load32U(w32(0)));
        limb(f, b, i);
        f.instruction(&Instruction::I64Load32U(w32(0)));
        f.instruction(&Instruction::I64Sub);
        f.instruction(&Instruction::LocalGet(borrow));
        f.instruction(&Instruction::I64Sub);
        f.instruction(&Instruction::LocalSet(t));
        limb(f, dst, i);
        f.instruction(&Instruction::LocalGet(t));
        f.instruction(&Instruction::I64Store32(w32(0)));
        // The high word of a negative difference is all ones.
        f.instruction(&Instruction::LocalGet(t));
        f.instruction(&Instruction::I64Const(63));
        f.instruction(&Instruction::I64ShrU);
        f.instruction(&Instruction::LocalSet(borrow));
    });
    f.instruction(&Instruction::End);
    f
}

fn build_big_cmp() -> Function {
    // (a, b) -> i32; locals: i (2), x (3), y (4)
    let mut f = function(&[ValType::I32, ValType::I32, ValType::I32]);
    let (a, b, i, x, y) = (0, 1, 2, 3, 4);
    limb_loop(&mut f, i, true, |f| {
        limb(f, a, i);
        f.instruction(&Instruction::I32Load(w32(0)));
        f.instruction(&Instruction::LocalSet(x));
        limb(f, b, i);
        f.instruction(&Instruction::I32Load(w32(0)));
        f.instruction(&Instruction::LocalSet(y));
        f.instruction(&Instruction::LocalGet(x));
        f.instruction(&Instruction::LocalGet(y));
        f.instruction(&Instruction::I32Ne);
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Const(-1));
        f.instruction(&Instruction::LocalGet(x));
        f.instruction(&Instruction::LocalGet(y));
        f.instruction(&Instruction::I32GtU);
        f.instruction(&Instruction::Select);
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
    });
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::End);
    f
}

fn build_big_mul_pow10(ctx: &CompilationContext) -> Function {
    // (a, k)
    let mul = ctx.helper(Helper::BigMulSmall);
    let mut f = function(&[]);
    let (a, k) = (0, 1);
    for (chunk, factor) in [(9, 1_000_000_000), (1, 10)] {
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        f.instruction(&Instruction::LocalGet(k));
        f.instruction(&Instruction::I32Const(chunk));
        f.instruction(&Instruction::I32LtS);
        f.instruction(&Instruction::BrIf(1));
        f.instruction(&Instruction::LocalGet(a));
        f.instruction(&Instruction::I32Const(factor));
        f.instruction(&Instruction::Call(mul));
        f.instruction(&Instruction::LocalGet(k));
        f.instruction(&Instruction::I32Const(chunk));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::LocalSet(k));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
    }
    f.instruction(&Instruction::End);
    f
}

fn build_big_quorem(ctx: &CompilationContext) -> Function {
    // (r, s) -> d; locals: d (2)
    let cmp = ctx.helper(Helper::BigCmp);
    let sub = ctx.helper(Helper::BigSub);
    let mut f = function(&[ValType::I32]);
    let (r, s, d) = (0, 1, 2);
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(r));
    f.instruction(&Instruction::LocalGet(s));
    f.instruction(&Instruction::Call(cmp));
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32LtS);
    f.instruction(&Instruction::BrIf(1));
    f.instruction(&Instruction::LocalGet(r));
    f.instruction(&Instruction::LocalGet(r));
    f.instruction(&Instruction::LocalGet(s));
    f.instruction(&Instruction::Call(sub));
    f.instruction(&Instruction::LocalGet(d));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(d));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(d));
    f.instruction(&Instruction::End);
    f
}

/// Allocate a string blob for `len` bytes (a local), leaving the address of
/// its first byte in `out` and storing the length prefix and terminator.
pub(crate) fn alloc_string(f: &mut Function, ctx: &CompilationContext, len: u32, out: u32) {
    f.instruction(&Instruction::LocalGet(len));
    f.instruction(&Instruction::I32Const(5));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::Call(ctx.alloc_func_index));
    f.instruction(&Instruction::LocalTee(out));
    f.instruction(&Instruction::LocalGet(len));
    f.instruction(&Instruction::I32Store(w32(0)));
    f.instruction(&Instruction::LocalGet(out));
    f.instruction(&Instruction::I32Const(4));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalTee(out));
    f.instruction(&Instruction::LocalGet(len));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::I32Store8(byte()));
}

/// Return a new string of the bytes `text`, using `len` and `out` as scratch.
pub(crate) fn return_text(
    f: &mut Function,
    ctx: &CompilationContext,
    text: &str,
    len: u32,
    out: u32,
) {
    f.instruction(&Instruction::I32Const(text.len() as i32));
    f.instruction(&Instruction::LocalSet(len));
    alloc_string(f, ctx, len, out);
    for (i, b) in text.bytes().enumerate() {
        f.instruction(&Instruction::LocalGet(out));
        f.instruction(&Instruction::I32Const(b as i32));
        f.instruction(&Instruction::I32Store8(mem(i as u64, 0)));
    }
    f.instruction(&Instruction::LocalGet(out));
    f.instruction(&Instruction::Return);
}

/// The locals of the float conversions, after their one f64 parameter.
pub(crate) fn float_locals() -> Vec<ValType> {
    use ValType::{I32, I64};
    vec![
        I64, // 1 bits
        I64, // 2 f
        I32, // 3 e
        I32, // 4 neg
        I32, // 5 even
        I32, // 6 est
        I32, // 7 k
        I32, // 8 d
        I32, // 9 n
        I32, // 10 j
        I32, // 11 j1
        I32, // 12 c
        I32, // 13 out
        I32, // 14 len
        I32, // 15 i
        I32, // 16 expo
        I32, // 17 unequal
        I32, // 18 expf
        I32, // 19 p (write cursor)
    ]
}

/// From `bits` (local 1, the float's bits; it is positive, finite, and not
/// zero), set up Steele and White's exact state in the scratch bignums:
/// `R / S` is the value scaled into [0.1, 1) times 10, `MP` and `MM` the
/// scaled half-gaps to its neighbours, `k` (local 7) the decimal exponent of
/// the first digit, and `even` (local 5) whether the halfway points round
/// to it.
pub(crate) fn emit_float_scale(f: &mut Function, ctx: &CompilationContext) {
    let set = ctx.helper(Helper::BigSet);
    let mul = ctx.helper(Helper::BigMulSmall);
    let shl = ctx.helper(Helper::BigShl);
    let add = ctx.helper(Helper::BigAdd);
    let cmp = ctx.helper(Helper::BigCmp);
    let pow10 = ctx.helper(Helper::BigMulPow10);
    let (bits, fm, e, even, est, k, c, unequal, expf) = (1, 2, 3, 5, 6, 7, 12, 17, 18);
    let ins = |f: &mut Function, i: Instruction| {
        f.instruction(&i);
    };
    let big = |f: &mut Function, addr: i32| {
        f.instruction(&Instruction::I32Const(addr));
    };
    let set_small = |f: &mut Function, dst: i32, value: i64| {
        big(f, dst);
        f.instruction(&Instruction::I64Const(value));
        f.instruction(&Instruction::Call(set));
    };
    let shl_by = |f: &mut Function, dst: i32, amount: &dyn Fn(&mut Function)| {
        big(f, dst);
        amount(f);
        f.instruction(&Instruction::Call(shl));
    };
    let times = |f: &mut Function, dst: i32, m: i32| {
        big(f, dst);
        f.instruction(&Instruction::I32Const(m));
        f.instruction(&Instruction::Call(mul));
    };
    // v = f * 2**e, f the integer significand.
    ins(&mut *f, Instruction::LocalGet(bits));
    ins(&mut *f, Instruction::I64Const(52));
    ins(&mut *f, Instruction::I64ShrU);
    ins(&mut *f, Instruction::I64Const(0x7ff));
    ins(&mut *f, Instruction::I64And);
    ins(&mut *f, Instruction::I32WrapI64);
    ins(&mut *f, Instruction::LocalSet(expf));
    ins(&mut *f, Instruction::LocalGet(bits));
    ins(&mut *f, Instruction::I64Const(0x000f_ffff_ffff_ffff));
    ins(&mut *f, Instruction::I64And);
    ins(&mut *f, Instruction::LocalSet(fm));
    ins(&mut *f, Instruction::LocalGet(expf));
    ins(&mut *f, Instruction::If(BlockType::Empty));
    ins(&mut *f, Instruction::LocalGet(fm));
    ins(&mut *f, Instruction::I64Const(1 << 52));
    ins(&mut *f, Instruction::I64Or);
    ins(&mut *f, Instruction::LocalSet(fm));
    ins(&mut *f, Instruction::LocalGet(expf));
    ins(&mut *f, Instruction::I32Const(1075));
    ins(&mut *f, Instruction::I32Sub);
    ins(&mut *f, Instruction::LocalSet(e));
    ins(&mut *f, Instruction::Else);
    ins(&mut *f, Instruction::I32Const(-1074));
    ins(&mut *f, Instruction::LocalSet(e));
    ins(&mut *f, Instruction::End);
    // An even significand includes the halfway points in its interval.
    ins(&mut *f, Instruction::LocalGet(fm));
    ins(&mut *f, Instruction::I64Const(1));
    ins(&mut *f, Instruction::I64And);
    ins(&mut *f, Instruction::I64Eqz);
    ins(&mut *f, Instruction::LocalSet(even));
    // The gap below a power of two is half the gap above it.
    ins(&mut *f, Instruction::LocalGet(fm));
    ins(&mut *f, Instruction::I64Const(1 << 52));
    ins(&mut *f, Instruction::I64Eq);
    ins(&mut *f, Instruction::LocalGet(expf));
    ins(&mut *f, Instruction::I32Const(1));
    ins(&mut *f, Instruction::I32GtS);
    ins(&mut *f, Instruction::I32And);
    ins(&mut *f, Instruction::LocalSet(unequal));

    // r, s, m+, m- such that v = r / s and the rounding interval is
    // (r - m-, r + m+) / s.
    let e_plus = |delta: i32| {
        move |f: &mut Function| {
            f.instruction(&Instruction::LocalGet(3));
            f.instruction(&Instruction::I32Const(delta));
            f.instruction(&Instruction::I32Add);
        }
    };
    let minus_e = |delta: i32| {
        move |f: &mut Function| {
            f.instruction(&Instruction::I32Const(delta));
            f.instruction(&Instruction::LocalGet(3));
            f.instruction(&Instruction::I32Sub);
        }
    };
    ins(&mut *f, Instruction::I32Const(BIG_R));
    ins(&mut *f, Instruction::LocalGet(fm));
    ins(&mut *f, Instruction::Call(set));
    ins(&mut *f, Instruction::LocalGet(e));
    ins(&mut *f, Instruction::I32Const(0));
    ins(&mut *f, Instruction::I32GeS);
    ins(&mut *f, Instruction::If(BlockType::Empty));
    ins(&mut *f, Instruction::LocalGet(unequal));
    ins(&mut *f, Instruction::If(BlockType::Empty));
    shl_by(&mut *f, BIG_R, &e_plus(2));
    set_small(&mut *f, BIG_S, 4);
    set_small(&mut *f, BIG_MP, 1);
    shl_by(&mut *f, BIG_MP, &e_plus(1));
    set_small(&mut *f, BIG_MM, 1);
    shl_by(&mut *f, BIG_MM, &e_plus(0));
    ins(&mut *f, Instruction::Else);
    shl_by(&mut *f, BIG_R, &e_plus(1));
    set_small(&mut *f, BIG_S, 2);
    set_small(&mut *f, BIG_MP, 1);
    shl_by(&mut *f, BIG_MP, &e_plus(0));
    set_small(&mut *f, BIG_MM, 1);
    shl_by(&mut *f, BIG_MM, &e_plus(0));
    ins(&mut *f, Instruction::End);
    ins(&mut *f, Instruction::Else);
    ins(&mut *f, Instruction::LocalGet(unequal));
    ins(&mut *f, Instruction::If(BlockType::Empty));
    times(&mut *f, BIG_R, 4);
    set_small(&mut *f, BIG_S, 1);
    shl_by(&mut *f, BIG_S, &minus_e(2));
    set_small(&mut *f, BIG_MP, 2);
    set_small(&mut *f, BIG_MM, 1);
    ins(&mut *f, Instruction::Else);
    times(&mut *f, BIG_R, 2);
    set_small(&mut *f, BIG_S, 1);
    shl_by(&mut *f, BIG_S, &minus_e(1));
    set_small(&mut *f, BIG_MP, 1);
    set_small(&mut *f, BIG_MM, 1);
    ins(&mut *f, Instruction::End);
    ins(&mut *f, Instruction::End);

    // est = ceil(log10(v) - 1e-10), from v's binary exponent, which can only
    // be one too low; the fixup below corrects that.
    ins(&mut *f, Instruction::LocalGet(e));
    ins(&mut *f, Instruction::I32Const(63));
    ins(&mut *f, Instruction::LocalGet(fm));
    ins(&mut *f, Instruction::I64Clz);
    ins(&mut *f, Instruction::I32WrapI64);
    ins(&mut *f, Instruction::I32Sub);
    ins(&mut *f, Instruction::I32Add);
    ins(&mut *f, Instruction::F64ConvertI32S);
    ins(
        &mut *f,
        Instruction::F64Const(0.301_029_995_663_981_14_f64.into()),
    );
    ins(&mut *f, Instruction::F64Mul);
    ins(&mut *f, Instruction::F64Const(1e-10_f64.into()));
    ins(&mut *f, Instruction::F64Sub);
    ins(&mut *f, Instruction::F64Ceil);
    ins(&mut *f, Instruction::I32TruncF64S);
    ins(&mut *f, Instruction::LocalSet(est));
    ins(&mut *f, Instruction::LocalGet(est));
    ins(&mut *f, Instruction::I32Const(0));
    ins(&mut *f, Instruction::I32GeS);
    ins(&mut *f, Instruction::If(BlockType::Empty));
    big(&mut *f, BIG_S);
    ins(&mut *f, Instruction::LocalGet(est));
    ins(&mut *f, Instruction::Call(pow10));
    ins(&mut *f, Instruction::Else);
    for target in [BIG_R, BIG_MP, BIG_MM] {
        big(&mut *f, target);
        ins(&mut *f, Instruction::I32Const(0));
        ins(&mut *f, Instruction::LocalGet(est));
        ins(&mut *f, Instruction::I32Sub);
        ins(&mut *f, Instruction::Call(pow10));
    }
    ins(&mut *f, Instruction::End);

    // Too low (r + m+ reaches s, inclusively for an even significand): the
    // first digit is one place further left.
    let sum_cmp = |f: &mut Function| {
        big(f, BIG_T);
        big(f, BIG_R);
        big(f, BIG_MP);
        f.instruction(&Instruction::Call(add));
        big(f, BIG_T);
        big(f, BIG_S);
        f.instruction(&Instruction::Call(cmp));
    };
    sum_cmp(&mut *f);
    ins(&mut *f, Instruction::LocalTee(c));
    ins(&mut *f, Instruction::I32Const(0));
    ins(&mut *f, Instruction::I32GtS);
    ins(&mut *f, Instruction::LocalGet(c));
    ins(&mut *f, Instruction::I32Eqz);
    ins(&mut *f, Instruction::LocalGet(even));
    ins(&mut *f, Instruction::I32And);
    ins(&mut *f, Instruction::I32Or);
    ins(&mut *f, Instruction::If(BlockType::Empty));
    ins(&mut *f, Instruction::LocalGet(est));
    ins(&mut *f, Instruction::I32Const(1));
    ins(&mut *f, Instruction::I32Add);
    ins(&mut *f, Instruction::LocalSet(k));
    ins(&mut *f, Instruction::Else);
    ins(&mut *f, Instruction::LocalGet(est));
    ins(&mut *f, Instruction::LocalSet(k));
    for target in [BIG_R, BIG_MP, BIG_MM] {
        times(&mut *f, target, 10);
    }
    ins(&mut *f, Instruction::End);
}

/// The shortest digits that read back as `v` (positive, finite, nonzero):
/// Steele and White's free-format digit generation over exact bignums,
/// deciding each digit and the final rounding as Gay's `dtoa` does in mode
/// 0. Leaves the digits (values 0-9) at `DIGITS`, their count at `DIG_N` (also
/// returned), and the decimal-point position at `DIG_K`: the value is
/// 0.DIGITS * 10**k.
pub(crate) fn build_shortest_digits(ctx: &CompilationContext) -> Function {
    let copy = ctx.helper(Helper::BigCopy);
    let mul = ctx.helper(Helper::BigMulSmall);
    let add = ctx.helper(Helper::BigAdd);
    let cmp = ctx.helper(Helper::BigCmp);
    let quorem = ctx.helper(Helper::BigQuoRem);

    // Parameter 0 is v; the locals follow.
    let mut f = function(&float_locals());
    let v = 0;
    let (bits, even, k, d, n, j, j1, c) = (1, 5, 7, 8, 9, 10, 11, 12);
    let ins = |f: &mut Function, i: Instruction| {
        f.instruction(&i);
    };
    let big = |f: &mut Function, addr: i32| {
        f.instruction(&Instruction::I32Const(addr));
    };
    // big op helpers
    let times = |f: &mut Function, dst: i32, m: i32| {
        big(f, dst);
        f.instruction(&Instruction::I32Const(m));
        f.instruction(&Instruction::Call(mul));
    };

    // The caller hands over a positive, finite, nonzero value.
    ins(&mut f, Instruction::LocalGet(v));
    ins(&mut f, Instruction::I64ReinterpretF64);
    ins(&mut f, Instruction::LocalSet(bits));
    emit_float_scale(&mut f, ctx);
    let sum_cmp = |f: &mut Function| {
        big(f, BIG_T);
        big(f, BIG_R);
        big(f, BIG_MP);
        f.instruction(&Instruction::Call(add));
        big(f, BIG_T);
        big(f, BIG_S);
        f.instruction(&Instruction::Call(cmp));
    };

    // Digits, until one of them pins the value down.
    let push = |f: &mut Function, delta: i32| {
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::I32Const(DIGITS));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalGet(d));
        f.instruction(&Instruction::I32Const(delta));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::I32Store8(byte()));
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(n));
    };
    ins(&mut f, Instruction::I32Const(0));
    ins(&mut f, Instruction::LocalSet(n));
    ins(&mut f, Instruction::Block(BlockType::Empty)); // done
    ins(&mut f, Instruction::Loop(BlockType::Empty));
    big(&mut f, BIG_R);
    big(&mut f, BIG_S);
    ins(&mut f, Instruction::Call(quorem));
    ins(&mut f, Instruction::LocalSet(d));
    big(&mut f, BIG_R);
    big(&mut f, BIG_MM);
    ins(&mut f, Instruction::Call(cmp));
    ins(&mut f, Instruction::LocalSet(j));
    sum_cmp(&mut f);
    ins(&mut f, Instruction::LocalSet(j1));
    // r + m+ == s exactly, even significand: the next digit up only if
    // r > m-.
    ins(&mut f, Instruction::LocalGet(j1));
    ins(&mut f, Instruction::I32Eqz);
    ins(&mut f, Instruction::LocalGet(even));
    ins(&mut f, Instruction::I32And);
    ins(&mut f, Instruction::If(BlockType::Empty));
    ins(&mut f, Instruction::LocalGet(d));
    ins(&mut f, Instruction::LocalGet(j));
    ins(&mut f, Instruction::I32Const(0));
    ins(&mut f, Instruction::I32GtS);
    ins(&mut f, Instruction::I32Add);
    ins(&mut f, Instruction::LocalSet(d));
    push(&mut f, 0);
    ins(&mut f, Instruction::Br(2));
    ins(&mut f, Instruction::End);
    // Low enough (r < m-, or equal for an even significand).
    ins(&mut f, Instruction::LocalGet(j));
    ins(&mut f, Instruction::I32Const(0));
    ins(&mut f, Instruction::I32LtS);
    ins(&mut f, Instruction::LocalGet(j));
    ins(&mut f, Instruction::I32Eqz);
    ins(&mut f, Instruction::LocalGet(even));
    ins(&mut f, Instruction::I32And);
    ins(&mut f, Instruction::I32Or);
    ins(&mut f, Instruction::If(BlockType::Empty));
    // ... and high enough too: the nearer of d and d + 1, ties to even.
    ins(&mut f, Instruction::LocalGet(j1));
    ins(&mut f, Instruction::I32Const(0));
    ins(&mut f, Instruction::I32GtS);
    ins(&mut f, Instruction::If(BlockType::Empty));
    big(&mut f, BIG_T);
    big(&mut f, BIG_R);
    ins(&mut f, Instruction::Call(copy));
    times(&mut f, BIG_T, 2);
    big(&mut f, BIG_T);
    big(&mut f, BIG_S);
    ins(&mut f, Instruction::Call(cmp));
    ins(&mut f, Instruction::LocalTee(c));
    ins(&mut f, Instruction::I32Const(0));
    ins(&mut f, Instruction::I32GtS);
    ins(&mut f, Instruction::LocalGet(c));
    ins(&mut f, Instruction::I32Eqz);
    ins(&mut f, Instruction::LocalGet(d));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32And);
    ins(&mut f, Instruction::I32And);
    ins(&mut f, Instruction::I32Or);
    ins(&mut f, Instruction::LocalGet(d));
    ins(&mut f, Instruction::I32Add);
    ins(&mut f, Instruction::LocalSet(d));
    ins(&mut f, Instruction::End);
    push(&mut f, 0);
    ins(&mut f, Instruction::Br(2));
    ins(&mut f, Instruction::End);
    // High enough only.
    ins(&mut f, Instruction::LocalGet(j1));
    ins(&mut f, Instruction::I32Const(0));
    ins(&mut f, Instruction::I32GtS);
    ins(&mut f, Instruction::If(BlockType::Empty));
    push(&mut f, 1);
    ins(&mut f, Instruction::Br(2));
    ins(&mut f, Instruction::End);
    // Neither: keep the digit and go on.
    push(&mut f, 0);
    for target in [BIG_R, BIG_MP, BIG_MM] {
        times(&mut f, target, 10);
    }
    ins(&mut f, Instruction::Br(0));
    ins(&mut f, Instruction::End);
    ins(&mut f, Instruction::End);

    // A last digit of 10 carries into the ones before it; a run of nines
    // that all carry becomes a 1 one place to the left.
    let digit_at = |f: &mut Function, index: &dyn Fn(&mut Function)| {
        index(f);
        f.instruction(&Instruction::I32Const(DIGITS));
        f.instruction(&Instruction::I32Add);
    };
    let last = |f: &mut Function| {
        f.instruction(&Instruction::LocalGet(9));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Sub);
    };
    digit_at(&mut f, &last);
    ins(&mut f, Instruction::I32Load8U(byte()));
    ins(&mut f, Instruction::I32Const(10));
    ins(&mut f, Instruction::I32Eq);
    ins(&mut f, Instruction::If(BlockType::Empty));
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32Sub);
    ins(&mut f, Instruction::LocalSet(n));
    ins(&mut f, Instruction::Block(BlockType::Empty));
    ins(&mut f, Instruction::Loop(BlockType::Empty));
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::I32Eqz);
    ins(&mut f, Instruction::BrIf(1));
    digit_at(&mut f, &last);
    ins(&mut f, Instruction::I32Load8U(byte()));
    ins(&mut f, Instruction::I32Const(9));
    ins(&mut f, Instruction::I32Ne);
    ins(&mut f, Instruction::BrIf(1));
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32Sub);
    ins(&mut f, Instruction::LocalSet(n));
    ins(&mut f, Instruction::Br(0));
    ins(&mut f, Instruction::End);
    ins(&mut f, Instruction::End);
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::If(BlockType::Empty));
    digit_at(&mut f, &last);
    digit_at(&mut f, &last);
    ins(&mut f, Instruction::I32Load8U(byte()));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32Add);
    ins(&mut f, Instruction::I32Store8(byte()));
    ins(&mut f, Instruction::Else);
    ins(&mut f, Instruction::I32Const(DIGITS));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32Store8(byte()));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::LocalSet(n));
    ins(&mut f, Instruction::LocalGet(k));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32Add);
    ins(&mut f, Instruction::LocalSet(k));
    ins(&mut f, Instruction::End);
    ins(&mut f, Instruction::End);
    // Trailing zeros carry nothing.
    ins(&mut f, Instruction::Block(BlockType::Empty));
    ins(&mut f, Instruction::Loop(BlockType::Empty));
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32LeS);
    ins(&mut f, Instruction::BrIf(1));
    digit_at(&mut f, &last);
    ins(&mut f, Instruction::I32Load8U(byte()));
    ins(&mut f, Instruction::BrIf(1));
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::I32Const(1));
    ins(&mut f, Instruction::I32Sub);
    ins(&mut f, Instruction::LocalSet(n));
    ins(&mut f, Instruction::Br(0));
    ins(&mut f, Instruction::End);
    ins(&mut f, Instruction::End);

    ins(&mut f, Instruction::I32Const(DIG_N));
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::I32Store(w32(0)));
    ins(&mut f, Instruction::I32Const(DIG_K));
    ins(&mut f, Instruction::LocalGet(k));
    ins(&mut f, Instruction::I32Store(w32(0)));
    ins(&mut f, Instruction::LocalGet(n));
    ins(&mut f, Instruction::End);
    f
}

// ---------------------------------------------------------------------------
// str() and repr()
// ---------------------------------------------------------------------------

/// Why `str()` (or `repr()`, with `repr`) of a value of `ty` cannot be given
/// CPython's answer here, if it cannot.
pub fn str_unsupported(ctx: &CompilationContext, ty: &IRType, repr: bool) -> Option<String> {
    unsupported_rendering(ctx, ty, repr, false)
}

/// [`str_unsupported`] for a call site that renders through
/// [`emit_rendering_call`], which passes on an exception a `__str__` or
/// `__repr__` raises.
pub fn render_unsupported(ctx: &CompilationContext, ty: &IRType, repr: bool) -> Option<String> {
    unsupported_rendering(ctx, ty, repr, true)
}

/// Whether rendering a value of `ty` runs a `__str__` or `__repr__` (of an
/// instance, or of one inside a collection) that can raise.
pub(crate) fn rendering_can_raise(ctx: &CompilationContext, ty: &IRType, repr: bool) -> bool {
    match ty {
        IRType::List(e) | IRType::Set(e) | IRType::VarTuple(e) => rendering_can_raise(ctx, e, true),
        IRType::Dict(k, v) => {
            rendering_can_raise(ctx, k, true) || rendering_can_raise(ctx, v, true)
        }
        IRType::Tuple(members) => members.iter().any(|m| rendering_can_raise(ctx, m, true)),
        IRType::Optional(inner) => rendering_can_raise(ctx, inner, repr),
        IRType::Class(name) => {
            let Some(info) = ctx.get_class_info(name) else {
                return false;
            };
            let method = if !repr && info.methods.contains_key("__str__") {
                "__str__"
            } else {
                "__repr__"
            };
            let Some(index) = info.methods.get(method) else {
                return false;
            };
            ctx.can_raise.contains(index)
                || ctx.virtual_call(name, method).is_some_and(|(column, _)| {
                    ctx.virtual_implementations(column)
                        .any(|f| ctx.can_raise.contains(&f))
                })
        }
        _ => false,
    }
}

/// Call the rendering helper `index` for a value of `ty` (on the stack) from
/// the program's code: a `__str__` or `__repr__` it runs that raises leaves
/// the exception pending, which unwinds from here as from any call.
pub(crate) fn emit_rendering_call(
    func: &mut Function,
    ctx: &CompilationContext,
    ty: &IRType,
    repr: bool,
    index: u32,
) {
    if !rendering_can_raise(ctx, ty, repr) {
        func.instruction(&Instruction::Call(index));
        return;
    }
    crate::compiler::function::emit_call_depth_step(func, 1);
    func.instruction(&Instruction::Call(index));
    crate::compiler::function::emit_call_depth_step(func, -1);
    func.instruction(&Instruction::LocalSet(ctx.temp_local));
    crate::compiler::function::emit_post_call_check(func, ctx);
    func.instruction(&Instruction::LocalGet(ctx.temp_local));
}

fn unsupported_rendering(
    ctx: &CompilationContext,
    ty: &IRType,
    repr: bool,
    raising_ok: bool,
) -> Option<String> {
    let str_unsupported = |ctx: &CompilationContext, ty: &IRType, repr: bool| {
        unsupported_rendering(ctx, ty, repr, raising_ok)
    };
    match ty {
        IRType::Int
        | IRType::Bool
        | IRType::Float
        | IRType::String
        | IRType::Bytes
        | IRType::None
        | IRType::Unknown
        | IRType::Any
        | IRType::Range
        | IRType::ClassRef(_) => None,
        IRType::List(e) | IRType::Set(e) | IRType::VarTuple(e) => str_unsupported(ctx, e, true),
        IRType::Dict(k, v) => {
            str_unsupported(ctx, k, true).or_else(|| str_unsupported(ctx, v, true))
        }
        IRType::Tuple(members) => members.iter().find_map(|m| str_unsupported(ctx, m, true)),
        IRType::Optional(inner) => match inner.as_ref() {
            // None is the null pointer, which no instance or string is, and an
            // optional number is boxed.
            IRType::Class(_) | IRType::String | IRType::List(_) | IRType::Dict(..) => {
                str_unsupported(ctx, inner, repr)
            }
            IRType::Int | IRType::Bool | IRType::Float => None,
            other => Some(format!(
                "an Optional[{}], whose None and whose 0 are the same word here",
                crate::type_to_string(other)
            )),
        },
        IRType::Class(name) => {
            let info = ctx.get_class_info(name)?;
            let method = if !repr && info.methods.contains_key("__str__") {
                "__str__"
            } else {
                "__repr__"
            };
            match info.methods.get(method) {
                None => Some(format!(
                    "an instance of '{name}', which defines no __repr__ (CPython's default \
                     shows its memory address, which nothing here reproduces). Hint: define \
                     __repr__, or make it a @dataclass"
                )),
                Some(index) if !raising_ok && ctx.can_raise.contains(index) => {
                    Some(format!("an instance of '{name}', whose {method} can raise"))
                }
                Some(_) => None,
            }
        }
        other => Some(format!(
            "a value of type '{}'",
            crate::type_to_string(other)
        )),
    }
}

/// Copy the string at `piece` (a local holding a string offset) to the
/// cursor in `p`, advancing it. Uses `len` as scratch.
fn copy_piece(f: &mut Function, p: u32, piece: u32, len: u32) {
    f.instruction(&Instruction::LocalGet(piece));
    f.instruction(&Instruction::I32Const(4));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::I32Load(w32(0)));
    f.instruction(&Instruction::LocalSet(len));
    f.instruction(&Instruction::LocalGet(p));
    f.instruction(&Instruction::LocalGet(piece));
    f.instruction(&Instruction::LocalGet(len));
    f.instruction(&Instruction::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
    f.instruction(&Instruction::LocalGet(p));
    f.instruction(&Instruction::LocalGet(len));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(p));
}

/// Write the bytes of `text` at the cursor in `p`, advancing it.
fn put_text(f: &mut Function, p: u32, text: &str) {
    for b in text.bytes() {
        f.instruction(&Instruction::LocalGet(p));
        f.instruction(&Instruction::I32Const(b as i32));
        f.instruction(&Instruction::I32Store8(byte()));
        f.instruction(&Instruction::LocalGet(p));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(p));
    }
}

/// Push the string offset `repr` (or `str`) of the value of `ty` on the stack
/// gives, by calling its helper.
fn call_str_of(f: &mut Function, ctx: &CompilationContext, ty: &IRType, repr: bool) {
    let ty = match ty {
        IRType::Unknown | IRType::Any => IRType::Int,
        other => other.clone(),
    };
    let raises = rendering_can_raise(ctx, &ty, repr);
    let helper = if repr {
        Helper::Repr(ty)
    } else {
        Helper::Str(ty)
    };
    f.instruction(&Instruction::Call(ctx.helper(helper)));
    // A member's `__repr__` that raised: stop, with the exception pending.
    if raises {
        f.instruction(&Instruction::GlobalGet(
            crate::compiler::context::EXC_TYPE_GLOBAL,
        ));
        f.instruction(&Instruction::If(BlockType::Empty));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::Return);
        f.instruction(&Instruction::End);
    }
}

/// Load the value of `ty` at the slot address on the stack: an f64 for a
/// float, a word otherwise.
fn load_slot(f: &mut Function, ty: &IRType) {
    if matches!(ty, IRType::Float) {
        f.instruction(&Instruction::F64Load(mem(0, 3)));
    } else {
        f.instruction(&Instruction::I32Load(w32(0)));
    }
}

/// `str(v)` or `repr(v)` for a value of `ty`.
fn build_str(ctx: &CompilationContext, ty: &IRType, repr: bool) -> Function {
    use ValType::I32;
    match ty {
        IRType::Float => {
            let mut f = function(&[]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::Call(ctx.helper(Helper::FloatRepr)));
            f.instruction(&Instruction::End);
            f
        }
        IRType::String if repr => {
            let mut f = function(&[]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::Call(ctx.helper(Helper::StrRepr)));
            f.instruction(&Instruction::End);
            f
        }
        IRType::String => {
            let mut f = function(&[]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::End);
            f
        }
        IRType::Bytes => {
            let mut f = function(&[]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::Call(ctx.helper(Helper::BytesRepr)));
            f.instruction(&Instruction::End);
            f
        }
        // A class shows as `<class '__main__.A'>`, `str` and `repr` alike.
        IRType::ClassRef(_) => {
            let mut f = function(&[]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::Call(
                ctx.helper(Helper::ClassNameOf { repr: true }),
            ));
            f.instruction(&Instruction::End);
            f
        }
        IRType::Bool | IRType::None => {
            // locals: len (1), out (2)
            let mut f = function(&[I32, I32]);
            if matches!(ty, IRType::None) {
                return_text(&mut f, ctx, "None", 1, 2);
            } else {
                f.instruction(&Instruction::LocalGet(0));
                f.instruction(&Instruction::If(BlockType::Empty));
                return_text(&mut f, ctx, "True", 1, 2);
                f.instruction(&Instruction::End);
                return_text(&mut f, ctx, "False", 1, 2);
            }
            f.instruction(&Instruction::I32Const(0));
            f.instruction(&Instruction::End);
            f
        }
        IRType::Optional(inner) => {
            // locals: len (1), out (2)
            let mut f = function(&[I32, I32]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::I32Eqz);
            f.instruction(&Instruction::If(BlockType::Empty));
            return_text(&mut f, ctx, "None", 1, 2);
            f.instruction(&Instruction::End);
            f.instruction(&Instruction::LocalGet(0));
            // An optional number is a cell holding it.
            match inner.as_ref() {
                IRType::Float => {
                    f.instruction(&Instruction::F64Load(mem(0, 3)));
                }
                IRType::Int | IRType::Bool => {
                    f.instruction(&Instruction::I32Load(w32(0)));
                }
                _ => {}
            }
            call_str_of(&mut f, ctx, inner, repr);
            f.instruction(&Instruction::End);
            f
        }
        IRType::Class(name) => {
            let mut f = function(&[]);
            let info = ctx.get_class_info(name);
            let method = match info {
                Some(info) if !repr && info.methods.contains_key("__str__") => "__str__",
                _ => "__repr__",
            };
            f.instruction(&Instruction::LocalGet(0));
            match ctx.virtual_call(name, method) {
                Some((column, type_index)) => {
                    f.instruction(&Instruction::LocalGet(0));
                    f.instruction(&Instruction::I32Load(w32(0)));
                    f.instruction(&Instruction::I32Const(1));
                    f.instruction(&Instruction::I32Sub);
                    f.instruction(&Instruction::I32Const(ctx.vtable_stride as i32));
                    f.instruction(&Instruction::I32Mul);
                    f.instruction(&Instruction::I32Const((ctx.vtable_base + column) as i32));
                    f.instruction(&Instruction::I32Add);
                    f.instruction(&Instruction::CallIndirect {
                        type_index,
                        table_index: 0,
                    });
                }
                None => match info.and_then(|i| i.methods.get(method)) {
                    Some(index) => {
                        f.instruction(&Instruction::Call(*index));
                    }
                    None => {
                        f.instruction(&Instruction::Unreachable);
                    }
                },
            }
            f.instruction(&Instruction::End);
            f
        }
        IRType::Range => build_range_repr(ctx),
        IRType::List(_)
        | IRType::VarTuple(_)
        | IRType::Set(_)
        | IRType::Dict(..)
        | IRType::Tuple(_) => build_container_repr(ctx, ty),
        // Ints, and the untyped words that read as ints.
        _ => {
            let mut f = function(&[]);
            f.instruction(&Instruction::LocalGet(0));
            f.instruction(&Instruction::Call(ctx.i32_to_str_func_index));
            f.instruction(&Instruction::End);
            f
        }
    }
}

/// `range(start, stop)` or `range(start, stop, step)`.
fn build_range_repr(ctx: &CompilationContext) -> Function {
    use ValType::I32;
    // param 0: the range; locals: a (1), b (2), c (3), total (4), out (5),
    // p (6), len (7)
    let mut f = function(&[I32, I32, I32, I32, I32, I32, I32]);
    let (r, a, b, c, total, out, p, len) = (0, 1, 2, 3, 4, 5, 6, 7);
    for (local, offset) in [(a, 0), (b, 4), (c, 8)] {
        f.instruction(&Instruction::LocalGet(r));
        f.instruction(&Instruction::I32Load(w32(offset)));
        f.instruction(&Instruction::Call(ctx.i32_to_str_func_index));
        f.instruction(&Instruction::LocalSet(local));
    }
    let length = |f: &mut Function, piece: u32| {
        f.instruction(&Instruction::LocalGet(piece));
        f.instruction(&Instruction::I32Const(4));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::I32Load(w32(0)));
    };
    // "range(" a ", " b [", " c] ")"
    f.instruction(&Instruction::I32Const(9));
    length(&mut f, a);
    f.instruction(&Instruction::I32Add);
    length(&mut f, b);
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(total));
    f.instruction(&Instruction::LocalGet(r));
    f.instruction(&Instruction::I32Load(w32(8)));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(total));
    f.instruction(&Instruction::I32Const(2));
    f.instruction(&Instruction::I32Add);
    length(&mut f, c);
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(total));
    f.instruction(&Instruction::End);
    alloc_string(&mut f, ctx, total, out);
    f.instruction(&Instruction::LocalGet(out));
    f.instruction(&Instruction::LocalSet(p));
    put_text(&mut f, p, "range(");
    copy_piece(&mut f, p, a, len);
    put_text(&mut f, p, ", ");
    copy_piece(&mut f, p, b, len);
    f.instruction(&Instruction::LocalGet(r));
    f.instruction(&Instruction::I32Load(w32(8)));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Ne);
    f.instruction(&Instruction::If(BlockType::Empty));
    put_text(&mut f, p, ", ");
    copy_piece(&mut f, p, c, len);
    f.instruction(&Instruction::End);
    put_text(&mut f, p, ")");
    f.instruction(&Instruction::LocalGet(out));
    f.instruction(&Instruction::End);
    f
}

/// The repr of a list, tuple, set, or dict: the repr of each member, joined
/// with ", " (and ": " between a key and its value) inside the container's
/// brackets. A one-member tuple keeps its comma; an empty set is `set()`.
fn build_container_repr(ctx: &CompilationContext, ty: &IRType) -> Function {
    use ValType::I32;
    // param 0: the container; locals: n (1), i (2), pieces (3), total (4),
    // out (5), p (6), piece (7), len (8), addr (9), cap (10)
    let mut f = function(&[I32, I32, I32, I32, I32, I32, I32, I32, I32, I32]);
    let (v, n, i, pieces, total, out, p, piece, len, addr, cap) =
        (0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10);
    let (open, close, empty) = match ty {
        IRType::List(_) => ("[", "]", "[]"),
        IRType::Set(_) => ("{", "}", "set()"),
        IRType::Dict(..) => ("{", "}", "{}"),
        _ => ("(", ")", "()"),
    };
    let per_entry = if matches!(ty, IRType::Dict(..)) { 2 } else { 1 };
    // n = the member count; pieces = room for each member's repr (two per
    // dict entry).
    f.instruction(&Instruction::LocalGet(v));
    f.instruction(&Instruction::I32Load(w32(0)));
    f.instruction(&Instruction::LocalTee(n));
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::If(BlockType::Empty));
    return_text(&mut f, ctx, empty, len, out);
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::LocalGet(n));
    f.instruction(&Instruction::I32Const(4 * per_entry));
    f.instruction(&Instruction::I32Mul);
    f.instruction(&Instruction::Call(ctx.alloc_func_index));
    f.instruction(&Instruction::LocalSet(pieces));
    // total = brackets + separators, then each piece's length.
    f.instruction(&Instruction::LocalGet(n));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::I32Const(2));
    f.instruction(&Instruction::I32Mul);
    f.instruction(&Instruction::I32Const(2));
    f.instruction(&Instruction::I32Add);
    if per_entry == 2 {
        // ": " per entry
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::I32Const(2));
        f.instruction(&Instruction::I32Mul);
        f.instruction(&Instruction::I32Add);
    }
    if matches!(ty, IRType::Tuple(_) | IRType::VarTuple(_)) {
        // "(x,)"
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::I32Add);
    }
    f.instruction(&Instruction::LocalSet(total));

    // Store piece `slot` (a word index into `pieces`) from the string offset
    // on the stack, adding its length to the total.
    let store_piece = |f: &mut Function, slot: &dyn Fn(&mut Function)| {
        f.instruction(&Instruction::LocalSet(piece));
        f.instruction(&Instruction::LocalGet(pieces));
        slot(f);
        f.instruction(&Instruction::I32Const(2));
        f.instruction(&Instruction::I32Shl);
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalGet(piece));
        f.instruction(&Instruction::I32Store(w32(0)));
        f.instruction(&Instruction::LocalGet(total));
        f.instruction(&Instruction::LocalGet(piece));
        f.instruction(&Instruction::I32Const(4));
        f.instruction(&Instruction::I32Sub);
        f.instruction(&Instruction::I32Load(w32(0)));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(total));
    };
    let index = |f: &mut Function| {
        f.instruction(&Instruction::LocalGet(2));
    };
    match ty {
        IRType::Tuple(members) => {
            for (k, member) in members.iter().enumerate() {
                f.instruction(&Instruction::LocalGet(v));
                f.instruction(&Instruction::I32Load(w32(COLLECTION_DATA as u64)));
                f.instruction(&Instruction::I32Const((k as u32 * COLLECTION_SLOT) as i32));
                f.instruction(&Instruction::I32Add);
                load_slot(&mut f, member);
                call_str_of(&mut f, ctx, member, true);
                store_piece(&mut f, &|f: &mut Function| {
                    f.instruction(&Instruction::I32Const(k as i32));
                });
            }
        }
        IRType::List(e) | IRType::VarTuple(e) => {
            limb_count_loop(&mut f, i, n, |f| {
                f.instruction(&Instruction::LocalGet(v));
                f.instruction(&Instruction::I32Load(w32(COLLECTION_DATA as u64)));
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                f.instruction(&Instruction::I32Mul);
                f.instruction(&Instruction::I32Add);
                load_slot(f, e);
                call_str_of(f, ctx, e, true);
                store_piece(f, &index);
            });
        }
        IRType::Dict(key, value) => {
            limb_count_loop(&mut f, i, n, |f| {
                for (which, member) in [(0u32, key.as_ref()), (1, value.as_ref())] {
                    f.instruction(&Instruction::LocalGet(v));
                    f.instruction(&Instruction::I32Load(w32(COLLECTION_DATA as u64)));
                    f.instruction(&Instruction::LocalGet(i));
                    f.instruction(&Instruction::I32Const(DICT_ENTRY as i32));
                    f.instruction(&Instruction::I32Mul);
                    f.instruction(&Instruction::I32Add);
                    if which == 1 {
                        f.instruction(&Instruction::I32Const(COLLECTION_SLOT as i32));
                        f.instruction(&Instruction::I32Add);
                    }
                    load_slot(f, member);
                    call_str_of(f, ctx, member, true);
                    store_piece(f, &|f: &mut Function| {
                        f.instruction(&Instruction::LocalGet(2));
                        f.instruction(&Instruction::I32Const(1));
                        f.instruction(&Instruction::I32Shl);
                        f.instruction(&Instruction::I32Const(which as i32));
                        f.instruction(&Instruction::I32Add);
                    });
                }
            });
        }
        IRType::Set(e) => {
            // The live buckets in table order; `addr` counts the pieces.
            use crate::compiler::sets::{
                SET_BUCKET, SET_BUCKET_VALUE, SET_CAP, SET_DATA, SET_LIVE,
            };
            f.instruction(&Instruction::LocalGet(v));
            f.instruction(&Instruction::I32Load(w32(SET_CAP as u64)));
            f.instruction(&Instruction::LocalSet(cap));
            f.instruction(&Instruction::I32Const(0));
            f.instruction(&Instruction::LocalSet(addr));
            limb_count_loop(&mut f, i, cap, |f| {
                f.instruction(&Instruction::LocalGet(v));
                f.instruction(&Instruction::I32Load(w32(SET_DATA as u64)));
                f.instruction(&Instruction::LocalGet(i));
                f.instruction(&Instruction::I32Const(SET_BUCKET as i32));
                f.instruction(&Instruction::I32Mul);
                f.instruction(&Instruction::I32Add);
                f.instruction(&Instruction::LocalTee(piece));
                f.instruction(&Instruction::I32Load(w32(0)));
                f.instruction(&Instruction::I32Const(SET_LIVE));
                f.instruction(&Instruction::I32Eq);
                f.instruction(&Instruction::If(BlockType::Empty));
                f.instruction(&Instruction::LocalGet(piece));
                f.instruction(&Instruction::I32Const(SET_BUCKET_VALUE as i32));
                f.instruction(&Instruction::I32Add);
                load_slot(f, e);
                call_str_of(f, ctx, e, true);
                store_piece(f, &|f: &mut Function| {
                    f.instruction(&Instruction::LocalGet(9));
                });
                f.instruction(&Instruction::LocalGet(addr));
                f.instruction(&Instruction::I32Const(1));
                f.instruction(&Instruction::I32Add);
                f.instruction(&Instruction::LocalSet(addr));
                f.instruction(&Instruction::End);
            });
        }
        _ => {}
    }

    alloc_string(&mut f, ctx, total, out);
    f.instruction(&Instruction::LocalGet(out));
    f.instruction(&Instruction::LocalSet(p));
    put_text(&mut f, p, open);
    // Each piece, with ", " between and ": " inside a dict entry.
    f.instruction(&Instruction::LocalGet(n));
    f.instruction(&Instruction::I32Const(per_entry));
    f.instruction(&Instruction::I32Mul);
    f.instruction(&Instruction::LocalSet(cap));
    limb_count_loop(&mut f, i, cap, |f| {
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::If(BlockType::Empty));
        if per_entry == 2 {
            f.instruction(&Instruction::LocalGet(i));
            f.instruction(&Instruction::I32Const(1));
            f.instruction(&Instruction::I32And);
            f.instruction(&Instruction::If(BlockType::Empty));
            put_text(f, p, ": ");
            f.instruction(&Instruction::Else);
            put_text(f, p, ", ");
            f.instruction(&Instruction::End);
        } else {
            put_text(f, p, ", ");
        }
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::LocalGet(pieces));
        f.instruction(&Instruction::LocalGet(i));
        f.instruction(&Instruction::I32Const(2));
        f.instruction(&Instruction::I32Shl);
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::I32Load(w32(0)));
        f.instruction(&Instruction::LocalSet(piece));
        copy_piece(f, p, piece, len);
    });
    if matches!(ty, IRType::Tuple(_) | IRType::VarTuple(_)) {
        f.instruction(&Instruction::LocalGet(n));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::If(BlockType::Empty));
        put_text(&mut f, p, ",");
        f.instruction(&Instruction::End);
    }
    put_text(&mut f, p, close);
    f.instruction(&Instruction::LocalGet(out));
    f.instruction(&Instruction::End);
    f
}

/// `for i in 0..n` over locals `i` and `n`.
fn limb_count_loop(f: &mut Function, i: u32, n: u32, body: impl FnOnce(&mut Function)) {
    f.instruction(&Instruction::I32Const(0));
    f.instruction(&Instruction::LocalSet(i));
    f.instruction(&Instruction::Block(BlockType::Empty));
    f.instruction(&Instruction::Loop(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::LocalGet(n));
    f.instruction(&Instruction::I32GeS);
    f.instruction(&Instruction::BrIf(1));
    body(f);
    f.instruction(&Instruction::LocalGet(i));
    f.instruction(&Instruction::I32Const(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(i));
    f.instruction(&Instruction::Br(0));
    f.instruction(&Instruction::End);
    f.instruction(&Instruction::End);
}

/// The code points CPython's `str.isprintable` rejects that this escapes in a
/// string's repr: the C1 controls, Unicode's space separators other than the
/// ASCII space, the line and paragraph separators, the format characters, the
/// surrogates, and the private-use and noncharacter ranges. (An unassigned
/// code point is also unprintable to CPython; it is not listed here, and
/// prints as itself.)
const UNPRINTABLE: &[(u32, u32)] = &[
    (0x80, 0xa0),
    (0xad, 0xad),
    (0x600, 0x605),
    (0x61c, 0x61c),
    (0x6dd, 0x6dd),
    (0x70f, 0x70f),
    (0x890, 0x891),
    (0x8e2, 0x8e2),
    (0x1680, 0x1680),
    (0x180e, 0x180e),
    (0x2000, 0x200f),
    (0x2028, 0x202f),
    (0x205f, 0x2064),
    (0x2066, 0x206f),
    (0x3000, 0x3000),
    (0xd800, 0xf8ff),
    (0xfeff, 0xfeff),
    (0xfff9, 0xfffb),
    (0xfffe, 0xffff),
    (0x110bd, 0x110bd),
    (0x110cd, 0x110cd),
    (0x13430, 0x1343f),
    (0x1bca0, 0x1bca3),
    (0x1d173, 0x1d17a),
    (0xe0000, 0xe00ff),
    (0xf0000, 0x10ffff),
];

/// `repr` of a string (quoted, escaping the backslash, the quote, `\t \n \r`,
/// other controls as `\xNN`, and the unprintable code points above as
/// `\xNN`/`\uNNNN`/`\UNNNNNNNN`), or of bytes (`b'...'`, every byte outside
/// printable ASCII as `\xNN`). The quote is `'` unless the text holds a `'`
/// and no `"`, as CPython chooses.
fn build_text_repr(ctx: &CompilationContext, bytes: bool) -> Function {
    use ValType::I32;
    // param 0: the offset; locals: n (1), i (2), quote (3), total (4),
    // out (5), p (6), b (7), cp (8), w (9), sq (10), dq (11), esc (12)
    let mut f = function(&[I32, I32, I32, I32, I32, I32, I32, I32, I32, I32, I32, I32]);
    let (s, n, i, quote, total, out, p, b, cp, w, sq, dq, esc) =
        (0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);
    let get = |f: &mut Function, l: u32| {
        f.instruction(&Instruction::LocalGet(l));
    };
    let byte_at = |f: &mut Function, offset: i32| {
        f.instruction(&Instruction::LocalGet(0));
        f.instruction(&Instruction::LocalGet(2));
        f.instruction(&Instruction::I32Add);
        if offset != 0 {
            f.instruction(&Instruction::I32Const(offset));
            f.instruction(&Instruction::I32Add);
        }
        f.instruction(&Instruction::I32Load8U(byte()));
    };
    f.instruction(&Instruction::LocalGet(s));
    f.instruction(&Instruction::I32Const(4));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::I32Load(w32(0)));
    f.instruction(&Instruction::LocalSet(n));
    // Which quotes the text holds.
    limb_count_loop(&mut f, i, n, |f| {
        byte_at(f, 0);
        f.instruction(&Instruction::LocalTee(b));
        f.instruction(&Instruction::I32Const(b'\'' as i32));
        f.instruction(&Instruction::I32Eq);
        get(f, sq);
        f.instruction(&Instruction::I32Or);
        f.instruction(&Instruction::LocalSet(sq));
        get(f, b);
        f.instruction(&Instruction::I32Const(b'"' as i32));
        f.instruction(&Instruction::I32Eq);
        get(f, dq);
        f.instruction(&Instruction::I32Or);
        f.instruction(&Instruction::LocalSet(dq));
    });
    f.instruction(&Instruction::I32Const(b'"' as i32));
    f.instruction(&Instruction::I32Const(b'\'' as i32));
    get(&mut f, sq);
    get(&mut f, dq);
    f.instruction(&Instruction::I32Eqz);
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::Select);
    f.instruction(&Instruction::LocalSet(quote));

    // For the code unit at i: its width in the text (w), the code point (cp),
    // and how it is written (esc: 0 as itself, 1 a two-character escape, 2
    // \xNN, 3 \uNNNN, 4 \UNNNNNNNN).
    let classify = |f: &mut Function| {
        byte_at(f, 0);
        f.instruction(&Instruction::LocalSet(b));
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::LocalSet(w));
        get(f, b);
        f.instruction(&Instruction::LocalSet(cp));
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalSet(esc));
        get(f, b);
        f.instruction(&Instruction::I32Const(0x80));
        f.instruction(&Instruction::I32LtU);
        f.instruction(&Instruction::If(BlockType::Empty));
        {
            // \\ and the quote; \t \n \r; other controls
            for ch in *b"\\\t\n\r" {
                get(f, b);
                f.instruction(&Instruction::I32Const(ch as i32));
                f.instruction(&Instruction::I32Eq);
                f.instruction(&Instruction::If(BlockType::Empty));
                f.instruction(&Instruction::I32Const(1));
                f.instruction(&Instruction::LocalSet(esc));
                f.instruction(&Instruction::End);
            }
            get(f, b);
            get(f, quote);
            f.instruction(&Instruction::I32Eq);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::I32Const(1));
            f.instruction(&Instruction::LocalSet(esc));
            f.instruction(&Instruction::End);
            get(f, esc);
            f.instruction(&Instruction::I32Eqz);
            get(f, b);
            f.instruction(&Instruction::I32Const(0x20));
            f.instruction(&Instruction::I32LtU);
            get(f, b);
            f.instruction(&Instruction::I32Const(0x7f));
            f.instruction(&Instruction::I32Eq);
            f.instruction(&Instruction::I32Or);
            f.instruction(&Instruction::I32And);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::I32Const(2));
            f.instruction(&Instruction::LocalSet(esc));
            f.instruction(&Instruction::End);
        }
        f.instruction(&Instruction::Else);
        if bytes {
            f.instruction(&Instruction::I32Const(2));
            f.instruction(&Instruction::LocalSet(esc));
        } else {
            // Decode UTF-8: the lead byte gives the width.
            get(f, b);
            f.instruction(&Instruction::I32Const(0xe0));
            f.instruction(&Instruction::I32LtU);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::I32Const(2));
            f.instruction(&Instruction::LocalSet(w));
            get(f, b);
            f.instruction(&Instruction::I32Const(0x1f));
            f.instruction(&Instruction::I32And);
            f.instruction(&Instruction::LocalSet(cp));
            f.instruction(&Instruction::Else);
            get(f, b);
            f.instruction(&Instruction::I32Const(0xf0));
            f.instruction(&Instruction::I32LtU);
            f.instruction(&Instruction::If(BlockType::Empty));
            f.instruction(&Instruction::I32Const(3));
            f.instruction(&Instruction::LocalSet(w));
            get(f, b);
            f.instruction(&Instruction::I32Const(0x0f));
            f.instruction(&Instruction::I32And);
            f.instruction(&Instruction::LocalSet(cp));
            f.instruction(&Instruction::Else);
            f.instruction(&Instruction::I32Const(4));
            f.instruction(&Instruction::LocalSet(w));
            get(f, b);
            f.instruction(&Instruction::I32Const(0x07));
            f.instruction(&Instruction::I32And);
            f.instruction(&Instruction::LocalSet(cp));
            f.instruction(&Instruction::End);
            f.instruction(&Instruction::End);
            for k in 1..4 {
                get(f, w);
                f.instruction(&Instruction::I32Const(k));
                f.instruction(&Instruction::I32GtS);
                f.instruction(&Instruction::If(BlockType::Empty));
                get(f, cp);
                f.instruction(&Instruction::I32Const(6));
                f.instruction(&Instruction::I32Shl);
                byte_at(f, k);
                f.instruction(&Instruction::I32Const(0x3f));
                f.instruction(&Instruction::I32And);
                f.instruction(&Instruction::I32Or);
                f.instruction(&Instruction::LocalSet(cp));
                f.instruction(&Instruction::End);
            }
            for &(lo, hi) in UNPRINTABLE {
                get(f, cp);
                f.instruction(&Instruction::I32Const(lo as i32));
                f.instruction(&Instruction::I32GeU);
                get(f, cp);
                f.instruction(&Instruction::I32Const(hi as i32));
                f.instruction(&Instruction::I32LeU);
                f.instruction(&Instruction::I32And);
                f.instruction(&Instruction::If(BlockType::Empty));
                // \xNN below 0x100, \uNNNN below 0x10000, else \U
                f.instruction(&Instruction::I32Const(4));
                f.instruction(&Instruction::I32Const(3));
                get(f, cp);
                f.instruction(&Instruction::I32Const(0x10000));
                f.instruction(&Instruction::I32GeU);
                f.instruction(&Instruction::Select);
                f.instruction(&Instruction::I32Const(2));
                get(f, cp);
                f.instruction(&Instruction::I32Const(0x100));
                f.instruction(&Instruction::I32GeU);
                f.instruction(&Instruction::Select);
                f.instruction(&Instruction::LocalSet(esc));
                f.instruction(&Instruction::End);
            }
        }
        f.instruction(&Instruction::End);
    };
    // How many characters the unit at i writes, per esc.
    let width = |f: &mut Function| {
        // [w, 2, 4, 6, 10][esc]
        get(f, esc);
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::If(BlockType::Result(I32)));
        get(f, w);
        f.instruction(&Instruction::Else);
        get(f, esc);
        f.instruction(&Instruction::I32Const(4));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::If(BlockType::Result(I32)));
        f.instruction(&Instruction::I32Const(10));
        f.instruction(&Instruction::Else);
        get(f, esc);
        f.instruction(&Instruction::I32Const(2));
        f.instruction(&Instruction::I32Mul);
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
    };
    // Walk the text by units.
    let walk = |f: &mut Function, each: &dyn Fn(&mut Function)| {
        f.instruction(&Instruction::I32Const(0));
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Block(BlockType::Empty));
        f.instruction(&Instruction::Loop(BlockType::Empty));
        get(f, i);
        get(f, n);
        f.instruction(&Instruction::I32GeU);
        f.instruction(&Instruction::BrIf(1));
        classify(f);
        each(f);
        get(f, i);
        get(f, w);
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(i));
        f.instruction(&Instruction::Br(0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
    };
    f.instruction(&Instruction::I32Const(if bytes { 3 } else { 2 }));
    f.instruction(&Instruction::LocalSet(total));
    walk(&mut f, &|f: &mut Function| {
        get(f, total);
        width(f);
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(total));
    });
    alloc_string(&mut f, ctx, total, out);
    get(&mut f, out);
    f.instruction(&Instruction::LocalSet(p));
    let put = |f: &mut Function, value: &dyn Fn(&mut Function)| {
        get(f, p);
        value(f);
        f.instruction(&Instruction::I32Store8(byte()));
        get(f, p);
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(p));
    };
    let hex_digit = |f: &mut Function, shift: i32| {
        // "0123456789abcdef"[(cp >> shift) & 15]
        get(f, cp);
        f.instruction(&Instruction::I32Const(shift));
        f.instruction(&Instruction::I32ShrU);
        f.instruction(&Instruction::I32Const(15));
        f.instruction(&Instruction::I32And);
        f.instruction(&Instruction::LocalTee(b));
        f.instruction(&Instruction::I32Const(b'0' as i32));
        f.instruction(&Instruction::I32Add);
        get(f, b);
        f.instruction(&Instruction::I32Const(b'a' as i32 - 10));
        f.instruction(&Instruction::I32Add);
        get(f, b);
        f.instruction(&Instruction::I32Const(10));
        f.instruction(&Instruction::I32LtU);
        f.instruction(&Instruction::Select);
    };
    if bytes {
        put(&mut f, &|f: &mut Function| {
            f.instruction(&Instruction::I32Const(b'b' as i32));
        });
    }
    put(&mut f, &|f: &mut Function| get(f, quote));
    walk(&mut f, &|f: &mut Function| {
        get(f, esc);
        f.instruction(&Instruction::I32Eqz);
        f.instruction(&Instruction::If(BlockType::Empty));
        // the unit as itself
        get(f, p);
        f.instruction(&Instruction::LocalGet(0));
        get(f, i);
        f.instruction(&Instruction::I32Add);
        get(f, w);
        f.instruction(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        get(f, p);
        get(f, w);
        f.instruction(&Instruction::I32Add);
        f.instruction(&Instruction::LocalSet(p));
        f.instruction(&Instruction::Else);
        put(f, &|f: &mut Function| {
            f.instruction(&Instruction::I32Const(b'\\' as i32));
        });
        get(f, esc);
        f.instruction(&Instruction::I32Const(1));
        f.instruction(&Instruction::I32Eq);
        f.instruction(&Instruction::If(BlockType::Empty));
        // \\, \', \", \t, \n, \r
        put(f, &|f: &mut Function| {
            for (from, to) in [(b'\t', b't'), (b'\n', b'n'), (b'\r', b'r')] {
                get(f, cp);
                f.instruction(&Instruction::I32Const(from as i32));
                f.instruction(&Instruction::I32Eq);
                f.instruction(&Instruction::If(BlockType::Result(I32)));
                f.instruction(&Instruction::I32Const(to as i32));
                f.instruction(&Instruction::Else);
            }
            get(f, cp);
            for _ in 0..3 {
                f.instruction(&Instruction::End);
            }
        });
        f.instruction(&Instruction::Else);
        // x, u, or U, then the hex digits
        put(f, &|f: &mut Function| {
            // esc == 4 ? 'U' : esc == 3 ? 'u' : 'x'
            f.instruction(&Instruction::I32Const(b'U' as i32));
            f.instruction(&Instruction::I32Const(b'u' as i32));
            f.instruction(&Instruction::I32Const(b'x' as i32));
            get(f, esc);
            f.instruction(&Instruction::I32Const(3));
            f.instruction(&Instruction::I32Eq);
            f.instruction(&Instruction::Select);
            get(f, esc);
            f.instruction(&Instruction::I32Const(4));
            f.instruction(&Instruction::I32Eq);
            f.instruction(&Instruction::Select);
        });
        for (digits, threshold) in [(8, 4), (4, 3)] {
            get(f, esc);
            f.instruction(&Instruction::I32Const(threshold));
            f.instruction(&Instruction::I32GeS);
            f.instruction(&Instruction::If(BlockType::Empty));
            for k in (2..digits).rev() {
                if digits == 8 && k < 4 {
                    break;
                }
                put(f, &|f: &mut Function| hex_digit(f, k * 4));
            }
            f.instruction(&Instruction::End);
        }
        put(f, &|f: &mut Function| hex_digit(f, 4));
        put(f, &|f: &mut Function| hex_digit(f, 0));
        f.instruction(&Instruction::End);
        f.instruction(&Instruction::End);
    });
    put(&mut f, &|f: &mut Function| get(f, quote));
    get(&mut f, out);
    f.instruction(&Instruction::End);
    f
}

/// `(dict) -> list`: a new list of the dict's keys, in insertion order, each
/// slot copied whole (a float key included). What a walk over a dict visits.
fn build_dict_keys(ctx: &CompilationContext) -> Function {
    use crate::compiler::context::{COLLECTION_CAP, COLLECTION_HEADER};
    let mut f = function(&[ValType::I32; 4]);
    let d = 0;
    let (count, list, i, data) = (1, 2, 3, 4);
    let ins = |f: &mut Function, list: &[Instruction]| {
        for i in list {
            f.instruction(i);
        }
    };
    ins(
        &mut f,
        &[
            Instruction::LocalGet(d),
            Instruction::I32Load(w32(0)),
            Instruction::LocalTee(count),
            Instruction::I32Const(COLLECTION_SLOT as i32),
            Instruction::I32Mul,
            Instruction::I32Const(COLLECTION_HEADER as i32),
            Instruction::I32Add,
            Instruction::Call(ctx.alloc_func_index),
            Instruction::LocalTee(list),
            Instruction::LocalGet(count),
            Instruction::I32Store(w32(0)),
            Instruction::LocalGet(list),
            Instruction::LocalGet(count),
            Instruction::I32Store(w32(COLLECTION_CAP as u64)),
            Instruction::LocalGet(list),
            Instruction::LocalGet(list),
            Instruction::I32Const(COLLECTION_HEADER as i32),
            Instruction::I32Add,
            Instruction::I32Store(w32(COLLECTION_DATA as u64)),
            Instruction::LocalGet(d),
            Instruction::I32Load(w32(COLLECTION_DATA as u64)),
            Instruction::LocalSet(data),
            Instruction::Block(BlockType::Empty),
            Instruction::Loop(BlockType::Empty),
            Instruction::LocalGet(i),
            Instruction::LocalGet(count),
            Instruction::I32GeU,
            Instruction::BrIf(1),
            // list slot i = dict entry i's key slot
            Instruction::LocalGet(list),
            Instruction::LocalGet(i),
            Instruction::I32Const(COLLECTION_SLOT as i32),
            Instruction::I32Mul,
            Instruction::I32Add,
            Instruction::LocalGet(data),
            Instruction::LocalGet(i),
            Instruction::I32Const(DICT_ENTRY as i32),
            Instruction::I32Mul,
            Instruction::I32Add,
            Instruction::I64Load(mem(0, 2)),
            Instruction::I64Store(mem(COLLECTION_HEADER as u64, 2)),
            Instruction::LocalGet(i),
            Instruction::I32Const(1),
            Instruction::I32Add,
            Instruction::LocalSet(i),
            Instruction::Br(0),
            Instruction::End,
            Instruction::End,
            Instruction::LocalGet(list),
            Instruction::End,
        ],
    );
    f
}
