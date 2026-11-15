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

#[path = "graphics.rs"]
pub mod graphics;

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
    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("instantiate: {e}"))?
        .start(&mut store)
        .map_err(|e| format!("start: {e}"))?;
    if let Ok(init) = instance.get_typed_func::<(), i32>(&store, "__waspy_init") {
        match init.call(&mut store, ()) {
            Ok(-2) => {
                return Err(format!(
                    "start: {}",
                    store.data().js.failure.clone().unwrap_or_default()
                ))
            }
            Ok(_) => {}
            Err(e) => return Err(format!("start: {e}")),
        }
    }
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
    let (instance, mut store) = instantiate_unstarted(wasm, host);
    start_bridge(&mut store, &instance);
    (instance, store)
}

/// Instantiate, leaving a module using the bridge's top level to its first
/// use (the `__main__` block runs it).
fn instantiate_unstarted(wasm: &[u8], host: Host) -> (Instance, Store<Host>) {
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

/// Run the top level of a module using the JavaScript bridge, as the
/// loader does before its first call into it (such a module has no start
/// section). Nothing for any other module.
pub fn start_bridge(store: &mut Store<Host>, instance: &Instance) {
    if let Ok(init) = instance.get_typed_func::<(), i32>(&*store, "__waspy_init") {
        let r = init.call(&mut *store, ()).expect("the top level runs");
        assert!(
            r != -2,
            "the top level raised: {}",
            store.data().js.failure.clone().unwrap_or_default()
        );
    }
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
    let (instance, mut store) = instantiate_unstarted(&wasm, Host::with_input(stdin, args));
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
    let (instance, mut store) = instantiate_unstarted(&wasm, host);
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
    /// A DOM node (`document`, an element, a text node).
    Node(Rc<RefCell<Node>>),
    /// An element's `classList`.
    ClassList(Rc<RefCell<Node>>),
    /// A Python function JavaScript can call (`create_proxy`).
    PyFunc(Rc<PyFn>),
    /// An object implemented in Rust: a typed array, a canvas's context
    /// (`graphics`).
    Host(Rc<dyn graphics::HostObject>),
}

/// What the loader's `function` import makes of a Python function: its
/// environment, how many arguments it takes, whether it is called once, its
/// registry id, and whether it was destroyed.
pub struct PyFn {
    pub env: i32,
    pub arity: usize,
    pub once: bool,
    pub id: i32,
    pub alive: std::cell::Cell<bool>,
}

/// A DOM node, as much of one as the tests need: an element's tag, its
/// attributes in the order they were set, its own properties (`value`), its
/// `style`, its children, and its event listeners; a text node's data.
pub struct Node {
    /// `DIV` for an element, `#text`, or `#document`.
    pub name: String,
    pub data: String,
    pub attrs: Vec<(String, String)>,
    pub props: Vec<(String, Js)>,
    pub style: Js,
    pub children: Vec<Rc<RefCell<Node>>>,
    pub parent: Option<std::rc::Weak<RefCell<Node>>>,
    /// (type, listener, once).
    pub listeners: Vec<(String, Js, bool)>,
}

impl Node {
    fn new(name: &str) -> Rc<RefCell<Node>> {
        Rc::new(RefCell::new(Node {
            name: name.to_string(),
            data: String::new(),
            attrs: Vec::new(),
            props: Vec::new(),
            style: Js::object(vec![]),
            children: Vec::new(),
            parent: None,
            listeners: Vec::new(),
        }))
    }
    fn text_node(data: &str) -> Rc<RefCell<Node>> {
        let n = Node::new("#text");
        n.borrow_mut().data = data.to_string();
        n
    }
    fn is_element(&self) -> bool {
        !self.name.starts_with('#')
    }
    fn attr(&self, name: &str) -> Option<String> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
    fn set_attr(&mut self, name: &str, value: String) {
        match self.attrs.iter_mut().find(|(k, _)| k == name) {
            Some(slot) => slot.1 = value,
            None => self.attrs.push((name.to_string(), value)),
        }
    }
    fn classes(&self) -> Vec<String> {
        self.attr("class")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }
    fn set_classes(&mut self, classes: &[String]) {
        self.set_attr("class", classes.join(" "));
    }
    fn text_content(&self) -> String {
        if self.name == "#text" {
            return self.data.clone();
        }
        self.children
            .iter()
            .map(|c| c.borrow().text_content())
            .collect()
    }
    fn outer_html(&self) -> String {
        if self.name == "#text" {
            return self
                .data
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
        }
        let tag = self.name.to_lowercase();
        let attrs: String = self
            .attrs
            .iter()
            .map(|(k, v)| {
                format!(
                    " {k}=\"{}\"",
                    v.replace('&', "&amp;").replace('"', "&quot;")
                )
            })
            .collect();
        format!("<{tag}{attrs}>{}</{tag}>", self.inner_html())
    }
    fn inner_html(&self) -> String {
        self.children
            .iter()
            .map(|c| c.borrow().outer_html())
            .collect()
    }
}

/// Detach a node from its parent.
fn detach(node: &Rc<RefCell<Node>>) {
    let parent = node.borrow().parent.as_ref().and_then(|p| p.upgrade());
    if let Some(parent) = parent {
        parent
            .borrow_mut()
            .children
            .retain(|c| !Rc::ptr_eq(c, node));
    }
    node.borrow_mut().parent = None;
}

/// Insert `child` into `parent` at `at` (the end when None), moving it from
/// wherever it was.
fn insert(parent: &Rc<RefCell<Node>>, child: &Rc<RefCell<Node>>, at: Option<usize>) {
    detach(child);
    child.borrow_mut().parent = Some(Rc::downgrade(parent));
    let mut p = parent.borrow_mut();
    let at = at.unwrap_or(p.children.len()).min(p.children.len());
    p.children.insert(at, child.clone());
}

/// Each descendant of `node`, in document order.
fn descendants(node: &Rc<RefCell<Node>>, out: &mut Vec<Rc<RefCell<Node>>>) {
    for c in node.borrow().children.iter() {
        out.push(c.clone());
        descendants(c, out);
    }
}

/// Whether an element matches a compound selector (`li`, `#id`, `.a.b`,
/// `li.done`).
fn matches_compound(node: &Node, compound: &str) -> bool {
    if !node.is_element() {
        return false;
    }
    let mut rest = compound;
    let tag_end = rest.find(['#', '.']).unwrap_or(rest.len());
    let tag = &rest[..tag_end];
    if !tag.is_empty() && tag != "*" && !tag.eq_ignore_ascii_case(&node.name) {
        return false;
    }
    rest = &rest[tag_end..];
    while !rest.is_empty() {
        let kind = rest.as_bytes()[0];
        let body = &rest[1..];
        let end = body.find(['#', '.']).unwrap_or(body.len());
        let part = &body[..end];
        let ok = match kind {
            b'#' => node.attr("id").as_deref() == Some(part),
            _ => node.classes().iter().any(|c| c == part),
        };
        if !ok {
            return false;
        }
        rest = &body[end..];
    }
    true
}

/// Whether an element matches a selector of compound parts joined by
/// descendant combinators (`ul .item`), or any of a comma-separated list.
fn matches_selector(node: &Rc<RefCell<Node>>, selector: &str) -> bool {
    selector.split(',').any(|one| {
        let parts: Vec<&str> = one.split_whitespace().collect();
        let Some((last, ancestors)) = parts.split_last() else {
            return false;
        };
        if !matches_compound(&node.borrow(), last) {
            return false;
        }
        let mut at = node.borrow().parent.as_ref().and_then(|p| p.upgrade());
        for want in ancestors.iter().rev() {
            loop {
                let Some(a) = at else { return false };
                let next = a.borrow().parent.as_ref().and_then(|p| p.upgrade());
                if matches_compound(&a.borrow(), want) {
                    at = next;
                    break;
                }
                at = next;
            }
        }
        true
    })
}

fn query(root: &Rc<RefCell<Node>>, selector: &str) -> Vec<Rc<RefCell<Node>>> {
    let mut all = Vec::new();
    descendants(root, &mut all);
    all.into_iter()
        .filter(|n| matches_selector(n, selector))
        .collect()
}

/// `document`: an `html` element holding `head` and `body`.
fn new_document() -> Js {
    let doc = Node::new("#document");
    let html = Node::new("HTML");
    insert(&doc, &html, None);
    insert(&html, &Node::new("HEAD"), None);
    insert(&html, &Node::new("BODY"), None);
    Js::Node(doc)
}

/// The JavaScript class a DOM element is an instance of, as `String(el)`
/// names it.
fn element_class(tag: &str) -> &'static str {
    match tag {
        "DIV" => "HTMLDivElement",
        "SPAN" => "HTMLSpanElement",
        "P" => "HTMLParagraphElement",
        "UL" => "HTMLUListElement",
        "OL" => "HTMLOListElement",
        "LI" => "HTMLLIElement",
        "BUTTON" => "HTMLButtonElement",
        "INPUT" => "HTMLInputElement",
        "BODY" => "HTMLBodyElement",
        "CANVAS" => "HTMLCanvasElement",
        _ => "HTMLElement",
    }
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
            Js::ClassList(n) if name == "length" => {
                Some(Js::Num(n.borrow().classes().len() as f64))
            }
            Js::ClassList(_) => DOM_METHODS
                .contains(&name)
                .then(|| Js::function(|_, _, _| Ok(Js::Undefined))),
            Js::Node(n) => node_get(n, name),
            Js::Host(o) => o.get(name),
            _ => None,
        }
    }
    fn set(&self, name: &str, value: Js) -> Result<(), Js> {
        match self {
            Js::Node(n) => {
                node_set(n, name, value);
                Ok(())
            }
            Js::Host(o) => o.set(name, &value),
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
            Js::Node(n) => {
                let n = n.borrow();
                match n.name.as_str() {
                    "#document" => "[object HTMLDocument]".to_string(),
                    "#text" => "[object Text]".to_string(),
                    tag => format!("[object {}]", element_class(tag)),
                }
            }
            Js::ClassList(n) => n.borrow().classes().join(" "),
            Js::PyFunc(_) => "function () { [python code] }".to_string(),
            Js::Host(o) => o.text(),
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
            (Js::Node(a), Js::Node(b)) | (Js::ClassList(a), Js::ClassList(b)) => Rc::ptr_eq(a, b),
            (Js::PyFunc(a), Js::PyFunc(b)) => Rc::ptr_eq(a, b),
            (Js::Host(a), Js::Host(b)) => {
                std::ptr::eq(Rc::as_ptr(a) as *const u8, Rc::as_ptr(b) as *const u8)
            }
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

/// The methods a DOM node or a `classList` answers (through `call_method`).
const DOM_METHODS: [&str; 27] = [
    "createElement",
    "createTextNode",
    "getElementById",
    "querySelector",
    "querySelectorAll",
    "appendChild",
    "append",
    "removeChild",
    "remove",
    "insertBefore",
    "replaceChildren",
    "setAttribute",
    "getAttribute",
    "hasAttribute",
    "removeAttribute",
    "contains",
    "addEventListener",
    "removeEventListener",
    "dispatchEvent",
    "click",
    "add",
    "toggle",
    "item",
    "closest",
    "hasChildNodes",
    "cloneNode",
    "getContext",
];

fn node_or_null(n: Option<Rc<RefCell<Node>>>) -> Js {
    n.map_or(Js::Null, Js::Node)
}

fn node_get(node: &Rc<RefCell<Node>>, name: &str) -> Option<Js> {
    let n = node.borrow();
    let parent = || n.parent.as_ref().and_then(|p| p.upgrade());
    let siblings = |elements: bool, offset: isize| -> Js {
        let Some(p) = parent() else { return Js::Null };
        let p = p.borrow();
        let list: Vec<&Rc<RefCell<Node>>> = p
            .children
            .iter()
            .filter(|c| !elements || c.borrow().is_element())
            .collect();
        let at = list.iter().position(|c| Rc::ptr_eq(c, node));
        at.and_then(|i| {
            list.get((i as isize + offset) as usize)
                .map(|c| (*c).clone())
        })
        .map_or(Js::Null, Js::Node)
    };
    let elements = || -> Vec<Rc<RefCell<Node>>> {
        n.children
            .iter()
            .filter(|c| c.borrow().is_element())
            .cloned()
            .collect()
    };
    let arr = |items: Vec<Rc<RefCell<Node>>>| {
        Js::Arr(Rc::new(RefCell::new(
            items.into_iter().map(Js::Node).collect(),
        )))
    };
    Some(match name {
        "tagName" | "nodeName" if n.is_element() => Js::Str(n.name.clone()),
        "nodeName" => Js::Str(n.name.clone()),
        "nodeType" => Js::Num(match n.name.as_str() {
            "#text" => 3.0,
            "#document" => 9.0,
            _ => 1.0,
        }),
        "id" => Js::Str(n.attr("id").unwrap_or_default()),
        "className" => Js::Str(n.attr("class").unwrap_or_default()),
        "textContent" if n.name == "#document" => Js::Null,
        "textContent" | "innerText" => Js::Str(n.text_content()),
        "data" | "nodeValue" if n.name == "#text" => Js::Str(n.data.clone()),
        "innerHTML" => Js::Str(n.inner_html()),
        "outerHTML" => Js::Str(n.outer_html()),
        "children" => arr(elements()),
        "childNodes" => arr(n.children.clone()),
        "childElementCount" => Js::Num(elements().len() as f64),
        "firstChild" => node_or_null(n.children.first().cloned()),
        "lastChild" => node_or_null(n.children.last().cloned()),
        "firstElementChild" => node_or_null(elements().first().cloned()),
        "lastElementChild" => node_or_null(elements().last().cloned()),
        "parentNode" => node_or_null(parent()),
        "parentElement" => node_or_null(parent().filter(|p| p.borrow().is_element())),
        "nextSibling" => siblings(false, 1),
        "previousSibling" => siblings(false, -1),
        "nextElementSibling" => siblings(true, 1),
        "previousElementSibling" => siblings(true, -1),
        "style" => n.style.clone(),
        "classList" => Js::ClassList(node.clone()),
        "documentElement" if n.name == "#document" => node_or_null(n.children.first().cloned()),
        "body" | "head" if n.name == "#document" => {
            let html = n.children.first()?.clone();
            let found = html
                .borrow()
                .children
                .iter()
                .find(|c| c.borrow().name.eq_ignore_ascii_case(name))
                .cloned();
            node_or_null(found)
        }
        method if DOM_METHODS.contains(&method) => Js::function(|_, _, _| Ok(Js::Undefined)),
        other => {
            return n
                .props
                .iter()
                .find(|(k, _)| k == other)
                .map(|(_, v)| v.clone())
        }
    })
}

fn node_set(node: &Rc<RefCell<Node>>, name: &str, value: Js) {
    match name {
        "id" => node.borrow_mut().set_attr("id", value.text()),
        "className" => node.borrow_mut().set_attr("class", value.text()),
        "textContent" | "innerText" | "innerHTML" => {
            let text = value.text();
            assert!(
                name != "innerHTML" || !text.contains('<'),
                "the harness DOM does not parse HTML: {text}"
            );
            let children = std::mem::take(&mut node.borrow_mut().children);
            for c in children {
                c.borrow_mut().parent = None;
            }
            if !text.is_empty() {
                insert(node, &Node::text_node(&text), None);
            }
        }
        "data" | "nodeValue" => node.borrow_mut().data = value.text(),
        other => {
            let canvas_size = matches!(other, "width" | "height") && node.borrow().name == "CANVAS";
            // A canvas's size is a whole number of pixels.
            let value = if canvas_size {
                Js::Num(value.number().trunc())
            } else {
                value
            };
            {
                let mut n = node.borrow_mut();
                match n.props.iter_mut().find(|(k, _)| k == other) {
                    Some(slot) => slot.1 = value,
                    None => n.props.push((other.to_string(), value)),
                }
            }
            if canvas_size {
                graphics::Ctx2D::resized(node);
            }
        }
    }
}

/// The node a value is (a DOM method's argument), or a TypeError.
fn as_node(value: &Js, what: &str) -> Result<Rc<RefCell<Node>>, Js> {
    match value {
        Js::Node(n) => Ok(n.clone()),
        Js::Str(s) => Ok(Node::text_node(s)),
        other => Err(js_error(
            "TypeError",
            format!(
                "Failed to execute '{what}' on 'Node': parameter 1 is not of type 'Node' ({})",
                other.text()
            ),
        )),
    }
}

/// A DOM node's or a `classList`'s method.
fn dom_method(
    caller: &mut Caller<'_, Host>,
    target: &Js,
    name: &str,
    args: &[Js],
) -> Option<Result<Js, Js>> {
    let arg = |k: usize| args.get(k).cloned().unwrap_or(Js::Undefined);
    let text = |k: usize| arg(k).text();
    if let Js::ClassList(node) = target {
        let mut classes = node.borrow().classes();
        let answer = match name {
            "add" => {
                for a in args {
                    let c = a.text();
                    if !classes.contains(&c) {
                        classes.push(c);
                    }
                }
                Js::Undefined
            }
            "remove" => {
                classes.retain(|c| !args.iter().any(|a| a.text() == *c));
                Js::Undefined
            }
            "toggle" => {
                let c = text(0);
                let on = match args.get(1) {
                    Some(Js::Bool(b)) => *b,
                    _ => !classes.contains(&c),
                };
                classes.retain(|x| *x != c);
                if on {
                    classes.push(c);
                }
                Js::Bool(on)
            }
            "contains" => return Some(Ok(Js::Bool(classes.contains(&text(0))))),
            "item" => {
                let i = arg(0).number() as usize;
                return Some(Ok(classes.get(i).cloned().map_or(Js::Null, Js::Str)));
            }
            _ => return None,
        };
        node.borrow_mut().set_classes(&classes);
        return Some(Ok(answer));
    }
    let Js::Node(node) = target else {
        return None;
    };
    let node = node.clone();
    Some(match name {
        "createElement" => {
            let el = Node::new(&text(0).to_uppercase());
            if el.borrow().name == "CANVAS" {
                el.borrow_mut().props.extend([
                    ("width".to_string(), Js::Num(300.0)),
                    ("height".to_string(), Js::Num(150.0)),
                ]);
            }
            Ok(Js::Node(el))
        }
        "getContext" => {
            let Js::Node(node) = target else {
                return None;
            };
            let canvas = node.borrow().name == "CANVAS";
            Ok(match text(0).as_str() {
                "2d" if canvas && !graphics::WebGl::on(node) => graphics::Ctx2D::of(node),
                "webgl" | "experimental-webgl" if canvas => graphics::WebGl::of(node),
                _ => Js::Null,
            })
        }
        "createTextNode" => Ok(Js::Node(Node::text_node(&text(0)))),
        "getElementById" => {
            let id = text(0);
            let mut all = Vec::new();
            descendants(&node, &mut all);
            Ok(node_or_null(all.into_iter().find(|n| {
                n.borrow().attr("id").as_deref() == Some(id.as_str())
            })))
        }
        "querySelector" => Ok(node_or_null(query(&node, &text(0)).into_iter().next())),
        "querySelectorAll" => Ok(Js::Arr(Rc::new(RefCell::new(
            query(&node, &text(0)).into_iter().map(Js::Node).collect(),
        )))),
        "closest" => {
            let sel = text(0);
            let mut at = Some(node.clone());
            while let Some(n) = at {
                if matches_selector(&n, &sel) {
                    return Some(Ok(Js::Node(n)));
                }
                at = n.borrow().parent.as_ref().and_then(|p| p.upgrade());
            }
            Ok(Js::Null)
        }
        "appendChild" => as_node(&arg(0), "appendChild").map(|c| {
            insert(&node, &c, None);
            Js::Node(c)
        }),
        "append" => {
            for a in args {
                match as_node(a, "append") {
                    Ok(c) => insert(&node, &c, None),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Js::Undefined)
        }
        "replaceChildren" => {
            let old = std::mem::take(&mut node.borrow_mut().children);
            for c in old {
                c.borrow_mut().parent = None;
            }
            for a in args {
                match as_node(a, "replaceChildren") {
                    Ok(c) => insert(&node, &c, None),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Js::Undefined)
        }
        "insertBefore" => as_node(&arg(0), "insertBefore").and_then(|c| {
            let at =
                match arg(1) {
                    Js::Node(r) => match node
                        .borrow()
                        .children
                        .iter()
                        .position(|x| Rc::ptr_eq(x, &r))
                    {
                        Some(i) => Some(i),
                        None => return Err(js_error(
                            "NotFoundError",
                            "Failed to execute 'insertBefore' on 'Node': The node before which \
                             the new node is to be inserted is not a child of this node.",
                        )),
                    },
                    _ => None,
                };
            let before = at.map(|i| node.borrow().children[i].clone());
            detach(&c);
            let at = before.and_then(|b| {
                node.borrow()
                    .children
                    .iter()
                    .position(|x| Rc::ptr_eq(x, &b))
            });
            insert(&node, &c, at);
            Ok(Js::Node(c))
        }),
        "removeChild" => as_node(&arg(0), "removeChild").and_then(|c| {
            let ours = c
                .borrow()
                .parent
                .as_ref()
                .and_then(|p| p.upgrade())
                .is_some_and(|p| Rc::ptr_eq(&p, &node));
            if !ours {
                return Err(js_error(
                    "NotFoundError",
                    "Failed to execute 'removeChild' on 'Node': The node to be removed is not a \
                     child of this node.",
                ));
            }
            detach(&c);
            Ok(Js::Node(c))
        }),
        "remove" => {
            detach(&node);
            Ok(Js::Undefined)
        }
        "hasChildNodes" => Ok(Js::Bool(!node.borrow().children.is_empty())),
        "contains" => Ok(Js::Bool(match arg(0) {
            Js::Node(other) => {
                let mut at = Some(other);
                let mut found = false;
                while let Some(n) = at {
                    if Rc::ptr_eq(&n, &node) {
                        found = true;
                        break;
                    }
                    at = n.borrow().parent.as_ref().and_then(|p| p.upgrade());
                }
                found
            }
            _ => false,
        })),
        "setAttribute" => {
            node.borrow_mut().set_attr(&text(0), text(1));
            Ok(Js::Undefined)
        }
        "getAttribute" => Ok(node.borrow().attr(&text(0)).map_or(Js::Null, Js::Str)),
        "hasAttribute" => Ok(Js::Bool(node.borrow().attr(&text(0)).is_some())),
        "removeAttribute" => {
            let k = text(0);
            node.borrow_mut().attrs.retain(|(a, _)| *a != k);
            Ok(Js::Undefined)
        }
        "addEventListener" => {
            let once = match arg(2) {
                options @ Js::Obj(..) => matches!(options.get("once"), Some(Js::Bool(true))),
                _ => false,
            };
            let (kind, listener) = (text(0), arg(1));
            let mut n = node.borrow_mut();
            if !n
                .listeners
                .iter()
                .any(|(k, l, _)| *k == kind && l.same(&listener))
            {
                n.listeners.push((kind, listener, once));
            }
            Ok(Js::Undefined)
        }
        "removeEventListener" => {
            let (kind, listener) = (text(0), arg(1));
            node.borrow_mut()
                .listeners
                .retain(|(k, l, _)| !(*k == kind && l.same(&listener)));
            Ok(Js::Undefined)
        }
        "dispatchEvent" => dispatch(caller, &node, arg(0)),
        "click" => {
            let event = new_event("click", true);
            dispatch(caller, &node, event).map(|_| Js::Undefined)
        }
        _ => return None,
    })
}

/// `new Event(type, {bubbles})`.
fn new_event(kind: &str, bubbles: bool) -> Js {
    let flag = |name: &'static str| {
        Js::function(move |_, this, _| {
            this.set(name, Js::Bool(true))?;
            Ok(Js::Undefined)
        })
    };
    Js::object(vec![
        ("type", Js::Str(kind.to_string())),
        ("bubbles", Js::Bool(bubbles)),
        ("defaultPrevented", Js::Bool(false)),
        ("target", Js::Null),
        ("currentTarget", Js::Null),
        ("preventDefault", flag("defaultPrevented")),
        ("stopPropagation", flag("__stopped")),
    ])
}

/// Dispatch an event at a node: its listeners for the event's type, then,
/// for one that bubbles, each ancestor's, until one stops it. Whether no
/// listener prevented its default.
fn dispatch(
    caller: &mut Caller<'_, Host>,
    target: &Rc<RefCell<Node>>,
    event: Js,
) -> Result<Js, Js> {
    let kind = event.get("type").map(|t| t.text()).unwrap_or_default();
    let bubbles = matches!(event.get("bubbles"), Some(Js::Bool(true)));
    event.set("target", Js::Node(target.clone()))?;
    let mut at = Some(target.clone());
    while let Some(node) = at {
        event.set("currentTarget", Js::Node(node.clone()))?;
        let listeners: Vec<(Js, bool)> = node
            .borrow()
            .listeners
            .iter()
            .filter(|(k, _, _)| *k == kind)
            .map(|(_, l, once)| (l.clone(), *once))
            .collect();
        for (listener, once) in listeners {
            if once {
                node.borrow_mut()
                    .listeners
                    .retain(|(k, l, _)| !(*k == kind && l.same(&listener)));
            }
            call_value(
                caller,
                &listener,
                &Js::Node(node.clone()),
                std::slice::from_ref(&event),
            )?;
        }
        if !bubbles || matches!(event.get("__stopped"), Some(Js::Bool(true))) {
            break;
        }
        at = node.borrow().parent.as_ref().and_then(|p| p.upgrade());
    }
    Ok(Js::Bool(!matches!(
        event.get("defaultPrevented"),
        Some(Js::Bool(true))
    )))
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
    /// Whether the host has JavaScript Promise Integration, so a program's
    /// `__main__` block may suspend on a promise (the loader's `start()`).
    pub jspi: bool,
    /// The settled promise `next_settled` handed over last.
    taken: Option<(bool, Js)>,
    /// The callbacks `requestAnimationFrame` asked to run at the next
    /// frame, by id, and the last id given.
    pub frames: Vec<(i32, Js)>,
    pub frame_ids: i32,
    /// What a callback JavaScript ran threw, as a browser reports it.
    pub uncaught: Vec<String>,
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
            jspi: true,
            taken: None,
            frames: Vec::new(),
            frame_ids: 0,
            uncaught: Vec::new(),
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
    let event = Js::function(|_, _, args| {
        let kind = args.first().map(Js::text).unwrap_or_default();
        let bubbles = matches!(
            args.get(1).and_then(|init| init.get("bubbles")),
            Some(Js::Bool(true))
        );
        Ok(new_event(&kind, bubbles))
    });
    // `fetch(url, init)`: a response from `fetch_routes` (an URL ending
    // `/__echo` answers what it was sent, as JSON), or a rejected promise for
    // any other, as a network failure.
    let fetch = Js::function(|_, _, args| {
        let url = args.first().map(Js::text).unwrap_or_default();
        let init = args.get(1).cloned().unwrap_or(Js::Undefined);
        let field = |name: &str| init.get(name).map(|v| v.text());
        let (status, status_text, body, kind) = if url.ends_with("/__echo") {
            let header = match init.get("headers") {
                Some(h @ Js::Obj(..)) => h.get("Content-Type").map(|v| v.text()),
                _ => None,
            };
            let echoed = serde_json::json!({
                "method": field("method").unwrap_or_else(|| "GET".to_string()),
                "body": field("body").unwrap_or_default(),
                "type": header.unwrap_or_default(),
            });
            (200, "OK", echoed.to_string(), "application/json")
        } else {
            match fetch_route(&url) {
                Some(route) => route,
                None => {
                    let failure = js_error("TypeError", "Failed to fetch");
                    return Ok(Js::Promise(Rc::new(RefCell::new(Some((true, failure))))));
                }
            }
        };
        Ok(Js::Promise(Rc::new(RefCell::new(Some((
            false,
            fetch_response(&url, status, status_text, &body, kind),
        ))))))
    });
    object
        .set(
            "assign",
            Js::function(|_, _, args| {
                let target = args.first().cloned().unwrap_or(Js::Undefined);
                for source in args.iter().skip(1) {
                    if let Js::Obj(props, None) = source {
                        for (k, v) in props.borrow().iter() {
                            target.set(k, v.clone())?;
                        }
                    }
                }
                Ok(target)
            }),
        )
        .unwrap();
    object
        .set(
            "fromEntries",
            Js::function(|_, _, args| {
                let out = Js::object(vec![]);
                if let Some(Js::Arr(pairs)) = args.first() {
                    for pair in pairs.borrow().iter() {
                        if let Js::Arr(kv) = pair {
                            let kv = kv.borrow();
                            out.set(&kv[0].text(), kv[1].clone())?;
                        }
                    }
                }
                Ok(out)
            }),
        )
        .unwrap();
    let reflect = Js::object(vec![
        ("apply", Js::function(|_, _, _| Ok(Js::Undefined))),
        ("ownKeys", Js::function(|_, _, _| Ok(Js::Undefined))),
    ]);
    let globals = Js::object(vec![
        ("fetch", fetch),
        ("Reflect", reflect),
        ("document", new_document()),
        ("Event", event),
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
    for (name, value) in graphics::globals() {
        globals.set(name, value).unwrap();
    }
    globals.set("globalThis", globals.clone()).unwrap();
    globals
}

/// The responses the tests' `fetch` knows: (status, status text, body,
/// content type) by URL.
fn fetch_route(url: &str) -> Option<(u16, &'static str, String, &'static str)> {
    match url {
        "https://api.test/scores" => Some((
            200,
            "OK",
            r#"{"ada": 7, "bo": 12}"#.to_string(),
            "application/json",
        )),
        "https://api.test/missing" => {
            Some((404, "Not Found", "nothing here".to_string(), "text/plain"))
        }
        _ => None,
    }
}

/// A `Response`: its status, headers, and a body `text()` reads once.
fn fetch_response(url: &str, status: u16, status_text: &str, body: &str, kind: &str) -> Js {
    let headers = {
        let kind = kind.to_string();
        Js::object(vec![(
            "entries",
            Js::function(move |_, _, _| {
                Ok(Js::Arr(Rc::new(RefCell::new(vec![Js::Arr(Rc::new(
                    RefCell::new(vec![
                        Js::Str("content-type".to_string()),
                        Js::Str(kind.clone()),
                    ]),
                ))]))))
            }),
        )])
    };
    let response = Js::object(vec![
        ("status", Js::Num(status as f64)),
        ("ok", Js::Bool((200..300).contains(&status))),
        ("statusText", Js::Str(status_text.to_string())),
        ("url", Js::Str(url.to_string())),
        ("redirected", Js::Bool(false)),
        ("type", Js::Str("basic".to_string())),
        ("bodyUsed", Js::Bool(false)),
        ("headers", headers),
        ("__body", Js::Str(body.to_string())),
    ]);
    let read = Js::function(|_, this, _| {
        if matches!(this.get("bodyUsed"), Some(Js::Bool(true))) {
            let failure = js_error(
                "TypeError",
                "Failed to execute 'text' on 'Response': body stream already read",
            );
            return Ok(Js::Promise(Rc::new(RefCell::new(Some((true, failure))))));
        }
        this.set("bodyUsed", Js::Bool(true))?;
        let body = this.get("__body").unwrap_or(Js::Str(String::new()));
        Ok(Js::Promise(Rc::new(RefCell::new(Some((false, body))))))
    });
    response.set("text", read).unwrap();
    let (url, status_text, body, kind) = (
        url.to_string(),
        status_text.to_string(),
        body.to_string(),
        kind.to_string(),
    );
    response
        .set(
            "clone",
            Js::function(move |_, _, _| {
                Ok(fetch_response(&url, status, &status_text, &body, &kind))
            }),
        )
        .unwrap();
    response
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
        (Js::PyFunc(p), "destroy") => destroy_python(caller, &p.clone()),
        // `Reflect.apply(f, this, args)`.
        (Js::Obj(..), "apply")
            if target.get("apply").is_some() && target.get("ownKeys").is_some() =>
        {
            let f = args.first().cloned().unwrap_or(Js::Undefined);
            let this = args.get(1).cloned().unwrap_or(Js::Undefined);
            let items = match args.get(2) {
                Some(Js::Arr(items)) => items.borrow().clone(),
                _ => Vec::new(),
            };
            call_value(caller, &f, &this, &items)
        }
        (Js::Node(_) | Js::ClassList(_), _) if DOM_METHODS.contains(&name) => {
            dom_method(caller, target, name, &args)
                .unwrap_or_else(|| Err(js_error("TypeError", format!("{name} is not a function"))))
        }
        (Js::Host(o), _) => {
            let o = o.clone();
            match o.call(target, caller.data_mut(), name, &args) {
                Some(answer) => answer,
                None => Err(js_error("TypeError", format!("{name} is not a function"))),
            }
        }
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
        Js::PyFunc(p) => call_python(caller, p, args),
        _ => Err(js_error("TypeError", "not a function")),
    }
}

/// A call of a Python function's proxy, as the loader makes it: through the
/// export `__waspy_call_<arity>`.
fn call_python(caller: &mut Caller<'_, Host>, p: &Rc<PyFn>, args: &[Js]) -> Result<Js, Js> {
    if !p.alive.get() {
        return Err(js_error("Error", "Object has already been destroyed"));
    }
    if args.len() != p.arity {
        let s = |n: usize| if n == 1 { "" } else { "s" };
        return Err(js_error(
            "PythonError",
            format!(
                "TypeError: the function takes {} positional argument{} but {} {} given",
                p.arity,
                s(p.arity),
                args.len(),
                if args.len() == 1 { "was" } else { "were" }
            ),
        ));
    }
    let name = format!("__waspy_call_{}", p.arity);
    let Some(Extern::Func(f)) = caller.get_export(&name) else {
        return Err(js_error("TypeError", format!("no export {name}")));
    };
    let mut params = vec![Value::I32(p.env)];
    for a in args {
        params.push(Value::I32(caller.data_mut().js.add(a.clone())));
    }
    let mut out = [Value::I32(0)];
    f.call(&mut *caller, &params, &mut out)
        .map_err(|e| js_error("RuntimeError", e))?;
    if p.once {
        destroy_python(caller, p)?;
    }
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

/// A Python function's proxy's `destroy()`: Python lets go of it.
fn destroy_python(caller: &mut Caller<'_, Host>, p: &Rc<PyFn>) -> Result<Js, Js> {
    if p.alive.replace(false) {
        let Some(Extern::Func(f)) = caller.get_export("__waspy_forget") else {
            return Err(js_error("TypeError", "no export __waspy_forget"));
        };
        let mut out = [Value::I32(0)];
        f.call(&mut *caller, &[Value::I32(p.id)], &mut out)
            .map_err(|e| js_error("RuntimeError", e))?;
    }
    Ok(Js::Undefined)
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
                if !matches!(fv, Js::Obj(_, Some(_)) | Js::PyFunc(_)) {
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
                    || matches!(target, Js::Module(_))
                    || (matches!(target, Js::Node(_) | Js::ClassList(_))
                        && DOM_METHODS.contains(&name.as_str()))
                    || (matches!(target, Js::PyFunc(_)) && name == "destroy");
                if let Js::Host(o) = &target {
                    if !o.has_method(&name) {
                        return if o.get(&name).is_some() { -3 } else { -1 };
                    }
                } else if !builtin {
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
                if !matches!(fv, Js::Obj(_, Some(_)) | Js::PyFunc(_)) {
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
                Js::Obj(_, Some(_)) | Js::PyFunc(_) => 7,
                Js::Arr(_) => 8,
                Js::Obj(..)
                | Js::Error(_)
                | Js::Module(_)
                | Js::Iter(_)
                | Js::Promise(_)
                | Js::Node(_)
                | Js::ClassList(_)
                | Js::Host(_) => 9,
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
                Js::Str(s) => s.chars().count() as i32,
                Js::Host(o) => o.length().map_or(-1, |n| n as i32),
                v @ (Js::Obj(..) | Js::ClassList(_)) => match v.get("length") {
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
                // A string is indexed by code points, as Pyodide's str is.
                let (items, missing) = match w.value(h) {
                    Js::Arr(items) => (items.borrow().clone(), -1),
                    Js::Str(s) => (s.chars().map(|ch| Js::Str(ch.to_string())).collect(), -4),
                    Js::Host(o) => match o.length() {
                        Some(n) => ((0..n).map(|k| o.item(k)).collect(), -1),
                        None => return -3,
                    },
                    _ => return -3,
                };
                let at = if i < 0 { i + items.len() as i32 } else { i };
                if at < 0 || at as usize >= items.len() {
                    return missing;
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
                Js::ClassList(n) => {
                    let classes = n.borrow().classes().into_iter().map(Js::Str).collect();
                    w.add(Js::Iter(Rc::new(RefCell::new((classes, 0)))))
                }
                Js::Host(o) => match o.length() {
                    Some(n) => {
                        let items = (0..n).map(|k| o.item(k)).collect();
                        w.add(Js::Iter(Rc::new(RefCell::new((items, 0)))))
                    }
                    None => -3,
                },
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
            "function",
            |mut c: Caller<'_, Host>, env: i32, arity: i32, once: i32, id: i32| -> i32 {
                c.data_mut().js.add(Js::PyFunc(Rc::new(PyFn {
                    env,
                    arity: arity as usize,
                    once: once != 0,
                    id,
                    alive: std::cell::Cell::new(true),
                })))
            },
        )
        .unwrap();
    // JSPI, as the loader's `start()` gives it: suspending hands back the
    // promises that settled, or moves the clock on to the next timer.
    linker
        .func_wrap(m, "can_suspend", |c: Caller<'_, Host>| -> i32 {
            c.data().js.jspi as i32
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "suspend",
            |mut c: Caller<'_, Host>, ms: wasmi::core::F64| -> i32 {
                let ms = f64::from(ms);
                if !c.data().js.settled.is_empty() {
                    return 0;
                }
                // The next animation frame, when one is asked for and falls
                // before the program's next timer: its callbacks run with
                // the frame's time, those they ask for at the frame after.
                if !c.data().js.frames.is_empty() {
                    let now = c.data().monotonic_ns;
                    let at = (now / graphics::FRAME_NS + 1) * graphics::FRAME_NS;
                    if ms < 0.0 || at as f64 <= now as f64 + ms * 1e6 {
                        c.data_mut().monotonic_ns = at;
                        let due = std::mem::take(&mut c.data_mut().js.frames);
                        for (_, callback) in due {
                            let time = Js::Num(at as f64 / 1e6);
                            if let Err(e) = call_value(&mut c, &callback, &Js::Undefined, &[time]) {
                                c.data_mut().js.uncaught.push(e.text());
                            }
                        }
                        return 0;
                    }
                }
                assert!(
                    ms >= 0.0,
                    "the program waits for what nothing will bring: no promise is pending \
                     and no animation frame asked for"
                );
                c.data_mut().monotonic_ns += (ms * 1e6).ceil() as u64;
                0
            },
        )
        .unwrap();
    linker
        .func_wrap(m, "next_settled", |mut c: Caller<'_, Host>| -> i32 {
            let w = &mut c.data_mut().js;
            if w.settled.is_empty() {
                w.taken = None;
                return -1;
            }
            let (token, rejected, value) = w.settled.remove(0);
            w.taken = Some((rejected, value));
            token
        })
        .unwrap();
    linker
        .func_wrap(m, "settled_failed", |c: Caller<'_, Host>| -> i32 {
            c.data().js.taken.as_ref().is_some_and(|t| t.0) as i32
        })
        .unwrap();
    linker
        .func_wrap(m, "settled_value", |mut c: Caller<'_, Host>| -> i32 {
            let w = &mut c.data_mut().js;
            let value = w.taken.as_ref().map_or(Js::Undefined, |t| t.1.clone());
            w.add(value)
        })
        .unwrap();
    linker
        .func_wrap(
            m,
            "set_item",
            |mut c: Caller<'_, Host>, h: i32, i: i32, v: i32| -> i32 {
                let w = &mut c.data_mut().js;
                let value = w.value(v);
                match w.value(h) {
                    Js::Str(_) => -4,
                    Js::Arr(items) => {
                        let mut items = items.borrow_mut();
                        let at = if i < 0 { i + items.len() as i32 } else { i };
                        // Before the start, a stray property no item reads.
                        if at < 0 {
                            return 0;
                        }
                        let at = at as usize;
                        if at >= items.len() {
                            items.resize(at + 1, Js::Undefined);
                        }
                        items[at] = value;
                        0
                    }
                    Js::Host(o) => match o.length() {
                        Some(n) => {
                            let at = if i < 0 { i + n as i32 } else { i };
                            // Outside it, a typed array ignores the store.
                            if at >= 0 && (at as usize) < n {
                                o.set_item(at as usize, &value);
                            }
                            0
                        }
                        None => -3,
                    },
                    _ => -3,
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "buffer_bytes",
            |mut c: Caller<'_, Host>, h: i32| -> i32 {
                let w = &mut c.data_mut().js;
                let value = w.value(h);
                match graphics::host::<graphics::TypedArray>(&value) {
                    Some(t) => {
                        w.bytes = t.bytes();
                        w.bytes.len() as i32
                    }
                    None => -1,
                }
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "assign",
            |mut c: Caller<'_, Host>, h: i32, at: i32, n: i32| -> i32 {
                let memory = memory_of(&mut c);
                let bytes = memory.data(&c)[at as usize..(at + n) as usize].to_vec();
                let w = &mut c.data_mut().js;
                let value = w.value(h);
                let Some(t) = graphics::host::<graphics::TypedArray>(&value) else {
                    return -1;
                };
                if !matches!(t.kind, graphics::Kind::U8 | graphics::Kind::U8Clamped) {
                    w.last_error = Js::Str(
                        "TypedArray and memorybuffer have incompatible formats".to_string(),
                    );
                    return -7;
                }
                let len = t.data.borrow().len();
                if len != n as usize {
                    w.last_error = Js::Str(format!(
                        "cannot assign to TypedArray of length {n} from buffer of length {len}"
                    ));
                    return -7;
                }
                *t.data.borrow_mut() = bytes.into_iter().map(f64::from).collect();
                0
            },
        )
        .unwrap();
    linker
        .func_wrap(
            m,
            "bytes",
            |mut c: Caller<'_, Host>, at: i32, n: i32| -> i32 {
                let memory = memory_of(&mut c);
                let bytes = memory.data(&c)[at as usize..(at + n) as usize].to_vec();
                let array = graphics::TypedArray::make(
                    graphics::Kind::U8,
                    bytes.into_iter().map(f64::from),
                );
                c.data_mut().js.add(array)
            },
        )
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
