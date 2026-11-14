//! Shared helpers for the integration test suite.
//!
//! These drive the public API exactly as a downstream user would: compile a
//! Python source string (or a bundled `examples/*.py` file) to WASM, validate
//! it, instantiate it with `wasmi`, and call exported functions. A module
//! imports only what its program uses from WASI preview 1: `print`, the
//! standard streams, `input`, `sys.argv`, `open()`, and the `__main__` block.
//! Every instance here is linked against one `Host`: an in-memory filesystem
//! and an in-process WASI that captures standard output and error and serves
//! standard input and arguments, so a test can assert what a program prints.
//! Unused definitions are ignored, and instantiation also exercises the start
//! and data sections.
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

/// The host every test instance runs against: an in-process WASI preview 1
/// over an in-memory filesystem (files persist across calls within one
/// store) that captures what the program writes to standard output and error
/// and serves it standard input and arguments.
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
    /// The time `clock_time_get` answers, in nanoseconds since the epoch, or
    /// the real time when None.
    pub clock_ns: Option<u64>,
    /// Each file's modification time, in nanoseconds: a file written to (or
    /// created) is stamped with the clock's time, and a rename keeps it.
    pub mtimes: HashMap<String, u64>,
    /// The monotonic clock, in nanoseconds: it stands still but for what
    /// `poll_oneoff` waits, which it moves on by at once, so a program's
    /// timers fall due in order without the test waiting for them.
    pub monotonic_ns: u64,
    /// The JavaScript a program using the bridge (`js`, `pyodide`) reaches.
    pub js: JsWorld,
}

/// The name file I/O tests know the host by.
pub type HostFs = Host;

impl Host {
    /// The clock's time, in nanoseconds since the epoch.
    pub fn now(&self) -> u64 {
        self.clock_ns.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64)
        })
    }

    /// The entries of the directory `dir` (`.` for the preopen): `.` and
    /// `..`, then each name in it, sorted, a directory being a prefix of a
    /// file's name. A directory is `3`, a file `4`, as WASI's filetypes are.
    fn entries(&self, dir: &str) -> Vec<(String, u8)> {
        let prefix = match dir.trim_start_matches("./").trim_end_matches('/') {
            "" | "." => String::new(),
            d => format!("{d}/"),
        };
        let mut names = std::collections::BTreeMap::new();
        for file in self.files.keys() {
            if let Some(rest) = file.trim_start_matches("./").strip_prefix(&prefix) {
                match rest.split_once('/') {
                    Some((sub, _)) => names.insert(sub.to_string(), 3u8),
                    None => names.insert(rest.to_string(), 4u8),
                };
            }
        }
        let mut out = vec![(".".to_string(), 3), ("..".to_string(), 3)];
        out.extend(names);
        out
    }

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

/// Define the host's imports, `wasi_snapshot_preview1` and the JavaScript
/// bridge's `waspy_js`.
pub fn link_host(linker: &mut Linker<Host>) {
    link_js(linker);
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
                            .entry(name.clone())
                            .or_default()
                            .extend_from_slice(&bytes);
                        let now = host.now();
                        host.mtimes.insert(name, now);
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
                let size = host.files.get(&name).map_or(0, Vec::len) as u64;
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
                let mtime = host.mtimes.get(&name).copied().unwrap_or(0);
                data[out as usize..out as usize + 64].fill(0);
                data[out as usize + 16] = filetype;
                data[out as usize + 32..out as usize + 40].copy_from_slice(&size.to_le_bytes());
                data[out as usize + 48..out as usize + 56].copy_from_slice(&mtime.to_le_bytes());
                0
            },
        )
        .expect("link path_filestat_get");
    linker
        .func_wrap(
            wasi,
            "path_unlink_file",
            |mut caller: Caller<'_, Host>, dir: i32, path: i32, len: i32| -> i32 {
                if dir != PREOPEN_FD {
                    return 8; // EBADF
                }
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let name = String::from_utf8_lossy(&data[path as usize..(path + len) as usize])
                    .trim_start_matches("./")
                    .to_string();
                host.mtimes.remove(&name);
                match host.files.remove(&name) {
                    Some(_) => 0,
                    None => 44, // ENOENT
                }
            },
        )
        .expect("link path_unlink_file");
    linker
        .func_wrap(
            wasi,
            "path_rename",
            |mut caller: Caller<'_, Host>,
             dir: i32,
             old: i32,
             old_len: i32,
             new_dir: i32,
             new: i32,
             new_len: i32|
             -> i32 {
                if dir != PREOPEN_FD || new_dir != PREOPEN_FD {
                    return 8; // EBADF
                }
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let text = |at: i32, n: i32| {
                    String::from_utf8_lossy(&data[at as usize..(at + n) as usize])
                        .trim_start_matches("./")
                        .to_string()
                };
                let (from, to) = (text(old, old_len), text(new, new_len));
                match host.files.remove(&from) {
                    Some(bytes) => {
                        if let Some(mtime) = host.mtimes.remove(&from) {
                            host.mtimes.insert(to.clone(), mtime);
                        }
                        host.files.insert(to, bytes);
                        0
                    }
                    None => 44, // ENOENT
                }
            },
        )
        .expect("link path_rename");
    linker
        .func_wrap(
            wasi,
            "clock_time_get",
            |mut caller: Caller<'_, Host>, clock: i32, _precision: i64, out: i32| -> i32 {
                let now = if clock == 1 {
                    caller.data().monotonic_ns
                } else {
                    caller.data().now()
                };
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
            "poll_oneoff",
            |mut caller: Caller<'_, Host>, subs: i32, events: i32, n: i32, nout: i32| -> i32 {
                let memory = memory_of(&mut caller);
                let mut waits = Vec::new();
                {
                    let data = memory.data(&caller);
                    for k in 0..n as usize {
                        let at = subs as usize + k * 48;
                        let field = |o: usize| {
                            u64::from_le_bytes(data[at + o..at + o + 8].try_into().unwrap())
                        };
                        assert_eq!(data[at + 8], 0, "poll_oneoff: only clock subscriptions");
                        let flags = u16::from_le_bytes([data[at + 40], data[at + 41]]);
                        waits.push((field(0), field(24), flags & 1 == 1));
                    }
                }
                let (userdata, timeout, absolute) = waits[0];
                assert!(
                    timeout < 1 << 50,
                    "the program waits forever (CPython's would hang here)"
                );
                let host = caller.data_mut();
                host.monotonic_ns = if absolute {
                    host.monotonic_ns.max(timeout)
                } else {
                    host.monotonic_ns + timeout
                };
                let data = memory.data_mut(&mut caller);
                let at = events as usize;
                data[at..at + 32].fill(0);
                data[at..at + 8].copy_from_slice(&userdata.to_le_bytes());
                put_word(data, nout as usize, 1);
                0
            },
        )
        .expect("link poll_oneoff");
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
    // fd 4 up, over the in-memory map `Host::files`.
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
                // A directory, for `fd_readdir`: neither readable nor writable.
                if oflags & 2 != 0 {
                    if host.files.contains_key(&name) {
                        return 54; // ENOTDIR
                    }
                    if host.entries(&name).len() == 2 && !matches!(name.as_str(), "." | "") {
                        return 44; // ENOENT
                    }
                    host.handles.push(Some((name, 0, false, false)));
                    let fd = FIRST_FILE_FD + host.handles.len() as i32 - 1;
                    put_word(data, fd_out as usize, fd as u32);
                    return 0;
                }
                let (create, truncate, append) =
                    (oflags & 1 != 0, oflags & 8 != 0, fdflags & 1 != 0);
                let now = host.now();
                if truncate {
                    host.files.insert(name.clone(), Vec::new());
                    host.mtimes.insert(name.clone(), now);
                } else if create {
                    if !host.files.contains_key(&name) {
                        host.mtimes.insert(name.clone(), now);
                    }
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
            "fd_readdir",
            |mut caller: Caller<'_, Host>,
             fd: i32,
             buf: i32,
             buf_len: i32,
             cookie: i64,
             used: i32|
             -> i32 {
                let memory = memory_of(&mut caller);
                let (data, host) = memory.data_and_store_mut(&mut caller);
                let Some(Some((dir, _, false, false))) =
                    host.handles.get((fd - FIRST_FILE_FD).max(0) as usize)
                else {
                    return 8; // EBADF
                };
                let mut out = Vec::new();
                for (i, (name, kind)) in host.entries(dir).iter().enumerate().skip(cookie as usize)
                {
                    out.extend_from_slice(&(i as u64 + 1).to_le_bytes());
                    out.extend_from_slice(&0u64.to_le_bytes());
                    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
                    out.extend_from_slice(&[*kind, 0, 0, 0]);
                    out.extend_from_slice(name.as_bytes());
                }
                // As much as fits: the last entry may be cut short.
                let n = out.len().min(buf_len as usize);
                data[buf as usize..buf as usize + n].copy_from_slice(&out[..n]);
                put_word(data, used as usize, n as u32);
                0
            },
        )
        .expect("link fd_readdir");
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

/// Compile a program and run its `__main__` block on `host` (its files, the
/// clock, the environment), answering what it printed and the host as the
/// program left it.
pub fn run_main_on(source: &str, host: Host) -> (Run, Host) {
    let wasm = compile(source);
    let (instance, mut store) = instantiate_wasm_with(&wasm, host);
    let start = instance
        .get_typed_func::<(), ()>(&store, "_start")
        .expect("an exported _start");
    let exit = match start.call(&mut store, ()) {
        Ok(()) => 0,
        Err(trap) => trap
            .i32_exit_status()
            .unwrap_or_else(|| panic!("_start trapped: {trap}")),
    };
    let host = store.into_data();
    let run = Run {
        stdout: String::from_utf8(host.stdout.clone()).expect("utf-8 stdout"),
        stderr: String::from_utf8(host.stderr.clone()).expect("utf-8 stderr"),
        exit,
    };
    (run, host)
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

/// Call an exported zero-argument `i32` function on a host-fs instance.
pub fn call_host_fs_i32(instance: &Instance, store: &mut Store<Host>, func: &str) -> i32 {
    instance
        .get_typed_func::<(), i32>(&mut *store, func)
        .unwrap_or_else(|_| panic!("exported i32 fn `{func}`"))
        .call(store, ())
        .expect("call")
}

// ---------------------------------------------------------------------------
// The JavaScript bridge's host: a small JavaScript, in process
// ---------------------------------------------------------------------------

use std::cell::RefCell;
use std::rc::Rc;

/// A JavaScript function: `this`, and the arguments.
pub type JsFunction = dyn Fn(&mut JsWorld, &Js, &[Js]) -> Result<Js, Js>;

/// A JavaScript value, as much of one as the tests need.
#[derive(Clone)]
pub enum Js {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Rc<RefCell<Vec<Js>>>),
    /// An object: its properties in insertion order, and the function it is
    /// when it can be called (and constructed: `Object`, `Array`).
    Obj(Rc<RefCell<Vec<(String, Js)>>>, Option<Rc<JsFunction>>),
    /// An iterator over a snapshot of an array.
    Iter(Rc<RefCell<(Vec<Js>, usize)>>),
    /// A thrown error, as `String(error)` shows it.
    Error(String),
    /// A separately compiled module, by the name the host loaded it under.
    Module(String),
    /// A promise: settled (rejected or not, with the value), or pending.
    Promise(Rc<RefCell<Option<(bool, Js)>>>),
}

impl std::fmt::Debug for Js {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Js({})", self.text())
    }
}

impl Js {
    pub fn object(props: Vec<(&str, Js)>) -> Js {
        Js::Obj(
            Rc::new(RefCell::new(
                props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            )),
            None,
        )
    }
    pub fn function(f: impl Fn(&mut JsWorld, &Js, &[Js]) -> Result<Js, Js> + 'static) -> Js {
        Js::Obj(Rc::new(RefCell::new(Vec::new())), Some(Rc::new(f)))
    }
    fn get(&self, name: &str) -> Option<Js> {
        match self {
            Js::Obj(props, _) => props
                .borrow()
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone()),
            Js::Arr(items) if name == "length" => Some(Js::Num(items.borrow().len() as f64)),
            Js::Str(s) if name == "length" => Some(Js::Num(s.encode_utf16().count() as f64)),
            _ => None,
        }
    }
    fn set(&self, name: &str, value: Js) -> Result<(), Js> {
        match self {
            Js::Obj(props, _) => {
                let mut props = props.borrow_mut();
                match props.iter_mut().find(|(k, _)| k == name) {
                    Some(slot) => slot.1 = value,
                    None => props.push((name.to_string(), value)),
                }
                Ok(())
            }
            _ => Err(Js::Error(format!(
                "TypeError: Cannot create property '{name}' on {}",
                self.text()
            ))),
        }
    }
    fn number_text(x: f64) -> String {
        if x.is_nan() {
            "NaN".to_string()
        } else if x.is_infinite() {
            if x > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
        } else if x == x.trunc() && x.abs() < 1e21 {
            format!("{}", x as i64)
        } else {
            format!("{x}")
        }
    }
    /// `String(value)`.
    pub fn text(&self) -> String {
        match self {
            Js::Undefined => "undefined".to_string(),
            Js::Null => "null".to_string(),
            Js::Bool(b) => b.to_string(),
            Js::Num(x) => Js::number_text(*x),
            Js::Str(s) => s.clone(),
            Js::Arr(items) => items
                .borrow()
                .iter()
                .map(|v| match v {
                    Js::Undefined | Js::Null => String::new(),
                    other => other.text(),
                })
                .collect::<Vec<_>>()
                .join(","),
            Js::Obj(_, Some(_)) => "function () { [native code] }".to_string(),
            Js::Obj(..) => "[object Object]".to_string(),
            Js::Iter(_) => "[object Array Iterator]".to_string(),
            Js::Error(e) => e.clone(),
            Js::Module(_) => "[object Module]".to_string(),
            Js::Promise(_) => "[object Promise]".to_string(),
        }
    }
    fn same(&self, other: &Js) -> bool {
        match (self, other) {
            (Js::Undefined, Js::Undefined) | (Js::Null, Js::Null) => true,
            (Js::Bool(a), Js::Bool(b)) => a == b,
            (Js::Num(a), Js::Num(b)) => a == b,
            (Js::Str(a), Js::Str(b)) => a == b,
            (Js::Arr(a), Js::Arr(b)) => Rc::ptr_eq(a, b),
            (Js::Obj(a, _), Js::Obj(b, _)) => Rc::ptr_eq(a, b),
            (Js::Module(a), Js::Module(b)) => a == b,
            _ => false,
        }
    }
    /// `JSON.stringify(value)`, keys in insertion order; Err for what it
    /// cannot write (a function, an iterator, a module).
    fn stringify(&self) -> Result<String, ()> {
        Ok(match self {
            Js::Undefined | Js::Null => "null".to_string(),
            Js::Bool(b) => b.to_string(),
            Js::Num(x) if x.is_finite() => Js::number_text(*x),
            Js::Num(_) => "null".to_string(),
            Js::Str(s) => serde_json::Value::String(s.clone()).to_string(),
            Js::Arr(items) => format!(
                "[{}]",
                items
                    .borrow()
                    .iter()
                    .map(|v| v.stringify())
                    .collect::<Result<Vec<_>, _>>()?
                    .join(",")
            ),
            Js::Obj(props, None) => {
                let mut parts = Vec::new();
                for (k, v) in props.borrow().iter() {
                    if !matches!(v, Js::Undefined) {
                        parts.push(format!(
                            "{}:{}",
                            serde_json::Value::String(k.clone()),
                            v.stringify()?
                        ));
                    }
                }
                format!("{{{}}}", parts.join(","))
            }
            _ => return Err(()),
        })
    }
    /// `JSON.parse(text)`, keeping an object's keys in document order.
    pub fn parse(text: &str) -> Result<Js, serde_json::Error> {
        struct Parsed(Js);
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Parsed;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<Parsed, E> {
                Ok(Parsed(Js::Null))
            }
            fn visit_bool<E>(self, b: bool) -> Result<Parsed, E> {
                Ok(Parsed(Js::Bool(b)))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Parsed, E> {
                Ok(Parsed(Js::Num(n as f64)))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Parsed, E> {
                Ok(Parsed(Js::Num(n as f64)))
            }
            fn visit_f64<E>(self, x: f64) -> Result<Parsed, E> {
                Ok(Parsed(Js::Num(x)))
            }
            fn visit_str<E>(self, s: &str) -> Result<Parsed, E> {
                Ok(Parsed(Js::Str(s.to_string())))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Parsed, A::Error> {
                let mut items = Vec::new();
                while let Some(Parsed(v)) = seq.next_element()? {
                    items.push(v);
                }
                Ok(Parsed(Js::Arr(Rc::new(RefCell::new(items)))))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Parsed, A::Error> {
                let mut props: Vec<(String, Js)> = Vec::new();
                while let Some((k, Parsed(v))) = map.next_entry::<String, Parsed>()? {
                    match props.iter_mut().find(|(key, _)| *key == k) {
                        Some(slot) => slot.1 = v,
                        None => props.push((k, v)),
                    }
                }
                Ok(Parsed(Js::Obj(Rc::new(RefCell::new(props)), None)))
            }
        }
        impl<'de> serde::Deserialize<'de> for Parsed {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Parsed, D::Error> {
                d.deserialize_any(Visitor)
            }
        }
        serde_json::from_str::<Parsed>(text).map(|p| p.0)
    }

    fn number(&self) -> f64 {
        match self {
            Js::Num(x) => *x,
            Js::Bool(b) => *b as i32 as f64,
            Js::Null => 0.0,
            Js::Str(s) => s.trim().parse().unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }
}

/// The JavaScript side of a bridge: its globals, the table of the values
/// Python holds by handle, the modules it can load, and what it threw last.
pub struct JsWorld {
    pub values: Vec<Option<Js>>,
    free: Vec<usize>,
    pub globals: Js,
    pub last_error: Js,
    bytes: Vec<u8>,
    /// What the last Python exception escaping a wrapped call was.
    pub failure: Option<String>,
    /// The modules `importlib.import_module(name)` loads, each an instance
    /// in the same store.
    pub modules: HashMap<String, Instance>,
    /// Promises Python awaits that have settled: (token, rejected, value),
    /// for the event loop to hand back (`drive_until`).
    pub settled: Vec<(i32, bool, Js)>,
    /// What each exported coroutine's call settled with, by token.
    pub resolved: HashMap<i32, (bool, Js)>,
}

impl Default for JsWorld {
    fn default() -> Self {
        JsWorld {
            values: Vec::new(),
            free: Vec::new(),
            globals: js_globals(),
            last_error: Js::Undefined,
            bytes: Vec::new(),
            failure: None,
            modules: HashMap::new(),
            settled: Vec::new(),
            resolved: HashMap::new(),
        }
    }
}

impl JsWorld {
    pub fn add(&mut self, value: Js) -> i32 {
        match self.free.pop() {
            Some(h) => {
                self.values[h] = Some(value);
                h as i32
            }
            None => {
                self.values.push(Some(value));
                (self.values.len() - 1) as i32
            }
        }
    }
    pub fn value(&self, h: i32) -> Js {
        self.values
            .get(h as usize)
            .and_then(|v| v.clone())
            .unwrap_or_else(|| panic!("a released or unknown JavaScript handle {h}"))
    }
    pub fn drop_handle(&mut self, h: i32) {
        self.values[h as usize] = None;
        self.free.push(h as usize);
    }
    /// How many handles Python holds.
    pub fn live(&self) -> usize {
        self.values.iter().filter(|v| v.is_some()).count()
    }
    fn fail(&mut self, error: Js) -> i32 {
        self.last_error = error;
        -2
    }
}

fn js_error(kind: &str, message: impl std::fmt::Display) -> Js {
    Js::Error(format!("{kind}: {message}"))
}

/// The globals the tests' JavaScript has: `Math`, `JSON`, `Object`, `Array`,
/// `String`, `Number`, and `globalThis`.
fn js_globals() -> Js {
    let num = |args: &[Js], k: usize| args.get(k).map_or(f64::NAN, Js::number);
    let math = Js::object(vec![
        ("PI", Js::Num(std::f64::consts::PI)),
        (
            "max",
            Js::function(|_, _, args| {
                Ok(Js::Num(
                    args.iter()
                        .map(Js::number)
                        .fold(f64::NEG_INFINITY, f64::max),
                ))
            }),
        ),
        (
            "min",
            Js::function(|_, _, args| {
                Ok(Js::Num(
                    args.iter().map(Js::number).fold(f64::INFINITY, f64::min),
                ))
            }),
        ),
        (
            "floor",
            Js::function(move |_, _, a| Ok(Js::Num(num(a, 0).floor()))),
        ),
        (
            "sqrt",
            Js::function(move |_, _, a| Ok(Js::Num(num(a, 0).sqrt()))),
        ),
        (
            "abs",
            Js::function(move |_, _, a| Ok(Js::Num(num(a, 0).abs()))),
        ),
    ]);
    let json = Js::object(vec![
        (
            "parse",
            Js::function(|_, _, args| {
                let text = args.first().map(Js::text).unwrap_or_default();
                Js::parse(&text).map_err(|e| js_error("SyntaxError", e))
            }),
        ),
        (
            "stringify",
            Js::function(|_, _, args| match args.first().unwrap_or(&Js::Undefined) {
                Js::Undefined => Ok(Js::Undefined),
                v => v
                    .stringify()
                    .map(Js::Str)
                    .map_err(|_| js_error("TypeError", "cannot serialize")),
            }),
        ),
    ]);
    let object = Js::function(|_, _, _| Ok(Js::object(vec![])));
    object
        .set(
            "keys",
            Js::function(|_, _, args| match args.first() {
                Some(Js::Obj(props, _)) => Ok(Js::Arr(Rc::new(RefCell::new(
                    props
                        .borrow()
                        .iter()
                        .map(|(k, _)| Js::Str(k.clone()))
                        .collect(),
                )))),
                _ => Ok(Js::Arr(Rc::new(RefCell::new(Vec::new())))),
            }),
        )
        .unwrap();
    let array = Js::function(|_, _, args| Ok(Js::Arr(Rc::new(RefCell::new(args.to_vec())))));
    let string =
        Js::function(|_, _, args| Ok(Js::Str(args.first().map(Js::text).unwrap_or_default())));
    let number = Js::function(move |_, _, args| Ok(Js::Num(num(args, 0))));
    let settled = |rejected: bool| {
        Js::function(move |_, _, args| {
            let value = args.first().cloned().unwrap_or(Js::Undefined);
            Ok(Js::Promise(Rc::new(RefCell::new(Some((rejected, value))))))
        })
    };
    let promise = Js::object(vec![("resolve", settled(false)), ("reject", settled(true))]);
    let error = Js::function(|_, _, args| {
        Ok(Js::Error(format!(
            "Error: {}",
            args.first().map(Js::text).unwrap_or_default()
        )))
    });
    let globals = Js::object(vec![
        ("Promise", promise),
        ("Error", error),
        ("Math", math),
        ("JSON", json),
        ("Object", object),
        ("Array", array),
        ("String", string),
        ("Number", number),
        ("undefined", Js::Undefined),
    ]);
    globals.set("globalThis", globals.clone()).unwrap();
    globals
}

/// An array's methods the tests call: `push`, and a module's exports.
fn call_method(
    caller: &mut Caller<'_, Host>,
    target: &Js,
    name: &str,
    args: Vec<Js>,
) -> Result<Js, Js> {
    match (target, name) {
        (Js::Arr(items), "push") => {
            items.borrow_mut().extend(args);
            Ok(Js::Num(items.borrow().len() as f64))
        }
        (Js::Arr(items), "join") => Ok(Js::Str(
            items
                .borrow()
                .iter()
                .map(Js::text)
                .collect::<Vec<_>>()
                .join(
                    &args
                        .first()
                        .map(Js::text)
                        .unwrap_or_else(|| ",".to_string()),
                ),
        )),
        (Js::Module(module), export) => call_module(caller, module, export, args),
        _ => match target.get(name) {
            Some(f @ Js::Obj(_, Some(_))) => call_value(caller, &f, target, &args),
            Some(_) => Err(js_error("TypeError", format!("{name} is not a function"))),
            None => Err(js_error("TypeError", format!("{name} is not a function"))),
        },
    }
}

fn call_value(caller: &mut Caller<'_, Host>, f: &Js, this: &Js, args: &[Js]) -> Result<Js, Js> {
    match f {
        Js::Obj(_, Some(function)) => {
            let function = function.clone();
            function(&mut caller.data_mut().js, this, args)
        }
        _ => Err(js_error("TypeError", "not a function")),
    }
}

/// A call of a separately compiled module's export, through its wrapper,
/// as the loader makes it.
fn call_module(
    caller: &mut Caller<'_, Host>,
    module: &str,
    export: &str,
    args: Vec<Js>,
) -> Result<Js, Js> {
    let Some(instance) = caller.data().js.modules.get(module).copied() else {
        return Err(js_error("TypeError", format!("no module {module}")));
    };
    let Some(Extern::Func(f)) = instance.get_export(&*caller, &format!("__jsx_{export}")) else {
        return Err(js_error("TypeError", format!("{export} is not a function")));
    };
    let handles: Vec<Value> = args
        .into_iter()
        .map(|a| Value::I32(caller.data_mut().js.add(a)))
        .collect();
    let mut out = [Value::I32(0)];
    f.call(&mut *caller, &handles, &mut out)
        .map_err(|e| js_error("RuntimeError", e))?;
    let r = out[0].i32().unwrap_or(-2);
    let world = &mut caller.data_mut().js;
    if r == -2 {
        let message = world.failure.take().unwrap_or_default();
        return Err(js_error("PythonError", message));
    }
    let value = world.value(r);
    world.drop_handle(r);
    Ok(value)
}

fn js_text(caller: &mut Caller<'_, Host>, ptr: i32, len: i32) -> String {
    let memory = memory_of(caller);
    let data = memory.data(&*caller);
    String::from_utf8_lossy(&data[ptr as usize..(ptr + len) as usize]).into_owned()
}

/// Define the bridge's imports over the host's `JsWorld`.
pub fn link_js(linker: &mut Linker<Host>) {
    let m = "waspy_js";
    linker
        .func_wrap(
            m,
            "global",
            |mut c: Caller<'_, Host>, p: i32, l: i32| -> i32 {
                let name = js_text(&mut c, p, l);
                let w = &mut c.data_mut().js;
                match w.globals.get(&name) {
                    Some(v) => w.add(v),
                    None => -1,
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "global_this", |mut c: Caller<'_, Host>| -> i32 {
            let w = &mut c.data_mut().js;
            let g = w.globals.clone();
            w.add(g)
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "get",
            |mut c: Caller<'_, Host>, h: i32, p: i32, l: i32| -> i32 {
                let name = js_text(&mut c, p, l);
                let w = &mut c.data_mut().js;
                match w.value(h) {
                    v @ (Js::Undefined | Js::Null) => w.fail(js_error(
                        "TypeError",
                        format!("Cannot read properties of {} (reading '{name}')", v.text()),
                    )),
                    v => match v.get(&name) {
                        Some(x) => w.add(x),
                        None => -1,
                    },
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "set",
            |mut c: Caller<'_, Host>, h: i32, p: i32, l: i32, v: i32| -> i32 {
                let name = js_text(&mut c, p, l);
                let w = &mut c.data_mut().js;
                let (target, value) = (w.value(h), w.value(v));
                match target.set(&name, value) {
                    Ok(()) => 0,
                    Err(e) => w.fail(e),
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "call",
            |mut c: Caller<'_, Host>, f: i32, args: i32| -> i32 {
                let (fv, argv) = {
                    let w = &c.data().js;
                    (w.value(f), w.value(args))
                };
                let Js::Arr(items) = argv else { unreachable!() };
                let items = items.borrow().clone();
                if !matches!(fv, Js::Obj(_, Some(_))) {
                    return -3;
                }
                match call_value(&mut c, &fv, &Js::Undefined, &items) {
                    Ok(r) => c.data_mut().js.add(r),
                    Err(e) => c.data_mut().js.fail(e),
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "call_method",
            |mut c: Caller<'_, Host>, h: i32, p: i32, l: i32, args: i32| -> i32 {
                let name = js_text(&mut c, p, l);
                let (target, argv) = {
                    let w = &c.data().js;
                    (w.value(h), w.value(args))
                };
                let Js::Arr(items) = argv else { unreachable!() };
                let items = items.borrow().clone();
                let builtin = matches!((&target, name.as_str()), (Js::Arr(_), "push" | "join"))
                    || matches!(target, Js::Module(_));
                if !builtin {
                    match target.get(&name) {
                        None => return -1,
                        Some(Js::Obj(_, Some(_))) => {}
                        Some(_) => return -3,
                    }
                }
                match call_method(&mut c, &target, &name, items) {
                    Ok(r) => c.data_mut().js.add(r),
                    Err(e) => c.data_mut().js.fail(e),
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "construct",
            |mut c: Caller<'_, Host>, f: i32, args: i32| -> i32 {
                let (fv, argv) = {
                    let w = &c.data().js;
                    (w.value(f), w.value(args))
                };
                let Js::Arr(items) = argv else { unreachable!() };
                let items = items.borrow().clone();
                if !matches!(fv, Js::Obj(_, Some(_))) {
                    return -3;
                }
                match call_value(&mut c, &fv, &Js::Undefined, &items) {
                    Ok(r) => c.data_mut().js.add(r),
                    Err(e) => c.data_mut().js.fail(e),
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "array", |mut c: Caller<'_, Host>| -> i32 {
            c.data_mut()
                .js
                .add(Js::Arr(Rc::new(RefCell::new(Vec::new()))))
        })
        .unwrap();
    linker
        .func_wrap(m, "push", |mut c: Caller<'_, Host>, a: i32, v: i32| {
            let w = &mut c.data_mut().js;
            let value = w.value(v);
            if let Js::Arr(items) = w.value(a) {
                items.borrow_mut().push(value);
            }
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "number",
            |mut c: Caller<'_, Host>, x: wasmi::core::F64| -> i32 {
                c.data_mut().js.add(Js::Num(f64::from(x)))
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "boolean", |mut c: Caller<'_, Host>, b: i32| -> i32 {
            c.data_mut().js.add(Js::Bool(b != 0))
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "string",
            |mut c: Caller<'_, Host>, p: i32, l: i32| -> i32 {
                let s = js_text(&mut c, p, l);
                c.data_mut().js.add(Js::Str(s))
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "undefined", |mut c: Caller<'_, Host>| -> i32 {
            c.data_mut().js.add(Js::Undefined)
        })
        .unwrap();
    linker
        .func_wrap(m, "kind", |c: Caller<'_, Host>, h: i32| -> i32 {
            match c.data().js.value(h) {
                Js::Undefined => 0,
                Js::Null => 1,
                Js::Bool(false) => 2,
                Js::Bool(true) => 3,
                Js::Num(x) if x == x.trunc() && x.abs() <= 9007199254740991.0 => 4,
                Js::Num(_) => 5,
                Js::Str(_) => 6,
                Js::Obj(_, Some(_)) => 7,
                Js::Arr(_) => 8,
                Js::Obj(..) | Js::Error(_) | Js::Module(_) | Js::Iter(_) | Js::Promise(_) => 9,
            }
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "to_number",
            |c: Caller<'_, Host>, h: i32| -> wasmi::core::F64 {
                wasmi::core::F64::from(c.data().js.value(h).number())
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "text", |mut c: Caller<'_, Host>, h: i32| -> i32 {
            let w = &mut c.data_mut().js;
            let t = w.value(h).text();
            w.add(Js::Str(t))
        })
        .unwrap();
    linker
        .func_wrap(m, "utf8", |mut c: Caller<'_, Host>, h: i32| -> i32 {
            let w = &mut c.data_mut().js;
            w.bytes = w.value(h).text().into_bytes();
            w.bytes.len() as i32
        })
        .unwrap();
    linker
        .func_wrap(m, "utf8_copy", |mut c: Caller<'_, Host>, at: i32| {
            let bytes = std::mem::take(&mut c.data_mut().js.bytes);
            let memory = memory_of(&mut c);
            memory.data_mut(&mut c)[at as usize..at as usize + bytes.len()].copy_from_slice(&bytes);
        })
        .unwrap();
    linker
        .func_wrap(m, "equal", |c: Caller<'_, Host>, a: i32, b: i32| -> i32 {
            let w = &c.data().js;
            w.value(a).same(&w.value(b)) as i32
        })
        .unwrap();
    linker
        .func_wrap(m, "length", |c: Caller<'_, Host>, h: i32| -> i32 {
            match c.data().js.value(h) {
                Js::Arr(items) => items.borrow().len() as i32,
                Js::Str(s) => s.encode_utf16().count() as i32,
                v @ Js::Obj(..) => match v.get("length") {
                    Some(Js::Num(n)) => n as i32,
                    _ => -1,
                },
                _ => -1,
            }
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "item",
            |mut c: Caller<'_, Host>, h: i32, i: i32| -> i32 {
                let w = &mut c.data_mut().js;
                let Js::Arr(items) = w.value(h) else {
                    return -3;
                };
                let items = items.borrow().clone();
                let at = if i < 0 { i + items.len() as i32 } else { i };
                if at < 0 || at as usize >= items.len() {
                    return -1;
                }
                w.add(items[at as usize].clone())
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "iterate", |mut c: Caller<'_, Host>, h: i32| -> i32 {
            let w = &mut c.data_mut().js;
            match w.value(h) {
                Js::Arr(items) => {
                    let snapshot = items.borrow().clone();
                    w.add(Js::Iter(Rc::new(RefCell::new((snapshot, 0)))))
                }
                Js::Str(s) => {
                    let chars = s.chars().map(|ch| Js::Str(ch.to_string())).collect();
                    w.add(Js::Iter(Rc::new(RefCell::new((chars, 0)))))
                }
                _ => -3,
            }
        })
        .unwrap();
    linker
        .func_wrap(m, "next", |mut c: Caller<'_, Host>, it: i32| -> i32 {
            let w = &mut c.data_mut().js;
            let Js::Iter(state) = w.value(it) else {
                return w.fail(js_error("TypeError", "not an iterator"));
            };
            let next = {
                let mut s = state.borrow_mut();
                let v = s.0.get(s.1).cloned();
                s.1 += 1;
                v
            };
            match next {
                Some(v) => w.add(v),
                None => -1,
            }
        })
        .unwrap();
    linker
        .func_wrap(m, "error", |mut c: Caller<'_, Host>| -> i32 {
            let w = &mut c.data_mut().js;
            let e = w.last_error.clone();
            w.add(e)
        })
        .unwrap();
    linker
        .func_wrap(m, "release", |mut c: Caller<'_, Host>, h: i32| {
            c.data_mut().js.drop_handle(h);
        })
        .unwrap();
    linker
        .func_wrap(m, "keep", |mut c: Caller<'_, Host>, h: i32| -> i32 {
            let w = &mut c.data_mut().js;
            let v = w.value(h);
            w.add(v)
        })
        .unwrap();
    linker
        .func_wrap(m, "stringify", |mut c: Caller<'_, Host>, h: i32| -> i32 {
            let w = &mut c.data_mut().js;
            match w.value(h).stringify() {
                Ok(j) => w.add(Js::Str(j)),
                Err(()) => -3,
            }
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "parse",
            |mut c: Caller<'_, Host>, p: i32, l: i32| -> i32 {
                let text = js_text(&mut c, p, l);
                let w = &mut c.data_mut().js;
                match Js::parse(&text) {
                    Ok(v) => w.add(v),
                    Err(e) => w.fail(js_error("SyntaxError", e)),
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "import_module",
            |mut c: Caller<'_, Host>, p: i32, l: i32| -> i32 {
                let name = js_text(&mut c, p, l);
                let w = &mut c.data_mut().js;
                if w.modules.contains_key(&name) {
                    w.add(Js::Module(name))
                } else {
                    -1
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "fail", |mut c: Caller<'_, Host>, p: i32, l: i32| {
            let text = js_text(&mut c, p, l);
            c.data_mut().js.failure = Some(text);
        })
        .unwrap();
    linker
        .func_wrap(m, "then", |mut c: Caller<'_, Host>, p: i32, token: i32| {
            let w = &mut c.data_mut().js;
            // A settled promise is handed back on the next turn; anything
            // else that is not a promise resolves with itself, as
            // `Promise.resolve(x).then` would.
            match w.value(p) {
                Js::Promise(state) => {
                    let settled = state.borrow().clone();
                    if let Some((rejected, value)) = settled {
                        w.settled.push((token, rejected, value));
                    }
                }
                other => w.settled.push((token, false, other)),
            }
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "resolve",
            |mut c: Caller<'_, Host>, token: i32, failed: i32, h: i32| {
                let w = &mut c.data_mut().js;
                let value = w.value(h);
                w.drop_handle(h);
                w.resolved.insert(token, (failed != 0, value));
            },
        )
        .unwrap();
}

/// Play JavaScript's event loop for an instance until the exported
/// coroutine call `token` settles: hand back the promises Python awaits as
/// they settle (`__waspy_settle`), and run timers as they fall due on the
/// harness's clock (`__waspy_drive`). Its value, or the `PythonError`'s
/// message when it rejected.
pub fn drive_until(store: &mut Store<Host>, instance: &Instance, token: i32) -> Result<Js, String> {
    let settle = instance
        .get_typed_func::<(i32, i32, i32), wasmi::core::F64>(&*store, "__waspy_settle")
        .expect("__waspy_settle");
    let drive = instance
        .get_typed_func::<(), wasmi::core::F64>(&*store, "__waspy_drive")
        .expect("__waspy_drive");
    let mut delay = f64::from(drive.call(&mut *store, ()).expect("drive"));
    for _ in 0..100_000 {
        if let Some((failed, value)) = store.data_mut().js.resolved.remove(&token) {
            return if failed { Err(value.text()) } else { Ok(value) };
        }
        let queued = std::mem::take(&mut store.data_mut().js.settled);
        if !queued.is_empty() {
            for (t, rejected, value) in queued {
                let h = store.data_mut().js.add(value);
                delay = f64::from(
                    settle
                        .call(&mut *store, (t, rejected as i32, h))
                        .expect("settle"),
                );
            }
            continue;
        }
        assert!(
            delay >= 0.0,
            "the coroutine waits on nothing that will come"
        );
        store.data_mut().monotonic_ns += (delay * 1e6).ceil() as u64;
        delay = f64::from(drive.call(&mut *store, ()).expect("drive"));
    }
    panic!("the exported coroutine never settled");
}
