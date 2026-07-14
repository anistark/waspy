//! Shared helpers for the integration test suite.
//!
//! These drive the public API exactly as a downstream user would: compile a
//! Python source string (or a bundled `examples/*.py` file) to WASM, validate
//! it, instantiate it with `wasmi`, and call exported functions. Compiled
//! waspy modules have no host imports — except programs using file I/O, which
//! import the four `waspy_host` functions — so the broad-sweep linker defines
//! no-op stubs for those (unused definitions are ignored) and instantiation
//! also exercises the start and data sections.
//!
//! The panicking helpers (`compile`, `instantiate`, `call_i32`, …) mirror the
//! in-crate test helpers in `src/lib.rs` and are for assertions with a known
//! expected result. The `try_*` variants return `Result` so a broad sweep can
//! collect every failure instead of aborting on the first.

use std::path::{Path, PathBuf};

use wasmi::{Engine, Instance, Linker, Store, Value};
use waspy::{
    compile_multiple_python_files_with_options, compile_python_to_wasm_with_options,
    CompilerOptions,
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
/// the data section. The linker carries no-op stubs for the `waspy_host` file
/// I/O imports (open -> -1, read -> 0, write -> its length, close -> 0) so
/// examples using `open()` instantiate too; modules without file I/O import
/// nothing and ignore the stubs.
pub fn try_instantiate(wasm: &[u8]) -> Result<(), String> {
    let engine = Engine::default();
    let module = wasmi::Module::new(&engine, wasm).map_err(|e| format!("validate: {e}"))?;
    let mut store = Store::new(&engine, ());
    let mut linker = Linker::<()>::new(&engine);
    linker
        .func_wrap("waspy_host", "open", |_: i32, _: i32, _: i32| -> i32 { -1 })
        .and_then(|l| l.func_wrap("waspy_host", "read", |_: i32, _: i32, _: i32| -> i32 { 0 }))
        .and_then(|l| {
            l.func_wrap("waspy_host", "write", |_: i32, _: i32, len: i32| -> i32 {
                len
            })
        })
        .and_then(|l| l.func_wrap("waspy_host", "close", |_: i32| -> i32 { 0 }))
        .map_err(|e| format!("linker: {e}"))?;
    linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("instantiate: {e}"))?
        .start(&mut store)
        .map_err(|e| format!("start: {e}"))?;
    Ok(())
}

/// Compile several Python files into one module, returning the error as a
/// string on failure. `sources` is `(file_name, source)` pairs.
pub fn try_compile_multi(sources: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let options = CompilerOptions {
        optimize: false,
        ..CompilerOptions::default()
    };
    compile_multiple_python_files_with_options(sources, &options).map_err(|e| e.to_string())
}

/// Compile a source string, asserting success.
pub fn compile(source: &str) -> Vec<u8> {
    try_compile(source).expect("compilation")
}

/// Instantiate already-built WASM bytes, asserting validation and start
/// succeed. Returns the live instance and store so a test can call exports.
pub fn instantiate_wasm(wasm: &[u8]) -> (Instance, Store<()>) {
    let engine = Engine::default();
    let module = wasmi::Module::new(&engine, wasm).expect("valid wasm module");
    let mut store = Store::new(&engine, ());
    let instance = Linker::<()>::new(&engine)
        .instantiate(&mut store, &module)
        .expect("instantiation")
        .start(&mut store)
        .expect("start");
    (instance, store)
}

/// Compile + instantiate a source string, returning the live instance and
/// store so a test can call exported functions.
pub fn instantiate(source: &str) -> (Instance, Store<()>) {
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
