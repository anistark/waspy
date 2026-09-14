//! Compile-time and module-size baseline for the end-to-end programs.
//!
//! The 0.16.0 milestone asks for a rough baseline so 1.0 has something to
//! compare against. "Rough" is the operative word: these are wall-clock
//! numbers from one machine, and the point is to notice an order-of-magnitude
//! regression, not to defend a percentage.
//!
//! Each program is compiled `RUNS` times per configuration and the **median**
//! is reported, which throws out the first-run page faults and the occasional
//! scheduler hiccup without pretending to statistical rigour. Both
//! configurations are measured, since Binaryen dominates the optimized number:
//!
//! - unoptimized: parse, lower, codegen
//! - optimized: the above plus the Binaryen pass
//!
//! Build it in release, or the numbers are meaningless:
//!
//! ```sh
//! just benchmark
//! ```

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use waspy::{compile_python_file_with_options, CompilerOptions};

/// Compilations per configuration. The median of nine is stable enough to
/// compare across releases and still runs in a couple of seconds.
const RUNS: usize = 9;

/// The programs the baseline covers: the three end-to-end programs the
/// milestone is built around, plus the feature example that produces the
/// largest module, as a second data point at a different shape.
const PROGRAMS: &[(&str, &str)] = &[
    ("shopping_cart", "examples/shopping_cart.py"),
    ("text_report", "examples/text_report.py"),
    ("library_project", "examples/library_project/main.py"),
    ("nested_collections", "examples/nested_collections.py"),
];

struct Row {
    name: &'static str,
    source_bytes: u64,
    unopt: Measurement,
    opt: Measurement,
}

struct Measurement {
    median: Duration,
    bytes: usize,
}

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if cfg!(debug_assertions) {
        eprintln!(
            "warning: this is a debug build, so the timings mean nothing. \
             Run `just benchmark`."
        );
    }

    let mut rows = Vec::new();
    for (name, path) in PROGRAMS {
        let entry = root.join(path);
        let source_bytes = std::fs::metadata(&entry)
            .with_context(|| format!("stat {path}"))?
            .len();
        // A multi-file program's source size is the whole project, since that
        // is what the compile time covers.
        let source_bytes = if entry.parent().map(|p| p != root.join("examples")) == Some(true) {
            project_source_bytes(entry.parent().expect("parent"))?
        } else {
            source_bytes
        };

        let unopt = measure(&entry, false)?;
        let opt = measure(&entry, true)?;
        println!(
            "{name:<20} {:>7} src  {:>8.2?} -> {:>6} B   {:>8.2?} -> {:>6} B (opt)",
            source_bytes, unopt.median, unopt.bytes, opt.median, opt.bytes
        );
        rows.push(Row {
            name,
            source_bytes,
            unopt,
            opt,
        });
    }

    print_markdown(&rows);
    Ok(())
}

/// Compile `entry` `RUNS` times and return the median wall-clock time along
/// with the module size, which is deterministic across runs.
fn measure(entry: &Path, optimize: bool) -> Result<Measurement> {
    let options = CompilerOptions {
        optimize,
        verbosity: waspy::Verbosity::Quiet,
    };

    let mut times = Vec::with_capacity(RUNS);
    let mut bytes = 0;
    for _ in 0..RUNS {
        let start = Instant::now();
        let wasm = compile_python_file_with_options(entry, &options)
            .with_context(|| format!("compile {}", entry.display()))?;
        times.push(start.elapsed());
        bytes = wasm.len();
    }
    times.sort_unstable();
    Ok(Measurement {
        median: times[RUNS / 2],
        bytes,
    })
}

/// Total bytes of every `.py` file in a project directory.
fn project_source_bytes(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("py") {
            total += std::fs::metadata(&path)?.len();
        }
    }
    Ok(total)
}

/// The table to paste into `CHANGELOG.md`, so the recorded baseline and the
/// tool that produced it never drift apart.
fn print_markdown(rows: &[Row]) {
    println!("\n--- for CHANGELOG.md ---\n");
    println!("| Program | Source | Compile | Module | Compile (opt) | Module (opt) |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for row in rows {
        println!(
            "| `{}` | {} | {} | {} | {} | {} |",
            row.name,
            bytes(row.source_bytes as usize),
            millis(row.unopt.median),
            bytes(row.unopt.bytes),
            millis(row.opt.median),
            bytes(row.opt.bytes),
        );
    }
}

fn millis(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn bytes(n: usize) -> String {
    if n >= 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}
