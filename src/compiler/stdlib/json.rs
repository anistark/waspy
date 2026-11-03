//! `json`: `dumps`, `dump`, `loads`, and `load`.
//!
//! A value's type is known where it is compiled, so `json.dumps(x)` is a call
//! of the writer for `x`'s type (and the options, which must be constants),
//! a helper built here that writes CPython's text into a growing buffer:
//! `json.encoder`'s separators, indentation, `sort_keys`, `ensure_ascii`'s
//! escapes, and its rendering of floats and of dict keys. A value CPython
//! cannot serialize (a set, an instance) is refused where it is compiled.
//!
//! `json.loads(s)` parses into the type the value is declared to have (the
//! variable, parameter, or return it is assigned to), so the document is
//! read by a parser for that type: CPython's scanner, whose errors are its
//! `JSONDecodeError` with the same message and position. A document the
//! declared type cannot hold (a string where an int is declared) raises a
//! `TypeError` naming both, where CPython would hand back the other value.
//!
//! The writer reads its options from a block made at run time (`OPT_*`), so
//! `dumps()`'s constants and a `json.JSONEncoder`'s fields drive the same
//! code. A hook is a function of the program the writer or the parser calls:
//! `dumps(default=f)` and an encoder's `default()` method for a value JSON has
//! no form for, `loads(object_hook=f)` and a decoder's `object_hook` for each
//! object. Its signature is static, so what it is called with and what it
//! answers are written and parsed by their types like any other value.

use crate::compiler::asm::{call, Asm, Ty, Var, E, X};
use crate::compiler::context::{
    CompilationContext, CALL_DEPTH_GLOBAL, COLLECTION_CAP, COLLECTION_DATA,
};
use crate::compiler::expression::{emit_expr, emit_value, recover_str_pair, wasm_value_type};
use crate::compiler::function::{emit_call_depth_step, emit_post_call_check, infer_value_type};
use crate::compiler::runtime::Helper;
use crate::ir::json::{
    DECODER_CLASS, ENCODER_CLASS, INDENT_FIELD, OBJECT_HOOK_FIELD, OBJECT_HOOK_TYPE_METHOD,
};
use crate::ir::{IRConstant, IRExpr, IRType, MemoryLayout};
use wasm_encoder::{Function, Instruction as I, ValType};

/// The writer's options, a block made at run time: the indentation's text
/// (0 for one line), the separators, the flags, and the hook's word (the
/// `default` closure, or the encoder whose method it is).
const OPT_INDENT: u64 = 0;
const OPT_ITEM_SEP: u64 = 4;
const OPT_KEY_SEP: u64 = 8;
const OPT_SORT_KEYS: u64 = 12;
const OPT_ENSURE_ASCII: u64 = 16;
const OPT_ALLOW_NAN: u64 = 20;
const OPT_HOOK: u64 = 24;
const OPTS_SIZE: i32 = 28;
/// The parser's state: the position, and the `object_hook` closure (or 0).
const STATE_HOOK: u64 = 4;

/// How `dumps` writes: its keyword arguments.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DumpOpts {
    /// The text one level of indentation is, or `None` for one line.
    indent: Option<String>,
    item_sep: String,
    key_sep: String,
    sort_keys: bool,
    ensure_ascii: bool,
    allow_nan: bool,
}

/// A function of the program that `dumps` calls for a value it has no form
/// for, or `loads` for each object, with its static signature.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Hook {
    /// A closure: `default=f`, `object_hook=f`.
    Closure { takes: IRType, gives: IRType },
    /// An encoder class's `default` method, by function index.
    Method {
        index: u32,
        takes: IRType,
        gives: IRType,
    },
}

impl Hook {
    fn takes(&self) -> &IRType {
        match self {
            Hook::Closure { takes, .. } | Hook::Method { takes, .. } => takes,
        }
    }
    fn gives(&self) -> &IRType {
        match self {
            Hook::Closure { gives, .. } | Hook::Method { gives, .. } => gives,
        }
    }
}

/// The `json` routines, built as helpers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum JsonFn {
    /// `() -> buf`: an empty buffer.
    BufNew,
    /// `(buf, n) -> addr`: room for `n` more bytes, at `addr`.
    BufReserve,
    /// `(buf, byte) -> ()`.
    BufByte,
    /// `(buf, s) -> ()`: a string's bytes.
    BufStr,
    /// `(buf) -> s`: the buffer's bytes as a string.
    BufFinish,
    /// `(s, buf) -> ()`: a string as a JSON string literal.
    Escape { ascii: bool },
    /// `(value, buf, depth, opts) -> ()`.
    Dump { ty: IRType, hook: Option<Hook> },
    /// `(value, opts) -> s`.
    Write { ty: IRType, hook: Option<Hook> },
    /// `(value, hook) -> s`: `dumps()` with constant options.
    Dumps {
        ty: IRType,
        opts: DumpOpts,
        hook: Option<Hook>,
    },
    /// `(encoder, value) -> s`: `encode()`, the options the encoder's fields.
    Encode {
        ty: IRType,
        class: String,
        hook: Option<Hook>,
    },
    /// `(s, state) -> ()`: past whitespace.
    Ws,
    /// `(s, byte) -> ()`: leave `JSONDecodeError(message)` pending, at the
    /// character the byte offset is.
    Fail(String),
    /// `(s, byte) -> ()`: leave the `TypeError` for a value that is not of
    /// the declared type.
    Mismatch(IRType),
    /// `(s, byte) -> ()`: leave the `TypeError` for an `object_hook` whose
    /// result is not of the declared type.
    HookMismatch { gives: IRType, want: IRType },
    /// `(s, state) -> s`: a string literal at the position.
    ParseString,
    /// `(s, state) -> ()`: past a value of any kind, raising CPython's
    /// `JSONDecodeError` for a document that is not JSON.
    Skip,
    /// `(s, state) -> value`: a value of the type at the position, an object
    /// given to the hook unless `raw` (the object the hook is called with).
    Load {
        ty: IRType,
        hook: Option<Hook>,
        raw: bool,
    },
    /// `(s, hook) -> value`: a whole document of the type.
    Loads { ty: IRType, hook: Option<Hook> },
}

impl JsonFn {
    pub(crate) fn signature(&self) -> (Vec<Ty>, Vec<Ty>) {
        use Ty::{F64, I32};
        let word = |t: &IRType| if matches!(t, IRType::Float) { F64 } else { I32 };
        match self {
            JsonFn::BufNew => (vec![], vec![I32]),
            JsonFn::BufReserve => (vec![I32, I32], vec![I32]),
            JsonFn::BufByte | JsonFn::BufStr => (vec![I32, I32], vec![]),
            JsonFn::BufFinish => (vec![I32], vec![I32]),
            JsonFn::Escape { .. } => (vec![I32, I32], vec![]),
            JsonFn::Dump { ty, .. } => (vec![word(ty), I32, I32, I32], vec![]),
            JsonFn::Write { ty, .. } | JsonFn::Dumps { ty, .. } => (vec![word(ty), I32], vec![I32]),
            JsonFn::Encode { ty, .. } => (vec![I32, word(ty)], vec![I32]),
            JsonFn::Ws | JsonFn::Skip => (vec![I32, I32], vec![]),
            JsonFn::Fail(_) | JsonFn::Mismatch(_) | JsonFn::HookMismatch { .. } => {
                (vec![I32, I32], vec![])
            }
            JsonFn::ParseString => (vec![I32, I32], vec![I32]),
            JsonFn::Load { ty, .. } => (vec![I32, I32], vec![word(ty)]),
            JsonFn::Loads { ty, .. } => (vec![I32, I32], vec![word(ty)]),
        }
    }
}

fn h(ctx: &CompilationContext, f: JsonFn) -> u32 {
    ctx.helper(Helper::Json(f))
}

/// A constant string.
fn text(ctx: &CompilationContext, s: &str) -> E {
    call(
        ctx.helper(Helper::ExcText(s.to_string())),
        vec![],
        Some(Ty::I32),
    )
}

fn put_byte(a: &mut Asm, ctx: &CompilationContext, buf: Var, b: impl Into<E>) {
    a.run(call(
        h(ctx, JsonFn::BufByte),
        vec![buf.into(), b.into()],
        None,
    ));
}

fn put_text(a: &mut Asm, ctx: &CompilationContext, buf: Var, s: &str) {
    if s.len() <= 2 {
        for b in s.bytes() {
            put_byte(a, ctx, buf, b as i32);
        }
    } else {
        a.run(call(
            h(ctx, JsonFn::BufStr),
            vec![buf.into(), text(ctx, s)],
            None,
        ));
    }
}

/// Return early (with `zero`, or nothing) when an exception is pending.
fn bail(a: &mut Asm, zero: Option<E>) {
    a.raw(I::GlobalGet(crate::compiler::context::EXC_TYPE_GLOBAL));
    a.raw(I::If(wasm_encoder::BlockType::Empty));
    if let Some(z) = zero {
        a.push(z);
    }
    a.raw(I::Return);
    a.raw(I::End);
}

fn zero_of(ty: &IRType) -> E {
    if matches!(ty, IRType::Float) {
        E::F64(0.0)
    } else {
        E::I32(0)
    }
}

fn memcpy(a: &mut Asm, dst: impl Into<E>, src: impl Into<E>, n: impl Into<E>) {
    a.push(dst);
    a.push(src);
    a.push(n);
    a.raw(I::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
}

fn hex_digit(d: E) -> E {
    let d2 = d.clone();
    d.clone().lt(10).select(d + 48, d2 + 87)
}

/// Build the helper for `f`.
pub(crate) fn build(ctx: &CompilationContext, f: &JsonFn) -> Function {
    use Ty::I32;
    let alloc = ctx.alloc_func_index;
    let (params, _) = f.signature();
    let mut a = Asm::new(&params);
    match f {
        JsonFn::BufNew => {
            let b = a.let_(call(alloc, vec![E::I32(12)], Some(I32)));
            a.store(I32, b, 0, 0);
            a.store(I32, b, 4, 64);
            a.store(I32, b, 8, call(alloc, vec![E::I32(64)], Some(I32)));
            a.ret(b);
        }
        JsonFn::BufReserve => {
            let (b, n) = (a.param(0), a.param(1));
            let len = a.let_(b.load(I32, 0));
            let need = a.let_(len + n);
            let cap = a.let_(b.load(I32, 4));
            a.if_(need.gt(cap), |a| {
                let doubled = a.let_(cap * 2);
                let newcap = a.let_(doubled.gt(need).select(doubled, need));
                let data = a.let_(call(alloc, vec![newcap.into()], Some(I32)));
                memcpy(a, data, b.load(I32, 8), len);
                a.store(I32, b, 8, data);
                a.store(I32, b, 4, newcap);
            });
            a.store(I32, b, 0, need);
            a.ret(b.load(I32, 8) + len);
        }
        JsonFn::BufByte => {
            let (b, byte) = (a.param(0), a.param(1));
            let at = a.let_(call(
                h(ctx, JsonFn::BufReserve),
                vec![b.into(), E::I32(1)],
                Some(I32),
            ));
            a.store8(at, 0, byte);
        }
        JsonFn::BufStr => {
            let (b, s) = (a.param(0), a.param(1));
            let n = a.let_((s - 4).load(I32, 0));
            let at = a.let_(call(
                h(ctx, JsonFn::BufReserve),
                vec![b.into(), n.into()],
                Some(I32),
            ));
            memcpy(&mut a, at, s, n);
        }
        JsonFn::BufFinish => {
            let b = a.param(0);
            let n = a.let_(b.load(I32, 0));
            let blob = a.let_(call(alloc, vec![n + 5], Some(I32)));
            a.store(I32, blob, 0, n);
            memcpy(&mut a, blob + 4, b.load(I32, 8), n);
            a.store8(blob + n + 4, 0, 0);
            a.ret(blob + 4);
        }
        JsonFn::Escape { ascii } => build_escape(&mut a, ctx, *ascii),
        JsonFn::Dump { ty, hook } => build_dump(&mut a, ctx, ty, hook.as_ref()),
        JsonFn::Write { ty, hook } => {
            let (v, o) = (a.param(0), a.param(1));
            let b = a.let_(call(h(ctx, JsonFn::BufNew), vec![], Some(I32)));
            a.run(call(
                h(
                    ctx,
                    JsonFn::Dump {
                        ty: ty.clone(),
                        hook: hook.clone(),
                    },
                ),
                vec![v.into(), b.into(), E::I32(0), o.into()],
                None,
            ));
            bail(&mut a, Some(E::I32(0)));
            a.ret(call(h(ctx, JsonFn::BufFinish), vec![b.into()], Some(I32)));
        }
        JsonFn::Dumps { ty, opts, hook } => {
            let (v, word) = (a.param(0), a.param(1));
            let o = a.let_(call(alloc, vec![E::I32(OPTS_SIZE)], Some(I32)));
            match &opts.indent {
                Some(indent) => a.store(I32, o, OPT_INDENT, text(ctx, indent)),
                None => a.store(I32, o, OPT_INDENT, 0),
            }
            a.store(I32, o, OPT_ITEM_SEP, text(ctx, &opts.item_sep));
            a.store(I32, o, OPT_KEY_SEP, text(ctx, &opts.key_sep));
            a.store(I32, o, OPT_SORT_KEYS, opts.sort_keys as i32);
            a.store(I32, o, OPT_ENSURE_ASCII, opts.ensure_ascii as i32);
            a.store(I32, o, OPT_ALLOW_NAN, opts.allow_nan as i32);
            a.store(I32, o, OPT_HOOK, word);
            let write = h(
                ctx,
                JsonFn::Write {
                    ty: ty.clone(),
                    hook: hook.clone(),
                },
            );
            a.ret(call(write, vec![v.into(), o.into()], Some(I32)));
        }
        JsonFn::Encode { ty, class, hook } => {
            let (enc, v) = (a.param(0), a.param(1));
            let o = a.let_(call(alloc, vec![E::I32(OPTS_SIZE)], Some(I32)));
            let fields = ctx.get_class_info(class).map(|ci| ci.field_offsets.clone());
            for (field, at) in [
                (INDENT_FIELD, OPT_INDENT),
                ("item_separator", OPT_ITEM_SEP),
                ("key_separator", OPT_KEY_SEP),
                ("sort_keys", OPT_SORT_KEYS),
                ("ensure_ascii", OPT_ENSURE_ASCII),
                ("allow_nan", OPT_ALLOW_NAN),
            ] {
                let offset = fields
                    .as_ref()
                    .and_then(|f| f.get(field).copied())
                    .expect("a JSONEncoder field");
                a.store(I32, o, at, enc.load(I32, offset));
            }
            a.store(I32, o, OPT_HOOK, enc);
            let write = h(
                ctx,
                JsonFn::Write {
                    ty: ty.clone(),
                    hook: hook.clone(),
                },
            );
            a.ret(call(write, vec![v.into(), o.into()], Some(I32)));
        }
        JsonFn::Ws => {
            let (s, st) = (a.param(0), a.param(1));
            let n = a.let_((s - 4).load(I32, 0));
            let pos = a.let_(st.load(I32, 0));
            a.loop_(|a, l| {
                a.brk_if(l, pos.ge(n));
                let c = a.let_((s + pos).load8(0));
                let space = c.eq(32) | c.eq(9) | c.eq(10) | c.eq(13);
                a.brk_unless(l, space);
                a.set(pos, pos + 1);
                a.cont(l);
            });
            a.store(I32, st, 0, pos);
        }
        JsonFn::Fail(message) => build_fail(&mut a, ctx, message),
        JsonFn::Mismatch(ty) => build_mismatch(&mut a, ctx, ty),
        JsonFn::HookMismatch { gives, want } => build_hook_mismatch(&mut a, ctx, gives, want),
        JsonFn::ParseString => build_parse_string(&mut a, ctx),
        JsonFn::Load { ty, hook, raw } => build_load(&mut a, ctx, ty, hook.as_ref(), *raw),
        JsonFn::Skip => build_skip(&mut a, ctx),
        JsonFn::Loads { ty, hook } => {
            let (s, word) = (a.param(0), a.param(1));
            let st = a.let_(call(alloc, vec![E::I32(8)], Some(I32)));
            // The document is JSON first, as CPython reads it, so a syntax
            // error is its error even where the type would not fit.
            a.store(I32, st, 0, 0);
            a.store(I32, st, STATE_HOOK, word);
            a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
            a.run(call(h(ctx, JsonFn::Skip), vec![s.into(), st.into()], None));
            bail(&mut a, Some(zero_of(ty)));
            a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
            a.if_(st.load(I32, 0).ne((s - 4).load(I32, 0)), |a| {
                a.run(call(
                    h(ctx, JsonFn::Fail("Extra data".to_string())),
                    vec![s.into(), st.load(I32, 0)],
                    None,
                ));
                a.ret(zero_of(ty));
            });
            a.store(I32, st, 0, 0);
            a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
            let load = h(
                ctx,
                JsonFn::Load {
                    ty: ty.clone(),
                    hook: hook.clone(),
                    raw: false,
                },
            );
            let v = a.let_(call(load, vec![s.into(), st.into()], Some(params_ty(ty))));
            bail(&mut a, Some(zero_of(ty)));
            a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
            a.if_(st.load(I32, 0).ne((s - 4).load(I32, 0)), |a| {
                a.run(call(
                    h(ctx, JsonFn::Fail("Extra data".to_string())),
                    vec![s.into(), st.load(I32, 0)],
                    None,
                ));
                a.ret(zero_of(ty));
            });
            a.ret(v);
        }
    }
    a.finish()
}

fn params_ty(ty: &IRType) -> Ty {
    if matches!(ty, IRType::Float) {
        Ty::F64
    } else {
        Ty::I32
    }
}

/// `"..."` with JSON's escapes; non-ASCII as `\uXXXX` (a surrogate pair past
/// the BMP) when `ascii`, else as its UTF-8.
fn build_escape(a: &mut Asm, ctx: &CompilationContext, ascii: bool) {
    use Ty::I32;
    let (s, b) = (a.param(0), a.param(1));
    let n = a.let_((s - 4).load(I32, 0));
    let i = a.local(I32);
    let c = a.local(I32);
    let cp = a.local(I32);
    put_byte(a, ctx, b, b'"' as i32);
    let put_u = |a: &mut Asm, v: Var| {
        put_byte(a, ctx, b, b'\\' as i32);
        put_byte(a, ctx, b, b'u' as i32);
        for shift in [12, 8, 4, 0] {
            put_byte(a, ctx, b, hex_digit((E::from(v) >> shift) & 15));
        }
    };
    a.set(i, 0);
    a.loop_(|a, l| {
        a.brk_if(l, i.ge(n));
        a.set(c, (s + i).load8(0));
        a.set(i, i + 1);
        // The two-character escapes.
        for (byte, esc) in [
            (b'"', b'"'),
            (b'\\', b'\\'),
            (b'\n', b'n'),
            (b'\r', b'r'),
            (b'\t', b't'),
            (8u8, b'b'),
            (12u8, b'f'),
        ] {
            a.if_(c.eq(byte as i32), |a| {
                put_byte(a, ctx, b, b'\\' as i32);
                put_byte(a, ctx, b, esc as i32);
                a.cont(l);
            });
        }
        a.if_(c.lt(0x20), |a| {
            a.set(cp, c);
            put_u(a, cp);
            a.cont(l);
        });
        if ascii {
            a.if_(c.lt(0x7f), |a| {
                put_byte(a, ctx, b, c);
                a.cont(l);
            });
            // A code point from its UTF-8 sequence.
            a.if_else(
                c.lt(0x80),
                |a| a.set(cp, c),
                |a| {
                    a.if_else(
                        c.lt(0xe0),
                        |a| {
                            a.set(cp, ((c & 0x1f) << 6) | ((s + i).load8(0) & 0x3f));
                            a.set(i, i + 1);
                        },
                        |a| {
                            a.if_else(
                                c.lt(0xf0),
                                |a| {
                                    a.set(
                                        cp,
                                        ((c & 0x0f) << 12)
                                            | (((s + i).load8(0) & 0x3f) << 6)
                                            | ((s + i).load8(1) & 0x3f),
                                    );
                                    a.set(i, i + 2);
                                },
                                |a| {
                                    a.set(
                                        cp,
                                        ((c & 0x07) << 18)
                                            | (((s + i).load8(0) & 0x3f) << 12)
                                            | (((s + i).load8(1) & 0x3f) << 6)
                                            | ((s + i).load8(2) & 0x3f),
                                    );
                                    a.set(i, i + 3);
                                },
                            );
                        },
                    );
                },
            );
            a.if_else(
                cp.ge(0x10000),
                |a| {
                    let v = a.let_(cp - 0x10000);
                    let hi = a.let_((v >> 10) + 0xd800);
                    let lo = a.let_((v & 0x3ff) + 0xdc00);
                    put_u(a, hi);
                    put_u(a, lo);
                },
                |a| put_u(a, cp),
            );
        } else {
            put_byte(a, ctx, b, c);
        }
        a.cont(l);
    });
    put_byte(a, ctx, b, b'"' as i32);
}

fn newline_indent(a: &mut Asm, ctx: &CompilationContext, buf: Var, depth: E, opts: Var) {
    let indent = a.let_(opts.load(Ty::I32, OPT_INDENT));
    a.if_(indent.ne(0), |a| {
        put_byte(a, ctx, buf, b'\n' as i32);
        let k = a.local(Ty::I32);
        a.for_(k, 0, depth, false, |a, _| {
            a.run(call(
                h(ctx, JsonFn::BufStr),
                vec![buf.into(), indent.into()],
                None,
            ))
        });
    });
}

/// The separator the option at `at` holds.
fn put_separator(a: &mut Asm, ctx: &CompilationContext, buf: Var, opts: Var, at: u64) {
    a.run(call(
        h(ctx, JsonFn::BufStr),
        vec![buf.into(), opts.load(Ty::I32, at)],
        None,
    ));
}

/// A string as a JSON string literal, escaped as `ensure_ascii` says.
fn put_escaped(a: &mut Asm, ctx: &CompilationContext, buf: Var, s: E, opts: Var) {
    let s = a.let_(s);
    let escape = |a: &mut Asm, ascii: bool| {
        a.run(call(
            h(ctx, JsonFn::Escape { ascii }),
            vec![s.into(), buf.into()],
            None,
        ))
    };
    a.if_else(
        opts.load(Ty::I32, OPT_ENSURE_ASCII),
        |a| escape(a, true),
        |a| escape(a, false),
    );
}

/// A float as `float.__repr__` writes it, or `NaN` / `Infinity` /
/// `-Infinity` (a `ValueError` without `allow_nan`).
fn put_float(a: &mut Asm, ctx: &CompilationContext, buf: Var, v: E, opts: Var) {
    let x = a.let_(v);
    let special = |a: &mut Asm, shown: &str| {
        let message = format!(
            "Out of range float values are not JSON compliant: {}",
            match shown {
                "NaN" => "nan",
                "Infinity" => "inf",
                _ => "-inf",
            }
        );
        a.if_else(
            opts.load(Ty::I32, OPT_ALLOW_NAN),
            |a| put_text(a, ctx, buf, shown),
            |a| {
                a.run(call(
                    ctx.helper(Helper::RaiseText("ValueError".to_string())),
                    vec![text(ctx, &message)],
                    None,
                ));
                a.raw(I::Return);
            },
        );
    };
    a.if_else(
        x.ne(x),
        |a| special(a, "NaN"),
        |a| {
            a.if_else(
                x.eq(f64::INFINITY),
                |a| special(a, "Infinity"),
                |a| {
                    a.if_else(
                        x.eq(f64::NEG_INFINITY),
                        |a| special(a, "-Infinity"),
                        |a| {
                            a.run(call(
                                h(ctx, JsonFn::BufStr),
                                vec![
                                    buf.into(),
                                    call(
                                        ctx.helper(Helper::FloatRepr),
                                        vec![x.into()],
                                        Some(Ty::I32),
                                    ),
                                ],
                                None,
                            ));
                        },
                    )
                },
            )
        },
    );
}

fn slot_value(a: &mut Asm, addr: E, ty: &IRType) -> E {
    let _ = a;
    addr.load(params_ty(ty), 0)
}

/// Whether the writer has a form of its own for a value of `ty` (the
/// collections' elements aside).
fn writes_natively(ty: &IRType) -> bool {
    matches!(
        ty,
        IRType::Int
            | IRType::Bool
            | IRType::Float
            | IRType::String
            | IRType::None
            | IRType::Optional(_)
            | IRType::List(_)
            | IRType::VarTuple(_)
            | IRType::Tuple(_)
            | IRType::Dict(..)
    )
}

/// Call `hook` with `value` through `word` (the closure, or the encoder
/// whose method it is), with the bookkeeping of a call that can raise; a
/// local holding its result.
fn call_hook(a: &mut Asm, ctx: &CompilationContext, hook: &Hook, word: E, value: E) -> Var {
    let word = a.let_(word);
    let value = a.let_(value);
    let depth = |a: &mut Asm, delta: i32| {
        a.raw(I::GlobalGet(CALL_DEPTH_GLOBAL));
        a.raw(I::I32Const(delta));
        a.raw(I::I32Add);
        a.raw(I::GlobalSet(CALL_DEPTH_GLOBAL));
    };
    depth(a, 1);
    match hook {
        Hook::Closure { takes, gives } => {
            a.push(value);
            a.push(word);
            a.push(word.load(Ty::I32, 0));
            a.raw(I::CallIndirect {
                type_index: closure_type(ctx, takes, gives).unwrap_or(0),
                table_index: 0,
            });
        }
        Hook::Method { index, .. } => {
            a.push(word);
            a.push(value);
            a.raw(I::Call(*index));
        }
    }
    depth(a, -1);
    let result = a.local(params_ty(hook.gives()));
    a.raw(I::LocalSet(result.0));
    result
}

/// The type index a closure of one parameter is called at.
fn closure_type(ctx: &CompilationContext, takes: &IRType, gives: &IRType) -> Option<u32> {
    let key = (
        vec![wasm_value_type(takes), ValType::I32],
        wasm_value_type(gives),
    );
    ctx.signature_types.get(&key).copied()
}

/// Write `value` of `ty` at `depth`.
fn build_dump(a: &mut Asm, ctx: &CompilationContext, ty: &IRType, hook: Option<&Hook>) {
    use Ty::I32;
    let (v, buf, depth, opts) = (a.param(0), a.param(1), a.param(2), a.param(3));
    let dump = |a: &mut Asm, ty: &IRType, value: E, depth: E| {
        a.run(call(
            h(
                ctx,
                JsonFn::Dump {
                    ty: ty.clone(),
                    hook: hook.cloned(),
                },
            ),
            vec![value, buf.into(), depth, opts.into()],
            None,
        ));
        bail(a, None);
    };
    // A value with no form of its own is written as what the hook answers
    // for it.
    if let Some(hook) = hook.filter(|_| !writes_natively(ty)) {
        let result = call_hook(a, ctx, hook, opts.load(I32, OPT_HOOK), v.into());
        bail(a, None);
        dump(a, hook.gives(), result.into(), depth.into());
        return;
    }
    match ty {
        IRType::Int => a.run(call(
            h(ctx, JsonFn::BufStr),
            vec![
                buf.into(),
                call(ctx.i32_to_str_func_index, vec![v.into()], Some(I32)),
            ],
            None,
        )),
        IRType::Bool => a.if_else(
            v,
            |a| put_text(a, ctx, buf, "true"),
            |a| put_text(a, ctx, buf, "false"),
        ),
        IRType::None => put_text(a, ctx, buf, "null"),
        IRType::Float => put_float(a, ctx, buf, v.into(), opts),
        IRType::String => put_escaped(a, ctx, buf, v.into(), opts),
        IRType::Optional(inner) => {
            a.if_else(
                v.eqz(),
                |a| put_text(a, ctx, buf, "null"),
                |a| {
                    // An optional number is a cell holding it.
                    let value = match inner.as_ref() {
                        IRType::Int | IRType::Bool | IRType::Float => {
                            slot_value(a, v.into(), inner)
                        }
                        _ => v.into(),
                    };
                    dump(a, inner, value, depth.into());
                },
            );
        }
        IRType::List(_) | IRType::VarTuple(_) | IRType::Tuple(_) => {
            let n = a.let_(v.load(I32, 0));
            a.if_(n.eqz(), |a| {
                put_text(a, ctx, buf, "[]");
                a.raw(I::Return);
            });
            put_byte(a, ctx, buf, b'[' as i32);
            let inner_depth = a.let_(depth + 1);
            newline_indent(a, ctx, buf, inner_depth.into(), opts);
            let data = a.let_(v.load(I32, COLLECTION_DATA as u64));
            match ty {
                IRType::Tuple(members) => {
                    for (k, m) in members.iter().enumerate() {
                        if k > 0 {
                            put_separator(a, ctx, buf, opts, OPT_ITEM_SEP);
                            newline_indent(a, ctx, buf, inner_depth.into(), opts);
                        }
                        let value = slot_value(a, data + (k as i32 * 8), m);
                        dump(a, m, value, inner_depth.into());
                    }
                }
                IRType::List(e) | IRType::VarTuple(e) => {
                    let i = a.local(I32);
                    let e = (**e).clone();
                    a.for_(i, 0, n, false, |a, _| {
                        a.if_(i.gt(0), |a| {
                            put_separator(a, ctx, buf, opts, OPT_ITEM_SEP);
                            newline_indent(a, ctx, buf, inner_depth.into(), opts);
                        });
                        let value = slot_value(a, data + i * 8, &e);
                        dump(a, &e, value, inner_depth.into());
                    });
                }
                _ => unreachable!(),
            }
            newline_indent(a, ctx, buf, depth.into(), opts);
            put_byte(a, ctx, buf, b']' as i32);
        }
        IRType::Dict(k, val) => {
            let n = a.let_(v.load(I32, 0));
            a.if_(n.eqz(), |a| {
                put_text(a, ctx, buf, "{}");
                a.raw(I::Return);
            });
            let data = a.let_(v.load(I32, COLLECTION_DATA as u64));
            // The entries' order: as stored, or sorted by key.
            let order = a.let_(call(ctx.alloc_func_index, vec![n * 4], Some(I32)));
            let i = a.local(I32);
            a.for_(i, 0, n, false, |a, _| a.store(I32, order + i * 4, 0, i));
            a.if_(opts.load(I32, OPT_SORT_KEYS), |a| {
                sort_entries(a, ctx, k, data, order, n)
            });
            put_byte(a, ctx, buf, b'{' as i32);
            let inner_depth = a.let_(depth + 1);
            newline_indent(a, ctx, buf, inner_depth.into(), opts);
            let entry = a.local(I32);
            let (k, val) = ((**k).clone(), (**val).clone());
            a.for_(i, 0, n, false, |a, _| {
                a.if_(i.gt(0), |a| {
                    put_separator(a, ctx, buf, opts, OPT_ITEM_SEP);
                    newline_indent(a, ctx, buf, inner_depth.into(), opts);
                });
                a.set(entry, data + (order + i * 4).load(I32, 0) * 16);
                // A key is written as a string, whatever its type.
                let key = slot_value(a, entry.into(), &k);
                match &k {
                    IRType::String => put_escaped(a, ctx, buf, key, opts),
                    IRType::Int => {
                        put_byte(a, ctx, buf, b'"' as i32);
                        a.run(call(
                            h(ctx, JsonFn::BufStr),
                            vec![
                                buf.into(),
                                call(ctx.i32_to_str_func_index, vec![key], Some(I32)),
                            ],
                            None,
                        ));
                        put_byte(a, ctx, buf, b'"' as i32);
                    }
                    IRType::Float => {
                        put_byte(a, ctx, buf, b'"' as i32);
                        put_float(a, ctx, buf, key, opts);
                        put_byte(a, ctx, buf, b'"' as i32);
                    }
                    IRType::Bool => a.if_else(
                        key,
                        |a| put_text(a, ctx, buf, "\"true\""),
                        |a| put_text(a, ctx, buf, "\"false\""),
                    ),
                    _ => put_text(a, ctx, buf, "\"null\""),
                }
                put_separator(a, ctx, buf, opts, OPT_KEY_SEP);
                let value = slot_value(a, entry + 8, &val);
                dump(a, &val, value, inner_depth.into());
            });
            newline_indent(a, ctx, buf, depth.into(), opts);
            put_byte(a, ctx, buf, b'}' as i32);
        }
        _ => {}
    }
}

/// Sort `order` (`n` entry indices) by the entries' keys, stably, as
/// `sorted(d.items())` orders them.
fn sort_entries(a: &mut Asm, ctx: &CompilationContext, k: &IRType, data: Var, order: Var, n: Var) {
    use Ty::I32;
    let _ = ctx;
    let (i, j, x) = (a.local(I32), a.local(I32), a.local(I32));
    let key_at = |slot: E| E::from(data) + slot * 16;
    let less = |a: &mut Asm, p: E, q: E| -> E {
        match k {
            IRType::String => byte_compare(a, key_at(p).load(I32, 0), key_at(q).load(I32, 0)).lt(0),
            IRType::Float => key_at(p).load(Ty::F64, 0).lt(key_at(q).load(Ty::F64, 0)),
            IRType::None => E::I32(0),
            _ => key_at(p).load(I32, 0).lt(key_at(q).load(I32, 0)),
        }
    };
    // Insertion sort.
    a.for_(i, 1, n, false, |a, _| {
        a.set(x, (order + i * 4).load(I32, 0));
        a.set(j, i - 1);
        a.loop_(|a, l| {
            a.brk_if(l, j.lt(0));
            let y = (order + j * 4).load(I32, 0);
            let earlier = less(a, x.into(), y.clone());
            a.brk_unless(l, earlier);
            a.store(I32, order + (j + 1) * 4, 0, y);
            a.set(j, j - 1);
            a.cont(l);
        });
        a.store(I32, order + (j + 1) * 4, 0, x);
    });
}

/// The three-way comparison of two strings' bytes (UTF-8 orders as code
/// points do).
fn byte_compare(a: &mut Asm, x: E, y: E) -> E {
    use Ty::I32;
    let (p, q) = (a.let_(x), a.let_(y));
    let (m, n) = (a.let_((p - 4).load(I32, 0)), a.let_((q - 4).load(I32, 0)));
    let r = a.let_(0);
    let i = a.local(I32);
    a.set(i, 0);
    a.loop_(|a, l| {
        a.if_(i.ge(m) | i.ge(n), |a| {
            a.set(r, m.lt(n).select(-1, m.gt(n).select(1, 0)));
            a.brk_if(l, 1);
        });
        let (c, d) = (a.let_((p + i).load8(0)), a.let_((q + i).load8(0)));
        a.if_(c.ne(d), |a| {
            a.set(r, c.lt_u(d).select(-1, 1));
            a.brk_if(l, 1);
        });
        a.set(i, i + 1);
        a.cont(l);
    });
    r.into()
}

/// `JSONDecodeError(message)` at the byte offset: `msg: line L column C
/// (char P)`, counted in characters as CPython counts them.
fn build_fail(a: &mut Asm, ctx: &CompilationContext, message: &str) {
    use Ty::I32;
    let (s, byte) = (a.param(0), a.param(1));
    let char_index = ctx.helper(Helper::CharIndex);
    let pos = a.let_(call(char_index, vec![s.into(), byte.into()], Some(I32)));
    let line = a.let_(1);
    let last = a.let_(-1);
    let i = a.local(I32);
    a.for_(i, 0, byte, false, |a, _| {
        a.if_((s + i).load8(0).eq(10), |a| {
            a.set(line, line + 1);
            a.set(last, i);
        });
    });
    let col = a.local(I32);
    a.if_else(
        last.lt(0),
        |a| a.set(col, pos + 1),
        |a| {
            a.set(
                col,
                pos - call(char_index, vec![s.into(), last.into()], Some(I32)),
            )
        },
    );
    let b = a.let_(call(h(ctx, JsonFn::BufNew), vec![], Some(I32)));
    let int = |v: Var| call(ctx.i32_to_str_func_index, vec![v.into()], Some(I32));
    for piece in [
        text(ctx, &format!("{message}: line ")),
        int(line),
        text(ctx, " column "),
        int(col),
        text(ctx, " (char "),
        int(pos),
        text(ctx, ")"),
    ] {
        a.run(call(h(ctx, JsonFn::BufStr), vec![b.into(), piece], None));
    }
    a.run(call(
        ctx.helper(Helper::RaiseText("JSONDecodeError".to_string())),
        vec![call(h(ctx, JsonFn::BufFinish), vec![b.into()], Some(I32))],
        None,
    ));
}

/// The `TypeError` for a JSON value of another kind than declared.
fn build_mismatch(a: &mut Asm, ctx: &CompilationContext, ty: &IRType) {
    use Ty::I32;
    let (s, byte) = (a.param(0), a.param(1));
    let pos = a.let_(call(
        ctx.helper(Helper::CharIndex),
        vec![s.into(), byte.into()],
        Some(I32),
    ));
    let c = a.let_((s + byte).load8(0));
    let b = a.let_(call(h(ctx, JsonFn::BufNew), vec![], Some(I32)));
    put_text(a, ctx, b, "json.loads(): the value at char ");
    a.run(call(
        h(ctx, JsonFn::BufStr),
        vec![
            b.into(),
            call(ctx.i32_to_str_func_index, vec![pos.into()], Some(I32)),
        ],
        None,
    ));
    for (first, kind) in [
        (b'{', " is an object"),
        (b'[', " is an array"),
        (b'"', " is a string"),
        (b't', " is a bool"),
        (b'f', " is a bool"),
        (b'n', " is null"),
    ] {
        a.if_(c.eq(first as i32), |a| put_text(a, ctx, b, kind));
    }
    let numeric = c.eq(b'-' as i32)
        | (c.ge(b'0' as i32) & c.le(b'9' as i32))
        | c.eq(b'N' as i32)
        | c.eq(b'I' as i32);
    a.if_(numeric, |a| put_text(a, ctx, b, " is a number"));
    put_text(
        a,
        ctx,
        b,
        &format!(", where {} is declared", crate::type_to_string(ty)),
    );
    a.run(call(
        ctx.helper(Helper::RaiseText("TypeError".to_string())),
        vec![call(h(ctx, JsonFn::BufFinish), vec![b.into()], Some(I32))],
        None,
    ));
}

/// The `TypeError` for an `object_hook` answering a value of another type
/// than declared where the object is.
fn build_hook_mismatch(a: &mut Asm, ctx: &CompilationContext, gives: &IRType, want: &IRType) {
    use Ty::I32;
    let (s, byte) = (a.param(0), a.param(1));
    let pos = a.let_(call(
        ctx.helper(Helper::CharIndex),
        vec![s.into(), byte.into()],
        Some(I32),
    ));
    let b = a.let_(call(h(ctx, JsonFn::BufNew), vec![], Some(I32)));
    put_text(
        a,
        ctx,
        b,
        "json.loads(): the object_hook's result for the object at char ",
    );
    a.run(call(
        h(ctx, JsonFn::BufStr),
        vec![
            b.into(),
            call(ctx.i32_to_str_func_index, vec![pos.into()], Some(I32)),
        ],
        None,
    ));
    put_text(
        a,
        ctx,
        b,
        &format!(
            " is a {}, where {} is declared",
            crate::type_to_string(gives),
            crate::type_to_string(want)
        ),
    );
    a.run(call(
        ctx.helper(Helper::RaiseText("TypeError".to_string())),
        vec![call(h(ctx, JsonFn::BufFinish), vec![b.into()], Some(I32))],
        None,
    ));
}

/// How a hook's answer of `gives` is held where `want` is declared, if it
/// can be.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    /// As it is.
    Same,
    /// In the cell an optional number is.
    Boxed,
}

fn fits(ctx: &CompilationContext, gives: &IRType, want: &IRType) -> Option<Fit> {
    match (gives, want) {
        (g, w) if g == w => Some(Fit::Same),
        (IRType::Class(g), IRType::Class(w)) if ctx.is_class_or_subclass(g, w) => Some(Fit::Same),
        (IRType::Bool, IRType::Int) => Some(Fit::Same),
        (g, IRType::Optional(inner)) => {
            fits(ctx, g, inner)?;
            Some(
                if matches!(inner.as_ref(), IRType::Int | IRType::Bool | IRType::Float) {
                    Fit::Boxed
                } else {
                    Fit::Same
                },
            )
        }
        _ => None,
    }
}

fn fail(a: &mut Asm, ctx: &CompilationContext, message: &str, s: Var, at: impl Into<E>) {
    a.run(call(
        h(ctx, JsonFn::Fail(message.to_string())),
        vec![s.into(), at.into()],
        None,
    ));
}

/// A string literal: CPython's `scanstring`.
fn build_parse_string(a: &mut Asm, ctx: &CompilationContext) {
    use Ty::I32;
    let (s, st) = (a.param(0), a.param(1));
    let n = a.let_((s - 4).load(I32, 0));
    let start = a.let_(st.load(I32, 0));
    let pos = a.let_(start + 1);
    let b = a.let_(call(h(ctx, JsonFn::BufNew), vec![], Some(I32)));
    let c = a.local(I32);
    let cp = a.local(I32);
    let hex4 = |a: &mut Asm, at: E| -> Var {
        // The four hex digits at address `at`, or -1.
        let v = a.let_(0);
        let k = a.local(I32);
        let at = a.let_(at);
        a.for_(k, 0, 4, false, |a, l| {
            let d = a.let_((at + k).load8(0));
            let value = a.local(I32);
            a.set(value, -1);
            a.if_(d.ge(b'0' as i32) & d.le(b'9' as i32), |a| {
                a.set(value, d - 48)
            });
            a.if_(d.ge(b'a' as i32) & d.le(b'f' as i32), |a| {
                a.set(value, d - 87)
            });
            a.if_(d.ge(b'A' as i32) & d.le(b'F' as i32), |a| {
                a.set(value, d - 55)
            });
            a.if_(value.lt(0), |a| {
                a.set(v, -1);
                a.brk_if(l, 1);
            });
            a.set(v, (v << 4) | value);
        });
        v
    };
    let encode = |a: &mut Asm, cp: Var| {
        // A code point as UTF-8.
        a.if_else(
            cp.lt(0x80),
            |a| put_byte(a, ctx, b, cp),
            |a| {
                a.if_else(
                    cp.lt(0x800),
                    |a| {
                        put_byte(a, ctx, b, (E::from(cp) >> 6) | 0xc0);
                        put_byte(a, ctx, b, (E::from(cp) & 0x3f) | 0x80);
                    },
                    |a| {
                        a.if_else(
                            cp.lt(0x10000),
                            |a| {
                                put_byte(a, ctx, b, (E::from(cp) >> 12) | 0xe0);
                                put_byte(a, ctx, b, ((E::from(cp) >> 6) & 0x3f) | 0x80);
                                put_byte(a, ctx, b, (E::from(cp) & 0x3f) | 0x80);
                            },
                            |a| {
                                put_byte(a, ctx, b, (E::from(cp) >> 18) | 0xf0);
                                put_byte(a, ctx, b, ((E::from(cp) >> 12) & 0x3f) | 0x80);
                                put_byte(a, ctx, b, ((E::from(cp) >> 6) & 0x3f) | 0x80);
                                put_byte(a, ctx, b, (E::from(cp) & 0x3f) | 0x80);
                            },
                        )
                    },
                )
            },
        );
    };
    a.loop_(|a, l| {
        a.if_(pos.ge(n), |a| {
            fail(a, ctx, "Unterminated string starting at", s, start);
            a.ret(0);
        });
        a.set(c, (s + pos).load8(0));
        a.if_(c.eq(b'"' as i32), |a| {
            a.set(pos, pos + 1);
            a.brk_if(l, 1);
        });
        a.if_(c.lt(0x20), |a| {
            fail(a, ctx, "Invalid control character at", s, pos);
            a.ret(0);
        });
        a.if_(c.ne(b'\\' as i32), |a| {
            put_byte(a, ctx, b, c);
            a.set(pos, pos + 1);
            a.cont(l);
        });
        // An escape.
        a.if_((pos + 1).ge(n), |a| {
            fail(a, ctx, "Unterminated string starting at", s, start);
            a.ret(0);
        });
        let e = a.let_((s + pos + 1).load8(0));
        for (esc, byte) in [
            (b'"', b'"'),
            (b'\\', b'\\'),
            (b'/', b'/'),
            (b'b', 8u8),
            (b'f', 12u8),
            (b'n', b'\n'),
            (b'r', b'\r'),
            (b't', b'\t'),
        ] {
            a.if_(e.eq(esc as i32), |a| {
                put_byte(a, ctx, b, byte as i32);
                a.set(pos, pos + 2);
                a.cont(l);
            });
        }
        a.if_(e.ne(b'u' as i32), |a| {
            fail(a, ctx, "Invalid \\escape", s, pos);
            a.ret(0);
        });
        a.if_((pos + 6).gt(n), |a| {
            fail(a, ctx, "Invalid \\uXXXX escape", s, pos + 1);
            a.ret(0);
        });
        let v = hex4(a, s + pos + 2);
        a.if_(v.lt(0), |a| {
            fail(a, ctx, "Invalid \\uXXXX escape", s, pos + 1);
            a.ret(0);
        });
        a.set(cp, v);
        a.set(pos, pos + 6);
        // A high surrogate followed by `\uDC00`..`\uDFFF` is one character.
        let pair = cp.ge(0xd800)
            & cp.le(0xdbff)
            & (pos + 6).le(n)
            & (s + pos).load8(0).eq(b'\\' as i32)
            & (s + pos).load8(1).eq(b'u' as i32);
        a.if_(pair, |a| {
            let low = hex4(a, s + pos + 2);
            a.if_(low.ge(0xdc00) & low.le(0xdfff), |a| {
                a.set(cp, (((cp - 0xd800) << 10) | (low - 0xdc00)) + 0x10000);
                a.set(pos, pos + 6);
            });
        });
        encode(a, cp);
        a.cont(l);
    });
    a.store(I32, st, 0, pos);
    a.ret(call(h(ctx, JsonFn::BufFinish), vec![b.into()], Some(I32)));
}

/// Past one value of any kind: CPython's `scan_once`, keeping nothing.
fn build_skip(a: &mut Asm, ctx: &CompilationContext) {
    use Ty::I32;
    let (s, st) = (a.param(0), a.param(1));
    let n = a.let_((s - 4).load(I32, 0));
    let pos = a.let_(st.load(I32, 0));
    a.if_(pos.ge(n), |a| {
        fail(a, ctx, "Expecting value", s, pos);
        a.raw(I::Return);
    });
    let c = a.let_((s + pos).load8(0));
    let is_word = |word: &str| -> E {
        let mut cond: E = (pos + word.len() as i32).le(n);
        for (k, byte) in word.bytes().enumerate() {
            cond = cond & (s + pos).load8(k as u64).eq(byte as i32);
        }
        cond
    };
    for word in ["true", "false", "null", "NaN", "Infinity", "-Infinity"] {
        let len = word.len() as i32;
        a.if_(is_word(word), |a| {
            a.store(I32, st, 0, pos + len);
            a.raw(I::Return);
        });
    }
    a.if_(c.eq(b'"' as i32), |a| {
        a.run(call(
            h(ctx, JsonFn::ParseString),
            vec![s.into(), st.into()],
            Some(I32),
        ));
        a.raw(I::Return);
    });
    let skip_next = |a: &mut Asm| {
        a.run(call(h(ctx, JsonFn::Skip), vec![s.into(), st.into()], None));
        bail(a, None);
    };
    let ws = |a: &mut Asm| a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
    // An array or an object: members until the closing bracket.
    for (open, close, object) in [(b'[', b']', false), (b'{', b'}', true)] {
        a.if_(c.eq(open as i32), |a| {
            a.store(I32, st, 0, pos + 1);
            ws(a);
            let p0 = a.let_(st.load(I32, 0));
            a.if_(p0.lt(n) & (s + p0).load8(0).eq(close as i32), |a| {
                a.store(I32, st, 0, p0 + 1);
                a.raw(I::Return);
            });
            a.loop_(|a, l| {
                if object {
                    let p = a.let_(st.load(I32, 0));
                    a.if_(p.ge(n) | (s + p).load8(0).ne(b'"' as i32), |a| {
                        fail(
                            a,
                            ctx,
                            "Expecting property name enclosed in double quotes",
                            s,
                            p,
                        );
                        a.raw(I::Return);
                    });
                    a.run(call(
                        h(ctx, JsonFn::ParseString),
                        vec![s.into(), st.into()],
                        Some(I32),
                    ));
                    bail(a, None);
                    ws(a);
                    let q = a.let_(st.load(I32, 0));
                    a.if_(q.ge(n) | (s + q).load8(0).ne(b':' as i32), |a| {
                        fail(a, ctx, "Expecting ':' delimiter", s, q);
                        a.raw(I::Return);
                    });
                    a.store(I32, st, 0, q + 1);
                    ws(a);
                }
                skip_next(a);
                ws(a);
                let r = a.let_(st.load(I32, 0));
                a.if_(r.ge(n), |a| {
                    fail(a, ctx, "Expecting ',' delimiter", s, r);
                    a.raw(I::Return);
                });
                let d = a.let_((s + r).load8(0));
                a.if_(d.eq(close as i32), |a| {
                    a.store(I32, st, 0, r + 1);
                    a.raw(I::Return);
                });
                a.if_(d.ne(b',' as i32), |a| {
                    fail(a, ctx, "Expecting ',' delimiter", s, r);
                    a.raw(I::Return);
                });
                a.store(I32, st, 0, r + 1);
                ws(a);
                let t = a.let_(st.load(I32, 0));
                a.if_(t.lt(n) & (s + t).load8(0).eq(close as i32), |a| {
                    let what = if object { "object" } else { "array" };
                    fail(
                        a,
                        ctx,
                        &format!("Illegal trailing comma before end of {what}"),
                        s,
                        r,
                    );
                    a.raw(I::Return);
                });
                a.cont(l);
            });
        });
    }
    // A number, by CPython's grammar.
    let i = a.let_(pos);
    let digit = |at: E| -> E {
        let at2 = at.clone();
        (s + at).load8(0).ge(b'0' as i32) & (s + at2).load8(0).le(b'9' as i32)
    };
    let not_a_value = |a: &mut Asm| {
        fail(a, ctx, "Expecting value", s, pos);
        a.raw(I::Return);
    };
    a.if_(c.eq(b'-' as i32), |a| a.set(i, i + 1));
    a.if_(i.ge(n), |a| not_a_value(a));
    a.if_else(
        (s + i).load8(0).eq(b'0' as i32),
        |a| a.set(i, i + 1),
        |a| {
            a.if_(digit(i.into()).eqz(), |a| not_a_value(a));
            a.while_(i.lt(n) & digit(i.into()), |a, _| a.set(i, i + 1));
        },
    );
    a.if_(
        (i + 1).lt(n) & (s + i).load8(0).eq(b'.' as i32) & digit(i + 1),
        |a| {
            a.set(i, i + 2);
            a.while_(i.lt(n) & digit(i.into()), |a, _| a.set(i, i + 1));
        },
    );
    a.if_(
        (i + 1).lt(n) & ((s + i).load8(0).eq(b'e' as i32) | (s + i).load8(0).eq(b'E' as i32)),
        |a| {
            let save = a.let_(i);
            a.set(i, i + 1);
            a.if_(
                i.lt(n - 1) & ((s + i).load8(0).eq(b'+' as i32) | (s + i).load8(0).eq(b'-' as i32)),
                |a| {
                    a.set(i, i + 1);
                },
            );
            a.if_else(
                i.lt(n) & digit(i.into()),
                |a| a.while_(i.lt(n) & digit(i.into()), |a, _| a.set(i, i + 1)),
                |a| a.set(i, save),
            );
        },
    );
    a.store(I32, st, 0, i);
}

/// A value of `ty` at the state's position (past any whitespace).
fn build_load(a: &mut Asm, ctx: &CompilationContext, ty: &IRType, hook: Option<&Hook>, raw: bool) {
    use Ty::{F64, I32};
    let (s, st) = (a.param(0), a.param(1));
    let n = a.let_((s - 4).load(I32, 0));
    let pos = a.let_(st.load(I32, 0));
    let zero = zero_of(ty);
    a.if_(pos.ge(n), |a| {
        fail(a, ctx, "Expecting value", s, pos);
        a.ret(zero.clone());
    });
    let c = a.let_((s + pos).load8(0));
    let member = |t: &IRType| {
        h(
            ctx,
            JsonFn::Load {
                ty: t.clone(),
                hook: hook.cloned(),
                raw: false,
            },
        )
    };
    // An object, while there is a hook, is what the hook answers for it.
    if let Some(hook) = hook.filter(|_| !raw) {
        let word = a.let_(st.load(I32, STATE_HOOK));
        a.if_(c.eq(b'{' as i32) & word.ne(0), |a| {
            let parse = h(
                ctx,
                JsonFn::Load {
                    ty: hook.takes().clone(),
                    hook: Some(hook.clone()),
                    raw: true,
                },
            );
            let object = a.let_(call(parse, vec![s.into(), st.into()], Some(I32)));
            bail(a, Some(zero.clone()));
            let result = call_hook(a, ctx, hook, word.into(), object.into());
            bail(a, Some(zero.clone()));
            match fits(ctx, hook.gives(), ty) {
                Some(Fit::Same) => a.ret(result),
                Some(Fit::Boxed) => {
                    let cell = a.let_(call(ctx.alloc_func_index, vec![E::I32(8)], Some(I32)));
                    a.store(params_ty(hook.gives()), cell, 0, result);
                    a.ret(cell);
                }
                None => {
                    a.run(call(
                        h(
                            ctx,
                            JsonFn::HookMismatch {
                                gives: hook.gives().clone(),
                                want: ty.clone(),
                            },
                        ),
                        vec![s.into(), pos.into()],
                        None,
                    ));
                    a.ret(zero.clone());
                }
            }
        });
    }
    // Whether the text at the position is `word`.
    let is_word = |word: &str| -> E {
        let mut cond: E = (pos + word.len() as i32).le(n);
        for (k, byte) in word.bytes().enumerate() {
            cond = cond & (s + pos).load8(k as u64).eq(byte as i32);
        }
        cond
    };
    let mismatch = |a: &mut Asm| {
        a.run(call(
            h(ctx, JsonFn::Mismatch(ty.clone())),
            vec![s.into(), pos.into()],
            None,
        ));
        a.ret(zero.clone());
    };
    let value_start = c.eq(b'{' as i32)
        | c.eq(b'[' as i32)
        | c.eq(b'"' as i32)
        | c.eq(b'-' as i32)
        | (c.ge(b'0' as i32) & c.le(b'9' as i32))
        | is_word("true")
        | is_word("false")
        | is_word("null")
        | is_word("NaN")
        | is_word("Infinity");
    // A value of another kind is a mismatch; anything else is not a value.
    let other = |a: &mut Asm| {
        a.if_(value_start.clone(), |a| mismatch(a));
        fail(a, ctx, "Expecting value", s, pos);
        a.ret(zero.clone());
    };
    match ty {
        IRType::Int | IRType::Float => {
            let floaty = matches!(ty, IRType::Float);
            if floaty {
                for (word, value) in [
                    ("NaN", f64::NAN),
                    ("Infinity", f64::INFINITY),
                    ("-Infinity", f64::NEG_INFINITY),
                ] {
                    let len = word.len() as i32;
                    a.if_(is_word(word), |a| {
                        a.store(I32, st, 0, pos + len);
                        a.ret(value);
                    });
                }
            }
            // CPython's number grammar; anything that does not start one
            // is a value of another kind, or none.
            a.if_(
                (c.eq(b'-' as i32) | (c.ge(b'0' as i32) & c.le(b'9' as i32))).eqz(),
                |a| other(a),
            );
            let i = a.let_(pos);
            let is_float = a.let_(0);
            let digit = |at: E| -> E {
                let at2 = at.clone();
                (s + at).load8(0).ge(b'0' as i32) & (s + at2).load8(0).le(b'9' as i32)
            };
            let not_a_number = |a: &mut Asm| {
                fail(a, ctx, "Expecting value", s, pos);
                a.ret(zero.clone());
            };
            a.if_(c.eq(b'-' as i32), |a| a.set(i, i + 1));
            a.if_(i.ge(n), |a| not_a_number(a));
            a.if_else(
                (s + i).load8(0).eq(b'0' as i32),
                |a| a.set(i, i + 1),
                |a| {
                    a.if_(digit(i.into()).eqz(), |a| not_a_number(a));
                    a.while_(i.lt(n) & digit(i.into()), |a, _| a.set(i, i + 1));
                },
            );
            a.if_(
                (i + 1).lt(n) & (s + i).load8(0).eq(b'.' as i32) & digit(i + 1),
                |a| {
                    a.set(is_float, 1);
                    a.set(i, i + 2);
                    a.while_(i.lt(n) & digit(i.into()), |a, _| a.set(i, i + 1));
                },
            );
            a.if_(
                (i + 1).lt(n)
                    & ((s + i).load8(0).eq(b'e' as i32) | (s + i).load8(0).eq(b'E' as i32)),
                |a| {
                    let save = a.let_(i);
                    a.set(i, i + 1);
                    a.if_(
                        i.lt(n - 1)
                            & ((s + i).load8(0).eq(b'+' as i32) | (s + i).load8(0).eq(b'-' as i32)),
                        |a| {
                            a.set(i, i + 1);
                        },
                    );
                    a.if_else(
                        i.lt(n) & digit(i.into()),
                        |a| {
                            a.set(is_float, 1);
                            a.while_(i.lt(n) & digit(i.into()), |a, _| a.set(i, i + 1));
                        },
                        |a| a.set(i, save),
                    );
                },
            );
            if floaty {
                // The digits as a str, parsed as `float()` parses them.
                let len = a.let_(i - pos);
                let blob = a.let_(call(ctx.alloc_func_index, vec![len + 5], Some(I32)));
                a.store(I32, blob, 0, len);
                memcpy(a, blob + 4, s + pos, len);
                a.store8(blob + len + 4, 0, 0);
                a.store(I32, st, 0, i);
                a.ret(call(
                    ctx.helper(Helper::ParseFloat),
                    vec![blob + 4],
                    Some(F64),
                ));
            } else {
                a.if_(is_float.ne(0), |a| mismatch(a));
                let v = a.let_(0);
                let k = a.let_(pos);
                let neg = a.let_((s + pos).load8(0).eq(b'-' as i32));
                a.if_(neg.ne(0), |a| a.set(k, k + 1));
                a.while_(k.lt(i), |a, _| {
                    a.set(v, v * 10 + ((s + k).load8(0) - 48));
                    a.set(k, k + 1);
                });
                a.store(I32, st, 0, i);
                a.ret(neg.select(E::I32(0) - v, v));
            }
        }
        IRType::Bool => {
            a.if_(is_word("true"), |a| {
                a.store(I32, st, 0, pos + 4);
                a.ret(1);
            });
            a.if_(is_word("false"), |a| {
                a.store(I32, st, 0, pos + 5);
                a.ret(0);
            });
            other(a);
        }
        IRType::None => {
            a.if_(is_word("null"), |a| {
                a.store(I32, st, 0, pos + 4);
                a.ret(0);
            });
            other(a);
        }
        IRType::String => {
            a.if_(c.eq(b'"' as i32), |a| {
                a.ret(call(
                    h(ctx, JsonFn::ParseString),
                    vec![s.into(), st.into()],
                    Some(I32),
                ));
            });
            other(a);
        }
        IRType::Optional(inner) => {
            a.if_(is_word("null"), |a| {
                a.store(I32, st, 0, pos + 4);
                a.ret(0);
            });
            let value = a.let_(call(
                member(inner),
                vec![s.into(), st.into()],
                Some(params_ty(inner)),
            ));
            bail(a, Some(E::I32(0)));
            match inner.as_ref() {
                // An optional number is a cell holding it.
                IRType::Int | IRType::Bool | IRType::Float => {
                    let cell = a.let_(call(ctx.alloc_func_index, vec![E::I32(8)], Some(I32)));
                    a.store(params_ty(inner), cell, 0, value);
                    a.ret(cell);
                }
                _ => a.ret(value),
            }
        }
        IRType::List(_) | IRType::VarTuple(_) | IRType::Tuple(_) => {
            a.if_(c.ne(b'[' as i32), |a| other(a));
            a.store(I32, st, 0, pos + 1);
            a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
            let slots = a.let_(call(h(ctx, JsonFn::BufNew), vec![], Some(I32)));
            let count = a.let_(0);
            let members: Vec<IRType> = match ty {
                IRType::Tuple(ms) => ms.clone(),
                IRType::List(e) | IRType::VarTuple(e) => vec![(**e).clone()],
                _ => unreachable!(),
            };
            let fixed = matches!(ty, IRType::Tuple(_));
            let p0 = a.let_(st.load(I32, 0));
            let done = a.let_(0);
            a.if_(p0.lt(n) & (s + p0).load8(0).eq(b']' as i32), |a| {
                a.store(I32, st, 0, p0 + 1);
                a.set(done, 1);
            });
            a.if_(done.eqz(), |a| {
                a.loop_(|a, l| {
                    let slot = a.let_(call(
                        h(ctx, JsonFn::BufReserve),
                        vec![slots.into(), E::I32(8)],
                        Some(I32),
                    ));
                    let load_member = |a: &mut Asm, m: &IRType| {
                        let value = a.let_(call(
                            member(m),
                            vec![s.into(), st.into()],
                            Some(params_ty(m)),
                        ));
                        bail(a, Some(E::I32(0)));
                        a.store(params_ty(m), slot, 0, value);
                    };
                    if fixed {
                        // A tuple has as many members as its type says.
                        a.if_(count.ge(members.len() as i32), |a| {
                            a.run(call(
                                h(ctx, JsonFn::Mismatch(ty.clone())),
                                vec![s.into(), pos.into()],
                                None,
                            ));
                            a.ret(0);
                        });
                        for (k, m) in members.iter().enumerate() {
                            a.if_(count.eq(k as i32), |a| load_member(a, m));
                        }
                    } else {
                        load_member(a, &members[0]);
                    }
                    a.set(count, count + 1);
                    a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
                    let p = a.let_(st.load(I32, 0));
                    a.if_(p.ge(n), |a| {
                        fail(a, ctx, "Expecting ',' delimiter", s, p);
                        a.ret(0);
                    });
                    let d = a.let_((s + p).load8(0));
                    a.if_(d.eq(b']' as i32), |a| {
                        a.store(I32, st, 0, p + 1);
                        a.brk_if(l, 1);
                    });
                    a.if_(d.ne(b',' as i32), |a| {
                        fail(a, ctx, "Expecting ',' delimiter", s, p);
                        a.ret(0);
                    });
                    a.store(I32, st, 0, p + 1);
                    a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
                    let q = a.let_(st.load(I32, 0));
                    a.if_(q.lt(n) & (s + q).load8(0).eq(b']' as i32), |a| {
                        fail(a, ctx, "Illegal trailing comma before end of array", s, p);
                        a.ret(0);
                    });
                    a.cont(l);
                });
            });
            if fixed {
                a.if_(count.ne(members.len() as i32), |a| {
                    a.run(call(
                        h(ctx, JsonFn::Mismatch(ty.clone())),
                        vec![s.into(), pos.into()],
                        None,
                    ));
                    a.ret(0);
                });
            }
            // The slots are the list's data block.
            let header = a.let_(call(ctx.alloc_func_index, vec![E::I32(16)], Some(I32)));
            a.store(I32, header, 0, count);
            a.store(I32, header, COLLECTION_CAP as u64, slots.load(I32, 4) >> 3);
            a.store(I32, header, COLLECTION_DATA as u64, slots.load(I32, 8));
            a.ret(header);
        }
        IRType::Dict(_, val) => {
            a.if_(c.ne(b'{' as i32), |a| other(a));
            a.store(I32, st, 0, pos + 1);
            a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
            let entries = a.let_(call(h(ctx, JsonFn::BufNew), vec![], Some(I32)));
            let count = a.let_(0);
            let p0 = a.let_(st.load(I32, 0));
            let done = a.let_(0);
            a.if_(p0.lt(n) & (s + p0).load8(0).eq(b'}' as i32), |a| {
                a.store(I32, st, 0, p0 + 1);
                a.set(done, 1);
            });
            let val = (**val).clone();
            a.if_(done.eqz(), |a| {
                a.loop_(|a, l| {
                    let p = a.let_(st.load(I32, 0));
                    a.if_(p.ge(n) | (s + p).load8(0).ne(b'"' as i32), |a| {
                        fail(
                            a,
                            ctx,
                            "Expecting property name enclosed in double quotes",
                            s,
                            p,
                        );
                        a.ret(0);
                    });
                    let key = a.let_(call(
                        h(ctx, JsonFn::ParseString),
                        vec![s.into(), st.into()],
                        Some(I32),
                    ));
                    bail(a, Some(E::I32(0)));
                    a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
                    let q = a.let_(st.load(I32, 0));
                    a.if_(q.ge(n) | (s + q).load8(0).ne(b':' as i32), |a| {
                        fail(a, ctx, "Expecting ':' delimiter", s, q);
                        a.ret(0);
                    });
                    a.store(I32, st, 0, q + 1);
                    a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
                    let value = a.let_(call(
                        member(&val),
                        vec![s.into(), st.into()],
                        Some(params_ty(&val)),
                    ));
                    bail(a, Some(E::I32(0)));
                    // A repeated key keeps its first position and takes the
                    // later value, as a dict assignment does.
                    let data = a.let_(entries.load(I32, 8));
                    let k = a.local(I32);
                    let found = a.let_(-1);
                    a.for_(k, 0, count, false, |a, fl| {
                        let other_key = (data + k * 16).load(I32, 0);
                        let same = byte_compare(a, key.into(), other_key);
                        a.if_(same.eqz(), |a| {
                            a.set(found, k);
                            a.brk_if(fl, 1);
                        });
                    });
                    a.if_else(
                        found.ge(0),
                        |a| a.store(params_ty(&val), data + found * 16, 8, value),
                        |a| {
                            let slot = a.let_(call(
                                h(ctx, JsonFn::BufReserve),
                                vec![entries.into(), E::I32(16)],
                                Some(I32),
                            ));
                            a.store(I32, slot, 0, key);
                            a.store(params_ty(&val), slot, 8, value);
                            a.set(count, count + 1);
                        },
                    );
                    a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
                    let r = a.let_(st.load(I32, 0));
                    a.if_(r.ge(n), |a| {
                        fail(a, ctx, "Expecting ',' delimiter", s, r);
                        a.ret(0);
                    });
                    let d = a.let_((s + r).load8(0));
                    a.if_(d.eq(b'}' as i32), |a| {
                        a.store(I32, st, 0, r + 1);
                        a.brk_if(l, 1);
                    });
                    a.if_(d.ne(b',' as i32), |a| {
                        fail(a, ctx, "Expecting ',' delimiter", s, r);
                        a.ret(0);
                    });
                    a.store(I32, st, 0, r + 1);
                    a.run(call(h(ctx, JsonFn::Ws), vec![s.into(), st.into()], None));
                    let t = a.let_(st.load(I32, 0));
                    a.if_(t.lt(n) & (s + t).load8(0).eq(b'}' as i32), |a| {
                        fail(a, ctx, "Illegal trailing comma before end of object", s, r);
                        a.ret(0);
                    });
                    a.cont(l);
                });
            });
            let header = a.let_(call(ctx.alloc_func_index, vec![E::I32(16)], Some(I32)));
            a.store(I32, header, 0, count);
            a.store(
                I32,
                header,
                COLLECTION_CAP as u64,
                entries.load(I32, 4) >> 4,
            );
            a.store(I32, header, COLLECTION_DATA as u64, entries.load(I32, 8));
            a.ret(header);
        }
        // An instance comes only from the hook, for an object.
        IRType::Class(_) => other(a),
        _ => {
            a.ret(zero);
        }
    }
}

/// `ty` with its untyped words read as ints, as everywhere else (an empty
/// literal's elements, an unannotated collection's).
fn words_as_ints(ty: &IRType) -> IRType {
    let inner = |t: &IRType| Box::new(words_as_ints(t));
    match ty {
        IRType::Unknown | IRType::Any => IRType::Int,
        IRType::List(e) => IRType::List(inner(e)),
        IRType::VarTuple(e) => IRType::VarTuple(inner(e)),
        IRType::Tuple(ms) => IRType::Tuple(ms.iter().map(words_as_ints).collect()),
        IRType::Dict(k, v) => IRType::Dict(inner(k), inner(v)),
        IRType::Optional(e) => IRType::Optional(inner(e)),
        other => other.clone(),
    }
}

/// Why `dumps` of a `ty` cannot be given CPython's text here, if it cannot,
/// where `hook` is called for what JSON has no form for.
fn dump_unsupported(ty: &IRType, hook: Option<&Hook>, ctx: &CompilationContext) -> Option<String> {
    dump_check(ty, hook, ctx, &mut Vec::new())
}

/// `dump_unsupported`, `through` the types the hook is being asked for.
fn dump_check(
    ty: &IRType,
    hook: Option<&Hook>,
    ctx: &CompilationContext,
    through: &mut Vec<IRType>,
) -> Option<String> {
    match ty {
        IRType::Int | IRType::Bool | IRType::Float | IRType::String | IRType::None => None,
        IRType::Optional(inner) => dump_check(inner, hook, ctx, through),
        IRType::List(e) | IRType::VarTuple(e) => dump_check(e, hook, ctx, through),
        IRType::Tuple(ms) => ms.iter().find_map(|m| dump_check(m, hook, ctx, through)),
        IRType::Dict(k, v) => match k.as_ref() {
            IRType::String | IRType::Int | IRType::Float | IRType::Bool | IRType::None => {
                dump_check(v, hook, ctx, through)
            }
            other => Some(format!(
                "keys must be str, int, float, bool or None, not {}",
                crate::compiler::operators::python_type_name(other)
            )),
        },
        IRType::Unknown | IRType::Any => {
            Some("a value whose type is not known here. Hint: annotate it".to_string())
        }
        other => {
            let why = match other {
                IRType::Set(_) => "Object of type set is not JSON serializable".to_string(),
                IRType::Class(c) => format!("Object of type {c} is not JSON serializable"),
                other => format!(
                    "Object of type {} is not JSON serializable",
                    crate::compiler::operators::python_type_name(other)
                ),
            };
            let Some(hook) = hook else {
                return Some(why);
            };
            let takes = match (other, hook.takes()) {
                (t, w) if t == w => true,
                (IRType::Class(c), IRType::Class(w)) => ctx.is_class_or_subclass(c, w),
                _ => false,
            };
            if !takes {
                return Some(format!(
                    "{why}, and the default given takes a {}, not a {}",
                    crate::type_to_string(hook.takes()),
                    crate::type_to_string(other)
                ));
            }
            if through.contains(other) {
                return Some(format!(
                    "a {}, for which the default given answers a value it is called with again",
                    crate::type_to_string(other)
                ));
            }
            through.push(other.clone());
            let gives = hook.gives().clone();
            let why = dump_check(&gives, Some(hook), ctx, through);
            through.pop();
            why
        }
    }
}

/// Why `loads` cannot parse into a `ty`, if it cannot, where each object is
/// given to `hook`.
fn load_unsupported(ty: &IRType, hook: Option<&Hook>, ctx: &CompilationContext) -> Option<String> {
    match ty {
        IRType::Int | IRType::Bool | IRType::Float | IRType::String | IRType::None => None,
        IRType::Optional(inner) => load_unsupported(inner, hook, ctx),
        IRType::List(e) | IRType::VarTuple(e) => load_unsupported(e, hook, ctx),
        IRType::Tuple(ms) if !ms.is_empty() => {
            ms.iter().find_map(|m| load_unsupported(m, hook, ctx))
        }
        IRType::Dict(k, v) => match k.as_ref() {
            IRType::String => load_unsupported(v, hook, ctx),
            other => Some(format!(
                "a dict keyed by {}: a JSON object's keys are strs",
                crate::type_to_string(other)
            )),
        },
        IRType::Class(_) if hook.is_some_and(|h| fits(ctx, h.gives(), ty).is_some()) => None,
        IRType::Unknown | IRType::Any => Some(
            "a value whose type is not declared: json.loads() parses into the type the value \
             is declared to have. Hint: annotate the variable it is assigned to, for example \
             'data: dict[str, int] = json.loads(text)'"
                .to_string(),
        ),
        other => Some(format!(
            "a {}, which JSON has no value for",
            crate::type_to_string(other)
        )),
    }
}

/// The hook a closure value of type `t` is, reported where it cannot be one;
/// `what` names the argument.
fn closure_hook(t: &IRType, ctx: &CompilationContext, what: &str) -> Option<Hook> {
    let hook = match t {
        IRType::Callable {
            params,
            return_type,
        } => match params.as_slice() {
            [p] if !matches!(p, IRType::Unknown | IRType::Any)
                && !matches!(return_type.as_ref(), IRType::Unknown | IRType::Any) =>
            {
                Some(Hook::Closure {
                    takes: p.clone(),
                    gives: (**return_type).clone(),
                })
            }
            _ => None,
        },
        _ => None,
    };
    match hook {
        Some(Hook::Closure { takes, gives }) if closure_type(ctx, &takes, &gives).is_some() => {
            Some(Hook::Closure { takes, gives })
        }
        _ => {
            ctx.report(format!(
                "{what} must be a function of one parameter, both it and the return annotated, \
                 for example 'def encode(p: Point) -> dict[str, int]': it is called with values \
                 of that type. Hint: a lambda's parameter has no type here, so use a 'def'"
            ));
            None
        }
    }
}

/// Why an `object_hook` taking `takes` cannot be called with the objects of
/// a document, if it cannot.
fn hook_unsupported(hook: &Hook, ctx: &CompilationContext) -> Option<String> {
    match hook.takes() {
        IRType::Dict(k, v) if matches!(k.as_ref(), IRType::String) => {
            load_unsupported(v, Some(hook), ctx)
        }
        other => Some(format!(
            "an object_hook taking a {}: it is called with each JSON object, a dict[str, ...]",
            crate::type_to_string(other)
        )),
    }
}

/// The constant value of a keyword argument, or why it must be one.
fn constant<'e>(args: &'e [IRExpr], name: &str) -> Option<&'e IRExpr> {
    args.iter().find_map(|a| match a {
        IRExpr::Keyword { name: n, value } if n == name => Some(value.as_ref()),
        _ => None,
    })
}

fn dump_opts(arguments: &[IRExpr], ctx: &CompilationContext) -> Option<DumpOpts> {
    let refuse = |m: String| {
        ctx.report(m);
        None
    };
    for a in arguments {
        if let IRExpr::Keyword { name, .. } = a {
            if !matches!(
                name.as_str(),
                "indent"
                    | "separators"
                    | "sort_keys"
                    | "ensure_ascii"
                    | "allow_nan"
                    | "skipkeys"
                    | "check_circular"
                    | "default"
            ) {
                return refuse(format!("json.dumps()'s '{name}' argument is not supported"));
            }
        }
    }
    let flag = |name: &str, default: bool| -> Option<bool> {
        match constant(arguments, name) {
            None => Some(default),
            Some(IRExpr::Const(IRConstant::Bool(b))) => Some(*b),
            Some(_) => None,
        }
    };
    // `skipkeys` skips only keys of a type this refuses, and `check_circular`
    // finds only cycles no typed value has, so neither changes the text.
    let (Some(sort_keys), Some(ensure_ascii), Some(allow_nan), Some(_), Some(_)) = (
        flag("sort_keys", false),
        flag("ensure_ascii", true),
        flag("allow_nan", true),
        flag("skipkeys", false),
        flag("check_circular", true),
    ) else {
        return refuse("json.dumps()'s flags must be the constants True or False".to_string());
    };
    let indent = match constant(arguments, "indent") {
        None | Some(IRExpr::Const(IRConstant::None)) => None,
        Some(IRExpr::Const(IRConstant::Int(n))) => Some(" ".repeat((*n).max(0) as usize)),
        Some(IRExpr::Const(IRConstant::String(s))) => Some(s.clone()),
        Some(_) => return refuse("json.dumps()'s indent must be a constant".to_string()),
    };
    let (item_sep, key_sep) = match constant(arguments, "separators") {
        None | Some(IRExpr::Const(IRConstant::None)) => (
            if indent.is_some() { "," } else { ", " }.to_string(),
            ": ".to_string(),
        ),
        Some(IRExpr::TupleLiteral(items)) => match items.as_slice() {
            [IRExpr::Const(IRConstant::String(i)), IRExpr::Const(IRConstant::String(k))] => {
                (i.clone(), k.clone())
            }
            _ => {
                return refuse(
                    "json.dumps()'s separators must be a tuple of two constant strs".to_string(),
                )
            }
        },
        Some(_) => {
            return refuse(
                "json.dumps()'s separators must be a tuple of two constant strs".to_string(),
            )
        }
    };
    Some(DumpOpts {
        indent,
        item_sep,
        key_sep,
        sort_keys,
        ensure_ascii,
        allow_nan,
    })
}

/// Call `index` from the program's code, with the bookkeeping of a call that
/// can raise.
fn raising_call(func: &mut Function, ctx: &CompilationContext, index: u32, result_f64: bool) {
    emit_call_depth_step(func, 1);
    func.instruction(&I::Call(index));
    emit_call_depth_step(func, -1);
    if result_f64 {
        func.instruction(&I::LocalSet(ctx.temp_local_f64));
        emit_post_call_check(func, ctx);
        func.instruction(&I::LocalGet(ctx.temp_local_f64));
    } else {
        func.instruction(&I::LocalSet(ctx.temp_local));
        emit_post_call_check(func, ctx);
        func.instruction(&I::LocalGet(ctx.temp_local));
    }
}

/// Drop the words of a value of `ty` emitted as a pair or one word, and
/// push a placeholder of `result`, after a refusal.
fn abandon(func: &mut Function, words: usize, result: &IRType) -> IRType {
    for _ in 0..words {
        func.instruction(&I::Drop);
    }
    match result {
        IRType::Float => {
            func.instruction(&I::F64Const(0.0.into()));
        }
        IRType::String => {
            func.instruction(&I::I32Const(0));
            func.instruction(&I::I32Const(0));
        }
        IRType::None => {}
        _ => {
            func.instruction(&I::I32Const(0));
        }
    }
    result.clone()
}

/// Emit the value `dumps` or `encode` writes, a single word (or f64) on the
/// stack, and answer the type it is written by.
fn emit_dumped(
    value: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    // A collections class or a named tuple serializes as the dict or tuple
    // it is.
    let value = crate::compiler::dunders::json_view(value, ctx).unwrap_or_else(|| value.clone());
    let emitted = emit_expr(&value, func, ctx, memory_layout, None);
    if matches!(emitted, IRType::String | IRType::Bytes) {
        func.instruction(&I::Drop);
    }
    match &emitted {
        // A whole value with no type has none to write by.
        IRType::Unknown | IRType::Any => emitted,
        other => words_as_ints(other),
    }
}

/// Emit `json.<name>(...)`, or `None` when it is not implemented.
pub(crate) fn emit_call(
    name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected: Option<&IRType>,
) -> Option<IRType> {
    let positional: Vec<&IRExpr> = arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect();
    match name {
        "dumps" | "dump" => {
            let want = if name == "dump" { 2 } else { 1 };
            if positional.len() != want {
                ctx.report(format!(
                    "json.{name}() takes the value{} positionally",
                    if want == 2 { " and the file" } else { "" }
                ));
                return Some(abandon(
                    func,
                    0,
                    if want == 2 {
                        &IRType::None
                    } else {
                        &IRType::String
                    },
                ));
            }
            let opts = dump_opts(arguments, ctx)?;
            if name == "dump" {
                // `fp.write(dumps(obj, ...))`.
                let mut dumps_args: Vec<IRExpr> = vec![positional[0].clone()];
                dumps_args.extend(
                    arguments
                        .iter()
                        .filter(|a| matches!(a, IRExpr::Keyword { .. }))
                        .cloned(),
                );
                let write = IRExpr::MethodCall {
                    object: Box::new(positional[1].clone()),
                    method_name: "write".to_string(),
                    arguments: vec![IRExpr::MethodCall {
                        object: Box::new(IRExpr::Variable("json".to_string())),
                        method_name: "dumps".to_string(),
                        arguments: dumps_args,
                    }],
                };
                let t = emit_expr(&write, func, ctx, memory_layout, None);
                if !matches!(t, IRType::None) {
                    func.instruction(&I::Drop);
                }
                return Some(IRType::None);
            }
            let ty = emit_dumped(positional[0], func, ctx, memory_layout);
            // `default=`, the closure called for a value with no form of its
            // own, after the value as CPython evaluates them.
            let hook = match constant(arguments, "default") {
                None | Some(IRExpr::Const(IRConstant::None)) => {
                    func.instruction(&I::I32Const(0));
                    None
                }
                Some(default) => {
                    let t = emit_value(default, func, ctx, memory_layout, None);
                    match closure_hook(&t, ctx, "json.dumps()'s default") {
                        Some(hook) => Some(hook),
                        None => return Some(abandon(func, 2, &IRType::String)),
                    }
                }
            };
            if let Some(why) = dump_unsupported(&ty, hook.as_ref(), ctx) {
                ctx.report(format!("json.dumps() of {why}"));
                return Some(abandon(func, 2, &IRType::String));
            }
            let index = h(ctx, JsonFn::Dumps { ty, opts, hook });
            raising_call(func, ctx, index, false);
            recover_str_pair(func, ctx);
            Some(IRType::String)
        }
        "loads" | "load" => {
            let target = expected.cloned().unwrap_or(IRType::Unknown);
            let mut object_hook = None;
            for a in arguments {
                if let IRExpr::Keyword { name: k, value } = a {
                    match k.as_str() {
                        "object_hook" if matches!(**value, IRExpr::Const(IRConstant::None)) => {}
                        "object_hook" => object_hook = Some(value.as_ref()),
                        _ => {
                            ctx.report(format!("json.{name}()'s '{k}' argument is not supported"));
                            return Some(abandon(func, 0, &target));
                        }
                    }
                }
            }
            if positional.len() != 1 {
                ctx.report(format!("json.{name}() takes one argument positionally"));
                return Some(abandon(func, 0, &target));
            }
            let source = if name == "load" {
                IRExpr::MethodCall {
                    object: Box::new(positional[0].clone()),
                    method_name: "read".to_string(),
                    arguments: Vec::new(),
                }
            } else {
                positional[0].clone()
            };
            let t = emit_expr(&source, func, ctx, memory_layout, Some(&IRType::String));
            if !matches!(t, IRType::String) {
                ctx.report(format!(
                    "json.loads() takes a str, not a {}",
                    crate::type_to_string(&t)
                ));
                let words = if matches!(t, IRType::Bytes) { 2 } else { 1 };
                return Some(abandon(func, words, &target));
            }
            func.instruction(&I::Drop);
            let hook = match object_hook {
                None => {
                    func.instruction(&I::I32Const(0));
                    None
                }
                Some(e) => {
                    let t = emit_value(e, func, ctx, memory_layout, None);
                    match closure_hook(&t, ctx, &format!("json.{name}()'s object_hook")) {
                        Some(hook) => Some(hook),
                        None => return Some(abandon(func, 2, &target)),
                    }
                }
            };
            emit_loads(name, target, hook, func, ctx)
        }
        _ => None,
    }
}

/// Call the parser of `target`, the document and the hook's word on the
/// stack, after checking the target can be parsed into.
fn emit_loads(
    name: &str,
    target: IRType,
    hook: Option<Hook>,
    func: &mut Function,
    ctx: &CompilationContext,
) -> Option<IRType> {
    if let Some(why) = hook.as_ref().and_then(|h| hook_unsupported(h, ctx)) {
        ctx.report(format!("json.{name}() with {why}"));
        return Some(abandon(func, 2, &target));
    }
    if let Some(why) = load_unsupported(&target, hook.as_ref(), ctx) {
        ctx.report(format!("json.{name}() into {why}"));
        return Some(abandon(func, 2, &target));
    }
    let index = h(
        ctx,
        JsonFn::Loads {
            ty: target.clone(),
            hook,
        },
    );
    raising_call(func, ctx, index, matches!(target, IRType::Float));
    if matches!(target, IRType::String) {
        recover_str_pair(func, ctx);
    }
    Some(target)
}

/// The `default` method an encoder of `class` calls, if one of the
/// program's classes defines it; refused where an instance of a subclass
/// could call another.
fn encoder_hook(ctx: &CompilationContext, class: &str) -> Result<Option<Hook>, String> {
    let Some(info) = ctx.get_class_info(class) else {
        return Ok(None);
    };
    let (Some(owner), Some(&index)) = (
        info.method_owner.get("default"),
        info.methods.get("default"),
    ) else {
        return Ok(None);
    };
    let overridden = ctx.class_map.values().any(|ci| {
        ci.name != class
            && ci.mro.iter().any(|c| c == class)
            && ci.method_owner.get("default") != Some(owner)
    });
    if overridden {
        return Err(format!(
            "encode() on a {class}, a subclass of which overrides default(), is not supported:              the default called is the one {class} has"
        ));
    }
    let Some(f) = ctx.get_function_info(&format!("{owner}::default")) else {
        return Ok(None);
    };
    match (f.param_types.get(1), &f.return_type) {
        (Some(takes), gives)
            if !matches!(takes, IRType::Unknown | IRType::Any)
                && !matches!(gives, IRType::Unknown | IRType::Any) =>
        {
            Ok(Some(Hook::Method {
                index,
                takes: takes.clone(),
                gives: gives.clone(),
            }))
        }
        _ => Err(format!(
            "{owner}.default() must annotate its parameter and its return, for example \
             'def default(self, o: Point) -> dict[str, int]': it is called with values of that \
             type"
        )),
    }
}

/// `encoder.encode(o)` or `decoder.decode(s)` on an instance of `json`'s
/// classes or the program's subclasses of them; `None` for any other call.
pub(crate) fn emit_instance_call(
    object: &IRExpr,
    method_name: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected: Option<&IRType>,
) -> Option<IRType> {
    if !matches!(
        method_name,
        "encode" | "decode" | "iterencode" | "raw_decode"
    ) {
        return None;
    }
    let IRType::Class(class) = infer_value_type(object, ctx) else {
        return None;
    };
    let encoder = ctx.is_class_or_subclass(&class, ENCODER_CLASS);
    let decoder = ctx.is_class_or_subclass(&class, DECODER_CLASS);
    if !(encoder || decoder) {
        return None;
    }
    // A method of the program's own is called as any other.
    let own = ctx
        .get_class_info(&class)
        .is_some_and(|ci| ci.methods.contains_key(method_name));
    if own {
        return None;
    }
    let target = expected.cloned().unwrap_or(IRType::Unknown);
    let one_argument = arguments.len() == 1 && !matches!(arguments[0], IRExpr::Keyword { .. });
    match method_name {
        "encode" if encoder => {
            if !one_argument {
                ctx.report("JSONEncoder.encode() takes one argument");
                return Some(abandon(func, 0, &IRType::String));
            }
            emit_expr(object, func, ctx, memory_layout, None);
            let ty = emit_dumped(&arguments[0], func, ctx, memory_layout);
            let hook = match encoder_hook(ctx, &class) {
                Ok(hook) => hook,
                Err(why) => {
                    ctx.report(why);
                    return Some(abandon(func, 2, &IRType::String));
                }
            };
            if let Some(why) = dump_unsupported(&ty, hook.as_ref(), ctx) {
                ctx.report(format!("JSONEncoder.encode() of {why}"));
                return Some(abandon(func, 2, &IRType::String));
            }
            let index = h(ctx, JsonFn::Encode { ty, class, hook });
            raising_call(func, ctx, index, false);
            recover_str_pair(func, ctx);
            Some(IRType::String)
        }
        "decode" if decoder => {
            if !one_argument {
                ctx.report("JSONDecoder.decode() takes one argument");
                return Some(abandon(func, 0, &target));
            }
            let held = crate::compiler::equality::hold(ctx);
            emit_expr(object, func, ctx, memory_layout, None);
            func.instruction(&I::LocalSet(held));
            let t = emit_expr(
                &arguments[0],
                func,
                ctx,
                memory_layout,
                Some(&IRType::String),
            );
            if !matches!(t, IRType::String) {
                ctx.release_held();
                ctx.report(format!(
                    "JSONDecoder.decode() takes a str, not a {}",
                    crate::type_to_string(&t)
                ));
                let words = if matches!(t, IRType::Bytes) { 2 } else { 1 };
                return Some(abandon(func, words, &target));
            }
            func.instruction(&I::Drop);
            // The hook the decoder was made with, called at the signature
            // the program's object_hooks share.
            let offset = ctx
                .get_class_info(&class)
                .and_then(|ci| ci.field_offsets.get(OBJECT_HOOK_FIELD).copied())
                .unwrap_or(0);
            func.instruction(&I::LocalGet(held));
            func.instruction(&I::I32Load(wasm_encoder::MemArg {
                offset,
                align: 2,
                memory_index: 0,
            }));
            ctx.release_held();
            let hook = ctx
                .get_function_info(&format!("{DECODER_CLASS}::{OBJECT_HOOK_TYPE_METHOD}"))
                .and_then(|f| {
                    Some(Hook::Closure {
                        takes: f.param_types.get(1)?.clone(),
                        gives: f.return_type.clone(),
                    })
                });
            emit_loads("decode", target, hook, func, ctx)
        }
        _ => {
            ctx.report(format!(
                "{}.{method_name}() is not supported",
                if encoder {
                    "JSONEncoder"
                } else {
                    "JSONDecoder"
                }
            ));
            Some(abandon(
                func,
                0,
                if method_name == "iterencode" {
                    &IRType::String
                } else {
                    &IRType::Unknown
                },
            ))
        }
    }
}

/// What `json.<name>()` answers, before code is generated.
pub(crate) fn result_type(name: &str) -> Option<IRType> {
    match name {
        "dumps" => Some(IRType::String),
        "dump" => Some(IRType::None),
        _ => None,
    }
}
