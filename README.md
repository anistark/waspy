# Waspy

A Python to WebAssembly compiler written in Rust.

[![Crates.io Version](https://img.shields.io/crates/v/waspy)
](https://crates.io/crates/waspy) [![Crates.io Downloads](https://img.shields.io/crates/d/waspy)](https://crates.io/crates/waspy) [![Crates.io Downloads (latest version)](https://img.shields.io/crates/dv/waspy)](https://crates.io/crates/waspy) [![Open Source](https://img.shields.io/badge/open-source-brightgreen)](https://github.com/anistark/waspy) [![Contributors](https://img.shields.io/github/contributors/anistark/waspy)](https://github.com/anistark/waspy/graphs/contributors) ![maintenance-status](https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg)

![waspy](./assets/logo.png)

## Overview

Waspy translates Python functions into WebAssembly. The implementation supports basic arithmetic operations, control flow, and multiple functions with enhanced type support.

### Compilation Pipeline

## Overview

```sh
[Python Source Code]
       ↓ 
Parse & Analyze
       ↓
[Intermediate Representation]
       ↓
Generate & Optimize
       ↓
[WebAssembly Binary]
```

## Current Features

- Compiles Python functions to WebAssembly
- Supports multiple functions in a single WebAssembly module
- Compiles multiple files into a single module
- Handles control flow with if/else, while and for loops, including `break` and `continue`
- Processes variable declarations and assignments
- Supports type annotations for function parameters and return values
- Enables function calls between compiled functions
- Includes an expanded type system: integers, floats, booleans, strings
- Complete string operations support (slicing, concatenation, 20+ methods, formatting)
- Supports arithmetic operations (`+`, `-`, `*`, `/`, `%`, `//`, `**`)
- Processes comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Handles boolean operators (`and`, `or`)
- Provides improved error handling with detailed error messages
- Performs automatic WebAssembly optimization using Binaryen
- Detects and handles project structure and dependencies
- Supports module-level variables and class definitions with heap-allocated instances — multiple live instances per class, usable as function arguments and return values
- Object-oriented Python: single inheritance with `super()`, `isinstance`/`issubclass` over the class hierarchy, `@staticmethod`/`@classmethod`/`@property` (with setters), `@dataclass` (generated `__init__`/`__eq__`/`__repr__`), and abstract base classes via `abc.ABC`
- Collections: lists, dicts, sets, tuples, and ranges — literals, indexing, methods, and membership (`in`/`not in`), with full-precision f64 elements and hash-table sets
- Exception handling with `try`/`except`/`finally` and `raise`
- Comprehensions: list, set, and dict comprehensions with filters, multiple generators, nesting, and `{k: v for k, v in pairs}` unpacking
- Generators with real state preservation: `yield` suspends and resumes, `yield from` delegates, and `next()`/`send()`/`close()` work; user classes implementing `__iter__`/`__next__` iterate in `for` loops with `StopIteration` ending the loop
- Tuple targets in `for` loops (`for a, b in pairs`, star targets included) and the iterator-shaped builtins: `enumerate(xs[, start])`, `zip(...)`, and `dict.items()`/`.keys()`/`.values()`
- Closures with full variable capture: lambdas compile to real functions dispatched through a `call_indirect` table, capture enclosing variables (by value), and work as first-class values — returned, passed as arguments, and stored in collections
- Extended unpacking: `a, *b, c = xs` binds the starred target to the middle slice as a real list
- Bundled standard library runtime: `sys`, `os` (incl. `os.path`), `math`, `random`, `json`, `re`, `datetime`, `logging`, `collections`, `itertools`, `functools`

## Limitations

- Object instances are never reclaimed — the bump allocator has no `free`, so every instance lives until the module is torn down and `__del__` is not invoked
- Collections have a fixed compile-time capacity — growing one past its initial size (e.g. `.append` beyond a literal's length) overflows into the next region; runtime growth/reallocation is not yet implemented
- Generators cover the common shapes; `yield` inside `try`/`with` and generator methods (`yield` in a class method) are rejected at compile time, and `close()` skips `GeneratorExit`/`finally` semantics
- Closures capture by value at creation time — a captured variable mutated after the closure is created keeps its old value inside the closure (Python's late-binding cells are a follow-up); float captures are not yet supported
- Only stdlib modules import; user-written `.py` modules and file I/O are not supported
- `with` over a custom context manager does not yet compile ([#5](https://github.com/anistark/waspy/issues/5))
- No garbage collection or reference counting — the bump allocator never frees

## Installation

```sh
cargo add waspy
```

# Or add to your Cargo.toml
[dependencies]
waspy = "0.12.0"

## Quick Start

### Using the Library

```rust
use waspy::compile_python_to_wasm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let python_code = r#"
    def add(a: int, b: int) -> int:
        return a + b
        
    def fibonacci(n: int) -> int:
        if n <= 1:
            return n
        a = 0
        b = 1
        i = 2
        while i <= n:
            temp = a + b
            a = b
            b = temp
            i = i + 1
        return b
    "#;
    
    let wasm = compile_python_to_wasm(python_code)?;
    // Write to file or use the WebAssembly binary
    std::fs::write("output.wasm", &wasm)?;
    
    Ok(())
}
```

With Compiler Options:

```rust
use waspy::{compile_python_to_wasm_with_options, CompilerOptions, Verbosity};

let options = CompilerOptions {
    optimize: true,
    debug_info: true,
    generate_html: true,
    verbosity: Verbosity::Verbose,  // or Verbosity::Debug
    ..CompilerOptions::default()
};

let wasm = compile_python_to_wasm_with_options(python_code, &options)?;
```

### Verbosity Levels

Waspy supports different verbosity levels for logging output:

- **`Verbosity::Quiet`** - Minimal output (errors only)
- **`Verbosity::Normal`** - Standard output (default)
- **`Verbosity::Verbose`** - Detailed output
- **`Verbosity::Debug`** - Detail for debugging

If your project has `--verbose` or `--debug` flags, use the `from_flags` helper:

```rust
use waspy::{CompilerOptions, Verbosity};

// Map CLI flags to verbosity level
let options = CompilerOptions {
    verbosity: Verbosity::from_flags(verbose_flag, debug_flag),
    ..CompilerOptions::default()
};
```

For multiple files compilation:

```rust
use waspy::compile_multiple_python_files;

let sources = vec![
    ("math.py", "def add(a: int, b: int) -> int:\n    return a + b"),
    ("main.py", "def compute(x: int) -> int:\n    return add(x, 10)")
];

let wasm = compile_multiple_python_files(&sources, true)?;
```

For unoptimized WebAssembly (useful for debugging or further processing):

```rust
use waspy::compile_python_to_wasm_with_options;

let wasm = compile_python_to_wasm_with_options(python_code, false)?;
```

Compiling Projects:

```rust
use waspy::compile_python_project;

let wasm = compile_python_project("./my_python_project", true)?;
```

### Example Python Code

```python
def factorial(n: int) -> int:
    result = 1
    i = 1
    while i <= n:
        result = result * i
        i = i + 1
    return result

def max_num(a: float, b: float) -> float:
    if a > b:
        return a
    else:
        return b
```

### Using the Generated WebAssembly

The compiled WebAssembly can be used in various environments:

```js
// Browser or Node.js
WebAssembly.instantiate(wasmBuffer).then(result => {
  const instance = result.instance;
  console.log(instance.exports.factorial(5)); // 120
  console.log(instance.exports.max_num(42, 17)); // 42
});
```

## Implementation Details

### Multiple Functions

Waspy supports multiple function definitions:

- Each function is compiled to a separate WebAssembly function
- All functions are exported with their original names
- Functions can call other functions within the same module
- Functions from multiple files can be compiled into a single module

### Type System

The type system now includes:

- **Type Annotations**: Support for Python's type hints on function params and return values
- **Integers**: Mapped to WebAssembly's `i32` type
- **Floats**: Supported as `f64` with conversion to `i32` when necessary
- **Booleans**: Represented as `i32` (`0` for `false`, `1` for `true`)
- **Strings**: Support for string operations with compile-time optimization
- **Type Coercion**: Automatic conversion between compatible types when needed

### Control Flow

The compiler supports basic control flow constructs:

- **If/Else Statements**: Conditional execution using WebAssembly's block and branch instructions
- **While and For Loops**: Implemented using WebAssembly's loop and branch instructions
- **Break and Continue**: Early loop exit and next-iteration skip, including correct
  behavior when nested inside `if`/`try` blocks and in nested loops
- **Comparison Operators**: All standard Python comparison operators
- **Boolean Operators**: Support for `and` and `or` with short-circuit evaluation

### Variable Support

Waspy handles variables through WebAssembly locals:

- Local variables are allocated in the function's local variable space
- Assignment statements modify these locals
- Variables can be statically typed with annotations
- Type inference for variables based on usage

### Error Handling

Enhanced error reporting system:

- Detailed error messages with source location information
- Specific error types for different issues (parsing, type errors, etc.)
- Warnings for potential problems that don't prevent compilation

## Examples

Waspy includes several examples to demonstrate its functionality:

```sh
# Basic compiler example
cargo run --example simple_compiler

# Advanced compiler with options
cargo run --example advanced_compiler examples/typed_demo.py --metadata --html

# Multi-file compilation
cargo run --example multi_file_compiler examples/output/combined.wasm examples/basic_operations.py examples/calculator.py

# Project compilation
cargo run --example project_compiler examples/calculator_project examples/output/project.wasm

# Type system demonstration
cargo run --example typed_demo
```

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to get started.

## Roadmap

The path to 1.0 focuses on the remaining correctness and runtime gaps:

- Remaining object-model gaps: virtual dispatch through `self` (vtables) and multiple inheritance
- Growable collections (runtime reallocation past a literal's fixed capacity) and hashed `dict` lookups (sets already use an open-addressing table)
- User-written `.py` module imports, module caching, and file I/O
- Garbage collection / reference counting for the bump-allocated heap

![waspy](./assets/waspy.png)

## License

[MIT](./LICENSE)
