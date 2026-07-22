# Waspy

A Python to WebAssembly compiler written in Rust.

[![Crates.io Version](https://img.shields.io/crates/v/waspy)
](https://crates.io/crates/waspy) [![Crates.io Downloads](https://img.shields.io/crates/d/waspy)](https://crates.io/crates/waspy) [![Crates.io Downloads (latest version)](https://img.shields.io/crates/dv/waspy)](https://crates.io/crates/waspy) [![Open Source](https://img.shields.io/badge/open-source-brightgreen)](https://github.com/anistark/waspy) [![Contributors](https://img.shields.io/github/contributors/anistark/waspy)](https://github.com/anistark/waspy/graphs/contributors) ![maintenance-status](https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg)

![waspy](./assets/logo.png)

## Overview

Waspy compiles a typed subset of Python ahead of time into a standalone WebAssembly module — no interpreter or VM in the output. This README's [Supported Python subset](#supported-python-subset) and [Limitations](#limitations) sections are the authoritative statement of what compiles and runs today.

### Compilation Pipeline

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

## Supported Python subset

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
- Handles boolean operators (`and`, `or`) and bitwise operators (`&`, `|`, `^`, `<<`, `>>`)
- Built-in functions: `int()`, `float()`, `str()`, `bool()`, `len()`, `print()`, `min()`/`max()` (multiple arguments), `sum()` over lists/tuples (with optional start value)
- Rejects unsupported Python syntax up front with located errors and hints, instead of failing deep in code generation
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
- User-written module imports: `import mod`, `import mod as m`, and `from mod import f [as g]` resolve sibling `.py` files (and `pkg/mod.py` packages) and statically link them into the single output WASM module, with each module compiled exactly once however many import paths reach it
- File I/O through a documented host interface: `open()`, `read([n])`, `write(s)`, `close()`, and `with open(...) as f:` compile to four imported `waspy_host` functions the embedder provides (browser, Node, or any WASM runtime); modules that never call `open()` import nothing
- Bundled standard library runtime: `sys`, `os` (incl. `os.path`), `math`, `random`, `json`, `re`, `datetime`, `logging`, `collections`, `itertools`, `functools`

## Limitations

- Object instances are never reclaimed — the bump allocator has no `free`, so every instance lives until the module is torn down and `__del__` is not invoked
- Collections have a fixed compile-time capacity — growing one past its initial size (e.g. `.append` beyond a literal's length) overflows into the next region; runtime growth/reallocation is not yet implemented
- Generators cover the common shapes; `yield` inside `try`/`with` and generator methods (`yield` in a class method) are rejected at compile time, and `close()` skips `GeneratorExit`/`finally` semantics
- Closures capture by value at creation time — a captured variable mutated after the closure is created keeps its old value inside the closure (Python's late-binding cells are a follow-up); float captures are not yet supported
- Imported user modules share one flat namespace in the output module — two modules defining the same function name collide (first definition wins, with a warning)
- `f.read()` without a size reads up to 64 KiB per call; `open()` modes must be string literals
- `with` over a custom context manager does not yet compile ([#5](https://github.com/anistark/waspy/issues/5)); `with open(...)` works
- No garbage collection or reference counting — the bump allocator never frees

### Explicitly unsupported (rejected at compile time)

The compiler validates syntax up front and rejects these with a located error and a hint, rather than miscompiling them:

- `async def` / `await` / `async for` / `async with` (planned after 1.0)
- `match` statements, `global`, `nonlocal`, `del`, `assert`, `type` aliases, `except*`
- `from module import *`
- `*args`, `**kwargs`, and keyword-only parameters
- Metaclasses and other class keywords, multiple inheritance
- Loop `else:` clauses (`for`/`while ... else`)
- `min()`/`max()` over a single iterable argument (pass the values separately)

## Installation

```sh
cargo add waspy
```

Or add it to your `Cargo.toml`:

```toml
[dependencies]
waspy = "0.12.0"
```

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
    optimize: true,                 // Binaryen pass over the output (default: true)
    verbosity: Verbosity::Verbose,  // or Verbosity::Debug
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
use waspy::{compile_python_to_wasm_with_options, CompilerOptions};

let options = CompilerOptions {
    optimize: false,
    ..CompilerOptions::default()
};
let wasm = compile_python_to_wasm_with_options(python_code, &options)?;
```

Compiling an entry file with its own module imports resolved from disk (`import mod` finds the sibling `mod.py`, transitively):

```rust
use waspy::compile_python_file;

let wasm = compile_python_file("app/main.py", true)?;
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

- Python syntax errors report their line and column
- Known-unsupported constructs are rejected before code generation with the construct named, its location, the enclosing function, and a workaround hint
- Specific error types for different issues (parsing, type errors, unsupported features, name errors)
- Warnings for potential problems that don't prevent compilation (e.g. cross-module function name collisions)

### Comment Preservation

Comments from the Python sources are preserved in the generated binary as a `python.comments` custom section: UTF-8 text, one `file:line:text` entry per line. Custom sections carry no code, so this changes nothing about how the module runs, and the section survives optimization. Read it back with `waspy::core::comments::comments_from_wasm`, or with any tool that dumps WebAssembly custom sections.

### Testing

Every bundled example compiles, instantiates, and has its runtime results asserted by the integration suite (`tests/integration/`), alongside operator-level unit tests (`tests/unit/`). Run everything with `just test`, the full CI-equivalent gate with `just ci`, or compile every example through the real drivers with `just verify-examples`.

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

Or through the justfile:

```sh
just compile examples/typed_demo.py     # compile one file (reports sizes)
just verify-examples                    # compile every bundled example
just benchmark                          # time compilation (release build)
just examples                           # run the full driver suite
```

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to get started.

## Roadmap

The path to 1.0 focuses on the remaining correctness and runtime gaps:

- Remaining object-model gaps: virtual dispatch through `self` (vtables) and multiple inheritance
- Growable collections (runtime reallocation past a literal's fixed capacity) and hashed `dict` lookups (sets already use an open-addressing table)
- Garbage collection / reference counting for the bump-allocated heap

![waspy](./assets/waspy.png)

## License

[MIT](./LICENSE)
