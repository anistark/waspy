//! The host interface: WASI preview 1.
//!
//! A program's side effects go through the `wasi_snapshot_preview1` imports,
//! each imported only when the program uses it: `print` and `sys.stdout` /
//! `sys.stderr` import `fd_write`, `input` and `sys.stdin` import `fd_read`
//! (and `fd_write` for a prompt), `sys.argv` imports `args_sizes_get` and
//! `args_get`, and a `__main__` block that can raise imports `fd_write` and
//! `proc_exit` to report the exception the way CPython does. A program that
//! does none of these imports nothing, and instantiates with an empty import
//! object as before. The module runs unchanged under any WASI preview 1 host
//! (`wasmtime`, `node:wasi`, a browser shim).

use crate::compiler::context::CompilationContext;
use crate::compiler::runtime::{alloc_string, byte, function, w32, Helper};
use crate::ir::{IRBody, IRExpr, IRModule};
use wasm_encoder::{BlockType, Function, Instruction as I, ValType};

macro_rules! emit {
    ($f:expr, $($i:expr),* $(,)?) => {{
        $( $f.instruction(&$i); )*
    }};
}

/// A WASI preview 1 import.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Wasi {
    FdWrite,
    FdRead,
    ArgsSizesGet,
    ArgsGet,
    ProcExit,
    PathOpen,
    FdPrestatGet,
    FdPrestatDirName,
    FdClose,
    RandomGet,
    EnvironSizesGet,
    EnvironGet,
    PathFilestatGet,
    ClockTimeGet,
}

impl Wasi {
    pub const MODULE: &'static str = "wasi_snapshot_preview1";

    pub fn name(self) -> &'static str {
        match self {
            Wasi::FdWrite => "fd_write",
            Wasi::FdRead => "fd_read",
            Wasi::ArgsSizesGet => "args_sizes_get",
            Wasi::ArgsGet => "args_get",
            Wasi::ProcExit => "proc_exit",
            Wasi::PathOpen => "path_open",
            Wasi::FdPrestatGet => "fd_prestat_get",
            Wasi::FdPrestatDirName => "fd_prestat_dir_name",
            Wasi::FdClose => "fd_close",
            Wasi::RandomGet => "random_get",
            Wasi::EnvironSizesGet => "environ_sizes_get",
            Wasi::EnvironGet => "environ_get",
            Wasi::PathFilestatGet => "path_filestat_get",
            Wasi::ClockTimeGet => "clock_time_get",
        }
    }

    pub fn signature(self) -> (Vec<ValType>, Vec<ValType>) {
        use ValType::I32;
        match self {
            Wasi::FdWrite | Wasi::FdRead => (vec![I32; 4], vec![I32]),
            Wasi::ArgsSizesGet | Wasi::ArgsGet => (vec![I32; 2], vec![I32]),
            Wasi::ProcExit => (vec![I32], vec![]),
            // (dirfd, lookupflags, path, path_len, oflags, rights_base,
            // rights_inheriting, fdflags, fd_out) -> errno
            Wasi::PathOpen => (
                vec![
                    I32,
                    I32,
                    I32,
                    I32,
                    I32,
                    ValType::I64,
                    ValType::I64,
                    I32,
                    I32,
                ],
                vec![I32],
            ),
            Wasi::FdPrestatGet => (vec![I32; 2], vec![I32]),
            Wasi::FdPrestatDirName => (vec![I32; 3], vec![I32]),
            Wasi::FdClose => (vec![I32], vec![I32]),
            Wasi::RandomGet | Wasi::EnvironSizesGet | Wasi::EnvironGet => (vec![I32; 2], vec![I32]),
            // (fd, lookupflags, path, path_len, filestat_out) -> errno
            Wasi::PathFilestatGet => (vec![I32; 5], vec![I32]),
            // (clock id, precision, time_out) -> errno
            Wasi::ClockTimeGet => (vec![I32, ValType::I64, I32], vec![I32]),
        }
    }
}

/// Where the host helpers keep their state: after the code point cache,
/// below the heap.
const HOST: i32 = 106496;
/// One iovec, `[buf][len]`, and the count a call answers.
const IOV: i32 = HOST;
const NDONE: i32 = HOST + 8;
/// Standard input's buffer: `[buffer][read position][end][at EOF]`.
const STDIN_BUF: i32 = HOST + 12;
const STDIN_POS: i32 = HOST + 16;
const STDIN_END: i32 = HOST + 20;
const STDIN_EOF: i32 = HOST + 24;
/// `sys.argv`, once built.
const ARGV: i32 = HOST + 28;
/// A preopened directory's `prestat` (`[tag u8][pad][name_len u32]`), and
/// the descriptor `path_open` answers.
const PRESTAT: i32 = HOST + 32;
const FD_OUT: i32 = HOST + 40;
/// `sys.path`, once built.
const SYS_PATH: i32 = HOST + 44;
/// A resolved path: what is left of it past its preopen, `[rest][len]`.
pub(crate) const RESOLVED: i32 = HOST + 48;
/// The time `clock_time_get` answers, in nanoseconds.
pub(crate) const CLOCK_OUT: i32 = HOST + 56;
const STDIN_CHUNK: i32 = 4096;
const _: () = assert!(HOST + 64 <= crate::compiler::context::COLLECTION_HEAP_BASE as i32);

/// Whether `expr` names one of the standard streams (`sys.stdout`,
/// `sys.stderr`, `sys.stdin`), and which file descriptor it is.
pub(crate) fn std_stream(ctx: &CompilationContext, expr: &IRExpr) -> Option<i32> {
    let IRExpr::Attribute { object, attribute } = expr else {
        return None;
    };
    let IRExpr::Variable(module) = object.as_ref() else {
        return None;
    };
    if module != "sys" || ctx.get_local_info(module).is_some() {
        return None;
    }
    match attribute.as_str() {
        "stdin" => Some(0),
        "stdout" => Some(1),
        "stderr" => Some(2),
        _ => None,
    }
}

/// The WASI imports a module needs, in import order.
pub(crate) fn wasi_needed(module: &IRModule, main_can_raise: bool) -> Vec<Wasi> {
    let mut needed = std::collections::BTreeSet::new();
    let defined: std::collections::HashSet<&str> =
        module.functions.iter().map(|f| f.name.as_str()).collect();
    // `from os import getenv`: the names bound to stdlib functions.
    let from_imports: std::collections::HashMap<String, (String, String)> = module
        .imports
        .iter()
        .filter(|i| i.is_from_import && crate::stdlib::is_stdlib_module_path(&i.module))
        .filter_map(|i| {
            let name = i.name.clone()?;
            Some((
                i.alias.clone().unwrap_or_else(|| name.clone()),
                (i.module.clone(), name),
            ))
        })
        .collect();
    // What a use of `module.name` needs from the host.
    let stdlib_use = |module: &str, name: &str| -> &'static [Wasi] {
        match (module, name) {
            // The generator seeds itself from host entropy on its first draw
            // unless the program seeded it.
            ("random", _) | ("os", "urandom") => &[Wasi::RandomGet],
            ("os", "environ" | "getenv") => &[Wasi::EnvironSizesGet, Wasi::EnvironGet],
            ("os.path", "exists" | "isfile" | "isdir") => &[
                Wasi::PathFilestatGet,
                Wasi::FdPrestatGet,
                Wasi::FdPrestatDirName,
            ],
            _ => &[],
        }
    };
    let mut visit = |expr: &mut IRExpr| -> anyhow::Result<()> {
        let dotted = |e: &IRExpr| match e {
            IRExpr::Variable(m) => Some(m.clone()),
            IRExpr::Attribute { object, attribute } => match object.as_ref() {
                IRExpr::Variable(m) => Some(format!("{m}.{attribute}")),
                _ => None,
            },
            _ => None,
        };
        match &*expr {
            IRExpr::MethodCall {
                object,
                method_name,
                ..
            } => {
                if let Some(m) = dotted(object) {
                    needed.extend(stdlib_use(&m, method_name).iter().copied());
                }
            }
            IRExpr::Attribute { object, attribute } => {
                if let Some(m) = dotted(object) {
                    needed.extend(stdlib_use(&m, attribute).iter().copied());
                }
            }
            IRExpr::FunctionCall {
                function_name: n, ..
            }
            | IRExpr::Variable(n)
                if !defined.contains(n.as_str()) =>
            {
                if let Some((m, name)) = from_imports.get(n) {
                    needed.extend(stdlib_use(m, name).iter().copied());
                }
            }
            _ => {}
        }
        match expr {
            IRExpr::FunctionCall { function_name, .. }
                if !defined.contains(function_name.as_str()) =>
            {
                match function_name.as_str() {
                    "print" => {
                        needed.insert(Wasi::FdWrite);
                    }
                    // `datetime`'s reading of the clock.
                    crate::ir::DATETIME_CLOCK_FN => {
                        needed.insert(Wasi::ClockTimeGet);
                    }
                    "input" => {
                        needed.insert(Wasi::FdWrite);
                        needed.insert(Wasi::FdRead);
                    }
                    _ => {}
                }
            }
            IRExpr::Attribute { object, attribute } => {
                if let IRExpr::Variable(m) = object.as_ref() {
                    if m == "sys" {
                        match attribute.as_str() {
                            "stdout" | "stderr" => {
                                needed.insert(Wasi::FdWrite);
                            }
                            "stdin" => {
                                needed.insert(Wasi::FdRead);
                            }
                            "argv" => {
                                needed.insert(Wasi::ArgsSizesGet);
                                needed.insert(Wasi::ArgsGet);
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    };
    let mut bodies: Vec<IRBody> = module.functions.iter().map(|f| f.body.clone()).collect();
    bodies.extend(
        module
            .classes
            .iter()
            .flat_map(|c| c.methods.iter().map(|m| m.body.clone())),
    );
    for body in &mut bodies {
        let _ = crate::ir::visit_body(body, &mut visit);
    }
    if main_can_raise {
        needed.insert(Wasi::FdWrite);
        needed.insert(Wasi::ProcExit);
    }
    // `open()` and the file methods, unless they go to `waspy_host`.
    if crate::compiler::module::module_uses_file_io(module) && !legacy_file_io(module) {
        needed.extend([
            Wasi::FdRead,
            Wasi::FdWrite,
            Wasi::PathOpen,
            Wasi::FdPrestatGet,
            Wasi::FdPrestatDirName,
            Wasi::FdClose,
        ]);
    }
    needed.into_iter().collect()
}

/// Whether the module's file I/O compiles to the deprecated `waspy_host`
/// imports.
pub(crate) fn legacy_file_io(module: &IRModule) -> bool {
    module
        .metadata
        .get(crate::ir::FILE_IO_HOST_KEY)
        .is_some_and(|h| h == "waspy_host")
}

// ---------------------------------------------------------------------------
// Files, through WASI
// ---------------------------------------------------------------------------

/// The WASI errno `path_open` answered, as CPython reports it on a POSIX
/// host: the exception, its errno, and its text.
const OPEN_ERRORS: &[(i32, &str, i32, &str)] = &[
    (44, "FileNotFoundError", 2, "No such file or directory"),
    (2, "PermissionError", 13, "Permission denied"),
    (76, "PermissionError", 13, "Permission denied"),
    (63, "PermissionError", 1, "Operation not permitted"),
    (31, "IsADirectoryError", 21, "Is a directory"),
    (54, "NotADirectoryError", 20, "Not a directory"),
    (20, "FileExistsError", 17, "File exists"),
];

/// Leave `class("[Errno n] text: 'path'")` pending; the path is local 0.
fn raise_open_error(
    f: &mut Function,
    ctx: &CompilationContext,
    class: &str,
    errno: i32,
    text: &str,
) {
    emit!(
        f,
        I::Call(ctx.helper(Helper::ExcText(format!("[Errno {errno}] {text}: ")))),
        I::LocalGet(0),
        I::Call(ctx.helper(Helper::Repr(crate::ir::IRType::String))),
        I::Call(ctx.helper(Helper::Concat)),
        I::Call(ctx.helper(Helper::RaiseText(class.to_string()))),
        I::I32Const(-1),
        I::Return
    );
}

/// `(path, path_len, flags) -> fd`: `open()` through WASI. The path is
/// resolved against the preopened directories as wasi-libc does: an absolute
/// one against the longest preopen naming a prefix of it, a relative one
/// against the first relative preopen (`.`), else `/`. A failure leaves
/// CPython's exception pending (`FileNotFoundError: [Errno 2] No such file or
/// directory: 'x'`) and answers -1. `flags` are the mode bits of
/// `expression::file_mode_flags`.
/// `(path, path_len) -> dirfd`: the preopened directory a path resolves
/// against, as wasi-libc resolves it: an absolute path against the longest
/// preopen naming a prefix of it, a relative one against the first relative
/// preopen (`.`), else `/`. What is left of the path is at `RESOLVED`
/// (`[rest][rest_len]`); -1 when no directory holds it.
pub(crate) fn build_resolve_path(ctx: &CompilationContext) -> Function {
    let prestat_get = wasi(ctx, Wasi::FdPrestatGet);
    let dir_name = wasi(ctx, Wasi::FdPrestatDirName);
    let (path, len) = (0, 1);
    // locals
    let (fd, best, best_len, root, name, nlen, i, rest, rlen) = (2, 3, 4, 5, 6, 7, 8, 9, 10);
    let mut f = function(&[ValType::I32; 9]);
    let absolute = |f: &mut Function| {
        emit!(
            f,
            I::LocalGet(len),
            I::I32Const(0),
            I::I32GtS,
            I::If(BlockType::Result(ValType::I32)),
            I::LocalGet(path),
            I::I32Load8U(byte()),
            I::I32Const(b'/' as i32),
            I::I32Eq,
            I::Else,
            I::I32Const(0),
            I::End
        );
    };
    emit!(
        f,
        I::I32Const(-1),
        I::LocalSet(best),
        I::I32Const(-1),
        I::LocalSet(best_len),
        I::I32Const(3),
        I::LocalSet(fd),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(fd),
        I::I32Const(PRESTAT),
        I::Call(prestat_get),
        I::BrIf(1),
        // A directory: its name.
        I::I32Const(PRESTAT),
        I::I32Load8U(byte()),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(PRESTAT),
        I::I32Load(w32(4)),
        I::LocalTee(nlen),
        I::I32Const(1),
        I::I32Add,
        I::Call(ctx.alloc_func_index),
        I::LocalSet(name),
        I::LocalGet(fd),
        I::LocalGet(name),
        I::LocalGet(nlen),
        I::Call(dir_name),
        I::Drop,
        // Some hosts count a trailing NUL.
        I::LocalGet(nlen),
        I::If(BlockType::Empty),
        I::LocalGet(name),
        I::LocalGet(nlen),
        I::I32Add,
        I::I32Const(1),
        I::I32Sub,
        I::I32Load8U(byte()),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::LocalGet(nlen),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(nlen),
        I::End,
        I::End
    );
    absolute(&mut f);
    emit!(f, I::If(BlockType::Empty));
    // Absolute: the name is a prefix of the path, at a component boundary.
    emit!(
        f,
        I::LocalGet(nlen),
        I::LocalGet(len),
        I::I32LeU,
        I::LocalGet(nlen),
        I::I32Const(0),
        I::I32GtU,
        I::I32And,
        I::If(BlockType::Empty),
        I::I32Const(1),
        I::LocalSet(rest), // the names match so far
        I::I32Const(0),
        I::LocalSet(i),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(i),
        I::LocalGet(nlen),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(name),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load8U(byte()),
        I::LocalGet(path),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Ne,
        I::If(BlockType::Empty),
        I::I32Const(0),
        I::LocalSet(rest),
        I::Br(2),
        I::End,
        I::LocalGet(i),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(i),
        I::Br(0),
        I::End,
        I::End,
        // ... and ends where a component does (or is `/` itself).
        I::LocalGet(rest),
        I::LocalGet(nlen),
        I::LocalGet(len),
        I::I32Eq,
        I::LocalGet(nlen),
        I::I32Const(1),
        I::I32Eq,
        I::I32Or,
        I::LocalGet(nlen),
        I::LocalGet(len),
        I::I32LtU,
        I::If(BlockType::Result(ValType::I32)),
        I::LocalGet(path),
        I::LocalGet(nlen),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(b'/' as i32),
        I::I32Eq,
        I::Else,
        I::I32Const(0),
        I::End,
        I::I32Or,
        I::I32And,
        I::LocalGet(nlen),
        I::LocalGet(best_len),
        I::I32GtS,
        I::I32And,
        I::If(BlockType::Empty),
        I::LocalGet(fd),
        I::LocalSet(best),
        I::LocalGet(nlen),
        I::LocalSet(best_len),
        I::End,
        I::End
    );
    emit!(f, I::Else);
    // Relative: the first preopen not named from `/`, else `/`.
    emit!(
        f,
        I::LocalGet(nlen),
        I::I32Eqz,
        I::If(BlockType::Result(ValType::I32)),
        I::I32Const(1),
        I::Else,
        I::LocalGet(name),
        I::I32Load8U(byte()),
        I::I32Const(b'/' as i32),
        I::I32Ne,
        I::End,
        I::If(BlockType::Empty),
        I::LocalGet(best),
        I::I32Const(0),
        I::I32LtS,
        I::LocalGet(root),
        I::I32Or,
        I::If(BlockType::Empty),
        I::LocalGet(fd),
        I::LocalSet(best),
        I::I32Const(0),
        I::LocalSet(root),
        I::End,
        I::Else,
        I::LocalGet(best),
        I::I32Const(0),
        I::I32LtS,
        I::LocalGet(nlen),
        I::I32Const(1),
        I::I32Eq,
        I::I32And,
        I::If(BlockType::Empty),
        I::LocalGet(fd),
        I::LocalSet(best),
        I::I32Const(1),
        I::LocalSet(root),
        I::End,
        I::End
    );
    emit!(f, I::End, I::End); // absolute / relative, directory
    emit!(
        f,
        I::LocalGet(fd),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(fd),
        I::Br(0),
        I::End,
        I::End
    );
    // No directory to resolve it in.
    emit!(
        f,
        I::LocalGet(best),
        I::I32Const(0),
        I::I32LtS,
        I::If(BlockType::Empty),
        I::I32Const(-1),
        I::Return,
        I::End
    );
    // What is left of the path past the preopen's name.
    emit!(
        f,
        I::LocalGet(path),
        I::LocalSet(rest),
        I::LocalGet(len),
        I::LocalSet(rlen)
    );
    absolute(&mut f);
    emit!(
        f,
        I::If(BlockType::Empty),
        I::LocalGet(path),
        I::LocalGet(best_len),
        I::I32Add,
        I::LocalSet(rest),
        I::LocalGet(len),
        I::LocalGet(best_len),
        I::I32Sub,
        I::LocalSet(rlen),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(rlen),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(rest),
        I::I32Load8U(byte()),
        I::I32Const(b'/' as i32),
        I::I32Ne,
        I::BrIf(1),
        I::LocalGet(rest),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(rest),
        I::LocalGet(rlen),
        I::I32Const(1),
        I::I32Sub,
        I::LocalSet(rlen),
        I::Br(0),
        I::End,
        I::End,
        // The preopen itself is `.` within it.
        I::LocalGet(rlen),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::LocalGet(name),
        I::I32Const(b'.' as i32),
        I::I32Store8(byte()),
        I::LocalGet(name),
        I::LocalSet(rest),
        I::I32Const(1),
        I::LocalSet(rlen),
        I::End,
        I::End
    );
    emit!(
        f,
        I::I32Const(RESOLVED),
        I::LocalGet(rest),
        I::I32Store(w32(0)),
        I::I32Const(RESOLVED),
        I::LocalGet(rlen),
        I::I32Store(w32(4)),
        I::LocalGet(best),
        I::End
    );
    f
}

pub(crate) fn build_file_open(ctx: &CompilationContext) -> Function {
    let path_open = wasi(ctx, Wasi::PathOpen);
    let (path, len, flags) = (0, 1, 2);
    // locals
    let (best, rest, rlen, oflags, fdflags, errno) = (3, 4, 5, 6, 7, 8);
    let mut f = function(&[ValType::I32; 6]);
    emit!(
        f,
        I::LocalGet(path),
        I::LocalGet(len),
        I::Call(ctx.helper(Helper::ResolvePath)),
        I::LocalTee(best),
        I::I32Const(0),
        I::I32LtS,
        I::If(BlockType::Empty)
    );
    raise_open_error(
        &mut f,
        ctx,
        "FileNotFoundError",
        2,
        "No such file or directory",
    );
    emit!(
        f,
        I::End,
        I::I32Const(RESOLVED),
        I::I32Load(w32(0)),
        I::LocalSet(rest),
        I::I32Const(RESOLVED),
        I::I32Load(w32(4)),
        I::LocalSet(rlen)
    );
    // The mode: write truncates or creates, append creates and appends.
    let write = crate::compiler::expression::FILE_FLAG_WRITE;
    let append = crate::compiler::expression::FILE_FLAG_APPEND;
    emit!(
        f,
        I::I32Const(0),
        I::LocalSet(oflags),
        I::I32Const(0),
        I::LocalSet(fdflags),
        I::LocalGet(flags),
        I::I32Const(write),
        I::I32And,
        I::If(BlockType::Empty),
        I::I32Const(1 | 8), // CREAT | TRUNC
        I::LocalSet(oflags),
        I::End,
        I::LocalGet(flags),
        I::I32Const(append),
        I::I32And,
        I::If(BlockType::Empty),
        I::I32Const(1), // CREAT
        I::LocalSet(oflags),
        I::I32Const(1), // APPEND
        I::LocalSet(fdflags),
        I::End
    );
    // read, seek, fdstat_set_flags, sync, tell, write, advise, allocate
    let rights: i64 = 0x1FE;
    emit!(
        f,
        I::LocalGet(best),
        I::I32Const(1), // follow symlinks
        I::LocalGet(rest),
        I::LocalGet(rlen),
        I::LocalGet(oflags),
        I::I64Const(rights),
        I::I64Const(rights),
        I::LocalGet(fdflags),
        I::I32Const(FD_OUT),
        I::Call(path_open),
        I::LocalTee(errno),
        I::If(BlockType::Empty)
    );
    for (wasi_errno, class, errno_number, text) in OPEN_ERRORS {
        emit!(
            f,
            I::LocalGet(errno),
            I::I32Const(*wasi_errno),
            I::I32Eq,
            I::If(BlockType::Empty)
        );
        raise_open_error(&mut f, ctx, class, *errno_number, text);
        emit!(f, I::End);
    }
    raise_open_error(&mut f, ctx, "OSError", 5, "Input/output error");
    emit!(f, I::End, I::I32Const(FD_OUT), I::I32Load(w32(0)), I::End);
    f
}

/// `(fd, buf, len) -> n`: one `fd_read` into the buffer; -1 on an error.
pub(crate) fn build_file_read(ctx: &CompilationContext) -> Function {
    let fd_read = wasi(ctx, Wasi::FdRead);
    let mut f = function(&[]);
    emit!(
        f,
        I::I32Const(IOV),
        I::LocalGet(1),
        I::I32Store(w32(0)),
        I::I32Const(IOV),
        I::LocalGet(2),
        I::I32Store(w32(4)),
        I::LocalGet(0),
        I::I32Const(IOV),
        I::I32Const(1),
        I::I32Const(NDONE),
        I::Call(fd_read),
        I::If(BlockType::Result(ValType::I32)),
        I::I32Const(-1),
        I::Else,
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::End,
        I::End
    );
    f
}

/// `(fd, buf, len) -> n`: write all of the buffer; -1 on an error.
pub(crate) fn build_file_write(ctx: &CompilationContext) -> Function {
    let fd_write = wasi(ctx, Wasi::FdWrite);
    let (fd, buf, len, at, left) = (0, 1, 2, 3, 4);
    let mut f = function(&[ValType::I32; 2]);
    emit!(
        f,
        I::LocalGet(buf),
        I::LocalSet(at),
        I::LocalGet(len),
        I::LocalSet(left),
        I::Block(BlockType::Empty),
        I::Loop(BlockType::Empty),
        I::LocalGet(left),
        I::I32Eqz,
        I::BrIf(1),
        I::I32Const(IOV),
        I::LocalGet(at),
        I::I32Store(w32(0)),
        I::I32Const(IOV),
        I::LocalGet(left),
        I::I32Store(w32(4)),
        I::LocalGet(fd),
        I::I32Const(IOV),
        I::I32Const(1),
        I::I32Const(NDONE),
        I::Call(fd_write),
        I::If(BlockType::Empty),
        I::I32Const(-1),
        I::Return,
        I::End,
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(at),
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::I32Add,
        I::LocalSet(at),
        I::LocalGet(left),
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::I32Sub,
        I::LocalSet(left),
        I::Br(0),
        I::End,
        I::End,
        I::LocalGet(len),
        I::LocalGet(left),
        I::I32Sub,
        I::End
    );
    f
}

/// `(fd) -> status`: `fd_close`; 0, or -1 on an error.
pub(crate) fn build_file_close(ctx: &CompilationContext) -> Function {
    let fd_close = wasi(ctx, Wasi::FdClose);
    let mut f = function(&[]);
    emit!(
        f,
        I::LocalGet(0),
        I::Call(fd_close),
        I::If(BlockType::Result(ValType::I32)),
        I::I32Const(-1),
        I::Else,
        I::I32Const(0),
        I::End,
        I::End
    );
    f
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

pub(crate) fn wasi(ctx: &CompilationContext, import: Wasi) -> u32 {
    *ctx.wasi.get(&import).unwrap_or_else(|| {
        panic!(
            "the {} import was not reserved; the import walk missed a use",
            import.name()
        )
    })
}

/// `(fd, s) -> ()`: write the string to the file descriptor, all of it.
pub(crate) fn build_write_fd(ctx: &CompilationContext) -> Function {
    let fd_write = wasi(ctx, Wasi::FdWrite);
    let mut f = function(&[ValType::I32; 2]);
    let (fd, s, at, left) = (0, 1, 2, 3);
    emit!(
        f,
        I::LocalGet(s),
        I::LocalSet(at),
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalSet(left)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(left),
        I::I32Eqz,
        I::BrIf(1),
        I::I32Const(IOV),
        I::LocalGet(at),
        I::I32Store(w32(0)),
        I::I32Const(IOV),
        I::LocalGet(left),
        I::I32Store(w32(4)),
        I::I32Const(NDONE),
        I::I32Const(0),
        I::I32Store(w32(0)),
        I::LocalGet(fd),
        I::I32Const(IOV),
        I::I32Const(1),
        I::I32Const(NDONE),
        I::Call(fd_write),
        // An error, or no progress: the host will take no more.
        I::BrIf(1),
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::I32Eqz,
        I::BrIf(1),
        I::LocalGet(at),
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::I32Add,
        I::LocalSet(at),
        I::LocalGet(left),
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::I32Sub,
        I::LocalSet(left),
        I::Br(0),
        I::End,
        I::End,
        I::End
    );
    f
}

/// Refill standard input's buffer, keeping the bytes not yet read. Leaves
/// the number of new bytes (0 at EOF) on the stack.
fn emit_refill(f: &mut Function, ctx: &CompilationContext, keep: u32, n: u32, buf: u32) {
    let fd_read = wasi(ctx, Wasi::FdRead);
    // The first time, a buffer.
    emit!(
        f,
        I::I32Const(STDIN_BUF),
        I::I32Load(w32(0)),
        I::LocalTee(buf),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(STDIN_CHUNK),
        I::Call(ctx.alloc_func_index),
        I::LocalSet(buf),
        I::I32Const(STDIN_BUF),
        I::LocalGet(buf),
        I::I32Store(w32(0)),
        I::End,
        // Move what is left to the front.
        I::I32Const(STDIN_END),
        I::I32Load(w32(0)),
        I::I32Const(STDIN_POS),
        I::I32Load(w32(0)),
        I::I32Sub,
        I::LocalSet(keep),
        I::LocalGet(buf),
        I::LocalGet(buf),
        I::I32Const(STDIN_POS),
        I::I32Load(w32(0)),
        I::I32Add,
        I::LocalGet(keep),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::I32Const(STDIN_POS),
        I::I32Const(0),
        I::I32Store(w32(0)),
        I::I32Const(STDIN_END),
        I::LocalGet(keep),
        I::I32Store(w32(0)),
        I::I32Const(0),
        I::LocalSet(n),
        // Read into the rest, unless at EOF already.
        I::I32Const(STDIN_EOF),
        I::I32Load(w32(0)),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(IOV),
        I::LocalGet(buf),
        I::LocalGet(keep),
        I::I32Add,
        I::I32Store(w32(0)),
        I::I32Const(IOV),
        I::I32Const(STDIN_CHUNK),
        I::LocalGet(keep),
        I::I32Sub,
        I::I32Store(w32(4)),
        I::I32Const(NDONE),
        I::I32Const(0),
        I::I32Store(w32(0)),
        I::I32Const(0),
        I::I32Const(IOV),
        I::I32Const(1),
        I::I32Const(NDONE),
        I::Call(fd_read),
        I::If(BlockType::Empty),
        // An error reads as the end of input.
        I::I32Const(NDONE),
        I::I32Const(0),
        I::I32Store(w32(0)),
        I::End,
        I::I32Const(NDONE),
        I::I32Load(w32(0)),
        I::LocalTee(n),
        I::I32Eqz,
        I::If(BlockType::Empty),
        I::I32Const(STDIN_EOF),
        I::I32Const(1),
        I::I32Store(w32(0)),
        I::End,
        I::I32Const(STDIN_END),
        I::LocalGet(keep),
        I::LocalGet(n),
        I::I32Add,
        I::I32Store(w32(0)),
        I::End,
        I::LocalGet(n)
    );
}

/// Append the `len` bytes at `src` to the growing line (`line`, `line_len`,
/// `cap` locals; `line` 0 until the first piece), growing it as needed.
#[allow(clippy::too_many_arguments)]
fn emit_append(
    f: &mut Function,
    ctx: &CompilationContext,
    line: u32,
    line_len: u32,
    cap: u32,
    src: u32,
    len: u32,
    tmp: u32,
) {
    // Grow to at least line_len + len, doubling.
    emit!(
        f,
        I::LocalGet(line_len),
        I::LocalGet(len),
        I::I32Add,
        I::LocalGet(cap),
        I::I32GtU,
        I::If(BlockType::Empty),
        I::LocalGet(cap),
        I::I32Const(1),
        I::I32Shl,
        I::LocalGet(line_len),
        I::LocalGet(len),
        I::I32Add,
        I::I32Const(64),
        I::I32Add,
        I::LocalTee(tmp),
        I::LocalGet(cap),
        I::I32Const(1),
        I::I32Shl,
        I::LocalGet(tmp),
        I::I32GtU,
        I::Select,
        I::LocalSet(cap),
        I::LocalGet(cap),
        I::Call(ctx.alloc_func_index),
        I::LocalTee(tmp),
        I::LocalGet(line),
        I::LocalGet(line_len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(tmp),
        I::LocalSet(line),
        I::End,
        I::LocalGet(line),
        I::LocalGet(line_len),
        I::I32Add,
        I::LocalGet(src),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(line_len),
        I::LocalGet(len),
        I::I32Add,
        I::LocalSet(line_len)
    );
}

/// `() -> str | 0`: the next line of standard input, its newline kept when
/// `keep_newline` (`readline`) and dropped otherwise (`input`); with `whole`,
/// everything left (`read`). A line ends at `\n` alone, as on CPython's POSIX
/// standard input, which does no newline translation, so a `\r` stays. 0 at
/// EOF with nothing read. Bytes that are not UTF-8 raise
/// `UnicodeDecodeError`.
pub(crate) fn build_read_line(
    ctx: &CompilationContext,
    keep_newline: bool,
    whole: bool,
) -> Function {
    let check = ctx.helper(Helper::Utf8Check);
    let mut f = function(&[ValType::I32; 13]);
    let (line, line_len, cap, i, end, b, keep, n, buf, start, tmp, out, got) =
        (0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12);
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    // Scan the buffered bytes for the end of the line.
    emit!(
        f,
        I::I32Const(STDIN_BUF),
        I::I32Load(w32(0)),
        I::LocalSet(buf),
        I::I32Const(STDIN_POS),
        I::I32Load(w32(0)),
        I::LocalTee(start),
        I::LocalSet(i),
        I::I32Const(STDIN_END),
        I::I32Load(w32(0)),
        I::LocalSet(end)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(i), I::LocalGet(end), I::I32GeU, I::BrIf(1));
    if !whole {
        emit!(
            f,
            I::LocalGet(buf),
            I::LocalGet(i),
            I::I32Add,
            I::I32Load8U(byte()),
            I::LocalTee(b),
            I::I32Const(b'\n' as i32),
            I::I32Eq,
            I::BrIf(1)
        );
    }
    incr(&mut f, i, 1);
    emit!(f, I::Br(0), I::End, I::End);
    // The bytes before it (with it, for readline) join the line.
    emit!(
        f,
        I::LocalGet(buf),
        I::LocalGet(start),
        I::I32Add,
        I::LocalSet(tmp)
    );
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(start),
        I::I32Sub,
        I::LocalSet(n)
    );
    if keep_newline && !whole {
        // with the newline, if there is one
        emit!(
            f,
            I::LocalGet(n),
            I::LocalGet(i),
            I::LocalGet(end),
            I::I32LtU,
            I::I32Add,
            I::LocalSet(n)
        );
    }
    emit_append(&mut f, ctx, line, line_len, cap, tmp, n, out);
    emit!(
        f,
        I::I32Const(STDIN_POS),
        I::LocalGet(i),
        I::I32Store(w32(0))
    );
    if !whole {
        // Found: past it, and done.
        emit!(
            f,
            I::LocalGet(i),
            I::LocalGet(end),
            I::I32LtU,
            I::If(BlockType::Empty),
            I::I32Const(STDIN_POS),
            I::LocalGet(i),
            I::I32Const(1),
            I::I32Add,
            I::I32Store(w32(0)),
            I::I32Const(1),
            I::LocalSet(got),
            I::Br(2),
            I::End
        );
    }
    // The buffer ran out first: more, or the end.
    emit_refill(&mut f, ctx, keep, n, buf);
    emit!(f, I::BrIf(0), I::End, I::End);
    let _ = b;
    // EOF with nothing read.
    emit!(
        f,
        I::LocalGet(got),
        I::LocalGet(line_len),
        I::I32Or,
        I::I32Eqz,
        I::If(BlockType::Empty)
    );
    if whole {
        emit!(f, I::I32Const(0), I::LocalSet(line_len));
        alloc_string(&mut f, ctx, line_len, out);
        emit!(f, I::LocalGet(out), I::Return);
    } else {
        emit!(f, I::I32Const(0), I::Return);
    }
    emit!(f, I::End);
    alloc_string(&mut f, ctx, line_len, out);
    emit!(
        f,
        I::LocalGet(out),
        I::LocalGet(line),
        I::LocalGet(line_len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(out),
        I::Call(check),
        I::LocalGet(out),
        I::End
    );
    f
}

/// `(s) -> ()`: raise `UnicodeDecodeError` unless the string's bytes are
/// UTF-8, as CPython's text streams decode them strictly. The message is
/// CPython's (`'utf-8' codec can't decode byte 0xff in position 3: invalid
/// start byte`), except that the position counts from the start of the line
/// where CPython counts from the start of its read buffer.
pub(crate) fn build_utf8_check(ctx: &CompilationContext) -> Function {
    let error = ctx.helper(Helper::Utf8Error);
    let mut f = function(&[ValType::I32; 5]);
    let s = 0;
    let (n, i, b, w, k) = (1, 2, 3, 4, 5);
    // reason: 0 invalid start byte, 1 invalid continuation byte, 2 unexpected
    // end of data
    let fail = |f: &mut Function, reason: i32| {
        emit!(
            f,
            I::LocalGet(i),
            I::LocalGet(b),
            I::I32Const(reason),
            I::Call(error),
            I::Return
        );
    };
    emit!(
        f,
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalSet(n)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(n),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(s),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load8U(byte()),
        I::LocalTee(b),
        I::I32Const(0x80),
        I::I32LtU,
        I::If(BlockType::Empty)
    );
    incr(&mut f, i, 1);
    emit!(f, I::Br(1), I::End);
    // The width a lead byte promises; C2..F4 only.
    emit!(
        f,
        I::LocalGet(b),
        I::I32Const(0xc2),
        I::I32LtU,
        I::LocalGet(b),
        I::I32Const(0xf4),
        I::I32GtU,
        I::I32Or,
        I::If(BlockType::Empty)
    );
    fail(&mut f, 0);
    emit!(
        f,
        I::End,
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
        I::I32Const(1),
        I::LocalSet(k)
    );
    // Each continuation byte, or the end of the data first.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(f, I::LocalGet(k), I::LocalGet(w), I::I32GeU, I::BrIf(1));
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(k),
        I::I32Add,
        I::LocalGet(n),
        I::I32GeU,
        I::If(BlockType::Empty)
    );
    fail(&mut f, 2);
    emit!(
        f,
        I::End,
        I::LocalGet(s),
        I::LocalGet(i),
        I::I32Add,
        I::LocalGet(k),
        I::I32Add,
        I::I32Load8U(byte()),
        I::I32Const(0xc0),
        I::I32And,
        I::I32Const(0x80),
        I::I32Ne,
        // the second byte also bars overlong forms, surrogates, and code
        // points past 0x10FFFF
        I::LocalGet(k),
        I::I32Const(1),
        I::I32Eq,
        I::LocalGet(s),
        I::LocalGet(i),
        I::I32Add,
        I::I32Load8U(crate::compiler::runtime::mem(1, 0)),
        I::LocalSet(n),
        I::LocalGet(b),
        I::I32Const(0xe0),
        I::I32Eq,
        I::LocalGet(n),
        I::I32Const(0xa0),
        I::I32LtU,
        I::I32And,
        I::LocalGet(b),
        I::I32Const(0xed),
        I::I32Eq,
        I::LocalGet(n),
        I::I32Const(0x9f),
        I::I32GtU,
        I::I32And,
        I::I32Or,
        I::LocalGet(b),
        I::I32Const(0xf0),
        I::I32Eq,
        I::LocalGet(n),
        I::I32Const(0x90),
        I::I32LtU,
        I::I32And,
        I::I32Or,
        I::LocalGet(b),
        I::I32Const(0xf4),
        I::I32Eq,
        I::LocalGet(n),
        I::I32Const(0x8f),
        I::I32GtU,
        I::I32And,
        I::I32Or,
        I::I32And,
        I::I32Or,
        // restore the length
        I::LocalGet(s),
        I::I32Const(4),
        I::I32Sub,
        I::I32Load(w32(0)),
        I::LocalSet(n),
        I::If(BlockType::Empty)
    );
    fail(&mut f, 1);
    emit!(f, I::End);
    incr(&mut f, k, 1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(f, I::LocalGet(i), I::LocalGet(w), I::I32Add, I::LocalSet(i));
    emit!(f, I::Br(0), I::End, I::End, I::End);
    f
}

/// `(position, byte, reason) -> ()`: leave CPython's `UnicodeDecodeError`
/// pending for the byte at the position.
pub(crate) fn build_utf8_error(ctx: &CompilationContext) -> Function {
    let concat = ctx.helper(Helper::Concat);
    let text = |s: &str| ctx.helper(Helper::ExcText(s.to_string()));
    let mut f = function(&[ValType::I32; 2]);
    let (pos, b, reason, hex, out) = (0, 1, 2, 3, 4);
    // two hex digits of the byte
    let digit = |f: &mut Function, shift: i32| {
        emit!(
            f,
            I::LocalGet(b),
            I::I32Const(shift),
            I::I32ShrU,
            I::I32Const(15),
            I::I32And,
            I::LocalTee(out),
            I::I32Const(b'0' as i32),
            I::I32Add,
            I::LocalGet(out),
            I::I32Const(b'a' as i32 - 10),
            I::I32Add,
            I::LocalGet(out),
            I::I32Const(10),
            I::I32LtU,
            I::Select
        );
    };
    emit!(f, I::Call(text("00")), I::LocalTee(hex));
    digit(&mut f, 4);
    emit!(f, I::I32Store8(byte()), I::LocalGet(hex));
    digit(&mut f, 0);
    emit!(f, I::I32Store8(crate::compiler::runtime::mem(1, 0)));
    emit!(
        f,
        I::Call(text("'utf-8' codec can't decode byte 0x")),
        I::LocalGet(hex),
        I::Call(concat),
        I::Call(text(" in position ")),
        I::Call(concat),
        I::LocalGet(pos),
        I::Call(ctx.helper(Helper::Str(crate::ir::IRType::Int))),
        I::Call(concat),
        I::Call(text(": invalid start byte")),
        I::Call(text(": invalid continuation byte")),
        I::Call(text(": unexpected end of data")),
        I::LocalGet(reason),
        I::I32Const(1),
        I::I32Eq,
        I::Select,
        I::LocalGet(reason),
        I::I32Eqz,
        I::Select,
        I::Call(concat),
        I::Call(ctx.helper(Helper::RaiseText("UnicodeDecodeError".to_string()))),
        I::End
    );
    f
}

/// `() -> list`: `sys.argv`, built from the host's arguments the first time
/// and the same list after.
pub(crate) fn build_argv(ctx: &CompilationContext) -> Function {
    use crate::compiler::context::{
        COLLECTION_CAP, COLLECTION_DATA, COLLECTION_HEADER, COLLECTION_SLOT,
    };
    let sizes = wasi(ctx, Wasi::ArgsSizesGet);
    let get = wasi(ctx, Wasi::ArgsGet);
    let check = ctx.helper(Helper::Utf8Check);
    let mut f = function(&[ValType::I32; 9]);
    let (argc, size, ptrs, bytes, list, i, p, len, out) = (0, 1, 2, 3, 4, 5, 6, 7, 8);
    emit!(
        f,
        I::I32Const(ARGV),
        I::I32Load(w32(0)),
        I::LocalTee(list),
        I::If(BlockType::Empty),
        I::LocalGet(list),
        I::Return,
        I::End,
        I::I32Const(IOV),
        I::I32Const(IOV + 4),
        I::Call(sizes),
        I::Drop,
        I::I32Const(IOV),
        I::I32Load(w32(0)),
        I::LocalSet(argc),
        I::I32Const(IOV + 4),
        I::I32Load(w32(0)),
        I::LocalSet(size),
        I::LocalGet(argc),
        I::I32Const(4),
        I::I32Mul,
        I::Call(ctx.alloc_func_index),
        I::LocalSet(ptrs),
        I::LocalGet(size),
        I::I32Const(1),
        I::I32Add,
        I::Call(ctx.alloc_func_index),
        I::LocalSet(bytes),
        I::LocalGet(ptrs),
        I::LocalGet(bytes),
        I::Call(get),
        I::Drop,
        // The list of the arguments, each a new string.
        I::LocalGet(argc),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::I32Const(COLLECTION_HEADER as i32),
        I::I32Add,
        I::Call(ctx.alloc_func_index),
        I::LocalTee(list),
        I::LocalGet(argc),
        I::I32Store(w32(0)),
        I::LocalGet(list),
        I::LocalGet(argc),
        I::I32Store(w32(COLLECTION_CAP as u64)),
        I::LocalGet(list),
        I::LocalGet(list),
        I::I32Const(COLLECTION_HEADER as i32),
        I::I32Add,
        I::I32Store(w32(COLLECTION_DATA as u64))
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(i),
        I::LocalGet(argc),
        I::I32GeU,
        I::BrIf(1),
        I::LocalGet(ptrs),
        I::LocalGet(i),
        I::I32Const(4),
        I::I32Mul,
        I::I32Add,
        I::I32Load(w32(0)),
        I::LocalTee(p),
        I::LocalSet(len)
    );
    // Each argument is NUL-terminated.
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::LocalGet(len),
        I::I32Load8U(byte()),
        I::I32Eqz,
        I::BrIf(1)
    );
    incr(&mut f, len, 1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(
        f,
        I::LocalGet(len),
        I::LocalGet(p),
        I::I32Sub,
        I::LocalSet(len)
    );
    alloc_string(&mut f, ctx, len, out);
    emit!(
        f,
        I::LocalGet(out),
        I::LocalGet(p),
        I::LocalGet(len),
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(out),
        I::Call(check),
        // CPython decodes the arguments with surrogateescape, which has no
        // counterpart in a UTF-8 string: an argument that is not UTF-8 traps.
        I::GlobalGet(crate::compiler::context::EXC_TYPE_GLOBAL),
        I::If(BlockType::Empty),
        I::Unreachable,
        I::End,
        I::LocalGet(list),
        I::LocalGet(i),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::I32Add,
        I::LocalGet(out),
        I::I32Store(w32(COLLECTION_HEADER as u64))
    );
    incr(&mut f, i, 1);
    emit!(f, I::Br(0), I::End, I::End);
    emit!(
        f,
        I::I32Const(ARGV),
        I::LocalGet(list),
        I::I32Store(w32(0)),
        I::LocalGet(list),
        I::End
    );
    f
}

/// `() -> list`: `sys.stdin.readlines()`, every line left, newlines kept.
pub(crate) fn build_read_lines(ctx: &CompilationContext) -> Function {
    use crate::compiler::context::{
        COLLECTION_CAP, COLLECTION_DATA, COLLECTION_HEADER, COLLECTION_SLOT,
    };
    let read_line = ctx.helper(Helper::ReadLine {
        keep_newline: true,
        whole: false,
    });
    let mut f = function(&[ValType::I32; 6]);
    let (list, count, cap, line, data, grown) = (0, 1, 2, 3, 4, 5);
    emit!(
        f,
        I::I32Const(8),
        I::LocalSet(cap),
        I::I32Const(COLLECTION_HEADER as i32),
        I::Call(ctx.alloc_func_index),
        I::LocalSet(list),
        I::LocalGet(cap),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::Call(ctx.alloc_func_index),
        I::LocalSet(data)
    );
    emit!(f, I::Block(BlockType::Empty), I::Loop(BlockType::Empty));
    emit!(
        f,
        I::Call(read_line),
        I::LocalTee(line),
        I::I32Eqz,
        I::BrIf(1),
        // A pending decode error ends the read; the caller unwinds.
        I::GlobalGet(crate::compiler::context::EXC_TYPE_GLOBAL),
        I::BrIf(1),
        // Full: twice the room, the lines so far copied over.
        I::LocalGet(count),
        I::LocalGet(cap),
        I::I32Eq,
        I::If(BlockType::Empty),
        I::LocalGet(cap),
        I::I32Const(1),
        I::I32Shl,
        I::LocalTee(cap),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::Call(ctx.alloc_func_index),
        I::LocalTee(grown),
        I::LocalGet(data),
        I::LocalGet(count),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::MemoryCopy {
            src_mem: 0,
            dst_mem: 0
        },
        I::LocalGet(grown),
        I::LocalSet(data),
        I::End,
        I::LocalGet(data),
        I::LocalGet(count),
        I::I32Const(COLLECTION_SLOT as i32),
        I::I32Mul,
        I::I32Add,
        I::LocalGet(line),
        I::I32Store(w32(0)),
        I::LocalGet(count),
        I::I32Const(1),
        I::I32Add,
        I::LocalSet(count),
        I::Br(0),
        I::End,
        I::End
    );
    emit!(
        f,
        I::LocalGet(list),
        I::LocalGet(count),
        I::I32Store(w32(0)),
        I::LocalGet(list),
        I::LocalGet(cap),
        I::I32Store(w32(COLLECTION_CAP as u64)),
        I::LocalGet(list),
        I::LocalGet(data),
        I::I32Store(w32(COLLECTION_DATA as u64)),
        I::LocalGet(list),
        I::End
    );
    f
}

/// `() -> ()`: `_start`, the WASI command entry: the `__main__` block, then
/// a non-zero exit if it raised an exception it did not catch.
pub(crate) fn build_start(ctx: &CompilationContext) -> Function {
    let main = ctx
        .get_function_info(crate::ir::MAIN_BLOCK_FN)
        .or_else(|| ctx.get_function_info("main"))
        .expect("_start is exported only with an entry function");
    let mut f = function(&[ValType::I32; 2]);
    // Every function answers one word, None included. Below a call the
    // runtime counts, an exception main does not catch returns here rather
    // than trapping.
    crate::compiler::function::emit_call_depth_step(&mut f, 1);
    emit!(f, I::Call(main.index), I::Drop);
    crate::compiler::function::emit_call_depth_step(&mut f, -1);
    emit!(
        f,
        I::GlobalGet(crate::compiler::context::EXC_TYPE_GLOBAL),
        I::If(BlockType::Empty)
    );
    if ctx.get_class_info("BaseException").is_some() && ctx.wasi.contains_key(&Wasi::FdWrite) {
        let write = ctx.helper(Helper::WriteFd);
        crate::compiler::exceptions::emit_report_uncaught(&mut f, ctx, Some(write));
    }
    if let Some(&exit) = ctx.wasi.get(&Wasi::ProcExit) {
        emit!(f, I::I32Const(1), I::Call(exit));
    }
    emit!(f, I::Unreachable, I::End, I::End);
    f
}

/// `() -> list`: `sys.path`, one list for the program's life. It is empty:
/// a compiled module links every import at compile time, and there is no
/// directory a running program imports from.
pub(crate) fn build_sys_path(ctx: &CompilationContext) -> Function {
    use crate::compiler::context::{COLLECTION_DATA, COLLECTION_HEADER};
    let mut f = function(&[ValType::I32]);
    let list = 0;
    emit!(
        f,
        I::I32Const(SYS_PATH),
        I::I32Load(w32(0)),
        I::LocalTee(list),
        I::If(BlockType::Empty),
        I::LocalGet(list),
        I::Return,
        I::End,
        I::I32Const(COLLECTION_HEADER as i32),
        I::Call(ctx.alloc_func_index),
        I::LocalTee(list),
        I::LocalGet(list),
        I::I32Const(COLLECTION_HEADER as i32),
        I::I32Add,
        I::I32Store(w32(COLLECTION_DATA as u64)),
        I::I32Const(SYS_PATH),
        I::LocalGet(list),
        I::I32Store(w32(0)),
        I::LocalGet(list),
        I::End
    );
    f
}
