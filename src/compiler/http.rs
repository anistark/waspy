//! Outgoing HTTP through WASI preview 2's `wasi:http`.
//!
//! A program makes a request through `requests` (`src/ir/pystdlib/requests.py`),
//! whose one call into the host is the intrinsic `__waspy_http(method,
//! scheme, authority, path, headers, body)`. Its helper here builds the
//! request over `wasi:http/types` (fields, an outgoing request, its body
//! written through `wasi:io/streams`), sends it with
//! `wasi:http/outgoing-handler`, blocks on the response, and answers one str
//! the Python side reads: `"R"`, the status's three digits, `"\n"`, each
//! header as `name\nvalue\n`, an empty line, and the body; or `"E"`, the step
//! that failed, `":"`, and the `error-code` case's index. The headers go in
//! the same way, as `name\nvalue\n` pairs.
//!
//! The imports are lowered by the canonical ABI: a result that does not fit
//! one value is written to a return area (the helper's scratch block), a list
//! the host answers is placed with the module's `cabi_realloc`, which takes
//! the block from `__alloc`. A module using them is made a component
//! (`compiler::component`).

use crate::compiler::asm::{call, Asm, Ty, E, X};
use crate::compiler::context::CompilationContext;
use crate::compiler::emit::Function;
use crate::compiler::runtime::Helper;
use crate::ir::{IRExpr, IRModule, IRType, MemoryLayout};
use wasm_encoder::{Instruction as I, ValType};

/// The intrinsic the `requests` shim calls.
pub const HTTP_FN: &str = "__waspy_http";

/// The shim's function that calls it: a module holding it makes requests.
pub const SEND_FN: &str = "__py_requests__send";

/// The imports the helper calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HttpImport {
    FieldsNew,
    FieldsAppend,
    FieldsEntries,
    FieldsDrop,
    RequestNew,
    SetMethod,
    SetScheme,
    SetAuthority,
    SetPath,
    RequestBody,
    BodyWrite,
    BodyFinish,
    FutureSubscribe,
    FutureGet,
    FutureDrop,
    Status,
    ResponseHeaders,
    Consume,
    ResponseDrop,
    IncomingStream,
    IncomingBodyDrop,
    Handle,
    PollBlock,
    PollDrop,
    OutputWrite,
    OutputDrop,
    InputRead,
    InputDrop,
}

impl HttpImport {
    pub const ALL: &'static [HttpImport] = &[
        HttpImport::FieldsNew,
        HttpImport::FieldsAppend,
        HttpImport::FieldsEntries,
        HttpImport::FieldsDrop,
        HttpImport::RequestNew,
        HttpImport::SetMethod,
        HttpImport::SetScheme,
        HttpImport::SetAuthority,
        HttpImport::SetPath,
        HttpImport::RequestBody,
        HttpImport::BodyWrite,
        HttpImport::BodyFinish,
        HttpImport::FutureSubscribe,
        HttpImport::FutureGet,
        HttpImport::FutureDrop,
        HttpImport::Status,
        HttpImport::ResponseHeaders,
        HttpImport::Consume,
        HttpImport::ResponseDrop,
        HttpImport::IncomingStream,
        HttpImport::IncomingBodyDrop,
        HttpImport::Handle,
        HttpImport::PollBlock,
        HttpImport::PollDrop,
        HttpImport::OutputWrite,
        HttpImport::OutputDrop,
        HttpImport::InputRead,
        HttpImport::InputDrop,
    ];

    /// The interface instance it is imported from.
    pub fn module(self) -> String {
        use HttpImport::*;
        let v = crate::compiler::http::wasi_version();
        match self {
            Handle => format!("wasi:http/outgoing-handler@{v}"),
            PollBlock | PollDrop => format!("wasi:io/poll@{v}"),
            OutputWrite | OutputDrop | InputRead | InputDrop => format!("wasi:io/streams@{v}"),
            _ => format!("wasi:http/types@{v}"),
        }
    }

    pub fn name(self) -> &'static str {
        use HttpImport::*;
        match self {
            FieldsNew => "[constructor]fields",
            FieldsAppend => "[method]fields.append",
            FieldsEntries => "[method]fields.entries",
            FieldsDrop => "[resource-drop]fields",
            RequestNew => "[constructor]outgoing-request",
            SetMethod => "[method]outgoing-request.set-method",
            SetScheme => "[method]outgoing-request.set-scheme",
            SetAuthority => "[method]outgoing-request.set-authority",
            SetPath => "[method]outgoing-request.set-path-with-query",
            RequestBody => "[method]outgoing-request.body",
            BodyWrite => "[method]outgoing-body.write",
            BodyFinish => "[static]outgoing-body.finish",
            FutureSubscribe => "[method]future-incoming-response.subscribe",
            FutureGet => "[method]future-incoming-response.get",
            FutureDrop => "[resource-drop]future-incoming-response",
            Status => "[method]incoming-response.status",
            ResponseHeaders => "[method]incoming-response.headers",
            Consume => "[method]incoming-response.consume",
            ResponseDrop => "[resource-drop]incoming-response",
            IncomingStream => "[method]incoming-body.stream",
            IncomingBodyDrop => "[resource-drop]incoming-body",
            Handle => "handle",
            PollBlock => "[method]pollable.block",
            PollDrop => "[resource-drop]pollable",
            OutputWrite => "[method]output-stream.blocking-write-and-flush",
            OutputDrop => "[resource-drop]output-stream",
            InputRead => "[method]input-stream.blocking-read",
            InputDrop => "[resource-drop]input-stream",
        }
    }

    /// The core signature the canonical ABI lowers it to.
    pub fn signature(self) -> (Vec<ValType>, Vec<ValType>) {
        use HttpImport::*;
        use ValType::{I32, I64};
        match self {
            FieldsNew => (vec![], vec![I32]),
            // (self, name, name_len, value, value_len, ret)
            FieldsAppend => (vec![I32; 6], vec![]),
            FieldsEntries | RequestBody | BodyWrite | FutureGet | Consume | IncomingStream => {
                (vec![I32; 2], vec![])
            }
            RequestNew | FutureSubscribe | Status | ResponseHeaders => (vec![I32], vec![I32]),
            // (self, case, text, text_len) -> result
            SetMethod => (vec![I32; 4], vec![I32]),
            // (self, some, case, text, text_len) -> result
            SetScheme => (vec![I32; 5], vec![I32]),
            // (self, some, text, text_len) -> result
            SetAuthority | SetPath => (vec![I32; 4], vec![I32]),
            // (this, some, trailers, ret) / (request, some, options, ret)
            BodyFinish | Handle => (vec![I32; 4], vec![]),
            // (self, bytes, len, ret)
            OutputWrite => (vec![I32; 4], vec![]),
            // (self, len: u64, ret)
            InputRead => (vec![I32, I64, I32], vec![]),
            FieldsDrop | FutureDrop | ResponseDrop | IncomingBodyDrop | PollBlock | PollDrop
            | OutputDrop | InputDrop => (vec![I32], vec![]),
        }
    }
}

/// The WASI version the imports name.
pub fn wasi_version() -> &'static str {
    #[cfg(feature = "wasi-http")]
    {
        crate::compiler::component::WASI_VERSION
    }
    #[cfg(not(feature = "wasi-http"))]
    {
        "0.2.12"
    }
}

/// Whether a module makes HTTP requests (holds the `requests` shim).
pub fn uses_http(module: &IRModule) -> bool {
    module.functions.iter().any(|f| f.name == SEND_FN)
}

fn import(ctx: &CompilationContext, f: HttpImport) -> u32 {
    *ctx.http.get(&f).unwrap_or_else(|| {
        panic!(
            "the {} import was not reserved; the module makes no request",
            f.name()
        )
    })
}

/// The blocks the preview 1 adapter is given for its own state (through
/// `cabi_realloc_adapter`), which it keeps as long as the module runs: each
/// is held here, where the collector reads them as roots, so it is never
/// freed. The word past them counts them.
const HTTP: i32 = 118784;
const PIN_SLOTS: i32 = 16;
pub(crate) const PINS: (i32, i32) = (HTTP, HTTP + PIN_SLOTS * 4);
const PIN_COUNT: i32 = HTTP + PIN_SLOTS * 4;
const _: () = assert!(PIN_COUNT + 4 <= crate::compiler::context::COLLECTION_HEAP_BASE as i32);

/// A runtime helper of the HTTP client.
#[derive(Clone, Debug, PartialEq)]
pub enum HttpFn {
    /// `(method, scheme, authority, path, headers, body) -> str`.
    Request,
    /// `cabi_realloc(old, old_size, align, new_size) -> ptr`, exported.
    Realloc,
    /// `cabi_realloc_adapter(...)`: the same, its blocks pinned.
    AdapterRealloc,
}

impl HttpFn {
    pub fn signature(&self) -> (Vec<ValType>, Vec<ValType>) {
        match self {
            HttpFn::Request => (vec![ValType::I32; 6], vec![ValType::I32]),
            HttpFn::Realloc | HttpFn::AdapterRealloc => (vec![ValType::I32; 4], vec![ValType::I32]),
        }
    }
}

/// `__waspy_http(...)`: the six strs as words, and the answer as a str.
pub fn emit_call(
    arguments: &[IRExpr],
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
) -> IRType {
    if arguments.len() != 6 || ctx.http.is_empty() {
        ctx.report(if ctx.http.is_empty() {
            "HTTP requests need the module to be made a component, which this build of waspy \
             cannot do (it was built without the wasi-http feature), or the module uses the \
             JavaScript bridge, where requests go through fetch (use httpx.AsyncClient)"
                .to_string()
        } else {
            format!("{HTTP_FN}() takes six strs")
        });
        func.instruction(&I::I32Const(0));
        func.instruction(&I::I32Const(0));
        return IRType::String;
    }
    for a in arguments {
        crate::compiler::stdlib::emit_str_word(HTTP_FN, a, func, ctx, memory_layout);
    }
    func.instruction(&I::Call(ctx.helper(Helper::Http(HttpFn::Request))));
    crate::compiler::expression::recover_str_pair(func, ctx);
    IRType::String
}

pub fn build(routine: &HttpFn, ctx: &CompilationContext) -> Function {
    match routine {
        HttpFn::Request => build_request(ctx),
        HttpFn::Realloc => build_realloc(ctx, false),
        HttpFn::AdapterRealloc => build_realloc(ctx, true),
    }
}

/// A str's length in bytes.
fn len(s: impl Into<E>) -> E {
    (s.into() - 4).load(Ty::I32, 0)
}

fn byte(s: impl Into<E>, i: impl Into<E>) -> E {
    (s.into() + i).load8(0)
}

/// Mark a block from `__alloc` as one the collector never reads.
fn no_scan(a: &mut Asm, block: crate::compiler::asm::Var) {
    a.store(Ty::I32, block - 4, 0, crate::compiler::gc::NOSCAN);
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

/// `cabi_realloc`: a new block from `__alloc` (never scanned, as what the
/// host places there is bytes and handles), the old one's bytes copied in.
/// The adapter's (`pin`) is also held in [`PINS`], and traps past its slots.
fn build_realloc(ctx: &CompilationContext, pin: bool) -> Function {
    let mut a = Asm::new(&[Ty::I32, Ty::I32, Ty::I32, Ty::I32]);
    let (old, old_size, align, size) = (a.param(0), a.param(1), a.param(2), a.param(3));
    a.if_(size.eq(0), |a| a.ret(align.e()));
    // `__alloc`'s blocks are 4-aligned; what is placed in one may need 8.
    let block = a.let_(call(ctx.alloc_fn(), vec![size + align], Some(Ty::I32)));
    no_scan(&mut a, block);
    let p = a.let_((block + align - 1) & (-align.e()));
    if pin {
        let n = a.let_(E::from(PIN_COUNT).load(Ty::I32, 0));
        a.if_(n.ge(PIN_SLOTS), |a| a.raw(I::Unreachable));
        a.store(Ty::I32, n * 4 + HTTP, 0, block.e());
        a.store(Ty::I32, PIN_COUNT, 0, n + 1);
    }
    a.if_(old.ne(0), |a| {
        let n = a.let_(old_size.e());
        a.if_(size.lt(n), |a| a.set(n, size.e()));
        copy(a, p, old, n);
    });
    a.ret(p.e());
    a.finish()
}

/// The request, sent, and its response read whole (see the module's doc).
fn build_request(ctx: &CompilationContext) -> Function {
    use HttpImport::*;
    let mut a = Asm::new(&[Ty::I32; 6]);
    let (method, scheme, authority, path, headers, body) = (
        a.param(0),
        a.param(1),
        a.param(2),
        a.param(3),
        a.param(4),
        a.param(5),
    );
    let imp = |f: HttpImport| import(ctx, f);
    // The return area, 8-aligned (an `error-code` holds a u64).
    let block = a.let_(call(ctx.alloc_fn(), vec![72.into()], Some(Ty::I32)));
    no_scan(&mut a, block);
    let ret = a.let_((block + 7) & -8);
    // An error answer: "E", the step (one letter), ":", and the code's
    // decimal digits (the error-code case, or 99 for none).
    let fail = |a: &mut Asm, step: u8, code: E| {
        let code = a.let_(code);
        let out = a.let_(call(ctx.alloc_fn(), vec![10.into()], Some(Ty::I32)));
        no_scan(a, out);
        a.store(Ty::I32, out, 0, 5);
        a.store8(out, 4, b'E' as i32);
        a.store8(out, 5, step as i32);
        a.store8(out, 6, b':' as i32);
        a.store8(out, 7, code / 10 % 10 + 48);
        a.store8(out, 8, code % 10 + 48);
        a.store8(out, 9, 0);
        a.ret(out + 4);
    };
    let fields = a.let_(call(imp(FieldsNew), vec![], Some(Ty::I32)));
    // The headers, `name\nvalue\n` pairs.
    let n = a.let_(len(headers));
    let i = a.let_(0);
    a.while_(i.lt(n), |a, _| {
        let name = a.let_(i.e());
        a.while_(byte(headers, i).ne(10), |a, _| a.set(i, i + 1));
        let name_len = a.let_(i - name);
        a.set(i, i + 1);
        let value = a.let_(i.e());
        a.while_(byte(headers, i).ne(10), |a, _| a.set(i, i + 1));
        let value_len = a.let_(i - value);
        a.set(i, i + 1);
        a.run(call(
            imp(FieldsAppend),
            vec![
                fields.e(),
                headers + name,
                name_len.e(),
                headers + value,
                value_len.e(),
                ret.e(),
            ],
            None,
        ));
        a.if_(ret.load8(0).ne(0), |a| fail(a, b'h', ret.load8(1)));
    });
    let req = a.let_(call(imp(RequestNew), vec![fields.e()], Some(Ty::I32)));
    // The method's case, or `other(text)`.
    let m = a.let_(9);
    let mlen = a.let_(len(method));
    for (k, word) in [
        "GET", "HEAD", "POST", "PUT", "DELETE", "CONNECT", "OPTIONS", "TRACE", "PATCH",
    ]
    .iter()
    .enumerate()
    {
        let mut same = mlen.eq(word.len() as i32);
        for (j, b) in word.bytes().enumerate() {
            same = same & byte(method, j as i32).eq(b as i32);
        }
        // Reading past a shorter method's end is reading its terminator and
        // the next block's bytes, which the length test already rules out.
        a.if_(m.eq(9) & mlen.eq(word.len() as i32), |a| {
            a.if_(same, |a| a.set(m, k as i32));
        });
    }
    a.if_(
        call(
            imp(SetMethod),
            vec![req.e(), m.e(), method.e(), mlen.e()],
            Some(Ty::I32),
        )
        .ne(0),
        |a| fail(a, b'm', 99.into()),
    );
    let slen = a.let_(len(scheme));
    let s = a.let_(2);
    a.if_(
        slen.eq(4) & byte(scheme, 0).eq(b'h' as i32) & byte(scheme, 3).eq(b'p' as i32),
        |a| a.set(s, 0),
    );
    a.if_(
        slen.eq(5) & byte(scheme, 0).eq(b'h' as i32) & byte(scheme, 4).eq(b's' as i32),
        |a| a.set(s, 1),
    );
    a.if_(
        call(
            imp(SetScheme),
            vec![req.e(), 1.into(), s.e(), scheme.e(), slen.e()],
            Some(Ty::I32),
        )
        .ne(0),
        |a| fail(a, b's', 99.into()),
    );
    a.if_(
        call(
            imp(SetAuthority),
            vec![req.e(), 1.into(), authority.e(), len(authority)],
            Some(Ty::I32),
        )
        .ne(0),
        |a| fail(a, b'a', 99.into()),
    );
    a.if_(
        call(
            imp(SetPath),
            vec![req.e(), 1.into(), path.e(), len(path)],
            Some(Ty::I32),
        )
        .ne(0),
        |a| fail(a, b'p', 99.into()),
    );
    a.run(call(imp(RequestBody), vec![req.e(), ret.e()], None));
    a.if_(ret.load8(0).ne(0), |a| fail(a, b'b', 99.into()));
    let outgoing = a.let_(ret.load(Ty::I32, 4));
    // Sent first, then the body written: the host reads it as it goes.
    a.run(call(
        imp(Handle),
        vec![req.e(), 0.into(), 0.into(), ret.e()],
        None,
    ));
    a.if_(ret.load8(0).ne(0), |a| fail(a, b'c', ret.load8(8)));
    let future = a.let_(ret.load(Ty::I32, 8));
    a.run(call(imp(BodyWrite), vec![outgoing.e(), ret.e()], None));
    a.if_(ret.load8(0).ne(0), |a| fail(a, b'w', 99.into()));
    let stream = a.let_(ret.load(Ty::I32, 4));
    let blen = a.let_(len(body));
    let off = a.let_(0);
    a.while_(off.lt(blen), |a, _| {
        let chunk = a.let_(blen - off);
        a.if_(chunk.gt(4096), |a| a.set(chunk, 4096));
        a.run(call(
            imp(OutputWrite),
            vec![stream.e(), body + off, chunk.e(), ret.e()],
            None,
        ));
        a.if_(ret.load8(0).ne(0), |a| fail(a, b'w', 99.into()));
        a.set(off, off + chunk);
    });
    a.run(call(imp(OutputDrop), vec![stream.e()], None));
    a.run(call(
        imp(BodyFinish),
        vec![outgoing.e(), 0.into(), 0.into(), ret.e()],
        None,
    ));
    a.if_(ret.load8(0).ne(0), |a| fail(a, b'f', ret.load8(8)));
    // The response.
    let poll = a.let_(call(imp(FutureSubscribe), vec![future.e()], Some(Ty::I32)));
    a.run(call(imp(PollBlock), vec![poll.e()], None));
    a.run(call(imp(PollDrop), vec![poll.e()], None));
    a.run(call(imp(FutureGet), vec![future.e(), ret.e()], None));
    a.if_(ret.load8(0).eq(0) | ret.load8(8).ne(0), |a| {
        fail(a, b'r', 99.into())
    });
    a.if_(ret.load8(16).ne(0), |a| fail(a, b'r', ret.load8(24)));
    let response = a.let_(ret.load(Ty::I32, 24));
    a.run(call(imp(FutureDrop), vec![future.e()], None));
    let status = a.let_(call(imp(Status), vec![response.e()], Some(Ty::I32)));
    let hdrs = a.let_(call(
        imp(ResponseHeaders),
        vec![response.e()],
        Some(Ty::I32),
    ));
    a.run(call(imp(FieldsEntries), vec![hdrs.e(), ret.e()], None));
    let entries = a.let_(ret.load(Ty::I32, 0));
    let count = a.let_(ret.load(Ty::I32, 4));
    a.run(call(imp(FieldsDrop), vec![hdrs.e()], None));
    a.run(call(imp(Consume), vec![response.e(), ret.e()], None));
    a.if_(ret.load8(0).ne(0), |a| fail(a, b'r', 99.into()));
    let incoming = a.let_(ret.load(Ty::I32, 4));
    a.run(call(imp(IncomingStream), vec![incoming.e(), ret.e()], None));
    a.if_(ret.load8(0).ne(0), |a| fail(a, b'r', 99.into()));
    let input = a.let_(ret.load(Ty::I32, 4));
    // The body, gathered in a block that doubles as it fills.
    let cap = a.let_(4096);
    let got = a.let_(0);
    let acc = a.let_(call(ctx.alloc_fn(), vec![cap.e()], Some(Ty::I32)));
    no_scan(&mut a, acc);
    a.loop_(|a, l| {
        a.run(call(
            imp(InputRead),
            vec![input.e(), E::from(65536i64), ret.e()],
            None,
        ));
        // Closed: the body is all read.
        a.if_(ret.load8(0).ne(0), |a| {
            a.brk_if(l, ret.load8(4).eq(1));
            fail(a, b'r', 99.into());
        });
        let chunk = a.let_(ret.load(Ty::I32, 4));
        let size = a.let_(ret.load(Ty::I32, 8));
        a.if_((got + size).gt(cap), |a| {
            a.while_((got + size).gt(cap), |a, _| a.set(cap, cap * 2));
            let grown = a.let_(call(ctx.alloc_fn(), vec![cap.e()], Some(Ty::I32)));
            no_scan(a, grown);
            copy(a, grown, acc, got);
            a.set(acc, grown.e());
        });
        copy(a, acc + got, chunk, size);
        a.set(got, got + size);
        a.cont(l);
    });
    a.run(call(imp(InputDrop), vec![input.e()], None));
    a.run(call(imp(IncomingBodyDrop), vec![incoming.e()], None));
    a.run(call(imp(ResponseDrop), vec![response.e()], None));
    // The answer: "R", the status, the headers, an empty line, the body.
    let total = a.let_(got + 6);
    let k = a.let_(0);
    a.while_(k.lt(count), |a, _| {
        let e = a.let_(entries + k * 16);
        a.set(total, total + e.load(Ty::I32, 4) + e.load(Ty::I32, 12) + 2);
        a.set(k, k + 1);
    });
    let out = a.let_(call(ctx.alloc_fn(), vec![total + 5], Some(Ty::I32)));
    no_scan(&mut a, out);
    a.store(Ty::I32, out, 0, total.e());
    a.set(out, out + 4);
    a.store8(out + total, 0, 0);
    a.store8(out, 0, b'R' as i32);
    a.store8(out, 1, status / 100 % 10 + 48);
    a.store8(out, 2, status / 10 % 10 + 48);
    a.store8(out, 3, status % 10 + 48);
    a.store8(out, 4, 10);
    let at = a.let_(out + 5);
    a.set(k, 0);
    a.while_(k.lt(count), |a, _| {
        let e = a.let_(entries + k * 16);
        let nl = a.let_(e.load(Ty::I32, 4));
        copy(a, at, e.load(Ty::I32, 0), nl);
        a.set(at, at + nl);
        a.store8(at, 0, 10);
        a.set(at, at + 1);
        let vl = a.let_(e.load(Ty::I32, 12));
        copy(a, at, e.load(Ty::I32, 8), vl);
        a.set(at, at + vl);
        a.store8(at, 0, 10);
        a.set(at, at + 1);
        a.set(k, k + 1);
    });
    a.store8(at, 0, 10);
    a.set(at, at + 1);
    copy(&mut a, at, acc, got);
    a.ret(out.e());
    a.finish()
}
