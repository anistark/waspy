# Waspy

A Python to WebAssembly compiler written in Rust.

[![Crates.io Version](https://img.shields.io/crates/v/waspy)
](https://crates.io/crates/waspy) [![Crates.io Downloads](https://img.shields.io/crates/d/waspy)](https://crates.io/crates/waspy) [![Crates.io Downloads (latest version)](https://img.shields.io/crates/dv/waspy)](https://crates.io/crates/waspy) [![Open Source](https://img.shields.io/badge/open-source-brightgreen)](https://github.com/anistark/waspy) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE) [![Contributors](https://img.shields.io/github/contributors/anistark/waspy)](https://github.com/anistark/waspy/graphs/contributors) ![maintenance-status](https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg)

![waspy](./assets/logo.png)

Waspy compiles a typed subset of Python ahead of time into a standalone WebAssembly module, with no interpreter or VM in the output. A construct either gives CPython's answer or is refused at compile time with a located error and a hint.

Try it without installing anything in the [playground](https://anistark.github.io/waspy/playground/).

## Installation

```sh
cargo add waspy
```

Or add it to your `Cargo.toml`:

```toml
[dependencies]
waspy = "0.18.0"
```

The default `optimize` feature runs Binaryen, a native C++ library, over the output. To build waspy for a target without a C++ toolchain, such as `wasm32-unknown-unknown`, see [Building without Binaryen](https://anistark.github.io/waspy/docs/library/#building-without-binaryen).

## Quick Start

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

Every module-level function is exported under its own name:

```js
// Browser or Node.js
const { instance } = await WebAssembly.instantiate(wasmBuffer);
console.log(instance.exports.factorial(5)); // 120
```

A program that prints, reads input, or opens files does so through WASI, so it runs under `wasmtime run`, Node's `node:wasi`, or a browser shim. You can also run a Python project directly with [wasmrun](https://github.com/anistark/wasmrun), which uses waspy as its Python plugin.

## Documentation

- [Documentation](https://anistark.github.io/waspy/docs/): getting started, the library API and options, the supported Python subset, numbers, limitations, and running compiled modules
- [Modules](https://anistark.github.io/waspy/modules/): the development board, feature by feature
- [What a program looks like](https://anistark.github.io/waspy/docs/programs/): one whole program, unedited
- [Playground](https://anistark.github.io/waspy/playground/): compile and run Python in the browser
- [Changelog](https://anistark.github.io/waspy/docs/changelog/)
- [API docs](https://docs.rs/waspy) on docs.rs

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for building, running the examples, and testing. If you find a program that compiles and answers differently from CPython, please [open an issue](https://github.com/anistark/waspy/issues).

![waspy](./assets/waspy.png)

## License

[MIT](./LICENSE)
