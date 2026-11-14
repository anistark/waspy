//! The JavaScript bridge: JS values in Python, as Pyodide hands them over.
//!
//! A program that imports `js` (or `pyodide`) reaches JavaScript through
//! the `waspy_js` imports, which the loader (`waspy::js_loader()`) provides.
//! JS values live in a table on the JavaScript side; Python holds one as a
//! `JsProxy` (`IRType::JsProxy`): a block holding the value's handle, which
//! the collector gives back to the table when the block dies. So a JS value
//! stays alive while Python can reach it, and no longer.
//!
//! What a JS value is to Python follows Pyodide: a number is an `int` when
//! it is a safe integer and a `float` otherwise, a string a `str`, a boolean
//! a `bool`, `undefined` None, `null` Pyodide's `jsnull`, and anything else
//! a proxy whose attributes and calls resolve by name at run time. Types are
//! static here, so the value stays a proxy until it is used, and is
//! converted there: stored where a type is declared (checked, with a
//! `TypeError` for a value of another kind, where Pyodide would keep it),
//! printed or given to `str()`, `int()`, `float()`, or `bool()`, compared,
//! or tested for truth. `.to_py()` converts a whole JS array or object into
//! the declared list or dict, and `to_js()` a list, tuple, or dict into a
//! JS array or object, both through JSON, which a value JSON cannot carry
//! refuses loudly.
//!
//! A Python value passed to JavaScript (an argument, a property set) is an
//! int, float, str, bool, None, or a proxy; anything else is refused with a
//! hint to pass `to_js(x)`, since Pyodide would pass a proxy of the Python
//! object, which has no counterpart here. A JS exception comes back as
//! `pyodide.ffi.JsException`, whose text is JavaScript's `String(error)`.

use crate::compiler::asm::{call, Asm, Ty, E, X};
use crate::compiler::context::CompilationContext;
use crate::compiler::emit::Function;
use crate::compiler::runtime::Helper;
use crate::ir::{IRConstant, IRExpr, IRModule, IRType};
use wasm_encoder::ValType;

/// The import module the bridge's functions come from.
pub const MODULE: &str = "waspy_js";

/// The intrinsics the bridge's operations are rewritten into (see
/// `rewrite`), each emitted by `emit_intrinsic`.
pub const JS_PREFIX: &str = "__js_";

/// A function of the bridge, imported from [`MODULE`]. A handle is an index
/// into the JavaScript side's table; a negative one answers a failure: -1
/// the attribute (or item) is not there, -2 JavaScript threw (`error()` has
/// it), -3 the operation does not apply to the value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum JsImport {
    /// `(name, len) -> h`: `globalThis[name]`, -1 when it has none.
    Global,
    /// `() -> h`: `globalThis`.
    GlobalThis,
    /// `(h, name, len) -> h`: `h[name]`.
    Get,
    /// `(h, name, len, v) -> status`: `h[name] = v`.
    Set,
    /// `(f, args) -> h`: `f(...args)`.
    Call,
    /// `(h, name, len, args) -> h`: `h[name](...args)`.
    CallMethod,
    /// `(f, args) -> h`: `new f(...args)`.
    Construct,
    /// `() -> h`: a new array.
    Array,
    /// `(array, v)`: push `v` (the value, not the handle).
    Push,
    /// `(x: f64) -> h`, `(b) -> h`, `(s, len) -> h`, `() -> h`.
    Number,
    Boolean,
    String,
    Undefined,
    /// `(h) -> kind`: 0 undefined, 1 null, 2 false, 3 true, 4 a safe integer,
    /// 5 another number, 6 a string, 7 a function, 8 an array, 9 another
    /// object, 10 anything else.
    Kind,
    /// `(h) -> f64`.
    ToNumber,
    /// `(h) -> h`: `String(h)`.
    Text,
    /// `(h) -> n`: the UTF-8 length of the string `h`, whose bytes
    /// `Utf8Copy` copies next.
    Utf8,
    /// `(at)`.
    Utf8Copy,
    /// `(a, b) -> bool`: `a === b`.
    Equal,
    /// `(h) -> n`: its `length` (or `size`), -1 when it has neither.
    Length,
    /// `(h, i) -> h`: an array's element.
    Item,
    /// `(h) -> it`, `(it) -> h`: the iteration protocol (-1 when done).
    Iterate,
    Next,
    /// `() -> h`: what JavaScript threw last.
    Error,
    /// `(h)`: let go of a handle.
    Release,
    /// `(h) -> h`: a second handle to the same value.
    Keep,
    /// `(h) -> h`: `JSON.stringify(h)`, refusing (-3) what JSON would not
    /// carry over as it is (a Set, a function, a cycle, a Map with keys that
    /// are not strings; one with string keys is the object it reads as).
    Stringify,
    /// `(s, len) -> h`: `JSON.parse(s)`.
    Parse,
    /// `(name, len) -> h`: the separately compiled module the host loaded
    /// under the name, -1 when it has none.
    ImportModule,
    /// `(s, len)`: what a Python exception escaping a call from JavaScript
    /// was, for the loader to throw.
    Fail,
    /// `(promise, token)`: settle the future `token` names when the promise
    /// does, through the export `__waspy_settle`.
    Then,
    /// `(token, failed, h)`: resolve (or reject) the Promise an exported
    /// coroutine's call answered.
    Resolve,
}

impl JsImport {
    pub fn name(self) -> &'static str {
        match self {
            JsImport::Global => "global",
            JsImport::GlobalThis => "global_this",
            JsImport::Get => "get",
            JsImport::Set => "set",
            JsImport::Call => "call",
            JsImport::CallMethod => "call_method",
            JsImport::Construct => "construct",
            JsImport::Array => "array",
            JsImport::Push => "push",
            JsImport::Number => "number",
            JsImport::Boolean => "boolean",
            JsImport::String => "string",
            JsImport::Undefined => "undefined",
            JsImport::Kind => "kind",
            JsImport::ToNumber => "to_number",
            JsImport::Text => "text",
            JsImport::Utf8 => "utf8",
            JsImport::Utf8Copy => "utf8_copy",
            JsImport::Equal => "equal",
            JsImport::Length => "length",
            JsImport::Item => "item",
            JsImport::Iterate => "iterate",
            JsImport::Next => "next",
            JsImport::Error => "error",
            JsImport::Release => "release",
            JsImport::Keep => "keep",
            JsImport::Stringify => "stringify",
            JsImport::Parse => "parse",
            JsImport::ImportModule => "import_module",
            JsImport::Fail => "fail",
            JsImport::Then => "then",
            JsImport::Resolve => "resolve",
        }
    }

    pub fn signature(self) -> (Vec<ValType>, Vec<ValType>) {
        use ValType::{F64, I32};
        match self {
            JsImport::Global | JsImport::String | JsImport::Parse | JsImport::ImportModule => {
                (vec![I32; 2], vec![I32])
            }
            JsImport::GlobalThis | JsImport::Array | JsImport::Undefined | JsImport::Error => {
                (vec![], vec![I32])
            }
            JsImport::Get => (vec![I32; 3], vec![I32]),
            JsImport::Set | JsImport::CallMethod => (vec![I32; 4], vec![I32]),
            JsImport::Call | JsImport::Construct | JsImport::Equal | JsImport::Item => {
                (vec![I32; 2], vec![I32])
            }
            JsImport::Push => (vec![I32; 2], vec![]),
            JsImport::Number => (vec![F64], vec![I32]),
            JsImport::Boolean
            | JsImport::Kind
            | JsImport::Text
            | JsImport::Utf8
            | JsImport::Length
            | JsImport::Iterate
            | JsImport::Next
            | JsImport::Keep
            | JsImport::Stringify => (vec![I32], vec![I32]),
            JsImport::ToNumber => (vec![I32], vec![F64]),
            JsImport::Utf8Copy | JsImport::Release => (vec![I32], vec![]),
            JsImport::Fail | JsImport::Then => (vec![I32; 2], vec![]),
            JsImport::Resolve => (vec![I32; 3], vec![]),
        }
    }

    pub const ALL: [JsImport; 32] = [
        JsImport::Global,
        JsImport::GlobalThis,
        JsImport::Get,
        JsImport::Set,
        JsImport::Call,
        JsImport::CallMethod,
        JsImport::Construct,
        JsImport::Array,
        JsImport::Push,
        JsImport::Number,
        JsImport::Boolean,
        JsImport::String,
        JsImport::Undefined,
        JsImport::Kind,
        JsImport::ToNumber,
        JsImport::Text,
        JsImport::Utf8,
        JsImport::Utf8Copy,
        JsImport::Equal,
        JsImport::Length,
        JsImport::Item,
        JsImport::Iterate,
        JsImport::Next,
        JsImport::Error,
        JsImport::Release,
        JsImport::Keep,
        JsImport::Stringify,
        JsImport::Parse,
        JsImport::ImportModule,
        JsImport::Fail,
        JsImport::Then,
        JsImport::Resolve,
    ];
}

/// Whether a module uses the bridge.
pub(crate) fn uses_bridge(module: &IRModule) -> bool {
    crate::ir::uses_js_bridge(module)
}

/// The index of a bridge import.
pub(crate) fn import(ctx: &CompilationContext, f: JsImport) -> u32 {
    *ctx.js.get(&f).unwrap_or_else(|| {
        panic!(
            "the bridge's {} import was not reserved; the module does not use the bridge",
            f.name()
        )
    })
}

/// A runtime helper of the bridge.
#[derive(Clone, Debug, PartialEq)]
pub enum JsFn {
    /// `(h) -> proxy`: a block holding the handle, which the collector
    /// releases with it.
    Wrap,
    /// `(status, name) -> ()`: leave the exception a failed operation
    /// raises pending (nothing for a status that is not negative).
    Check(JsOp),
    /// `(h) -> str`: the JavaScript string `h` as a `str`.
    Text,
    /// `(proxy) -> str`: `str(x)` of the value Pyodide would hand over.
    Str,
    /// `(proxy) -> str`: `repr(x)`.
    Repr,
    /// `(proxy) -> value`: the value stored where `ty` is declared, raising
    /// `TypeError` for one of another kind.
    As(IRType),
    /// `(proxy) -> i32`, `-> f64`: `int(x)`, `float(x)`.
    Int,
    Float,
    /// `(proxy) -> bool`: its truth, as Pyodide's value's.
    Truth,
    /// `(proxy) -> n`: `len(x)`.
    Len,
    /// `(proxy) -> value`: `x.to_py()`, into `ty`, through JSON.
    ToPy(IRType),
    /// `(value) -> h`: `to_js(x)` of a value of `ty`, through JSON.
    ToJs(IRType),
    /// `(kind) -> str`: what a value of the kind is called in an error.
    KindName,
    /// `(h, x: f64) -> bool`: the value equals the number, as Python's
    /// would (a bool as 0 or 1).
    EqNumber,
}

/// What a failed bridge operation was, for its error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JsOp {
    /// An attribute read (-1 is AttributeError).
    Attribute,
    /// A method call (-1 is AttributeError, -3 not callable).
    Method,
    /// A call (-3 is "not callable").
    Call,
    /// `new` (-3 is "not a constructor").
    Construct,
    /// An item (-1 IndexError, -3 not subscriptable).
    Item,
    /// Iteration (-3 not iterable).
    Iterate,
    /// `to_py()` (-3: what JSON would not carry).
    Stringify,
    /// A module loaded by name (-1 ModuleNotFoundError).
    Import,
    /// Anything else that only throws.
    Other,
}

impl JsFn {
    pub(crate) fn signature(&self) -> (Vec<ValType>, Vec<ValType>) {
        use ValType::{F64, I32};
        let word = |t: &IRType| if matches!(t, IRType::Float) { F64 } else { I32 };
        match self {
            JsFn::Wrap
            | JsFn::Text
            | JsFn::Str
            | JsFn::Repr
            | JsFn::Int
            | JsFn::Truth
            | JsFn::Len
            | JsFn::KindName => (vec![I32], vec![I32]),
            JsFn::Check(_) => (vec![I32, I32], vec![]),
            JsFn::EqNumber => (vec![I32, F64], vec![I32]),
            JsFn::Float => (vec![I32], vec![F64]),
            JsFn::As(ty) | JsFn::ToPy(ty) => (vec![I32], vec![word(ty)]),
            JsFn::ToJs(ty) => (vec![word(ty)], vec![I32]),
        }
    }
}

fn helper(ctx: &CompilationContext, f: JsFn) -> u32 {
    ctx.helper(Helper::Js(f))
}
fn imp(ctx: &CompilationContext, f: JsImport) -> u32 {
    import(ctx, f)
}
fn text(ctx: &CompilationContext, s: &str) -> E {
    call(
        ctx.helper(Helper::ExcText(s.to_string())),
        vec![],
        Some(Ty::I32),
    )
}
fn concat(ctx: &CompilationContext, a: impl Into<E>, b: impl Into<E>) -> E {
    call(
        ctx.helper(Helper::Concat),
        vec![a.into(), b.into()],
        Some(Ty::I32),
    )
}
fn raise_text(a: &mut Asm, ctx: &CompilationContext, class: &str, message: impl Into<E>) {
    a.run(call(
        ctx.helper(Helper::RaiseText(class.to_string())),
        vec![message.into()],
        None,
    ));
}
fn handle(proxy: impl Into<E>) -> E {
    proxy.into().load(Ty::I32, 0)
}

/// The block flag saying a block holds a JS handle, which the collector
/// releases when it frees the block.
pub const JSREF: i32 = crate::compiler::gc::JSREF;

pub(crate) fn build(f: &JsFn, ctx: &CompilationContext) -> Function {
    match f {
        JsFn::Wrap => build_wrap(ctx),
        JsFn::Check(op) => build_check(ctx, *op),
        JsFn::Text => build_text(ctx),
        JsFn::Str => build_str(ctx, false),
        JsFn::Repr => build_str(ctx, true),
        JsFn::As(ty) => build_as(ctx, ty),
        JsFn::Int => build_int(ctx),
        JsFn::Float => build_float(ctx),
        JsFn::Truth => build_truth(ctx),
        JsFn::Len => build_len(ctx),
        JsFn::ToPy(ty) => build_to_py(ctx, ty),
        JsFn::ToJs(ty) => build_to_js(ctx, ty),
        JsFn::KindName => build_kind_name(ctx),
        JsFn::EqNumber => build_eq_number(ctx),
    }
}

fn build_wrap(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let h = a.param(0);
    let p = a.let_(call(ctx.alloc_fn(), vec![E::I32(4)], Some(Ty::I32)));
    a.store(Ty::I32, p, 0, h);
    crate::compiler::gc::charge(&mut a, crate::compiler::gc::JS_VALUE_CHARGE);
    // The block holds a handle, which is no address: the collector reads
    // nothing in it, and gives the handle back when it frees it.
    a.store(Ty::I32, p - 4, 0, crate::compiler::gc::NOSCAN | JSREF);
    a.ret(p);
    a.finish()
}

fn build_check(ctx: &CompilationContext, op: JsOp) -> Function {
    let mut a = Asm::new(&[Ty::I32, Ty::I32]);
    let (status, name) = (a.param(0), a.param(1));
    a.if_(status.ge(0), |a| a.ret_void());
    // JavaScript threw.
    a.if_(status.eq(-2), |a| {
        let e = a.let_(call(imp(ctx, JsImport::Error), vec![], Some(Ty::I32)));
        let t = a.let_(call(
            imp(ctx, JsImport::Text),
            vec![e.into()],
            Some(Ty::I32),
        ));
        let message = a.let_(call(helper(ctx, JsFn::Text), vec![t.into()], Some(Ty::I32)));
        a.run(call(imp(ctx, JsImport::Release), vec![t.into()], None));
        a.run(call(imp(ctx, JsImport::Release), vec![e.into()], None));
        raise_text(a, ctx, "JsException", message);
        a.ret_void();
    });
    match op {
        JsOp::Attribute => {
            a.if_(status.eq(-1), |a| raise_text(a, ctx, "AttributeError", name));
        }
        JsOp::Method => {
            a.if_(status.eq(-1), |a| raise_text(a, ctx, "AttributeError", name));
            a.if_(status.eq(-3), |a| {
                raise_text(
                    a,
                    ctx,
                    "TypeError",
                    text(ctx, "'pyodide.ffi.JsProxy' object is not callable"),
                )
            });
        }
        JsOp::Item => {
            // `name` is the index.
            a.if_(status.eq(-1), |a| {
                raise_text(
                    a,
                    ctx,
                    "IndexError",
                    call(ctx.i32_to_str_fn(), vec![name.into()], Some(Ty::I32)),
                )
            });
            a.if_(status.eq(-3), |a| {
                raise_text(
                    a,
                    ctx,
                    "TypeError",
                    text(ctx, "'pyodide.ffi.JsProxy' object is not subscriptable"),
                )
            });
        }
        JsOp::Call => a.if_(status.eq(-3), |a| {
            raise_text(
                a,
                ctx,
                "TypeError",
                text(ctx, "'pyodide.ffi.JsProxy' object is not callable"),
            )
        }),
        JsOp::Construct => a.if_(status.eq(-3), |a| {
            raise_text(
                a,
                ctx,
                "TypeError",
                text(ctx, "'pyodide.ffi.JsProxy' object is not a constructor"),
            )
        }),
        JsOp::Iterate => a.if_(status.eq(-3), |a| {
            raise_text(
                a,
                ctx,
                "TypeError",
                text(ctx, "'pyodide.ffi.JsProxy' object is not iterable"),
            )
        }),
        JsOp::Stringify => a.if_(status.eq(-3), |a| {
            raise_text(
                a,
                ctx,
                "TypeError",
                text(
                    ctx,
                    "to_py(): a JavaScript value JSON cannot carry (a Set, a function, a cycle, or a Map with keys that are not strings)",
                ),
            )
        }),
        JsOp::Import => a.if_(status.eq(-1), |a| {
            let quoted = concat(ctx, concat(ctx, text(ctx, "'"), name), text(ctx, "'"));
            raise_text(
                a,
                ctx,
                "ModuleNotFoundError",
                concat(ctx, text(ctx, "No module named "), quoted),
            )
        }),
        JsOp::Other => {}
    }
    a.finish()
}

fn build_text(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let h = a.param(0);
    let n = a.let_(call(
        imp(ctx, JsImport::Utf8),
        vec![h.into()],
        Some(Ty::I32),
    ));
    // A fresh string of `n` bytes: `[len][bytes][nul]`, text the collector
    // does not read.
    let block = a.let_(call(ctx.alloc_fn(), vec![n + 5], Some(Ty::I32)));
    a.store(Ty::I32, block - 4, 0, crate::compiler::gc::NOSCAN);
    a.store(Ty::I32, block, 0, n);
    let at = a.let_(block + 4);
    a.run(call(imp(ctx, JsImport::Utf8Copy), vec![at.into()], None));
    a.store8(at + n, 0, 0);
    a.ret(at);
    a.finish()
}

/// The kind of the value in a proxy.
fn kind_of(
    a: &mut Asm,
    ctx: &CompilationContext,
    proxy: impl Into<E>,
) -> crate::compiler::asm::Var {
    a.let_(call(
        imp(ctx, JsImport::Kind),
        vec![handle(proxy)],
        Some(Ty::I32),
    ))
}

/// `str(x)` (or `repr(x)`) of the value Pyodide would hand over.
fn build_str(ctx: &CompilationContext, repr: bool) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let proxy = a.param(0);
    let h = a.let_(handle(proxy));
    let kind = kind_of(&mut a, ctx, proxy);
    for (k, shown) in [(0, "None"), (1, "jsnull"), (2, "False"), (3, "True")] {
        a.if_(kind.eq(k), |a| a.ret(text(ctx, shown)));
    }
    // A number: an int prints its digits (exactly, up to 2**53), a float as
    // Python's repr.
    a.if_(kind.eq(4) | kind.eq(5), |a| {
        let x = a.let_(call(
            imp(ctx, JsImport::ToNumber),
            vec![h.into()],
            Some(Ty::F64),
        ));
        a.if_(kind.eq(5), |a| {
            a.ret(call(
                ctx.helper(Helper::FloatRepr),
                vec![x.into()],
                Some(Ty::I32),
            ))
        });
        a.if_(x.e().ge(-2147483648.0) & x.e().le(2147483647.0), |a| {
            a.ret(call(
                ctx.i32_to_str_fn(),
                vec![x.e().to_i32()],
                Some(Ty::I32),
            ))
        });
        let digits = call(
            ctx.helper(Helper::FloatToString),
            vec![x.e().abs(), E::I32(b'f' as i32), E::I32(0), E::I32(0)],
            Some(Ty::I32),
        );
        a.if_(x.e().lt(0.0), |a| {
            a.ret(concat(ctx, text(ctx, "-"), digits.clone()))
        });
        a.ret(digits);
    });
    let t = a.let_(call(
        imp(ctx, JsImport::Text),
        vec![h.into()],
        Some(Ty::I32),
    ));
    let s = a.let_(call(helper(ctx, JsFn::Text), vec![t.into()], Some(Ty::I32)));
    a.run(call(imp(ctx, JsImport::Release), vec![t.into()], None));
    if repr {
        a.if_(kind.eq(6), |a| {
            a.ret(call(
                ctx.helper(Helper::StrRepr),
                vec![s.into()],
                Some(Ty::I32),
            ))
        });
    }
    a.ret(s);
    a.finish()
}

/// What a value of the kind is called in an error.
fn build_kind_name(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let kind = a.param(0);
    for (k, shown) in [
        (0, "undefined"),
        (1, "null"),
        (2, "a bool"),
        (3, "a bool"),
        (4, "an int"),
        (5, "a float"),
        (6, "a string"),
        (7, "a function"),
        (8, "an array"),
        (9, "an object"),
    ] {
        a.if_(kind.eq(k), |a| a.ret(text(ctx, shown)));
    }
    a.ret(text(ctx, "a value"));
    a.finish()
}

/// The `TypeError` for a JS value of another kind than the place declares.
fn raise_mismatch(a: &mut Asm, ctx: &CompilationContext, kind: impl Into<E>, ty: &IRType) {
    let name = call(
        helper(ctx, JsFn::KindName),
        vec![kind.into()],
        Some(Ty::I32),
    );
    let declared = crate::type_to_string(ty);
    let message = concat(
        ctx,
        concat(ctx, text(ctx, "the JavaScript value is "), name),
        concat(
            ctx,
            concat(ctx, text(ctx, ", where "), text(ctx, &declared)),
            text(ctx, " is declared"),
        ),
    );
    raise_text(a, ctx, "TypeError", message);
}

/// The value a proxy holds, stored where `ty` is declared.
fn build_as(ctx: &CompilationContext, ty: &IRType) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let proxy = a.param(0);
    let h = a.let_(handle(proxy));
    let kind = kind_of(&mut a, ctx, proxy);
    let number = || call(imp(ctx, JsImport::ToNumber), vec![h.into()], Some(Ty::F64));
    match ty {
        IRType::Int => {
            a.if_(kind.eq(4), |a| a.ret(number().to_i64_sat().wrap()));
            raise_mismatch(&mut a, ctx, kind, ty);
            a.ret(0);
        }
        IRType::Float => {
            a.if_(kind.eq(5), |a| a.ret(number()));
            raise_mismatch(&mut a, ctx, kind, ty);
            a.ret(0.0);
        }
        IRType::Bool => {
            a.if_(kind.eq(2) | kind.eq(3), |a| a.ret(kind.e().eq(3)));
            raise_mismatch(&mut a, ctx, kind, ty);
            a.ret(0);
        }
        IRType::String => {
            a.if_(kind.eq(6), |a| {
                a.ret(call(helper(ctx, JsFn::Text), vec![h.into()], Some(Ty::I32)))
            });
            raise_mismatch(&mut a, ctx, kind, ty);
            a.ret(text(ctx, ""));
        }
        IRType::None => {
            a.if_(kind.eq(0), |a| a.ret(0));
            raise_mismatch(&mut a, ctx, kind, ty);
            a.ret(0);
        }
        _ => {
            a.ret(proxy);
        }
    }
    a.finish()
}

fn build_int(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let proxy = a.param(0);
    let h = a.let_(handle(proxy));
    let kind = kind_of(&mut a, ctx, proxy);
    a.if_(kind.eq(2) | kind.eq(3), |a| a.ret(kind.e().eq(3)));
    a.if_(kind.eq(4) | kind.eq(5), |a| {
        let x = a.let_(call(
            imp(ctx, JsImport::ToNumber),
            vec![h.into()],
            Some(Ty::F64),
        ));
        a.if_(x.e().ne(x), |a| {
            raise_text(
                a,
                ctx,
                "ValueError",
                text(ctx, "cannot convert float NaN to integer"),
            );
            a.ret(0);
        });
        a.if_(x.e().abs().eq(f64::INFINITY), |a| {
            raise_text(
                a,
                ctx,
                "OverflowError",
                text(ctx, "cannot convert float infinity to integer"),
            );
            a.ret(0);
        });
        a.ret(x.e().trunc().to_i64_sat().wrap());
    });
    let message = |kind: i32| {
        match kind {
        0 => "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'",
        1 => "int() argument must be a string, a bytes-like object or a real number, not 'pyodide.ffi.JsNull'",
        6 => "int() of a JavaScript string is not supported here. Hint: int(str(x))",
        _ => "int() argument must be a string, a bytes-like object or a real number, not 'pyodide.ffi.JsProxy'",
    }
    };
    for k in [0, 1, 6] {
        a.if_(kind.eq(k), |a| {
            raise_text(a, ctx, "TypeError", text(ctx, message(k)));
            a.ret(0);
        });
    }
    raise_text(&mut a, ctx, "TypeError", text(ctx, message(9)));
    a.ret(0);
    a.finish()
}

fn build_float(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let proxy = a.param(0);
    let h = a.let_(handle(proxy));
    let kind = kind_of(&mut a, ctx, proxy);
    a.if_(kind.eq(2) | kind.eq(3), |a| a.ret(kind.e().eq(3).to_f64()));
    a.if_(kind.eq(4) | kind.eq(5), |a| {
        a.ret(call(
            imp(ctx, JsImport::ToNumber),
            vec![h.into()],
            Some(Ty::F64),
        ))
    });
    let message = |kind: i32| match kind {
        0 => "float() argument must be a string or a real number, not 'NoneType'",
        1 => "float() argument must be a string or a real number, not 'pyodide.ffi.JsNull'",
        6 => "float() of a JavaScript string is not supported here. Hint: float(str(x))",
        _ => "float() argument must be a string or a real number, not 'pyodide.ffi.JsProxy'",
    };
    for k in [0, 1, 6] {
        a.if_(kind.eq(k), |a| {
            raise_text(a, ctx, "TypeError", text(ctx, message(k)));
            a.ret(0.0);
        });
    }
    raise_text(&mut a, ctx, "TypeError", text(ctx, message(9)));
    a.ret(0.0);
    a.finish()
}

/// Truth as Pyodide's value's: None, jsnull, and False are false, a number
/// is true unless it is zero, a string or an array unless it is empty, and
/// anything else is true.
fn build_truth(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let proxy = a.param(0);
    let h = a.let_(handle(proxy));
    let kind = kind_of(&mut a, ctx, proxy);
    a.if_(kind.le(2), |a| a.ret(0));
    a.if_(kind.eq(4) | kind.eq(5), |a| {
        a.ret(call(imp(ctx, JsImport::ToNumber), vec![h.into()], Some(Ty::F64)).ne(0.0))
    });
    a.if_(kind.eq(6) | kind.eq(8), |a| {
        a.ret(call(imp(ctx, JsImport::Length), vec![h.into()], Some(Ty::I32)).gt(0))
    });
    a.ret(1);
    a.finish()
}

fn build_eq_number(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32, Ty::F64]);
    let (h, x) = (a.param(0), a.param(1));
    let kind = a.let_(call(
        imp(ctx, JsImport::Kind),
        vec![h.into()],
        Some(Ty::I32),
    ));
    a.if_(kind.eq(2) | kind.eq(3), |a| {
        a.ret(kind.e().eq(3).to_f64().eq(x))
    });
    a.if_(kind.eq(4) | kind.eq(5), |a| {
        a.ret(call(imp(ctx, JsImport::ToNumber), vec![h.into()], Some(Ty::F64)).eq(x))
    });
    a.ret(0);
    a.finish()
}

fn build_len(ctx: &CompilationContext) -> Function {
    let mut a = Asm::new(&[Ty::I32]);
    let proxy = a.param(0);
    let n = a.let_(call(
        imp(ctx, JsImport::Length),
        vec![handle(proxy)],
        Some(Ty::I32),
    ));
    a.if_(n.lt(0), |a| {
        raise_text(
            a,
            ctx,
            "TypeError",
            text(ctx, "object of type 'pyodide.ffi.JsProxy' has no len()"),
        );
        a.ret(0);
    });
    a.ret(n);
    a.finish()
}

/// `x.to_py()` into `ty`: `json.loads(JSON.stringify(x))` as that type.
fn build_to_py(ctx: &CompilationContext, ty: &IRType) -> Function {
    use crate::compiler::stdlib::json::JsonFn;
    let word = if matches!(ty, IRType::Float) {
        Ty::F64
    } else {
        Ty::I32
    };
    let mut a = Asm::new(&[Ty::I32]);
    let proxy = a.param(0);
    let t = a.let_(call(
        imp(ctx, JsImport::Stringify),
        vec![handle(proxy)],
        Some(Ty::I32),
    ));
    a.run(call(
        helper(ctx, JsFn::Check(JsOp::Stringify)),
        vec![t.into(), E::I32(0)],
        None,
    ));
    let zero = if word == Ty::F64 {
        E::F64(0.0)
    } else {
        E::I32(0)
    };
    a.if_(t.lt(0), |a| a.ret(zero.clone()));
    let s = a.let_(call(helper(ctx, JsFn::Text), vec![t.into()], Some(Ty::I32)));
    a.run(call(imp(ctx, JsImport::Release), vec![t.into()], None));
    a.ret(call(
        ctx.helper(Helper::Json(JsonFn::Loads {
            ty: ty.clone(),
            hook: None,
        })),
        vec![s.into(), E::I32(0)],
        Some(word),
    ));
    a.finish()
}

/// `to_js(x)` of a value of `ty`: `JSON.parse(json.dumps(x))`.
fn build_to_js(ctx: &CompilationContext, ty: &IRType) -> Function {
    use crate::compiler::stdlib::json::{DumpOpts, JsonFn};
    let word = if matches!(ty, IRType::Float) {
        Ty::F64
    } else {
        Ty::I32
    };
    let mut a = Asm::new(&[word]);
    let value = a.param(0);
    let s = a.let_(call(
        ctx.helper(Helper::Json(JsonFn::Dumps {
            ty: ty.clone(),
            opts: DumpOpts::defaults(),
            hook: None,
        })),
        vec![value.into(), E::I32(0)],
        Some(Ty::I32),
    ));
    let n = a.let_((s - 4).load(Ty::I32, 0));
    let h = a.let_(call(
        imp(ctx, JsImport::Parse),
        vec![s.into(), n.into()],
        Some(Ty::I32),
    ));
    a.run(call(
        helper(ctx, JsFn::Check(JsOp::Other)),
        vec![h.into(), E::I32(0)],
        None,
    ));
    a.ret(h);
    a.finish()
}

// ---- rewriting the program's operations -------------------------------------

/// The intrinsic of a JS operation.
pub(crate) fn intrinsic(name: &str) -> String {
    format!("{JS_PREFIX}{name}")
}

fn lit(s: &str) -> IRExpr {
    IRExpr::Const(IRConstant::String(s.to_string()))
}

fn js_call(name: &str, args: Vec<IRExpr>) -> IRExpr {
    IRExpr::FunctionCall {
        function_name: intrinsic(name),
        arguments: args,
    }
}

/// Whether `name` is the module `js` (not a local of that name).
fn is_js_module(name: &str, ctx: &CompilationContext) -> bool {
    name == "js" && ctx.get_local_info(name).is_none()
}

/// The JS global a name imported from `js` binds (`from js import document`).
fn js_import(name: &str, ctx: &CompilationContext) -> Option<String> {
    if ctx.get_local_info(name).is_some() || ctx.module_global_index.contains_key(name) {
        return None;
    }
    match ctx.stdlib_imports.get(name) {
        Some((module, attr)) if module == "js" => Some(attr.clone()),
        _ => None,
    }
}

/// The function of `pyodide.ffi` a call names (`to_js(x)`,
/// `pyodide.ffi.to_js(x)`).
fn ffi_call<'e>(expr: &'e IRExpr, ctx: &CompilationContext) -> Option<(String, &'e [IRExpr])> {
    match expr {
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if ctx.get_function_info(function_name).is_none() => {
            match ctx.stdlib_imports.get(function_name) {
                Some((module, attr)) if module == "pyodide.ffi" => Some((attr.clone(), arguments)),
                _ => None,
            }
        }
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => match object.as_ref() {
            IRExpr::Attribute {
                object: m,
                attribute,
            } if attribute == "ffi" => match m.as_ref() {
                IRExpr::Variable(p) if p == "pyodide" => Some((method_name.clone(), arguments)),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

fn is_proxy(expr: &IRExpr, ctx: &CompilationContext) -> bool {
    crate::compiler::function::infer_value_type(expr, ctx) == IRType::JsProxy
}

/// A JS operation as the intrinsic call it is compiled as, or None when
/// `expr` is none.
pub(crate) fn rewrite(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    if ctx.js.is_empty() {
        return None;
    }
    match expr {
        IRExpr::Attribute { object, attribute } => {
            if let IRExpr::Variable(m) = object.as_ref() {
                if is_js_module(m, ctx) {
                    return Some(js_call("global", vec![lit(attribute)]));
                }
            }
            if is_proxy(object, ctx) {
                return Some(js_call("get", vec![(**object).clone(), lit(attribute)]));
            }
            None
        }
        IRExpr::Variable(name) => js_import(name, ctx).map(|g| js_call("global", vec![lit(&g)])),
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => {
            if let Some((f, args)) = ffi_call(expr, ctx) {
                return ffi(&f, args, ctx);
            }
            if let IRExpr::Variable(m) = object.as_ref() {
                if is_js_module(m, ctx) {
                    let mut args = vec![js_call("global_this", vec![]), lit(method_name)];
                    args.extend(arguments.iter().cloned());
                    return Some(js_call("method", args));
                }
            }
            if !is_proxy(object, ctx) {
                return None;
            }
            let mut args = vec![(**object).clone()];
            match method_name.as_str() {
                "new" => {
                    args.extend(arguments.iter().cloned());
                    Some(js_call("new", args))
                }
                "to_py" if arguments.is_empty() => Some(js_call("to_py", args)),
                _ => {
                    args.push(lit(method_name));
                    args.extend(arguments.iter().cloned());
                    Some(js_call("method", args))
                }
            }
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            if function_name.starts_with(JS_PREFIX) {
                return None;
            }
            if let Some((f, args)) = ffi_call(expr, ctx) {
                return ffi(&f, args, ctx);
            }
            if let Some(g) = js_import(function_name, ctx) {
                let mut args = vec![js_call("global", vec![lit(&g)])];
                args.extend(arguments.iter().cloned());
                return Some(js_call("call", args));
            }
            if let Some(info) = ctx.get_local_info(function_name) {
                if info.var_type == IRType::JsProxy {
                    let mut args = vec![IRExpr::Variable(function_name.clone())];
                    args.extend(arguments.iter().cloned());
                    return Some(js_call("call", args));
                }
            }
            let [x] = arguments.as_slice() else {
                return None;
            };
            let builtin = match function_name.as_str() {
                "len" => "len",
                "str" => "str",
                "repr" => "repr",
                "int" => "int",
                "float" => "float",
                "bool" => "bool",
                _ => return None,
            };
            if ctx.get_function_info(function_name).is_some() || !is_proxy(x, ctx) {
                return None;
            }
            Some(js_call(builtin, vec![x.clone()]))
        }
        IRExpr::Indexing { container, index } if is_proxy(container, ctx) => Some(js_call(
            "item",
            vec![(**container).clone(), (**index).clone()],
        )),
        IRExpr::CompareOp { left, right, op } => {
            use crate::ir::IRCompareOp as C;
            let (l, r) = (is_proxy(left, ctx), is_proxy(right, ctx));
            if !l && !r {
                return None;
            }
            let none = |e: &IRExpr| matches!(e, IRExpr::Const(IRConstant::None));
            let test = match op {
                C::Is | C::IsNot if none(right) && l => js_call("is_none", vec![(**left).clone()]),
                C::Is | C::IsNot if none(left) && r => js_call("is_none", vec![(**right).clone()]),
                C::Is | C::IsNot if l && r => {
                    js_call("same", vec![(**left).clone(), (**right).clone()])
                }
                C::Eq | C::NotEq => js_call("eq", vec![(**left).clone(), (**right).clone()]),
                _ => return None,
            };
            Some(match op {
                C::IsNot | C::NotEq => IRExpr::UnaryOp {
                    operand: Box::new(test),
                    op: crate::ir::IRUnaryOp::Not,
                },
                _ => test,
            })
        }
        _ => None,
    }
}

/// A call of `pyodide.ffi`'s.
fn ffi(f: &str, args: &[IRExpr], ctx: &CompilationContext) -> Option<IRExpr> {
    match (f, args) {
        ("to_js", [x]) => {
            let literal = matches!(
                x,
                IRExpr::ListLiteral(_) | IRExpr::TupleLiteral(_) | IRExpr::DictLiteral(_)
            );
            let ty = crate::compiler::function::infer_value_type(x, ctx);
            match ty {
                _ if literal => Some(js_call("to_js", vec![x.clone()])),
                IRType::List(_) | IRType::Tuple(_) | IRType::VarTuple(_) | IRType::Dict(..) => {
                    Some(js_call("to_js", vec![x.clone()]))
                }
                // A primitive (or a proxy) is itself, as Pyodide's `to_js`
                // answers it.
                _ => Some(x.clone()),
            }
        }
        _ => None,
    }
}

/// The type an intrinsic call answers.
pub(crate) fn intrinsic_type(name: &str) -> Option<IRType> {
    let op = name.strip_prefix(JS_PREFIX)?;
    Some(match op {
        "len" | "int" => IRType::Int,
        "str" | "repr" => IRType::String,
        "float" => IRType::Float,
        "bool" | "eq" | "is_none" | "same" => IRType::Bool,
        "set" | "then" | "resolve" => IRType::None,
        "export" | "fail" => IRType::Int,
        "to_py" => IRType::Unknown,
        _ => IRType::JsProxy,
    })
}

// ---- the module, made ready for the bridge --------------------------------

/// The custom section listing the module's exports and their signatures,
/// which the loader wraps them by.
pub const EXPORTS_SECTION: &str = "waspy.exports";
/// The prefix of an export's JavaScript-facing wrapper, which takes and
/// answers handles and converts by the signature.
pub const EXPORT_PREFIX: &str = "__jsx_";
/// The prefix of the bridge's own functions; they are internal.
pub const BRIDGE_FN_PREFIX: &str = "__jsbridge_";

/// A type JSON (and so the bridge) carries across: numbers, strings, bools,
/// None, a JS value, and lists, tuples, dicts, and optionals of those.
fn crosses(ty: &IRType) -> bool {
    match ty {
        IRType::Int
        | IRType::Float
        | IRType::Bool
        | IRType::String
        | IRType::None
        | IRType::JsProxy => true,
        IRType::List(e) | IRType::VarTuple(e) | IRType::Optional(e) => {
            crosses(e) && !matches!(**e, IRType::JsProxy)
        }
        IRType::Tuple(ms) => ms.iter().all(|m| crosses(m) && *m != IRType::JsProxy),
        IRType::Dict(k, v) => {
            matches!(**k, IRType::String | IRType::Int) && crosses(v) && **v != IRType::JsProxy
        }
        _ => false,
    }
}

/// Whether a value of the type converts at its store (a scalar or a JS
/// value) rather than through JSON.
fn scalar(ty: &IRType) -> bool {
    matches!(
        ty,
        IRType::Int
            | IRType::Float
            | IRType::Bool
            | IRType::String
            | IRType::None
            | IRType::JsProxy
    )
}

/// The module with what the bridge needs: the names its operations carry,
/// placed in memory, and a JavaScript-facing wrapper for each export whose
/// signature crosses, with the list of exports the loader reads. None for a
/// module that does not use the bridge.
pub(crate) fn with_bridge(module: &IRModule) -> Option<IRModule> {
    use crate::ir::build::*;
    use crate::ir::{IRExceptHandler, IRFunction, IRStatement};
    if !uses_bridge(module) {
        return None;
    }
    let mut m = module.clone();
    // Every attribute and method name, and the names imported from `js`.
    let mut names = std::collections::BTreeSet::new();
    let mut bodies: Vec<crate::ir::IRBody> = m.functions.iter().map(|f| f.body.clone()).collect();
    bodies.extend(
        m.classes
            .iter()
            .flat_map(|c| c.methods.iter().map(|f| f.body.clone())),
    );
    fn targets(body: &crate::ir::IRBody, names: &mut std::collections::BTreeSet<String>) {
        for stmt in &body.statements {
            if let crate::ir::IRStatement::AttributeAssign { attribute, .. }
            | crate::ir::IRStatement::AttributeAugAssign { attribute, .. } = stmt
            {
                names.insert(attribute.clone());
            }
            for nested in crate::ir::infer::nested_bodies(stmt) {
                targets(nested, names);
            }
        }
    }
    for body in &bodies {
        targets(body, &mut names);
    }
    for body in &mut bodies {
        let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
            match e {
                IRExpr::Attribute { attribute, .. } => {
                    names.insert(attribute.clone());
                }
                IRExpr::MethodCall { method_name, .. } => {
                    names.insert(method_name.clone());
                }
                _ => {}
            }
            Ok(())
        });
    }
    for imp in &m.imports {
        if imp.module == "js" {
            if let Some(n) = &imp.name {
                names.insert(n.clone());
            }
        }
    }
    for n in names {
        m.memory_layout.add_string(&n);
    }

    // The exports, and a wrapper for each that crosses.
    let line_fn = format!("{BRIDGE_FN_PREFIX}line");
    let mut listed = Vec::new();
    let mut wrappers = Vec::new();
    for f in &module.functions {
        if f.name.starts_with("__lambda_")
            || f.name.starts_with("__waspy_")
            || crate::ir::is_internal_function(&f.name)
            || f.name.starts_with(crate::compiler::context::MODULE_INIT_FN)
        {
            continue;
        }
        // A coroutine function: what it resolves to, read off the runtime
        // class its state class derives from, and whether that class can
        // export it (its result crosses to JavaScript).
        let coroutine = match &f.return_type {
            IRType::Class(c) if c.starts_with(crate::ir::CORO_CLASS_PREFIX) => module
                .classes
                .iter()
                .find(|k| k.name == *c)
                .and_then(|k| k.bases.first())
                .map(|base| {
                    let exports = module
                        .classes
                        .iter()
                        .any(|k| k.name == *base && k.methods.iter().any(|m| m.name == "_export"));
                    let result = base
                        .strip_prefix("asyncio._Coro[")
                        .and_then(|r| r.strip_suffix(']'))
                        .unwrap_or("None")
                        .to_string();
                    (result, exports)
                }),
            _ => None,
        };
        let is_async = coroutine.is_some();
        let wrapped = f.params.iter().all(|p| crosses(&p.param_type))
            && match &coroutine {
                Some((_, exports)) => *exports,
                None => crosses(&f.return_type) || f.return_type == IRType::Unknown,
            };
        let returns = match &coroutine {
            Some((result, _)) => result.clone(),
            None => crate::type_to_string(&f.return_type),
        };
        listed.push(serde_json::json!({
            "name": f.name,
            "params": f.params.iter().map(|p| crate::type_to_string(&p.param_type)).collect::<Vec<_>>(),
            "returns": returns,
            "async": is_async,
            "wrapped": wrapped,
        }));
        if !wrapped {
            continue;
        }
        let mut statements = Vec::new();
        let mut arguments = Vec::new();
        for (k, p) in f.params.iter().enumerate() {
            let proxy = call(&intrinsic("adopt"), vec![v(&format!("__h{k}"))]);
            let value = if scalar(&p.param_type) {
                proxy
            } else {
                call(&intrinsic("to_py"), vec![proxy])
            };
            let x = format!("__x{k}");
            statements.push(assign_typed(&x, value, &p.param_type));
            arguments.push(v(&x));
        }
        let result = call(&f.name, arguments);
        if is_async {
            // The Promise's token; the task runs between JavaScript's turns.
            statements.push(ret_val(mcall(result, "_export", vec![])));
        } else if matches!(f.return_type, IRType::None | IRType::Unknown) {
            statements.push(expr_stmt(result));
            statements.push(ret_val(call(&intrinsic("export"), vec![none()])));
        } else {
            statements.push(assign_typed("__r", result, &f.return_type));
            statements.push(ret_val(call(&intrinsic("export"), vec![v("__r")])));
        }
        let body = vec![IRStatement::TryExcept {
            try_body: Box::new(crate::ir::IRBody { statements }),
            except_handlers: vec![IRExceptHandler {
                exception_types: vec!["BaseException".to_string()],
                name: Some("__e".to_string()),
                body: crate::ir::IRBody {
                    statements: vec![ret_val(call(
                        &intrinsic("fail"),
                        vec![call(&line_fn, vec![v("__e")])],
                    ))],
                },
            }],
            finally_body: None,
        }];
        wrappers.push(IRFunction {
            name: format!("{EXPORT_PREFIX}{}", f.name),
            params: (0..f.params.len())
                .map(|k| param(&format!("__h{k}"), &IRType::Int))
                .collect(),
            body: crate::ir::IRBody { statements: body },
            return_type: IRType::Int,
            decorators: Vec::new(),
        });
    }
    if !wrappers.is_empty() {
        m.functions.extend(wrappers);
        m.functions.push(exception_line(&line_fn));
        for t in [": ", "__main__."] {
            m.memory_layout.add_string(t);
        }
    }
    m.metadata.insert(
        EXPORTS_SECTION.to_string(),
        serde_json::Value::Array(listed).to_string(),
    );
    Some(m)
}

/// `(e) -> str`: the last line of CPython's report of an exception, which a
/// call from JavaScript throws as its message.
fn exception_line(name: &str) -> crate::ir::IRFunction {
    use crate::ir::build::*;
    use crate::ir::{IRCompareOp, IROp};
    let s = || IRType::String;
    method(
        name,
        vec![param("e", &IRType::Class("BaseException".to_string()))],
        s(),
        vec![
            assign_typed("shown", call("str", vec![call("type", vec![v("e")])]), &s()),
            assign_typed("name", slice(v("shown"), Some(int(8)), Some(int(-2))), &s()),
            if_(
                mcall(v("name"), "startswith", vec![text("__main__.")]),
                vec![assign("name", slice(v("name"), Some(int(9)), None))],
            ),
            assign_typed("said", call("str", vec![v("e")]), &s()),
            if_(
                cmp(len(v("said")), IRCompareOp::Eq, int(0)),
                vec![ret_val(v("name"))],
            ),
            ret_val(bin(
                bin(v("name"), IROp::Add, text(": ")),
                IROp::Add,
                v("said"),
            )),
        ],
    )
}

// ---- emitting the operations -------------------------------------------------

use crate::compiler::expression::{emit_expr, recover_str_pair};
use crate::compiler::function::emit_post_call_check;
use crate::ir::MemoryLayout;
use wasm_encoder::Instruction as I;

/// A held slot, or the scratch local (reported) when they run out.
fn hold(ctx: &CompilationContext) -> u32 {
    ctx.hold().unwrap_or_else(|| {
        ctx.report("a JavaScript operation nests more deeply than the compiler reserves room for");
        ctx.held_local_base
    })
}

/// The offset and length of a name an intrinsic carries.
fn name_of(e: Option<&IRExpr>, ml: &MemoryLayout, ctx: &CompilationContext) -> (i32, i32) {
    match e {
        Some(IRExpr::Const(IRConstant::String(s))) => match ml.string_offsets.get(s) {
            Some(off) => (*off as i32, s.len() as i32),
            None => {
                ctx.report(format!(
                    "the name '{s}' was never placed in memory; this is a code generation bug"
                ));
                (0, 0)
            }
        },
        _ => {
            ctx.report("a JavaScript name must be a literal here");
            (0, 0)
        }
    }
}

/// After an operation answered `r` (in the scratch local): raise what it
/// failed with, else wrap it.
fn finish(func: &mut Function, ctx: &CompilationContext, op: JsOp, name: i32) {
    func.instruction(&I::LocalGet(ctx.temp_local));
    func.instruction(&I::I32Const(name));
    func.instruction(&I::Call(helper(ctx, JsFn::Check(op))));
    emit_post_call_check(func, ctx);
    func.instruction(&I::LocalGet(ctx.temp_local));
    func.instruction(&I::Call(helper(ctx, JsFn::Wrap)));
}

/// Emit a Python value as a JS handle, answering whether the handle is a
/// temporary of this operation's (to be released once used) rather than a
/// proxy's own.
fn emit_value(
    arg: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    ml: &MemoryLayout,
) -> bool {
    if matches!(arg, IRExpr::Const(IRConstant::None)) {
        func.instruction(&I::Call(imp(ctx, JsImport::Undefined)));
        return true;
    }
    let ty = emit_expr(arg, func, ctx, ml, None);
    match ty {
        IRType::Int => {
            func.instruction(&I::F64ConvertI32S);
            func.instruction(&I::Call(imp(ctx, JsImport::Number)));
        }
        IRType::Float => {
            func.instruction(&I::Call(imp(ctx, JsImport::Number)));
        }
        IRType::Bool => {
            func.instruction(&I::Call(imp(ctx, JsImport::Boolean)));
        }
        IRType::String => {
            func.instruction(&I::Call(imp(ctx, JsImport::String)));
        }
        IRType::None => {
            func.instruction(&I::Call(imp(ctx, JsImport::Undefined)));
        }
        IRType::JsProxy => {
            func.instruction(&I::I32Load(crate::compiler::runtime::w32(0)));
            return false;
        }
        other => {
            ctx.report(format!(
                "a {} passed to JavaScript is not supported: Pyodide would pass a proxy of the \
                 Python object, which has no counterpart here. Hint: pass to_js(x) (from \
                 pyodide.ffi) for a list, tuple, or dict",
                crate::type_to_string(&other)
            ));
            match other {
                IRType::Float => {
                    func.instruction(&I::Drop);
                }
                IRType::String | IRType::Bytes => {
                    func.instruction(&I::Drop);
                    func.instruction(&I::Drop);
                }
                _ => {
                    func.instruction(&I::Drop);
                }
            }
            func.instruction(&I::Call(imp(ctx, JsImport::Undefined)));
        }
    }
    true
}

/// The arguments, as a fresh JS array whose handle is left on the stack.
fn emit_args(args: &[IRExpr], func: &mut Function, ctx: &CompilationContext, ml: &MemoryLayout) {
    let arr = hold(ctx);
    func.instruction(&I::Call(imp(ctx, JsImport::Array)));
    func.instruction(&I::LocalSet(arr));
    for arg in args {
        if let IRExpr::Keyword { name, .. } = arg {
            ctx.report(format!(
                "a keyword argument ('{name}=') in a call to JavaScript is not supported"
            ));
            continue;
        }
        let temp = emit_value(arg, func, ctx, ml);
        func.instruction(&I::LocalSet(ctx.temp_local));
        func.instruction(&I::LocalGet(arr));
        func.instruction(&I::LocalGet(ctx.temp_local));
        func.instruction(&I::Call(imp(ctx, JsImport::Push)));
        if temp {
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::Call(imp(ctx, JsImport::Release)));
        }
    }
    func.instruction(&I::LocalGet(arr));
    ctx.release_held();
}

/// Emit the receiver (a proxy) and leave its handle in a held slot, which
/// the caller releases.
fn emit_handle(
    obj: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    ml: &MemoryLayout,
) -> u32 {
    let ty = emit_expr(obj, func, ctx, ml, None);
    if ty != IRType::JsProxy {
        ctx.report(format!(
            "a JavaScript operation on a {} is not supported",
            crate::type_to_string(&ty)
        ));
    }
    func.instruction(&I::I32Load(crate::compiler::runtime::w32(0)));
    let slot = hold(ctx);
    func.instruction(&I::LocalSet(slot));
    slot
}

/// Emit a bridge intrinsic (`__js_<op>`), answering the type it leaves, or
/// None when `name` is not one.
pub(crate) fn emit_intrinsic(
    name: &str,
    args: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    ml: &MemoryLayout,
    expected: Option<&IRType>,
) -> Option<IRType> {
    let op = name.strip_prefix(JS_PREFIX)?;
    if ctx.js.is_empty() || ctx.get_function_info(name).is_some() {
        return None;
    }
    let call_import = |func: &mut Function, f: JsImport| {
        func.instruction(&I::Call(imp(ctx, f)));
    };
    Some(match op {
        "global" => {
            let (off, len) = name_of(args.first(), ml, ctx);
            func.instruction(&I::I32Const(off));
            func.instruction(&I::I32Const(len));
            call_import(func, JsImport::Global);
            func.instruction(&I::LocalSet(ctx.temp_local));
            finish(func, ctx, JsOp::Attribute, off);
            IRType::JsProxy
        }
        "global_this" => {
            call_import(func, JsImport::GlobalThis);
            func.instruction(&I::Call(helper(ctx, JsFn::Wrap)));
            IRType::JsProxy
        }
        "get" => {
            let (off, len) = name_of(args.get(1), ml, ctx);
            let o = emit_handle(&args[0], func, ctx, ml);
            func.instruction(&I::LocalGet(o));
            func.instruction(&I::I32Const(off));
            func.instruction(&I::I32Const(len));
            call_import(func, JsImport::Get);
            func.instruction(&I::LocalSet(ctx.temp_local));
            ctx.release_held();
            finish(func, ctx, JsOp::Attribute, off);
            IRType::JsProxy
        }
        "set" => {
            let (off, len) = name_of(args.get(1), ml, ctx);
            let o = emit_handle(&args[0], func, ctx, ml);
            let v = hold(ctx);
            let temp = emit_value(&args[2], func, ctx, ml);
            func.instruction(&I::LocalSet(v));
            func.instruction(&I::LocalGet(o));
            func.instruction(&I::I32Const(off));
            func.instruction(&I::I32Const(len));
            func.instruction(&I::LocalGet(v));
            call_import(func, JsImport::Set);
            func.instruction(&I::LocalSet(ctx.temp_local));
            if temp {
                func.instruction(&I::LocalGet(v));
                call_import(func, JsImport::Release);
            }
            ctx.release_held();
            ctx.release_held();
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::I32Const(0));
            func.instruction(&I::Call(helper(ctx, JsFn::Check(JsOp::Other))));
            emit_post_call_check(func, ctx);
            IRType::None
        }
        "method" | "call" | "new" => {
            let (receiver, rest, off, len) = if op == "method" {
                let (off, len) = name_of(args.get(1), ml, ctx);
                (&args[0], &args[2..], off, len)
            } else {
                (&args[0], &args[1..], 0, 0)
            };
            let o = emit_handle(receiver, func, ctx, ml);
            emit_args(rest, func, ctx, ml);
            let arr = hold(ctx);
            func.instruction(&I::LocalSet(arr));
            func.instruction(&I::LocalGet(o));
            let (import, check) = match op {
                "method" => {
                    func.instruction(&I::I32Const(off));
                    func.instruction(&I::I32Const(len));
                    func.instruction(&I::LocalGet(arr));
                    (JsImport::CallMethod, JsOp::Method)
                }
                "call" => {
                    func.instruction(&I::LocalGet(arr));
                    (JsImport::Call, JsOp::Call)
                }
                _ => {
                    func.instruction(&I::LocalGet(arr));
                    (JsImport::Construct, JsOp::Construct)
                }
            };
            call_import(func, import);
            func.instruction(&I::LocalSet(ctx.temp_local));
            func.instruction(&I::LocalGet(arr));
            call_import(func, JsImport::Release);
            ctx.release_held();
            ctx.release_held();
            finish(func, ctx, check, off);
            IRType::JsProxy
        }
        "item" => {
            let o = emit_handle(&args[0], func, ctx, ml);
            let ty = emit_expr(&args[1], func, ctx, ml, Some(&IRType::Int));
            if ty != IRType::Int {
                ctx.report(format!(
                    "an item of a JavaScript value by a {} is not supported; index an array by an int",
                    crate::type_to_string(&ty)
                ));
            }
            let i = hold(ctx);
            func.instruction(&I::LocalSet(i));
            func.instruction(&I::LocalGet(o));
            func.instruction(&I::LocalGet(i));
            call_import(func, JsImport::Item);
            func.instruction(&I::LocalSet(ctx.temp_local));
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::LocalGet(i));
            func.instruction(&I::Call(helper(ctx, JsFn::Check(JsOp::Item))));
            emit_post_call_check(func, ctx);
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::Call(helper(ctx, JsFn::Wrap)));
            ctx.release_held();
            ctx.release_held();
            IRType::JsProxy
        }
        "len" | "str" | "repr" | "int" | "float" | "bool" => {
            emit_expr(&args[0], func, ctx, ml, None);
            let (f, ty) = match op {
                "len" => (JsFn::Len, IRType::Int),
                "str" => (JsFn::Str, IRType::String),
                "repr" => (JsFn::Repr, IRType::String),
                "int" => (JsFn::Int, IRType::Int),
                "float" => (JsFn::Float, IRType::Float),
                _ => (JsFn::Truth, IRType::Bool),
            };
            func.instruction(&I::Call(helper(ctx, f)));
            if matches!(op, "len" | "int" | "float") {
                emit_post_call_check(func, ctx);
            }
            if ty == IRType::String {
                recover_str_pair(func, ctx);
            }
            ty
        }
        "is_none" => {
            emit_expr(&args[0], func, ctx, ml, None);
            func.instruction(&I::I32Load(crate::compiler::runtime::w32(0)));
            call_import(func, JsImport::Kind);
            func.instruction(&I::I32Eqz);
            IRType::Bool
        }
        "same" => {
            let a = emit_handle(&args[0], func, ctx, ml);
            let b = emit_handle(&args[1], func, ctx, ml);
            func.instruction(&I::LocalGet(a));
            func.instruction(&I::LocalGet(b));
            call_import(func, JsImport::Equal);
            ctx.release_held();
            ctx.release_held();
            IRType::Bool
        }
        "eq" => {
            emit_eq(&args[0], &args[1], func, ctx, ml);
            IRType::Bool
        }
        "to_py" => {
            let target = match expected {
                Some(t) if crosses(t) && *t != IRType::JsProxy => t.clone(),
                Some(IRType::JsProxy) => {
                    return Some(emit_expr(&args[0], func, ctx, ml, None));
                }
                Some(t) if !matches!(t, IRType::Unknown | IRType::Any) => {
                    ctx.report(format!(
                        "to_py() into '{}' is not supported: it converts into numbers, strs, \
                         bools, None, and lists, tuples, and dicts of those, with their element \
                         types given, for example 'd: dict[str, int] = obj.to_py()'",
                        crate::type_to_string(t)
                    ));
                    emit_expr(&args[0], func, ctx, ml, None);
                    return Some(t.clone());
                }
                _ => {
                    ctx.report(
                        "to_py() needs the type it converts into: store it where a type is \
                         declared, for example 'xs: list[int] = arr.to_py()'",
                    );
                    emit_expr(&args[0], func, ctx, ml, None);
                    return Some(IRType::JsProxy);
                }
            };
            emit_expr(&args[0], func, ctx, ml, None);
            func.instruction(&I::Call(helper(ctx, JsFn::ToPy(target.clone()))));
            emit_post_call_check(func, ctx);
            if matches!(target, IRType::String) {
                recover_str_pair(func, ctx);
            }
            target
        }
        "to_js" => {
            let ty = emit_expr(&args[0], func, ctx, ml, None);
            func.instruction(&I::Call(helper(ctx, JsFn::ToJs(ty))));
            emit_post_call_check(func, ctx);
            func.instruction(&I::Call(helper(ctx, JsFn::Wrap)));
            IRType::JsProxy
        }
        "iter" => {
            let o = emit_handle(&args[0], func, ctx, ml);
            func.instruction(&I::LocalGet(o));
            call_import(func, JsImport::Iterate);
            func.instruction(&I::LocalSet(ctx.temp_local));
            ctx.release_held();
            finish(func, ctx, JsOp::Iterate, 0);
            IRType::JsProxy
        }
        "next" => {
            // The next value, or (done) the stop flag set and no value.
            let o = emit_handle(&args[0], func, ctx, ml);
            func.instruction(&I::LocalGet(o));
            call_import(func, JsImport::Next);
            func.instruction(&I::LocalSet(ctx.temp_local));
            ctx.release_held();
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::I32Const(-1));
            func.instruction(&I::I32Eq);
            func.instruction(&I::If(wasm_encoder::BlockType::Result(ValType::I32)));
            func.instruction(&I::I32Const(1));
            func.instruction(&I::GlobalSet(1));
            func.instruction(&I::I32Const(0));
            func.instruction(&I::Else);
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::I32Const(0));
            func.instruction(&I::Call(helper(ctx, JsFn::Check(JsOp::Other))));
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::Call(helper(ctx, JsFn::Wrap)));
            func.instruction(&I::End);
            emit_post_call_check(func, ctx);
            IRType::JsProxy
        }
        "adopt" => {
            emit_expr(&args[0], func, ctx, ml, Some(&IRType::Int));
            func.instruction(&I::Call(helper(ctx, JsFn::Wrap)));
            IRType::JsProxy
        }
        "export" => {
            // A handle the caller in JavaScript owns.
            if matches!(args.first(), Some(IRExpr::Const(IRConstant::None))) {
                call_import(func, JsImport::Undefined);
                return Some(IRType::Int);
            }
            let ty = crate::compiler::function::infer_value_type(&args[0], ctx);
            if scalar(&ty) {
                if emit_value(&args[0], func, ctx, ml) {
                    return Some(IRType::Int);
                }
                call_import(func, JsImport::Keep);
            } else {
                let ty = emit_expr(&args[0], func, ctx, ml, None);
                func.instruction(&I::Call(helper(ctx, JsFn::ToJs(ty))));
                emit_post_call_check(func, ctx);
            }
            IRType::Int
        }
        "fail" => {
            emit_expr(&args[0], func, ctx, ml, Some(&IRType::String));
            call_import(func, JsImport::Fail);
            func.instruction(&I::I32Const(-2));
            IRType::Int
        }
        "then" => {
            let o = emit_handle(&args[0], func, ctx, ml);
            func.instruction(&I::LocalGet(o));
            emit_expr(&args[1], func, ctx, ml, Some(&IRType::Int));
            call_import(func, JsImport::Then);
            ctx.release_held();
            IRType::None
        }
        "resolve" => {
            for (k, arg) in args.iter().enumerate() {
                let ty = emit_expr(arg, func, ctx, ml, Some(&IRType::Int));
                if ty != IRType::Int {
                    ctx.report(format!("__js_resolve argument {k} is not an int"));
                }
            }
            call_import(func, JsImport::Resolve);
            IRType::None
        }
        "import" => {
            let ty = emit_expr(&args[0], func, ctx, ml, Some(&IRType::String));
            if ty != IRType::String {
                ctx.report("a module's name must be a str");
            }
            // (offset, length): keep the offset for the error.
            let len = hold(ctx);
            func.instruction(&I::LocalSet(len));
            let off = hold(ctx);
            func.instruction(&I::LocalTee(off));
            func.instruction(&I::LocalGet(len));
            call_import(func, JsImport::ImportModule);
            func.instruction(&I::LocalSet(ctx.temp_local));
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::LocalGet(off));
            func.instruction(&I::Call(helper(ctx, JsFn::Check(JsOp::Import))));
            emit_post_call_check(func, ctx);
            func.instruction(&I::LocalGet(ctx.temp_local));
            func.instruction(&I::Call(helper(ctx, JsFn::Wrap)));
            ctx.release_held();
            ctx.release_held();
            IRType::JsProxy
        }
        _ => return None,
    })
}

/// `a == b` with at least one side a JS value: two JS values are the same
/// value (`===`); one against a Python value compares as the value Pyodide
/// would hand over. Both sides are evaluated in the order written.
fn emit_eq(
    left: &IRExpr,
    right: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    ml: &MemoryLayout,
) {
    let left_proxy = crate::compiler::function::infer_value_type(left, ctx) == IRType::JsProxy;
    let (proxy, other) = if left_proxy {
        (left, right)
    } else {
        (right, left)
    };
    if matches!(other, IRExpr::Const(IRConstant::None)) {
        emit_expr(proxy, func, ctx, ml, None);
        func.instruction(&I::I32Load(crate::compiler::runtime::w32(0)));
        func.instruction(&I::Call(imp(ctx, JsImport::Kind)));
        func.instruction(&I::I32Eqz);
        return;
    }
    // The other side as a comparable: a number (f64, held), a JS string
    // (a handle, held, released after), or another JS value's handle.
    enum Side {
        Number(u32),
        Text(u32),
        Proxy(u32),
        Nothing,
    }
    let emit_other = |func: &mut Function| -> Side {
        let ty = emit_expr(other, func, ctx, ml, None);
        match ty {
            IRType::Int | IRType::Bool => {
                func.instruction(&I::F64ConvertI32S);
                let x = ctx.hold_f64().unwrap_or(0);
                func.instruction(&I::LocalSet(x));
                Side::Number(x)
            }
            IRType::Float => {
                let x = ctx.hold_f64().unwrap_or(0);
                func.instruction(&I::LocalSet(x));
                Side::Number(x)
            }
            IRType::String => {
                func.instruction(&I::Call(imp(ctx, JsImport::String)));
                let t = hold(ctx);
                func.instruction(&I::LocalSet(t));
                Side::Text(t)
            }
            IRType::JsProxy => {
                func.instruction(&I::I32Load(crate::compiler::runtime::w32(0)));
                let t = hold(ctx);
                func.instruction(&I::LocalSet(t));
                Side::Proxy(t)
            }
            IRType::None => Side::Nothing,
            other_ty => {
                ctx.report(format!(
                    "comparing a JavaScript value with a {} is not supported",
                    crate::type_to_string(&other_ty)
                ));
                emit_drop(func, &other_ty);
                Side::Nothing
            }
        }
    };
    let (h, side) = if left_proxy {
        let h = emit_handle(proxy, func, ctx, ml);
        (h, emit_other(func))
    } else {
        let side = emit_other(func);
        (emit_handle(proxy, func, ctx, ml), side)
    };
    match side {
        Side::Number(x) => {
            func.instruction(&I::LocalGet(h));
            func.instruction(&I::LocalGet(x));
            func.instruction(&I::Call(helper(ctx, JsFn::EqNumber)));
        }
        Side::Text(t) | Side::Proxy(t) => {
            func.instruction(&I::LocalGet(h));
            func.instruction(&I::LocalGet(t));
            func.instruction(&I::Call(imp(ctx, JsImport::Equal)));
            if matches!(side, Side::Text(_)) {
                func.instruction(&I::LocalGet(t));
                func.instruction(&I::Call(imp(ctx, JsImport::Release)));
            }
        }
        Side::Nothing => {
            func.instruction(&I::LocalGet(h));
            func.instruction(&I::Call(imp(ctx, JsImport::Kind)));
            func.instruction(&I::I32Eqz);
        }
    }
    // The receiver's slot was taken last when it came second.
    let (first_i32, number) = match side {
        Side::Number(_) => (false, true),
        Side::Text(_) | Side::Proxy(_) => (true, false),
        Side::Nothing => (false, false),
    };
    ctx.release_held();
    if first_i32 {
        ctx.release_held();
    }
    if number {
        ctx.release_held_f64();
    }
}

fn emit_drop(func: &mut Function, ty: &IRType) {
    match ty {
        IRType::None => {}
        IRType::String | IRType::Bytes => {
            func.instruction(&I::Drop);
            func.instruction(&I::Drop);
        }
        _ => {
            func.instruction(&I::Drop);
        }
    }
}

/// A JS value (on the stack) stored where `expected` is declared: converted
/// to that type, raising `TypeError` for a value of another kind. None when
/// the place keeps the proxy.
pub(crate) fn convert_at_store(
    func: &mut Function,
    ctx: &CompilationContext,
    expected: Option<&IRType>,
) -> Option<IRType> {
    let target = expected?;
    match target {
        IRType::Int | IRType::Float | IRType::Bool | IRType::String | IRType::None => {
            func.instruction(&I::Call(helper(ctx, JsFn::As(target.clone()))));
            emit_post_call_check(func, ctx);
            if *target == IRType::String {
                recover_str_pair(func, ctx);
            }
            Some(target.clone())
        }
        IRType::JsProxy | IRType::Unknown | IRType::Any => None,
        IRType::List(_)
        | IRType::Dict(..)
        | IRType::Tuple(_)
        | IRType::VarTuple(_)
        | IRType::Set(_) => {
            ctx.report(format!(
                "a JavaScript value stored where '{}' is declared: Pyodide keeps it a JsProxy. \
                 Hint: convert it with .to_py()",
                crate::type_to_string(target)
            ));
            Some(target.clone())
        }
        other => {
            ctx.report(format!(
                "a JavaScript value stored where '{}' is declared is not supported",
                crate::type_to_string(other)
            ));
            None
        }
    }
}

/// Whether a `for` loop walks a JavaScript value.
pub(crate) fn walks_js(iterable: &IRExpr, ctx: &CompilationContext) -> bool {
    !ctx.js.is_empty()
        && crate::compiler::function::infer_value_type(iterable, ctx) == IRType::JsProxy
}

/// `for x in v` over a JavaScript value, as the loop over JavaScript's
/// iteration protocol it is (numbered `seq`, so the scan and the code agree
/// on its iterator's local):
///
/// ```text
/// __jsit = __js_iter(v)
/// while True:
///     x = __js_next(__jsit)      # sets the stop flag when done
///     if <stopped>: break
///     <body>
/// ```
pub(crate) fn lower_for(
    seq: u32,
    target: &str,
    iterable: &IRExpr,
    loop_body: &crate::ir::IRBody,
) -> crate::ir::IRBody {
    use crate::ir::build::*;
    use crate::ir::IRStatement;
    let it = format!("__jsit_{seq}");
    let mut statements = vec![
        expr_stmt(call(crate::ir::STOP_CHECK_FN, vec![])),
        assign_typed(
            target,
            call(&intrinsic("next"), vec![v(&it)]),
            &IRType::JsProxy,
        ),
        if_(
            call(crate::ir::STOP_CHECK_FN, vec![]),
            vec![IRStatement::Break],
        ),
    ];
    statements.extend(loop_body.statements.iter().cloned());
    crate::ir::IRBody {
        statements: vec![
            assign_typed(
                &it,
                call(&intrinsic("iter"), vec![iterable.clone()]),
                &IRType::JsProxy,
            ),
            while_(boolean(true), statements),
        ],
    }
}
