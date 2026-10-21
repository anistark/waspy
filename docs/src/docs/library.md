---
title: Using the library
eyebrow: Start here
description: "Every entry point, the compiler options, and compiling several files or a whole project."
---

Waspy is a Rust library. Every entry point returns the WebAssembly binary as `Vec<u8>`, or an `anyhow::Error` describing what was refused and where.

## Entry points

| Function | Compiles |
|------|---------|
| `compile_python_to_wasm(source)` | one source string |
| `compile_python_to_wasm_with_options(source, &options)` | one source string, with options |
| `compile_python_file(path, optimize)` | an entry file, resolving its imports of sibling `.py` files from disk, transitively |
| `compile_multiple_python_files(&[(name, source)], optimize)` | several sources into one module |
| `compile_python_project(dir, optimize)` | a project directory |
| `get_python_file_metadata(source)` | nothing: returns each function's signature |

Each `compile_*` function has a `_with_options` form taking `&CompilerOptions`.

## Options

Build `CompilerOptions` with struct-update syntax over its default, so a new option never breaks your code:

```rust
use waspy::{compile_python_to_wasm_with_options, CompilerOptions, Verbosity};

let options = CompilerOptions {
    optimize: false,               // skip the Binaryen pass (default: true)
    verbosity: Verbosity::Verbose, // Quiet, Normal (default), Verbose, Debug
    ..CompilerOptions::default()
};
let wasm = compile_python_to_wasm_with_options(source, &options)?;
```

- `optimize` runs Binaryen over the output. The unoptimized module is already valid and correct; optimization only shrinks and speeds it up, at several times the cost of the rest of the pipeline, so it is worth skipping in a fast edit loop ([what it costs](/docs/programs/#what-it-costs)).
- `verbosity` sets how much the compiler logs. `Verbosity::from_flags(verbose, debug)` maps a tool's own `--verbose` and `--debug` flags onto it.
- `waspy_host_file_io` (deprecated) compiles `open()` to the older `waspy_host` imports instead of WASI: see [The `waspy_host` interface](/docs/running/#the-deprecated-waspy-host-interface).

## Building without Binaryen

Binaryen is a native C++ library behind the default `optimize` feature. Turn the feature off to build waspy for a target with no C++ toolchain, such as `wasm32-unknown-unknown`; `optimize` then has no effect and the output is the unoptimized module. The [playground](/playground/) runs waspy built this way.

```toml
[dependencies]
waspy = { version = "{{ site.version }}", default-features = false }
```

## Several files and projects

```rust
use waspy::{compile_multiple_python_files, compile_python_file, compile_python_project};

// Sources merged into one module, sharing one namespace
let sources = vec![
    ("math.py", "def add(a: int, b: int) -> int:\n    return a + b"),
    ("main.py", "def compute(x: int) -> int:\n    return add(x, 10)"),
];
let wasm = compile_multiple_python_files(&sources, true)?;

// An entry file: `import mod` finds the sibling mod.py, transitively
let wasm = compile_python_file("app/main.py", true)?;

// A whole project directory
let wasm = compile_python_project("./my_python_project", true)?;
```
