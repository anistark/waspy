//! `os` and `os.path`.
//!
//! The path functions are CPython's `posixpath` over the UTF-8 bytes of a
//! string: `/` and `.` never occur inside a multibyte character, so working on
//! bytes gives the same answers as working on characters. The host values go
//! through WASI: the environment (`environ_get`), entropy (`random_get`), and
//! what a path names (`path_filestat_get`, resolved against the preopens as
//! `open()` resolves it).
//!
//! WASI has no working directory and no process ids. As in wasi-libc, which
//! CPython on WASI is built on, the working directory is `/`, and the process
//! id is 42.

use crate::compiler::asm::{call, Asm, Ty, Var, E, X};
use crate::compiler::context::{
    CompilationContext, COLLECTION_CAP, COLLECTION_DATA, COLLECTION_HEADER, DICT_ENTRY,
};
use crate::compiler::expression::{emit_expr, recover_str_pair};
use crate::compiler::function::emit_post_call_check;
use crate::compiler::host::{wasi, Wasi};
use crate::compiler::runtime::Helper;
use crate::ir::{IRExpr, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction as I};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

/// The module's state, past the generator's: `os.environ` once built, and a
/// `filestat` for `path_filestat_get` to fill.
const OS: i32 = 117760;
const ENVIRON: i32 = OS;
const SIZES: i32 = OS + 8;
const STAT: i32 = OS + 16;
const _: () = assert!(STAT + 64 <= crate::compiler::context::COLLECTION_HEAP_BASE as i32);

/// The runtime routines. Strings are their offset word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OsFn {
    /// `() -> dict`: `os.environ`, one dict for the program's life.
    Environ,
    /// `(key) -> str | 0`: the environment's value for the key, 0 for none.
    Getenv,
    /// `(n) -> bytes`; ValueError for a negative n.
    Urandom,
    /// `(a, b) -> str`: `os.path.join(a, b)`.
    Join,
    /// `(p) -> str`: `os.path.basename`, `dirname`.
    Basename,
    Dirname,
    /// `(p) -> tuple`: `os.path.split`, `splitext`.
    Split,
    Splitext,
    /// `(p) -> str`: `os.path.normpath`, `abspath`.
    Normpath,
    Abspath,
    /// `(p) -> filetype`: what the path names, -1 for nothing.
    Kind,
}

impl OsFn {
    pub fn signature(self) -> (Vec<Ty>, Option<Ty>) {
        use Ty::I32;
        match self {
            OsFn::Environ => (vec![], Some(I32)),
            OsFn::Join => (vec![I32, I32], Some(I32)),
            _ => (vec![I32], Some(I32)),
        }
    }
}

fn of(ctx: &CompilationContext, routine: OsFn, args: Vec<E>) -> E {
    let (_, result) = routine.signature();
    call(ctx.helper(Helper::Os(routine)), args, result)
}

/// A string's length in bytes.
fn len(s: Var) -> E {
    (s - 4).load(Ty::I32, 0)
}

fn byte(s: impl Into<E>, i: impl Into<E>) -> E {
    (s.into() + i).load8(0)
}

/// A new string of `n` bytes, uninitialized, as `runtime::alloc_string`
/// lays one out.
fn new_str(a: &mut Asm, ctx: &CompilationContext, n: impl Into<E>) -> Var {
    let n = a.let_(n);
    let p = a.let_(call(ctx.alloc_func_index, vec![n + 5], Some(Ty::I32)));
    a.store(Ty::I32, p, 0, n);
    a.set(p, p + 4);
    a.store8(p + n, 0, 0);
    p
}

fn copy(a: &mut Asm, dst: impl Into<E>, src: impl Into<E>, n: impl Into<E>) {
    a.push(dst);
    a.push(src);
    a.push(n);
    a.raw(I::MemoryCopy {
        src_mem: 0,
        dst_mem: 0,
    });
}

/// `s[start:end]` over bytes, as a new string.
fn slice(
    a: &mut Asm,
    ctx: &CompilationContext,
    s: Var,
    start: impl Into<E>,
    end: impl Into<E>,
) -> Var {
    let start = a.let_(start);
    let n = a.let_(end.into() - start);
    let out = new_str(a, ctx, n);
    copy(a, out, s + start, n);
    out
}

/// `s.rfind(byte)`, -1 when absent.
fn rfind(a: &mut Asm, s: Var, c: u8) -> Var {
    let i = a.let_(len(s) - 1);
    a.while_(i.ge(0), |a, l| {
        a.brk_if(l, byte(s, i).eq(c as i32));
        a.set(i, i - 1);
    });
    i
}

/// A 2-tuple of strings, laid out as a list.
fn pair(a: &mut Asm, ctx: &CompilationContext, x: Var, y: Var) -> Var {
    let t = a.let_(call(
        ctx.alloc_func_index,
        vec![(COLLECTION_HEADER as i32 + 16).into()],
        Some(Ty::I32),
    ));
    a.store(Ty::I32, t, 0, 2);
    a.store(Ty::I32, t, COLLECTION_CAP as u64, 2);
    a.store(
        Ty::I32,
        t,
        COLLECTION_DATA as u64,
        t + COLLECTION_HEADER as i32,
    );
    a.store(Ty::I32, t, COLLECTION_HEADER as u64, x);
    a.store(Ty::I32, t, COLLECTION_HEADER as u64 + 8, y);
    t
}

/// The two strings are the same bytes.
fn same(a: &mut Asm, x: impl Into<E>, xn: impl Into<E>, y: impl Into<E>, yn: impl Into<E>) -> Var {
    let (x, xn, y, yn) = (a.let_(x), a.let_(xn), a.let_(y), a.let_(yn));
    let eq = a.let_(xn.eq(yn));
    let i = a.let_(0);
    a.while_(eq.ne(0) & i.lt(xn), |a, _| {
        a.set(eq, byte(x, i).eq(byte(y, i)));
        a.set(i, i + 1);
    });
    eq
}

pub(crate) fn build(ctx: &CompilationContext, routine: OsFn) -> Function {
    let (params, _) = routine.signature();
    let mut a = Asm::new(&params);
    match routine {
        OsFn::Environ => environ(&mut a, ctx),
        OsFn::Getenv => {
            let key = a.param(0);
            let env = a.let_(of(ctx, OsFn::Environ, vec![]));
            let data = a.let_(env.load(Ty::I32, COLLECTION_DATA as u64));
            let i = a.local(Ty::I32);
            a.for_(i, 0, env.load(Ty::I32, 0), false, |a, _| {
                let k = a.let_((i * DICT_ENTRY as i32 + data).load(Ty::I32, 0));
                let eq = same(a, k, len(k), key, len(key));
                a.if_(eq, |a| {
                    a.ret((i * DICT_ENTRY as i32 + data).load(Ty::I32, 8));
                });
            });
            a.ret(0);
        }
        OsFn::Urandom => {
            let n = a.param(0);
            a.if_(n.lt(0), |a| {
                let text = call(
                    ctx.helper(Helper::ExcText("negative argument not allowed".to_string())),
                    vec![],
                    Some(Ty::I32),
                );
                a.run(call(
                    ctx.helper(Helper::RaiseText("ValueError".to_string())),
                    vec![text],
                    None,
                ));
                a.ret(0);
            });
            let out = new_str(&mut a, ctx, n);
            a.run(call(
                wasi(ctx, Wasi::RandomGet),
                vec![out.e(), n.e()],
                Some(Ty::I32),
            ));
            a.ret(out);
        }
        OsFn::Join => {
            // posixpath.join for two parts: an absolute part replaces the
            // path; otherwise one separator joins them.
            let (x, y) = (a.param(0), a.param(1));
            let (xn, yn) = (a.let_(len(x)), a.let_(len(y)));
            a.if_(yn.gt(0) & byte(y, 0).eq(b'/' as i32), |a| a.ret(y));
            let glue = a.let_((xn.gt(0) & byte(x, xn - 1).ne(b'/' as i32)).select(1, 0));
            let out = new_str(&mut a, ctx, xn + glue + yn);
            copy(&mut a, out, x, xn);
            a.if_(glue, |a| a.store8(out + xn, 0, b'/' as i32));
            copy(&mut a, out + xn + glue, y, yn);
            a.ret(out);
        }
        OsFn::Basename => {
            let p = a.param(0);
            let i = rfind(&mut a, p, b'/');
            let out = slice(&mut a, ctx, p, i + 1, len(p));
            a.ret(out);
        }
        OsFn::Dirname => {
            let p = a.param(0);
            let head = dirname_end(&mut a, p);
            let out = slice(&mut a, ctx, p, 0, head);
            a.ret(out);
        }
        OsFn::Split => {
            let p = a.param(0);
            let i = rfind(&mut a, p, b'/');
            let tail = slice(&mut a, ctx, p, i + 1, len(p));
            let end = dirname_end(&mut a, p);
            let head = slice(&mut a, ctx, p, 0, end);
            let t = pair(&mut a, ctx, head, tail);
            a.ret(t);
        }
        OsFn::Splitext => {
            // genericpath._splitext: the last dot past the last separator,
            // unless only dots lead up to it.
            let p = a.param(0);
            let sep = rfind(&mut a, p, b'/');
            let dot = rfind(&mut a, p, b'.');
            let at = a.let_(len(p));
            a.if_(dot.gt(sep), |a| {
                let i = a.let_(sep + 1);
                a.while_(i.lt(dot), |a, l| {
                    a.if_(byte(p, i).ne(b'.' as i32), |a| a.set(at, dot));
                    a.brk_if(l, at.eq(dot));
                    a.set(i, i + 1);
                });
            });
            let root = slice(&mut a, ctx, p, 0, at);
            let ext = slice(&mut a, ctx, p, at, len(p));
            let t = pair(&mut a, ctx, root, ext);
            a.ret(t);
        }
        OsFn::Normpath => normpath(&mut a, ctx),
        OsFn::Abspath => {
            // join(getcwd(), p) for a relative p, with the cwd "/", then
            // normpath.
            let p = a.param(0);
            a.if_(len(p).gt(0) & byte(p, 0).eq(b'/' as i32), |a| {
                a.ret(of(ctx, OsFn::Normpath, vec![p.e()]));
            });
            let n = a.let_(len(p));
            let out = new_str(&mut a, ctx, n + 1);
            a.store8(out, 0, b'/' as i32);
            copy(&mut a, out + 1, p, n);
            a.ret(of(ctx, OsFn::Normpath, vec![out.e()]));
        }
        OsFn::Kind => {
            let p = a.param(0);
            let dir = a.let_(call(
                ctx.helper(Helper::ResolvePath),
                vec![p.e(), len(p)],
                Some(Ty::I32),
            ));
            a.if_(dir.lt(0), |a| a.ret(-1));
            let resolved = crate::compiler::host::RESOLVED;
            let errno = a.let_(call(
                wasi(ctx, Wasi::PathFilestatGet),
                vec![
                    dir.e(),
                    1.into(), // follow symlinks
                    E::from(resolved).load(Ty::I32, 0),
                    E::from(resolved).load(Ty::I32, 4),
                    STAT.into(),
                ],
                Some(Ty::I32),
            ));
            a.if_(errno.ne(0), |a| a.ret(-1));
            a.ret(E::from(STAT).load8(16));
        }
    }
    a.finish()
}

/// Where `dirname(p)` ends: the head of `split(p)`, with its trailing
/// separators stripped unless it is all separators.
fn dirname_end(a: &mut Asm, p: Var) -> Var {
    let i = rfind(a, p, b'/');
    let end = a.let_(i + 1);
    // all separators?
    let k = a.let_(0);
    a.while_(k.lt(end) & byte(p, k).eq(b'/' as i32), |a, _| {
        a.set(k, k + 1)
    });
    a.if_(k.lt(end), |a| {
        a.while_(end.gt(0) & byte(p, end - 1).eq(b'/' as i32), |a, _| {
            a.set(end, end - 1)
        });
    });
    end
}

/// posixpath.normpath.
fn normpath(a: &mut Asm, ctx: &CompilationContext) {
    let p = a.param(0);
    let n = a.let_(len(p));
    a.if_(n.eqz(), |a| {
        let out = new_str(a, ctx, 1);
        a.store8(out, 0, b'.' as i32);
        a.ret(out);
    });
    // One or two leading slashes are kept; three or more are one.
    let initial = a.let_(byte(p, 0).eq(b'/' as i32));
    a.if_(
        initial.ne(0)
            & n.ge(2)
            & byte(p, 1).eq(b'/' as i32)
            & (n.lt(3) | byte(p, 2).ne(b'/' as i32)),
        |a| a.set(initial, 2),
    );
    // The kept components, as (start, length) pairs.
    let comps = a.let_(call(ctx.alloc_func_index, vec![n * 8 + 8], Some(Ty::I32)));
    let count = a.let_(0);
    let start = a.let_(0);
    let i = a.local(Ty::I32);
    let at = |k: E| k * 8 + comps;
    a.for_(i, 0, n + 1, false, |a, _| {
        a.if_(
            i.eq(n) | byte(p, i.lt(n).select(i, n - 1)).eq(b'/' as i32),
            |a| {
                let clen = a.let_(i - start);
                let dot = a.let_(clen.eq(1) & byte(p, start).eq(b'.' as i32));
                let dotdot = a.let_(
                    clen.eq(2)
                        & byte(p, start).eq(b'.' as i32)
                        & byte(p, start + 1).eq(b'.' as i32),
                );
                a.if_(clen.ne(0) & dot.eqz(), |a| {
                    // A '..' is kept at the start of a relative path, and after
                    // another '..'; otherwise it removes the component before it.
                    let last_dotdot = a.let_(0);
                    a.if_(count.gt(0), |a| {
                        let ls = a.let_(at(count - 1).load(Ty::I32, 0));
                        a.set(
                            last_dotdot,
                            at(count - 1).load(Ty::I32, 4).eq(2)
                                & byte(p, ls).eq(b'.' as i32)
                                & byte(p, ls + 1).eq(b'.' as i32),
                        );
                    });
                    a.if_else(
                        dotdot.eqz() | (initial.eqz() & count.eqz()) | (count.gt(0) & last_dotdot),
                        |a| {
                            a.store(Ty::I32, at(count.e()), 0, start);
                            a.store(Ty::I32, at(count.e()), 4, clen);
                            a.set(count, count + 1);
                        },
                        |a| a.if_(count.gt(0), |a| a.set(count, count - 1)),
                    );
                });
                a.set(start, i + 1);
            },
        );
    });
    // '/' * initial + '/'.join(comps), or '.'
    let total = a.let_(initial + count.gt(0).select(count - 1, 0));
    let k = a.local(Ty::I32);
    a.for_(k, 0, count, false, |a, _| {
        a.set(total, total + at(k.e()).load(Ty::I32, 4));
    });
    a.if_(total.eqz(), |a| {
        let out = new_str(a, ctx, 1);
        a.store8(out, 0, b'.' as i32);
        a.ret(out);
    });
    let out = new_str(a, ctx, total);
    let w = a.let_(0);
    a.while_(w.lt(initial), |a, _| {
        a.store8(out + w, 0, b'/' as i32);
        a.set(w, w + 1);
    });
    a.for_(k, 0, count, false, |a, _| {
        a.if_(k.gt(0), |a| {
            a.store8(out + w, 0, b'/' as i32);
            a.set(w, w + 1);
        });
        let cs = a.let_(at(k.e()).load(Ty::I32, 0));
        let cl = a.let_(at(k.e()).load(Ty::I32, 4));
        copy(a, out + w, p + cs, cl);
        a.set(w, w + cl);
    });
    a.ret(out);
}

/// `os.environ`: the host environment's `KEY=value` strings as a dict of
/// strs, the first of a repeated key kept (CPython's `setdefault`).
fn environ(a: &mut Asm, ctx: &CompilationContext) {
    let cached = a.let_(E::from(ENVIRON).load(Ty::I32, 0));
    a.if_(cached, |a| a.ret(cached));
    a.run(call(
        wasi(ctx, Wasi::EnvironSizesGet),
        vec![SIZES.into(), (SIZES + 4).into()],
        Some(Ty::I32),
    ));
    let count = a.let_(E::from(SIZES).load(Ty::I32, 0));
    let size = a.let_(E::from(SIZES).load(Ty::I32, 4));
    let alloc = |n: E| call(ctx.alloc_func_index, vec![n], Some(Ty::I32));
    let ptrs = a.let_(alloc(count * 4 + 8));
    let buf = a.let_(alloc(size + 8));
    a.run(call(
        wasi(ctx, Wasi::EnvironGet),
        vec![ptrs.e(), buf.e()],
        Some(Ty::I32),
    ));
    let dict = a.let_(alloc(count * DICT_ENTRY as i32 + COLLECTION_HEADER as i32));
    let data = a.let_(dict + COLLECTION_HEADER as i32);
    a.store(Ty::I32, dict, COLLECTION_CAP as u64, count);
    a.store(Ty::I32, dict, COLLECTION_DATA as u64, data);
    let n = a.let_(0);
    let i = a.local(Ty::I32);
    let check = ctx.helper(Helper::Utf8Check);
    a.for_(i, 0, count, false, |a, _| {
        let entry = a.let_((i * 4 + ptrs).load(Ty::I32, 0));
        // its length, to the NUL, and where '=' is
        let elen = a.let_(0);
        a.while_(byte(entry, elen).ne(0), |a, _| a.set(elen, elen + 1));
        let eq = a.let_(0);
        a.while_(eq.lt(elen) & byte(entry, eq).ne(b'=' as i32), |a, _| {
            a.set(eq, eq + 1)
        });
        let key = new_str(a, ctx, eq);
        copy(a, key, entry, eq);
        let vlen = a.let_(elen - eq - 1);
        a.if_(vlen.lt(0), |a| a.set(vlen, 0));
        let value = new_str(a, ctx, vlen);
        copy(a, value, entry + eq + 1, vlen);
        a.run(call(check, vec![key.e()], None));
        a.run(call(check, vec![value.e()], None));
        // setdefault: skip a key already present
        let dup = a.let_(0);
        let j = a.let_(0);
        a.while_(j.lt(n) & dup.eqz(), |a, _| {
            let k = a.let_((j * DICT_ENTRY as i32 + data).load(Ty::I32, 0));
            let eqk = same(a, k, len(k), key, len(key));
            a.set(dup, eqk);
            a.set(j, j + 1);
        });
        a.if_(dup.eqz(), |a| {
            a.store(Ty::I32, n * DICT_ENTRY as i32 + data, 0, key);
            a.store(Ty::I32, n * DICT_ENTRY as i32 + data, 8, value);
            a.set(n, n + 1);
        });
    });
    a.store(Ty::I32, dict, 0, n);
    a.store(Ty::I32, ENVIRON, 0, dict);
    a.ret(dict);
}

// ---- the Python-level functions ---------------------------------------------

/// Emit a string argument, leaving its offset word; `false` (with a
/// placeholder) for anything else.
fn emit_str_word(
    what: &str,
    arg: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> bool {
    let ty = emit_expr(arg, func, ctx, memory_layout, None);
    match ty {
        IRType::String => {
            emit!(func, I::Drop);
            true
        }
        other => {
            emit!(func, I::Drop);
            if matches!(other, IRType::Bytes) {
                emit!(func, I::Drop);
            }
            ctx.report(format!(
                "{what}() takes a str here, got {}",
                crate::type_to_string(&other)
            ));
            emit!(func, I::I32Const(0));
            false
        }
    }
}

fn str_result(func: &mut Function, ctx: &CompilationContext) -> IRType {
    recover_str_pair(func, ctx);
    IRType::String
}

fn arity(name: &str, arguments: &[IRExpr], n: usize, ctx: &CompilationContext) -> bool {
    if arguments
        .iter()
        .any(|a| matches!(a, IRExpr::Keyword { .. }))
    {
        ctx.report(format!("{name}() takes no keyword arguments here"));
        return false;
    }
    if arguments.len() != n {
        ctx.report(format!(
            "{name}() takes {n} argument{}, got {}",
            if n == 1 { "" } else { "s" },
            arguments.len()
        ));
        return false;
    }
    true
}

/// `os.environ` read as a value.
pub(crate) fn emit_environ(func: &mut Function, ctx: &CompilationContext) -> IRType {
    emit!(func, I::Call(ctx.helper(Helper::Os(OsFn::Environ))));
    IRType::Dict(Box::new(IRType::String), Box::new(IRType::String))
}

/// Emit `os.<name>(...)` or `os.path.<name>(...)`, or `None` when it is not
/// implemented.
pub(crate) fn emit_call(
    qualified: &str,
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> Option<IRType> {
    let helper = |routine: OsFn| I::Call(ctx.helper(Helper::Os(routine)));
    let one_str = |func: &mut Function| {
        arity(qualified, arguments, 1, ctx)
            && emit_str_word(qualified, &arguments[0], func, ctx, memory_layout)
    };
    match qualified {
        "os.getcwd" => {
            if !arity(qualified, arguments, 0, ctx) {
                emit!(func, I::I32Const(0), I::I32Const(0));
                return Some(IRType::String);
            }
            emit!(func, I::Call(ctx.helper(Helper::ExcText("/".to_string()))));
            Some(str_result(func, ctx))
        }
        "os.getpid" => {
            arity(qualified, arguments, 0, ctx);
            emit!(func, I::I32Const(42));
            Some(IRType::Int)
        }
        "os.getenv" => {
            if arguments
                .iter()
                .any(|a| matches!(a, IRExpr::Keyword { .. }))
                || arguments.is_empty()
                || arguments.len() > 2
            {
                ctx.report("os.getenv() takes a key and an optional default, by position");
                emit!(func, I::I32Const(0));
                return Some(IRType::Optional(Box::new(IRType::String)));
            }
            if !emit_str_word(qualified, &arguments[0], func, ctx, memory_layout) {
                return Some(IRType::Optional(Box::new(IRType::String)));
            }
            emit!(func, helper(OsFn::Getenv));
            match arguments.get(1) {
                // A missing key is None.
                None => Some(IRType::Optional(Box::new(IRType::String))),
                Some(IRExpr::Const(crate::ir::IRConstant::None)) => {
                    Some(IRType::Optional(Box::new(IRType::String)))
                }
                Some(default) => {
                    let found = ctx.hold()?;
                    emit!(func, I::LocalSet(found));
                    if !emit_str_word(qualified, default, func, ctx, memory_layout) {
                        ctx.release_held();
                        emit!(func, I::Drop, I::I32Const(0), I::I32Const(0));
                        return Some(IRType::String);
                    }
                    let d = ctx.temp_local;
                    emit!(
                        func,
                        I::LocalSet(d),
                        I::LocalGet(found),
                        I::LocalGet(d),
                        I::LocalGet(found),
                        I::Select
                    );
                    ctx.release_held();
                    Some(str_result(func, ctx))
                }
            }
        }
        "os.urandom" => {
            if !arity(qualified, arguments, 1, ctx) {
                emit!(func, I::I32Const(0), I::I32Const(0));
                return Some(IRType::Bytes);
            }
            let ty = emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
            if !matches!(ty, IRType::Int | IRType::Bool) {
                ctx.report(format!(
                    "os.urandom() takes an int, got {}",
                    crate::type_to_string(&ty)
                ));
            }
            emit!(func, helper(OsFn::Urandom));
            emit_post_call_check(func, ctx);
            recover_str_pair(func, ctx);
            Some(IRType::Bytes)
        }
        "os.path.join" => {
            if arguments
                .iter()
                .any(|a| matches!(a, IRExpr::Keyword { .. }))
                || arguments.is_empty()
            {
                ctx.report("os.path.join() takes one or more paths, by position");
                emit!(func, I::I32Const(0), I::I32Const(0));
                return Some(IRType::String);
            }
            // join(a, b, c) is join(join(a, b), c); each part is held while
            // the next is emitted.
            if !emit_str_word(qualified, &arguments[0], func, ctx, memory_layout) {
                emit!(func, I::I32Const(0));
                return Some(str_result(func, ctx));
            }
            for arg in &arguments[1..] {
                let Some(acc) = ctx.hold() else {
                    ctx.report("os.path.join() nests too deeply");
                    return Some(str_result(func, ctx));
                };
                emit!(func, I::LocalSet(acc));
                emit_str_word(qualified, arg, func, ctx, memory_layout);
                let b = ctx.temp_local;
                emit!(
                    func,
                    I::LocalSet(b),
                    I::LocalGet(acc),
                    I::LocalGet(b),
                    helper(OsFn::Join)
                );
                ctx.release_held();
            }
            Some(str_result(func, ctx))
        }
        "os.path.basename" | "os.path.dirname" | "os.path.normpath" | "os.path.abspath" => {
            if !one_str(func) {
                emit!(func, I::I32Const(0));
                return Some(str_result(func, ctx));
            }
            emit!(
                func,
                helper(match qualified {
                    "os.path.basename" => OsFn::Basename,
                    "os.path.dirname" => OsFn::Dirname,
                    "os.path.normpath" => OsFn::Normpath,
                    _ => OsFn::Abspath,
                })
            );
            Some(str_result(func, ctx))
        }
        "os.path.split" | "os.path.splitext" => {
            if !one_str(func) {
                return Some(IRType::Tuple(vec![IRType::String, IRType::String]));
            }
            emit!(
                func,
                helper(if qualified == "os.path.split" {
                    OsFn::Split
                } else {
                    OsFn::Splitext
                })
            );
            Some(IRType::Tuple(vec![IRType::String, IRType::String]))
        }
        "os.path.exists" | "os.path.isfile" | "os.path.isdir" => {
            if !one_str(func) {
                return Some(IRType::Bool);
            }
            emit!(func, helper(OsFn::Kind));
            match qualified {
                "os.path.exists" => emit!(func, I::I32Const(0), I::I32GeS),
                // __WASI_FILETYPE_REGULAR_FILE
                "os.path.isfile" => emit!(func, I::I32Const(4), I::I32Eq),
                // __WASI_FILETYPE_DIRECTORY
                _ => emit!(func, I::I32Const(3), I::I32Eq),
            }
            Some(IRType::Bool)
        }
        "os.path.isabs" => {
            if !one_str(func) {
                return Some(IRType::Bool);
            }
            // starts with '/'
            let p = ctx.temp_local;
            emit!(
                func,
                I::LocalTee(p),
                I::I32Const(4),
                I::I32Sub,
                I::I32Load(crate::compiler::runtime::w32(0)),
                I::If(BlockType::Result(wasm_encoder::ValType::I32)),
                I::LocalGet(p),
                I::I32Load8U(crate::compiler::runtime::byte()),
                I::I32Const(b'/' as i32),
                I::I32Eq,
                I::Else,
                I::I32Const(0),
                I::End
            );
            Some(IRType::Bool)
        }
        _ => None,
    }
}

/// What `os.<name>()` / `os.path.<name>()` answers.
pub(crate) fn result_type(qualified: &str, arguments: &[IRExpr]) -> Option<IRType> {
    Some(match qualified {
        "os.getenv"
            if arguments.len() == 2
                && !matches!(arguments[1], IRExpr::Const(crate::ir::IRConstant::None)) =>
        {
            IRType::String
        }
        "os.getcwd" | "os.path.join" | "os.path.basename" | "os.path.dirname"
        | "os.path.normpath" | "os.path.abspath" => IRType::String,
        "os.getpid" => IRType::Int,
        "os.urandom" => IRType::Bytes,
        "os.getenv" => IRType::Optional(Box::new(IRType::String)),
        "os.path.split" | "os.path.splitext" => IRType::Tuple(vec![IRType::String, IRType::String]),
        "os.path.exists" | "os.path.isfile" | "os.path.isdir" | "os.path.isabs" => IRType::Bool,
        _ => return None,
    })
}
