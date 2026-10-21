---
title: Getting started
eyebrow: Documentation
description: Install waspy, compile your first Python function to WebAssembly, and run it.
---

Waspy compiles a typed subset of Python ahead of time into a standalone WebAssembly module, with no interpreter or VM in the output. It is a Rust library: you hand it Python source and get back the bytes of a `.wasm` module.

The rule it holds to: a construct either gives CPython's answer or is refused at compile time, with a located error and a hint. To try it without installing anything, open the [playground](/playground/).

## Install

Add the crate to a Rust project:

```sh
cargo add waspy
```

Or add it to `Cargo.toml` by hand:

```toml
[dependencies]
waspy = "{{ site.version }}"
```

The default `optimize` feature runs Binaryen, a native C++ library, over the output. To build waspy where there is no C++ toolchain, see [Building without Binaryen](/docs/library/#building-without-binaryen).

## Compile a function

```rust
use waspy::compile_python_to_wasm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let python_code = r#"
def factorial(n: int) -> int:
    result = 1
    i = 1
    while i <= n:
        result = result * i
        i = i + 1
    return result
"#;

    let wasm = compile_python_to_wasm(python_code)?;
    std::fs::write("factorial.wasm", &wasm)?;
    Ok(())
}
```

Parameter and return annotations pick the WebAssembly value types, so annotate every function you export. A program that leaves the supported subset fails here, with the construct, its line, the enclosing function, and a workaround in the error.

## Run it

Every module-level function is exported under its own name, so any WebAssembly host can call it:

```js
// Browser or Node.js
const { instance } = await WebAssembly.instantiate(wasmBuffer);
console.log(instance.exports.factorial(5)); // 120
```

A program that prints, reads input, or opens files does so through WASI preview 1, so the same module runs under `wasmtime run`, Node's `node:wasi`, or a small browser shim. A program with an `if __name__ == "__main__":` block runs like a script:

```sh
wasmtime run --dir . program.wasm arg1 arg2
```

You can also run a Python project directly with [wasmrun](https://github.com/anistark/wasmrun), which uses waspy as its Python plugin. [Running compiled modules](/docs/running/) covers the host interface in full.

## Where next

- [Using the library](/docs/library/): every entry point, the options, and compiling several files
- [The Python subset](/docs/python-subset/): what compiles today, and [what does not](/docs/limitations/)
- [What a program looks like](/docs/programs/): one whole program, unedited
- [Modules board](/modules/): every feature, each backed by a test asserting CPython's answer
