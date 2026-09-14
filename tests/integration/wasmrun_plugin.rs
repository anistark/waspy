//! The wasmrun plugin path, driven end to end.
//!
//! `README.md` points users at [wasmrun](https://github.com/anistark/wasmrun)
//! as the way to run a compiled module, so the plugin is a shipped entry point
//! and not a side door: whatever the library can compile, the plugin has to
//! compile the same way, write it where it says it did, and produce a module
//! that runs.
//!
//! `examples/plugin_test.rs` exercised the plugin before this, but it *printed*
//! its findings: a failed build printed a cross and the process still exited 0,
//! and the module it produced was never instantiated. It also built a
//! single-feature demo. These tests take one of the end-to-end programs
//! through `WasmBuilder::build`, assert the file lands where `BuildResult` says
//! it does, and then run it and check the answers against CPython's, the same
//! values `integration_coverage` asserts for the library path.
//!
//! Behind the `wasm-plugin` feature, like the plugin itself. `cargo test
//! --all-features` includes it; a default-feature build skips the binary.

#[path = "../utils/harness.rs"]
mod harness;

use std::path::{Path, PathBuf};

use harness::{call_instance_i32, examples_dir, instantiate_wasm, read_str};
use waspy::wasmrun::{BuildConfig, OptimizationLevel, Plugin, WaspyPlugin};

/// A build directory of this test's own, so a run never reads a module an
/// earlier one left behind.
fn output_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples/output/plugin_tests")
        .join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear the plugin test output directory");
    }
    dir
}

/// Build one input through the plugin and return the bytes it wrote to disk,
/// asserting the path in `BuildResult` is the file it actually produced.
fn build_through_plugin(input: &Path, name: &str, optimization: OptimizationLevel) -> Vec<u8> {
    let dir = output_dir(name);
    let builder = WaspyPlugin::new().get_builder();
    let config = BuildConfig {
        input: input.to_string_lossy().to_string(),
        output_dir: dir.to_string_lossy().to_string(),
        optimization,
        target_type: "wasm".to_string(),
        verbose: false,
        watch: false,
    };

    let result = builder
        .build(&config)
        .unwrap_or_else(|e| panic!("plugin build of {}: {e}", input.display()));
    assert_eq!(result.language, "python");
    assert!(
        result.file_size > 0,
        "plugin reported an empty module for {}",
        input.display()
    );

    let written = Path::new(&result.output_path);
    assert!(
        written.exists(),
        "plugin reported {} but wrote nothing there",
        result.output_path
    );
    let bytes = std::fs::read(written).expect("read the plugin's output");
    assert_eq!(
        bytes.len() as u64,
        result.file_size,
        "BuildResult.file_size disagrees with the file on disk"
    );
    assert_eq!(&bytes[0..4], b"\0asm", "the output is not a WASM module");
    bytes
}

/// The shopping cart through the plugin, asserted against the same values
/// `integration_coverage` asserts for the library path. This is the task the
/// milestone names: the plugin compiles and runs one of the whole programs.
#[test]
fn the_plugin_compiles_and_runs_a_whole_program() {
    let wasm = build_through_plugin(
        &examples_dir().join("shopping_cart.py"),
        "shopping_cart",
        OptimizationLevel::Release,
    );
    let (instance, mut store) = instantiate_wasm(&wasm);

    // 9.99*3 + 24.50*2 + 5.00*4 = 98.97, which earns the 5% tier: 94.0215.
    let checkout = instance
        .get_func(&store, "checkout")
        .expect("exported `checkout`");
    let mut results = [wasmi::Value::F64(0.0.into())];
    checkout
        .call(&mut store, &[], &mut results)
        .expect("call checkout");
    let total = match results[0] {
        wasmi::Value::F64(v) => f64::from(v),
        ref other => panic!("expected an f64 total, got {other:?}"),
    };
    assert!(
        (total - 94.0215).abs() < 1e-9,
        "checkout() answered {total}, CPython answers 94.0215"
    );

    assert_eq!(call_instance_i32(&instance, &mut store, "item_count"), 205);
}

/// A multi-file project through the plugin, entry file first: the plugin's
/// single-file branch compiles by path, so the entry's imports of sibling
/// modules have to resolve from disk and link into the one module it writes.
#[test]
fn the_plugin_resolves_a_multi_file_project() {
    let wasm = build_through_plugin(
        &examples_dir().join("library_project").join("main.py"),
        "library_project",
        OptimizationLevel::Release,
    );
    let (instance, mut store) = instantiate_wasm(&wasm);

    assert_eq!(call_instance_i32(&instance, &mut store, "catalog_size"), 3);
    assert_eq!(call_instance_i32(&instance, &mut store, "total_copies"), 6);
    assert_eq!(call_instance_i32(&instance, &mut store, "oldest"), 1965);

    // A string crossing two module boundaries: `reports` formats a
    // `models.Book` that `catalog` built.
    let first = call_instance_i32(&instance, &mut store, "first_line");
    assert_eq!(read_str(&instance, &store, first), "Dune by Herbert (1965)");
}

/// The plugin's optimization levels both produce a module that runs and gives
/// the same answer. `Debug` maps to an unoptimized build and `Release` runs
/// Binaryen, so this is the plugin's own version of the check
/// `just verify-runtime` makes for the library path.
#[test]
fn both_plugin_optimization_levels_answer_the_same() {
    for (level, name) in [
        (OptimizationLevel::Debug, "cart_debug"),
        (OptimizationLevel::Release, "cart_release"),
    ] {
        let wasm = build_through_plugin(&examples_dir().join("shopping_cart.py"), name, level);
        let (instance, mut store) = instantiate_wasm(&wasm);
        assert_eq!(
            call_instance_i32(&instance, &mut store, "item_count"),
            205,
            "{name} answered differently"
        );
    }
}

/// A program the compiler refuses must fail the plugin build too, rather than
/// writing a module or reporting success. The plugin returns its own error
/// type, so this checks the failure survives the conversion with its cause
/// intact.
#[test]
fn the_plugin_reports_a_compile_failure() {
    let dir = output_dir("broken");
    std::fs::create_dir_all(&dir).expect("create the input directory");
    let input = dir.join("broken.py");
    std::fs::write(&input, "def f() -> int\n    return 1\n").expect("write the broken input");

    let builder = WaspyPlugin::new().get_builder();
    let config = BuildConfig {
        input: input.to_string_lossy().to_string(),
        output_dir: dir.join("out").to_string_lossy().to_string(),
        optimization: OptimizationLevel::Debug,
        target_type: "wasm".to_string(),
        verbose: false,
        watch: false,
    };

    let err = builder
        .build(&config)
        .expect_err("a file that cannot be parsed must fail the plugin build")
        .to_string();
    assert!(
        err.contains("python"),
        "the error must name the language, got: {err}"
    );
    assert!(
        !dir.join("out").join("broken.wasm").exists(),
        "a failed build must not leave a module behind"
    );
}

/// The plugin's own declarations, which wasmrun reads to decide whether to
/// hand it a project. These are part of the shipped contract, so a change to
/// them is a change wasmrun sees.
#[test]
fn the_plugin_declares_what_it_handles() {
    let plugin = WaspyPlugin::new();
    let info = plugin.info();
    assert_eq!(info.name, "waspy");
    assert!(info.extensions.iter().any(|e| e == "py"));
    assert!(info.capabilities.compile_wasm);
    assert!(
        plugin.get_builder().check_dependencies().is_empty(),
        "the plugin is self-contained and must need no external tools"
    );

    let cart = examples_dir().join("shopping_cart.py");
    assert!(plugin.can_handle_project(&cart.to_string_lossy()));
    assert!(plugin.can_handle_project(&examples_dir().to_string_lossy()));

    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    assert!(!plugin.can_handle_project(&manifest.to_string_lossy()));
    assert!(!plugin.can_handle_project("nonexistent.py"));
}
