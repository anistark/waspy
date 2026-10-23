//! Shared helpers for the integration test suite.
//!
//! These drive the public API exactly as a downstream user would: compile a
//! Python source string (or a bundled `examples/*.py` file) to WASM, validate
//! it, instantiate it with `wasmi`, and call exported functions. A module
//! imports only what its program uses: the four `waspy_host` file functions
//! for `open()`, and WASI preview 1 for `print`, the standard streams,
//! `input`, `sys.argv`, and the `__main__` block. Every instance here is
//! linked against one `Host`: an in-memory filesystem and an in-process WASI
//! that captures standard output and error and serves standard input and
//! arguments, so a test can assert what a program prints. Unused definitions
//! are ignored, and instantiation also exercises the start and data sections.
//!
//! The panicking helpers (`compile`, `instantiate`, `call_i32`, …) mirror the
//! in-crate test helpers in `src/lib.rs` and are for assertions with a known
//! expected result. The `try_*` variants return `Result` so a broad sweep can
//! collect every failure instead of aborting on the first.

// The harness is shared by several test binaries; each uses the subset of
// helpers it needs, so an unused helper in one binary is expected.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wasmi::{Caller, Engine, Extern, Instance, Linker, Store, Value};
use waspy::{
    compile_multiple_python_files_with_options, compile_python_file_with_options,
    compile_python_to_wasm_with_options, CompilerOptions,
};

/// Examples that are not meant to be compiled on their own: they call functions
/// defined in a sibling file and only form a valid module when compiled
/// together (see `examples/multi_file_compiler.rs`). The standalone sweep skips
/// these; `calculator_multi_file_compiles_and_runs` covers them combined.
pub const MULTI_FILE_ONLY: &[&str] = &["calculator.py"];

/// Absolute path to the repository's `examples/` directory.
pub fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// Every `examples/*.py` file, sorted for stable, reproducible test output.
pub fn example_python_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(examples_dir())
        .expect("read examples/ directory")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("py"))
        .collect();
    files.sort();
    files
}

/// Read a bundled example by file name (e.g. `"loop_control.py"`).
pub fn read_example(file_name: &str) -> String {
    std::fs::read_to_string(examples_dir().join(file_name))
        .unwrap_or_else(|e| panic!("read example {file_name}: {e}"))
}

/// Compile a Python source string to an unoptimized WASM module, returning the
/// error as a string on failure. Optimization is off so a failure points at
/// codegen rather than at Binaryen.
pub fn try_compile(source: &str) -> Result<Vec<u8>, String> {
    let options = CompilerOptions {
        optimize: false,
        ..CompilerOptions::default()
    };
    // `{:#}` includes the anyhow cause chain, so a test can assert on the
    // root cause (e.g. "single inheritance") rather than only the outermost
    // "Failed to convert Python AST to IR" context.
    compile_python_to_wasm_with_options(source, &options).map_err(|e| format!("{e:#}"))
}

/// Validate the bytes as a WASM module and instantiate them, returning the
/// error as a string on failure. Validation checks structure, types, and
/// stack balance; instantiation then runs the start function and materializes
/// the data section, against the in-memory `Host`.
pub fn try_instantiate(wasm: &[u8]) -> Result<(), String> {
    let engine = Engine::default();
    let module = wasmi::Module::new(&engine, wasm).map_err(|e| format!("validate: {e}"))?;
    let mut store = Store::new(&engine, Host::default());
    let mut linker = Linker::<Host>::new(&engine);
    link_host(&mut linker);
    linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("instantiate: {e}"))?
        .start(&mut store)
        .map_err(|e| format!("start: {e}"))?;
    Ok(())
}

/// Compile several Python files into one module, returning the error as a
/// string on failure. `sources` is `(file_name, source)` pairs.
///
/// Like `try_compile`, the error is formatted with `{:#}` so the anyhow cause
/// chain comes through: the outer context names the file that failed and the
/// cause carries the located message, and a test wants to assert on both.
pub fn try_compile_multi(sources: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let options = CompilerOptions {
        optimize: false,
        ..CompilerOptions::default()
    };
    compile_multiple_python_files_with_options(sources, &options).map_err(|e| format!("{e:#}"))
}

/// Compile a source string, asserting success.
pub fn compile(source: &str) -> Vec<u8> {
    try_compile(source).expect("compilation")
}

/// Instantiate already-built WASM bytes, asserting validation and start
/// succeed. Returns the live instance and store so a test can call exports.
pub fn instantiate_wasm(wasm: &[u8]) -> (Instance, Store<Host>) {
    instantiate_wasm_with(wasm, Host::default())
}

/// `instantiate_wasm` with a host of the test's choosing (its standard
/// input, its arguments, files already in place).
pub fn instantiate_wasm_with(wasm: &[u8], host: Host) -> (Instance, Store<Host>) {
    let engine = Engine::default();
    let module = wasmi::Module::new(&engine, wasm).expect("valid wasm module");
    let mut store = Store::new(&engine, host);
    let mut linker = Linker::<Host>::new(&engine);
    link_host(&mut linker);
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiation")
        .start(&mut store)
        .expect("start");
    (instance, store)
}

/// Compile + instantiate a source string, returning the live instance and
/// store so a test can call exported functions.
pub fn instantiate(source: &str) -> (Instance, Store<Host>) {
    instantiate_wasm(&compile(source))
}

/// Call an exported zero-argument function returning `i32`.
pub fn call_i32(source: &str, func: &str) -> i32 {
    let (instance, mut store) = instantiate(source);
    instance
        .get_typed_func::<(), i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(&mut store, ())
        .expect("call")
}

/// Whether calling an exported zero-argument `i32` function traps, which is
/// how a fault CPython raises for, and nothing here can catch, surfaces.
pub fn call_i32_traps(source: &str, func: &str) -> bool {
    let (instance, mut store) = instantiate(source);
    instance
        .get_typed_func::<(), i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(&mut store, ())
        .is_err()
}

/// Strip the indentation every non-blank line of `source` shares, so a test
/// can write its Python indented inside a raw string.
pub fn dedent(source: &str) -> String {
    let lines: Vec<&str> = source.trim_matches('\n').lines().collect();
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out: String = lines
        .iter()
        .map(|l| l.get(indent..).unwrap_or("").trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    out.push('\n');
    out
}

/// Call an exported zero-argument function returning `f64`. Used to assert that
/// float values round-trip with full f64 precision (an f32 slot would lose the
/// low bits and fail an exact-equality check). wasmi 0.31's typed API does not
/// bind a bare `f64` result, so this uses the untyped call path.
pub fn call_f64(source: &str, func: &str) -> f64 {
    let (instance, mut store) = instantiate(source);
    let f = instance
        .get_func(&store, func)
        .unwrap_or_else(|| panic!("exported fn `{func}`"));
    let mut results = [Value::F64(0.0.into())];
    f.call(&mut store, &[], &mut results).expect("call");
    match results[0] {
        Value::F64(v) => f64::from(v),
        ref other => panic!("expected f64 result, got {other:?}"),
    }
}

/// Call an exported function taking two `i32` arguments and returning `i32`.
pub fn call_i32_2(source: &str, func: &str, a: i32, b: i32) -> i32 {
    let (instance, mut store) = instantiate(source);
    instance
        .get_typed_func::<(i32, i32), i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(&mut store, (a, b))
        .expect("call")
}

/// Call an exported function taking one `i32` argument and returning `i32`.
pub fn call_i32_1(source: &str, func: &str, a: i32) -> i32 {
    let (instance, mut store) = instantiate(source);
    instance
        .get_typed_func::<i32, i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(&mut store, a)
        .expect("call")
}

/// Call an exported function through the untyped API, for signatures the
/// typed helpers don't cover (f64 arguments, mixed i32/f64 parameter lists).
/// Returns the single result value.
pub fn call_untyped(source: &str, func: &str, args: &[Value]) -> Value {
    let (instance, mut store) = instantiate(source);
    let f = instance
        .get_func(&store, func)
        .unwrap_or_else(|| panic!("exported fn `{func}`"));
    let result_len = f.ty(&store).results().len();
    assert_eq!(result_len, 1, "`{func}` must return exactly one value");
    let mut results = [Value::I32(0)];
    f.call(&mut store, args, &mut results).expect("call");
    results[0].clone()
}

/// `call_untyped` asserting an `f64` result.
pub fn call_untyped_f64(source: &str, func: &str, args: &[Value]) -> f64 {
    match call_untyped(source, func, args) {
        Value::F64(v) => f64::from(v),
        other => panic!("expected f64 result, got {other:?}"),
    }
}

/// `call_untyped` asserting an `i32` result.
pub fn call_untyped_i32(source: &str, func: &str, args: &[Value]) -> i32 {
    match call_untyped(source, func, args) {
        Value::I32(v) => v,
        other => panic!("expected i32 result, got {other:?}"),
    }
}

/// Read a `str` result out of the module's memory. A compiled function hands
/// back the blob's offset, and every blob carries its length in the four bytes
/// before it, whether it is a literal in the data segment or a string built at
/// runtime (`STRING_LEN_PREFIX` in `src/ir/types.rs`). Reading the length
/// rather than scanning to a NUL means a wrong length fails the assertion
/// instead of being papered over.
pub fn read_str(instance: &Instance, store: &Store<Host>, offset: i32) -> String {
    let memory = instance
        .get_memory(store, "memory")
        .expect("exported `memory`");
    let data = memory.data(store);
    let start = offset as usize;
    assert!(start >= 4, "string offset {offset} has no length prefix");
    let len =
        u32::from_le_bytes(data[start - 4..start].try_into().expect("length prefix")) as usize;
    assert!(
        start + len <= data.len(),
        "string at {offset} with length {len} runs past memory"
    );
    String::from_utf8(data[start..start + len].to_vec()).expect("utf-8 string")
}

/// Call an exported zero-argument function returning `str`.
pub fn call_str(source: &str, func: &str) -> String {
    let (instance, mut store) = instantiate(source);
    let offset = instance
        .get_typed_func::<(), i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported str fn `{func}`"))
        .call(&mut store, ())
        .expect("call");
    read_str(&instance, &store, offset)
}

/// Call an exported function taking one `i32` argument and returning `str`.
pub fn call_str_1(source: &str, func: &str, a: i32) -> String {
    let (instance, mut store) = instantiate(source);
    let offset = instance
        .get_typed_func::<i32, i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported str fn `{func}`"))
        .call(&mut store, a)
        .expect("call");
    read_str(&instance, &store, offset)
}

/// Call an exported function taking two `i32` arguments and returning `str`.
pub fn call_str_2(source: &str, func: &str, a: i32, b: i32) -> String {
    let (instance, mut store) = instantiate(source);
    let offset = instance
        .get_typed_func::<(i32, i32), i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported str fn `{func}`"))
        .call(&mut store, (a, b))
        .expect("call");
    read_str(&instance, &store, offset)
}

/// Compile an entry `.py` file from disk (unoptimized), resolving its user
/// module imports like `waspy::compile_python_file` does, and instantiate it.
pub fn instantiate_file(entry: &Path) -> (Instance, Store<Host>) {
    let options = CompilerOptions {
        optimize: false,
        ..CompilerOptions::default()
    };
    let wasm = compile_python_file_with_options(entry, &options).expect("file compilation");
    instantiate_wasm(&wasm)
}

/// Call an exported zero-argument `i32` function on an already-instantiated
/// module (so several calls can share one live instance and memory).
pub fn call_instance_i32(instance: &Instance, store: &mut Store<Host>, func: &str) -> i32 {
    instance
        .get_typed_func::<(), i32>(&mut *store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(store, ())
        .expect("call")
}

/// The host every test instance runs against: an in-memory filesystem
/// backing the `waspy_host` imports (mirroring the reference host in
/// `src/lib.rs`'s file I/O tests, so files persist across calls within one
/// store), and an in-process WASI preview 1 that captures what the program
/// writes to standard output and error and serves it standard input and
/// arguments.
#[derive(Default)]
pub struct Host {
    pub files: HashMap<String, Vec<u8>>,
    /// fd -> (file name, read cursor, readable, writable); a closed fd is None.
    handles: Vec<Option<(String, usize, bool, bool)>>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdin: Vec<u8>,
    stdin_pos: usize,
    /// `sys.argv`, the program's name first.
    pub args: Vec<String>,
    /// The environment, as `KEY=value` strings.
    pub env: Vec<String>,
}

/// The name file I/O tests know the host by.
pub type HostFs = Host;

impl Host {
    /// A host whose standard input is `stdin` and whose arguments, after the
    /// program name, are `args`.
    pub fn with_input(stdin: &str, args: &[&str]) -> Host {
        let mut all = vec!["main.py".to_string()];
        all.extend(args.iter().map(|a| a.to_string()));
        Host {
            stdin: stdin.as_bytes().to_vec(),
            args: all,
            ..Host::default()
        }
    }
}

/// The WASI host's one preopened directory, and its first file descriptor.
const PREOPEN_FD: i32 = 3;
const FIRST_FILE_FD: i32 = 4;

const FLAG_READ: i32 = 1;
const FLAG_WRITE: i32 = 2;
const FLAG_APPEND: i32 = 4;
const FLAG_UPDATE: i32 = 16;

fn memory_of(caller: &mut Caller<'_, Host>) -> wasmi::Memory {
    caller
        .get_export("memory")
        .and_then(Extern::into_memory)
        .expect("exported memory")
}

fn word(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().expect("a word"))
}

fn put_word(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Define the host's imports: `waspy_host` and `wasi_snapshot_preview1`.
pub fn link_host(linker: &mut Linker<Host>) {
    link_host_fs(linker);
    let wasi = "wasi_snapshot_preview1";
    linker
        .func_wrap(
            wasi,
            "fd_write",
            |mut caller: Caller<'_, Host>, fd: i32, iovs: i32, n: i32, done: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let mut bytes = Vec::new();
                for k in 0..n as usize {
                    let base = word(data, iovs as usize + 8 * k) as usize;
                    let len = word(data, iovs as usize + 8 * k + 4) as usize;
                    bytes.extend_from_slice(&data[base..base + len]);
                }
                match fd {
                    1 => host.stdout.extend_from_slice(&bytes),
                    2 => host.stderr.extend_from_slice(&bytes),
                    fd if fd >= FIRST_FILE_FD => {
                        let Some(Some((name, _, _, true))) =
                            host.handles.get((fd - FIRST_FILE_FD) as usize)
                        else {
                            return 8;
                        };
                        let name = name.clone();
                        host.files
                            .entry(name)
                            .or_default()
                            .extend_from_slice(&bytes);
                    }
                    _ => return 8, // EBADF
                }
                put_word(data, done as usize, bytes.len() as u32);
                0
            },
        )
        .expect("link fd_write");
    linker
        .func_wrap(
            wasi,
            "environ_sizes_get",
            |mut caller: Caller<'_, Host>, count: i32, size: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let total: usize = host.env.iter().map(|e| e.len() + 1).sum();
                put_word(data, count as usize, host.env.len() as u32);
                put_word(data, size as usize, total as u32);
                0
            },
        )
        .expect("link environ_sizes_get");
    linker
        .func_wrap(
            wasi,
            "environ_get",
            |mut caller: Caller<'_, Host>, ptrs: i32, buf: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let mut at = buf as usize;
                for (k, entry) in host.env.iter().enumerate() {
                    put_word(data, ptrs as usize + 4 * k, at as u32);
                    data[at..at + entry.len()].copy_from_slice(entry.as_bytes());
                    data[at + entry.len()] = 0;
                    at += entry.len() + 1;
                }
                0
            },
        )
        .expect("link environ_get");
    linker
        .func_wrap(
            wasi,
            "path_filestat_get",
            |mut caller: Caller<'_, Host>,
             dir: i32,
             _flags: i32,
             path: i32,
             len: i32,
             out: i32|
             -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                if dir != PREOPEN_FD {
                    return 8; // EBADF
                }
                let name = String::from_utf8_lossy(&data[path as usize..(path + len) as usize])
                    .trim_start_matches("./")
                    .to_string();
                // The preopen is a directory, a file is a regular file, and
                // a prefix of a file's name is a directory holding it.
                let filetype = if name == "." || name.is_empty() {
                    3
                } else if host.files.contains_key(&name) {
                    4
                } else if host
                    .files
                    .keys()
                    .any(|f| f.starts_with(&format!("{}/", name.trim_end_matches('/'))))
                {
                    3
                } else {
                    return 44; // ENOENT
                };
                data[out as usize..out as usize + 64].fill(0);
                data[out as usize + 16] = filetype;
                0
            },
        )
        .expect("link path_filestat_get");
    linker
        .func_wrap(
            wasi,
            "clock_time_get",
            |mut caller: Caller<'_, Host>, _clock: i32, _precision: i64, out: i32| -> i32 {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64);
                let memory = memory_of(&mut caller);
                let data = memory.data_mut(&mut caller);
                data[out as usize..out as usize + 8].copy_from_slice(&now.to_le_bytes());
                0
            },
        )
        .expect("link clock_time_get");
    linker
        .func_wrap(
            wasi,
            "random_get",
            |mut caller: Caller<'_, Host>, buf: i32, len: i32| -> i32 {
                // Entropy, as far as a test can tell: a fresh xorshift stream
                // per call, seeded from the clock.
                let memory = memory_of(&mut caller);
                let data = memory.data_mut(&mut caller);
                let mut x = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64)
                    | 1;
                for byte in &mut data[buf as usize..(buf + len) as usize] {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    *byte = x as u8;
                }
                0
            },
        )
        .expect("link random_get");
    linker
        .func_wrap(
            wasi,
            "fd_read",
            |mut caller: Caller<'_, Host>, fd: i32, iovs: i32, n: i32, done: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                if fd >= FIRST_FILE_FD {
                    let Some(Some((name, pos, true, _))) =
                        host.handles.get_mut((fd - FIRST_FILE_FD) as usize)
                    else {
                        return 8;
                    };
                    let contents = host.files.get(name.as_str()).cloned().unwrap_or_default();
                    let mut total = 0;
                    for k in 0..n as usize {
                        let base = word(data, iovs as usize + 8 * k) as usize;
                        let len = word(data, iovs as usize + 8 * k + 4) as usize;
                        let take = contents.len().saturating_sub(*pos).min(len);
                        data[base..base + take].copy_from_slice(&contents[*pos..*pos + take]);
                        *pos += take;
                        total += take;
                    }
                    put_word(data, done as usize, total as u32);
                    return 0;
                }
                if fd != 0 {
                    return 8;
                }
                let mut total = 0;
                for k in 0..n as usize {
                    let base = word(data, iovs as usize + 8 * k) as usize;
                    let len = word(data, iovs as usize + 8 * k + 4) as usize;
                    let left = &host.stdin[host.stdin_pos..];
                    let take = left.len().min(len);
                    data[base..base + take].copy_from_slice(&left[..take]);
                    host.stdin_pos += take;
                    total += take;
                }
                put_word(data, done as usize, total as u32);
                0
            },
        )
        .expect("link fd_read");
    linker
        .func_wrap(
            wasi,
            "args_sizes_get",
            |mut caller: Caller<'_, Host>, argc: i32, size: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let bytes: usize = host.args.iter().map(|a| a.len() + 1).sum();
                put_word(data, argc as usize, host.args.len() as u32);
                put_word(data, size as usize, bytes as u32);
                0
            },
        )
        .expect("link args_sizes_get");
    linker
        .func_wrap(
            wasi,
            "args_get",
            |mut caller: Caller<'_, Host>, argv: i32, buf: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let mut at = buf as usize;
                for (k, arg) in host.args.iter().enumerate() {
                    put_word(data, argv as usize + 4 * k, at as u32);
                    data[at..at + arg.len()].copy_from_slice(arg.as_bytes());
                    data[at + arg.len()] = 0;
                    at += arg.len() + 1;
                }
                0
            },
        )
        .expect("link args_get");
    // Files: one preopened directory, `.`, at fd 3, and each open file from
    // fd 4 up, over the same in-memory map the `waspy_host` imports use.
    linker
        .func_wrap(
            wasi,
            "fd_prestat_get",
            |mut caller: Caller<'_, Host>, fd: i32, buf: i32| -> i32 {
                if fd != PREOPEN_FD {
                    return 8;
                }
                let memory = memory_of(&mut caller);
                let data = memory.data_mut(&mut caller);
                data[buf as usize] = 0; // a directory
                put_word(data, buf as usize + 4, 1);
                0
            },
        )
        .expect("link fd_prestat_get");
    linker
        .func_wrap(
            wasi,
            "fd_prestat_dir_name",
            |mut caller: Caller<'_, Host>, fd: i32, path: i32, _len: i32| -> i32 {
                if fd != PREOPEN_FD {
                    return 8;
                }
                let memory = memory_of(&mut caller);
                memory.data_mut(&mut caller)[path as usize] = b'.';
                0
            },
        )
        .expect("link fd_prestat_dir_name");
    linker
        .func_wrap(
            wasi,
            "path_open",
            |mut caller: Caller<'_, Host>,
             dir: i32,
             _lookup: i32,
             path: i32,
             len: i32,
             oflags: i32,
             _rights: i64,
             _inheriting: i64,
             fdflags: i32,
             fd_out: i32|
             -> i32 {
                if dir != PREOPEN_FD {
                    return 8;
                }
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let start = path as usize;
                let name = String::from_utf8_lossy(&data[start..start + len as usize]).to_string();
                let (create, truncate, append) =
                    (oflags & 1 != 0, oflags & 8 != 0, fdflags & 1 != 0);
                if truncate {
                    host.files.insert(name.clone(), Vec::new());
                } else if create {
                    host.files.entry(name.clone()).or_default();
                } else if !host.files.contains_key(&name) {
                    return 44; // ENOENT
                }
                let writable = create || truncate || append;
                host.handles.push(Some((name, 0, !writable, writable)));
                let fd = FIRST_FILE_FD + host.handles.len() as i32 - 1;
                put_word(data, fd_out as usize, fd as u32);
                0
            },
        )
        .expect("link path_open");
    linker
        .func_wrap(
            wasi,
            "fd_close",
            |mut caller: Caller<'_, Host>, fd: i32| -> i32 {
                match caller
                    .data_mut()
                    .handles
                    .get_mut((fd - FIRST_FILE_FD).max(-1) as usize)
                {
                    Some(handle) if fd >= FIRST_FILE_FD => {
                        *handle = None;
                        0
                    }
                    _ => 8,
                }
            },
        )
        .expect("link fd_close");
    linker
        .func_wrap(
            wasi,
            "proc_exit",
            |code: i32| -> Result<(), wasmi::core::Trap> { Err(wasmi::core::Trap::i32_exit(code)) },
        )
        .expect("link proc_exit");
}

/// What running a program's `__main__` block (`_start`) did.
#[derive(Debug, PartialEq)]
pub struct Run {
    pub stdout: String,
    pub stderr: String,
    /// The status it exited with: 0 when `_start` returned.
    pub exit: i32,
}

/// Compile a program, give it `stdin` and `args`, and run its `__main__`
/// block, as `wasmtime run` would.
pub fn run_main(source: &str, stdin: &str, args: &[&str]) -> Run {
    let wasm = compile(source);
    let (instance, mut store) = instantiate_wasm_with(&wasm, Host::with_input(stdin, args));
    let start = instance
        .get_typed_func::<(), ()>(&store, "_start")
        .expect("an exported _start");
    let exit = match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(trap) => trap
            .i32_exit_status()
            .unwrap_or_else(|| panic!("_start trapped: {trap}")),
    };
    let host = store.data();
    Run {
        stdout: String::from_utf8(host.stdout.clone()).expect("utf-8 stdout"),
        stderr: String::from_utf8(host.stderr.clone()).expect("utf-8 stderr"),
        exit,
    }
}

/// Call an exported zero-argument `i32` function and return its answer with
/// what it printed to standard output.
pub fn call_i32_printing(source: &str, func: &str) -> (i32, String) {
    let (instance, mut store) = instantiate(source);
    let value = instance
        .get_typed_func::<(), i32>(&store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(&mut store, ())
        .expect("call");
    let out = String::from_utf8(store.data().stdout.clone()).expect("utf-8 stdout");
    (value, out)
}

/// Compile (unoptimized) and instantiate with the in-memory host filesystem.
pub fn instantiate_with_host_fs(source: &str) -> (Instance, Store<Host>) {
    let wasm = try_compile(source).expect("compilation");
    instantiate_wasm(&wasm)
}

fn link_host_fs(linker: &mut Linker<Host>) {
    linker
        .func_wrap(
            "waspy_host",
            "open",
            |mut caller: Caller<'_, Host>, path_ptr: i32, path_len: i32, flags: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, fs) = memory.data_and_store_mut(&mut caller);
                let start = path_ptr as usize;
                let name =
                    String::from_utf8_lossy(&data[start..start + path_len as usize]).to_string();
                let readable = flags & (FLAG_READ | FLAG_UPDATE) != 0;
                let writable = flags & (FLAG_WRITE | FLAG_APPEND | FLAG_UPDATE) != 0;
                if flags & FLAG_WRITE != 0 {
                    fs.files.insert(name.clone(), Vec::new()); // truncate/create
                } else if flags & FLAG_APPEND != 0 {
                    fs.files.entry(name.clone()).or_default();
                } else if !fs.files.contains_key(&name) {
                    return -1; // read of a missing file
                }
                fs.handles.push(Some((name, 0, readable, writable)));
                (fs.handles.len() - 1) as i32
            },
        )
        .expect("link open");
    linker
        .func_wrap(
            "waspy_host",
            "read",
            |mut caller: Caller<'_, Host>, fd: i32, buf: i32, len: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, fs) = memory.data_and_store_mut(&mut caller);
                let Some(Some((name, pos, readable, _))) = fs.handles.get_mut(fd as usize) else {
                    return -1;
                };
                if !*readable {
                    return -1;
                }
                let contents = fs
                    .files
                    .get(name.as_str())
                    .map(|c| c.as_slice())
                    .unwrap_or(&[]);
                let n = (contents.len().saturating_sub(*pos)).min(len as usize);
                let start = buf as usize;
                data[start..start + n].copy_from_slice(&contents[*pos..*pos + n]);
                *pos += n;
                n as i32
            },
        )
        .expect("link read");
    linker
        .func_wrap(
            "waspy_host",
            "write",
            |mut caller: Caller<'_, Host>, fd: i32, buf: i32, len: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let (data, fs) = memory.data_and_store_mut(&mut caller);
                let Some(Some((name, _, _, writable))) = fs.handles.get(fd as usize) else {
                    return -1;
                };
                if !*writable {
                    return -1;
                }
                let start = buf as usize;
                let bytes = data[start..start + len as usize].to_vec();
                fs.files
                    .get_mut(name.as_str())
                    .expect("open file")
                    .extend_from_slice(&bytes);
                len
            },
        )
        .expect("link write");
    linker
        .func_wrap(
            "waspy_host",
            "close",
            |mut caller: Caller<'_, Host>, fd: i32| -> i32 {
                if let Some(handle) = caller.data_mut().handles.get_mut(fd as usize) {
                    *handle = None;
                    0
                } else {
                    -1
                }
            },
        )
        .expect("link close");
}

/// Call an exported zero-argument `i32` function on a host-fs instance.
pub fn call_host_fs_i32(instance: &Instance, store: &mut Store<Host>, func: &str) -> i32 {
    instance
        .get_typed_func::<(), i32>(&mut *store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(store, ())
        .expect("call")
}
