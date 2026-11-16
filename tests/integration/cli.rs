//! The `waspy` command line: build, check, and run, as a user runs them.

use std::path::PathBuf;
use std::process::Command;

fn waspy() -> Command {
    Command::new(env!("CARGO_BIN_EXE_waspy"))
}

/// A fresh directory for one test's files.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("waspy-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

#[test]
fn version_and_help() {
    let out = waspy().arg("--version").output().expect("waspy runs");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("waspy {}", env!("CARGO_PKG_VERSION"))
    );
    let out = waspy().arg("help").output().expect("waspy runs");
    assert!(String::from_utf8_lossy(&out.stdout).contains("waspy check <path>"));
    let out = waspy().arg("frobnicate").output().expect("waspy runs");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn build_writes_a_module() {
    let dir = scratch("build");
    let src = dir.join("app.py");
    std::fs::write(&src, "def double(x: int) -> int:\n    return x * 2\n").unwrap();
    let out = waspy()
        .args(["build", src.to_str().unwrap(), "--no-optimize"])
        .output()
        .expect("waspy runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let wasm = std::fs::read(dir.join("app.wasm")).expect("app.wasm written");
    assert_eq!(&wasm[..4], b"\0asm");
}

/// `check` reports each refusal as `file:line:column: error: message` and
/// exits with status 1; a program that compiles is `ok`.
#[test]
fn check_reports_located_errors() {
    let dir = scratch("check");
    let good = dir.join("good.py");
    std::fs::write(&good, "print(1 + 2)\n").unwrap();
    let out = waspy()
        .args(["check", good.to_str().unwrap()])
        .output()
        .expect("waspy runs");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("good.py: ok"));

    let bad = dir.join("bad.py");
    std::fs::write(
        &bad,
        "x = 1\nif x:\n    def g() -> int:\n        return 1\n",
    )
    .unwrap();
    let out = waspy()
        .args(["check", bad.to_str().unwrap()])
        .output()
        .expect("waspy runs");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("bad.py:3:"), "{err}");
    assert!(
        err.contains(": error: the function 'g' is defined inside"),
        "{err}"
    );
}

/// `run` compiles and runs with wasmtime, passing the arguments on and
/// exiting with the program's status. Skipped where wasmtime is not
/// installed.
#[test]
fn run_passes_arguments_and_status() {
    if Command::new("wasmtime").arg("--version").output().is_err() {
        eprintln!("wasmtime is not installed; skipping");
        return;
    }
    let dir = scratch("run");
    let src = dir.join("greet.py");
    std::fs::write(
        &src,
        "import sys\nprint('hello', sys.argv[1])\nsys.exit(3)\n",
    )
    .unwrap();
    let out = waspy()
        .args(["run", src.to_str().unwrap(), "bob"])
        .current_dir(&dir)
        .output()
        .expect("waspy runs");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hello bob\n");
    assert_eq!(out.status.code(), Some(3));
}
