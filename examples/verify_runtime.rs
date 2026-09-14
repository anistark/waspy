//! Build the 0.16.0 end-to-end programs for verification under a real
//! WebAssembly runtime, and emit a manifest of what to check.
//!
//! The integration suite already asserts these programs' results, but it does
//! so with `wasmi` inside the test process. This driver exists so the same
//! results can be asserted by engines a user would actually ship against
//! (Node's V8 and `wasmtime`), which is the 0.16.0 "each runs under a real
//! runtime, not only the test harness" task.
//!
//! ## How the checks travel
//!
//! The expected values live in Python, not in the runners. Each program is
//! compiled twice, with a checker suffix from `tests/fixtures/runtime/`
//! appended to its source: the suffix adds `runtime_check_*` functions that
//! compare the program's own results against what CPython answers and return
//! 1 or 0. A runner then only has to call every `runtime_check_*` export and
//! require the right answer.
//!
//! That indirection buys one thing: the `wasmtime` CLI can invoke an export
//! but cannot read the module's memory, so a host-side assertion on a returned
//! `str` is impossible there. Comparing inside the module keeps Node and
//! `wasmtime` checking exactly the same thing. A `runtime_check_negative_*`
//! function must answer 0, which is what keeps a checker honest: if string
//! comparison itself regressed to a constant `True`, the negative control
//! fails and the run is rejected.
//!
//! Both the unoptimized and the Binaryen-optimized binary are emitted and
//! checked, since the optimized one is what a user ships and Binaryen has
//! broken one of these programs before.
//!
//! Run it through `just verify-runtime` rather than directly; the runners in
//! `scripts/` consume the manifest this writes.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use waspy::{
    compile_python_file_with_options, compile_python_to_wasm_with_options, CompilerOptions,
};

/// Where the binaries and the manifest are written.
const OUT_DIR: &str = "examples/output/runtime";

/// A whole program to verify: either a single file or a multi-file project
/// compiled from its entry file.
struct Program {
    /// Name used for the emitted `.wasm` files and in the manifest.
    name: &'static str,
    /// The program's source, relative to the repository root.
    source: Source,
    /// The checker suffix appended to the source before compiling.
    checks: &'static str,
}

enum Source {
    /// One `examples/*.py` file, compiled from a source string.
    File(&'static str),
    /// A project directory, compiled from `<dir>/main.py` with its imports
    /// resolved from disk. The directory is copied into the output tree so the
    /// checker suffix never touches the program a reader sees.
    Project(&'static str),
}

const PROGRAMS: &[Program] = &[
    Program {
        name: "shopping_cart",
        source: Source::File("examples/shopping_cart.py"),
        checks: "tests/fixtures/runtime/shopping_cart_checks.py",
    },
    Program {
        name: "text_report",
        source: Source::File("examples/text_report.py"),
        checks: "tests/fixtures/runtime/text_report_checks.py",
    },
    Program {
        name: "library_project",
        source: Source::Project("examples/library_project"),
        checks: "tests/fixtures/runtime/library_project_checks.py",
    },
];

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out_dir = root.join(OUT_DIR);
    // A stale binary from an earlier build would be verified as if it were
    // current, so start from an empty tree.
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).context("clear the runtime output directory")?;
    }
    fs::create_dir_all(&out_dir).context("create the runtime output directory")?;

    let mut entries = Vec::new();
    for program in PROGRAMS {
        let checks = read(&root.join(program.checks))?;
        let names = check_function_names(&checks);
        anyhow::ensure!(
            !names.is_empty(),
            "{} defines no runtime_check_* functions",
            program.checks
        );
        anyhow::ensure!(
            names
                .iter()
                .any(|n| n.starts_with("runtime_check_negative_")),
            "{} has no negative control; a checker that cannot fail proves nothing",
            program.checks
        );

        for optimize in [false, true] {
            let wasm = build(root, program, &checks, optimize)?;
            let file_name = if optimize {
                format!("{}.opt.wasm", program.name)
            } else {
                format!("{}.wasm", program.name)
            };
            let path = out_dir.join(&file_name);
            fs::write(&path, &wasm).with_context(|| format!("write {}", path.display()))?;
            println!(
                "built {} ({} bytes, {})",
                file_name,
                wasm.len(),
                if optimize { "optimized" } else { "unoptimized" }
            );
            entries.push(serde_json::json!({
                "program": program.name,
                "optimized": optimize,
                "wasm": format!("{OUT_DIR}/{file_name}"),
                "checks": names
                    .iter()
                    .map(|name| serde_json::json!({
                        "function": name,
                        "expect": i32::from(!name.starts_with("runtime_check_negative_")),
                    }))
                    .collect::<Vec<_>>(),
            }));
        }
    }

    let manifest = out_dir.join("manifest.json");
    let total: usize = entries
        .iter()
        .map(|e| e["checks"].as_array().map_or(0, Vec::len))
        .sum();
    fs::write(
        &manifest,
        serde_json::to_string_pretty(&serde_json::json!({ "modules": entries }))?,
    )
    .with_context(|| format!("write {}", manifest.display()))?;
    println!(
        "\n{} modules, {total} checks, manifest at {}",
        entries.len(),
        manifest.display()
    );
    Ok(())
}

/// Compile one program with its checker suffix appended.
fn build(root: &Path, program: &Program, checks: &str, optimize: bool) -> Result<Vec<u8>> {
    let options = CompilerOptions {
        optimize,
        ..CompilerOptions::default()
    };
    match program.source {
        Source::File(path) => {
            let source = read(&root.join(path))? + checks;
            compile_python_to_wasm_with_options(&source, &options)
                .with_context(|| format!("compile {path} with its runtime checks"))
        }
        Source::Project(dir) => {
            let src_dir = root.join(dir);
            let dest = root.join(OUT_DIR).join(program.name);
            copy_python_files(&src_dir, &dest)?;
            let entry = dest.join("main.py");
            let main = read(&entry)? + checks;
            fs::write(&entry, main).with_context(|| format!("write {}", entry.display()))?;
            compile_python_file_with_options(&entry, &options)
                .with_context(|| format!("compile {dir} with its runtime checks"))
        }
    }
}

/// Copy a project's `.py` files into `dest`, flat: user module imports resolve
/// against the entry file's own directory, so a flat copy resolves the same way
/// the original does.
fn copy_python_files(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("create {}", dest.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read {}", src.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("py") {
            let target = dest.join(path.file_name().expect("file name"));
            fs::copy(&path, &target).with_context(|| format!("copy {}", path.display()))?;
        }
    }
    Ok(())
}

/// The `runtime_check_*` function names a checker suffix defines, in source
/// order. Reading them out of the Python means adding a check to the suffix is
/// all it takes for both runners to pick it up.
fn check_function_names(checks: &str) -> Vec<String> {
    checks
        .lines()
        .filter_map(|line| line.strip_prefix("def runtime_check_"))
        .filter_map(|rest| rest.split('(').next())
        .map(|name| format!("runtime_check_{name}"))
        .collect()
}

fn read(path: &PathBuf) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}
