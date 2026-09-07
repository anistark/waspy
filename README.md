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
- Includes an expanded type system: integers (32-bit, see [Numbers](#numbers)), floats, booleans, strings
- Complete string operations support (slicing, concatenation, 20+ methods, formatting)
- F-strings interpolate every placeholder: `f"{count} items"` renders each value with `str()` and concatenates the pieces, so an f-string is the `+` chain you would write by hand. Constant placeholders fold into the literal at compile time
- Supports arithmetic operations (`+`, `-`, `*`, `/`, `%`, `//`, `**`)
- Processes comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Handles boolean operators (`and`, `or`) and bitwise operators (`&`, `|`, `^`, `<<`, `>>`)
- Built-in functions: `int()`, `float()`, `str()`, `bool()`, `len()`, `print()`, `min()`/`max()` (multiple arguments), `sum()` over lists/tuples (with optional start value)
- Rejects unsupported Python syntax up front with located errors and hints, instead of failing deep in code generation
- Performs automatic WebAssembly optimization using Binaryen
- Detects and handles project structure and dependencies
- Supports module-level variables and class definitions with heap-allocated instances — multiple live instances per class, usable as function arguments and return values
- Object-oriented Python: single inheritance with `super()`, `isinstance`/`issubclass` over the class hierarchy, `@staticmethod`/`@classmethod`/`@property` (with setters), `@dataclass` (generated `__init__`/`__eq__`/`__repr__`), and abstract base classes via `abc.ABC`
- Collections: lists, dicts, sets, tuples, and ranges: literals, indexing, methods, and membership (`in`/`not in`), with full-precision f64 elements and hash-table sets; lists and dicts reallocate as they grow past their literal's size
- Exceptions: `raise` transfers control, `try`/`except`/`finally` catch and clean up, and an exception propagates out of a call into the caller's handler. `except (A, B):` catches either type, `except Exception:` catches any, and an exception nothing catches unwinds out of the program and traps
- Comprehensions: list, set, and dict comprehensions with filters, multiple generators, nesting, and `{k: v for k, v in pairs}` unpacking
- Generators with real state preservation: `yield` suspends and resumes, `yield from` delegates, and `next()`/`send()`/`close()` work; user classes implementing `__iter__`/`__next__` iterate in `for` loops with `StopIteration` ending the loop
- Tuple targets in `for` loops (`for a, b in pairs`, star targets included) and the iterator-shaped builtins: `enumerate(xs[, start])`, `zip(...)`, and `dict.items()`/`.keys()`/`.values()`
- Closures with full variable capture: lambdas compile to real functions dispatched through a `call_indirect` table, capture enclosing variables (by value), and work as first-class values — returned, passed as arguments, and stored in collections
- Extended unpacking: `a, *b, c = xs` binds the starred target to the middle slice as a real list
- User-written module imports: `import mod`, `import mod as m`, and `from mod import f [as g]` resolve sibling `.py` files (and `pkg/mod.py` packages) and statically link them into the single output WASM module, with each module compiled exactly once however many import paths reach it
- File I/O through a documented host interface: `open()`, `read([n])`, `write(s)`, `close()`, and `with open(...) as f:` compile to four imported `waspy_host` functions the embedder provides (browser, Node, or any WASM runtime); modules that never call `open()` import nothing
- Context managers: `with obj as name:` over a user class implementing `__enter__`/`__exit__`, including nested blocks, inherited protocols, and `__exit__` running before an early `return`
- Bundled standard library runtime: `sys`, `os` (incl. `os.path`), `math`, `random`, `json`, `re`, `datetime`, `logging`, `collections`, `itertools`, `functools`

## Numbers

`int` is a 32-bit two's-complement integer, and `float` is an IEEE-754 double.

This is a deliberate narrowing of Python's semantics, and part of the definition
of the subset waspy accepts rather than an accident of the backend. CPython's
`int` is arbitrary precision: it grows to hold whatever it is given. Here a
value that leaves the range -2147483648 to 2147483647 wraps around, so

```python
1000000 * 1000000   # CPython: 1000000000000    waspy: -727379968
2 ** 40             # CPython: 1099511627776    waspy: 0
2147483647 + 1      # CPython: 2147483648       waspy: -2147483648
```

A program whose integer values stay inside that range gets Python's answers. One
that leaves it gets two's-complement wraparound, quietly, the way it would in C
or Rust's release profile.

The alternative was to check every add, subtract, multiply, and power for
overflow and trap. That makes the divergence loud, but it costs instructions on
the hottest path in any program and rejects code that deliberately relies on
wrapping (hashes, checksums, PRNGs). A 64-bit integer would only move the
boundary rather than remove it: arbitrary precision needs heap-allocated digits
and an allocator call on every operation, which is a different project.

Everywhere else, waspy holds itself to the opposite standard: a construct either
produces Python's answer or fails loudly, never a quietly different one. Integer
width is the single documented exception, so if your program's arithmetic can
exceed 32 bits, it is not in the supported subset.

Float division by zero and integer division or modulo by zero raise
`ZeroDivisionError` rather than producing `inf` or trapping.

## Limitations

- Object instances are never reclaimed — the bump allocator has no `free`, so every instance lives until the module is torn down and `__del__` is not invoked
- Lists, dicts, and sets grow at runtime (`append`/`extend`/`insert`, `dict[key] = value` for a new key, and `set.add` reallocate when full). A collection's elements live in a block its header points at, so growing one never moves the collection: a list grown inside a function it was passed to, or reached by indexing another collection, is grown for every other name for it too
- Generators cover the common shapes; `yield` inside `try`/`with` and generator methods (`yield` in a class method) are rejected at compile time, and `close()` skips `GeneratorExit`/`finally` semantics
- Closures capture the variable, not a snapshot of it: a captured variable reassigned after the closure is made changes what the closure sees, and closures created in a loop share the loop variable. Capturing a float is not supported yet
- Imported user modules share one flat namespace in the output module — two modules defining the same function name collide (first definition wins, with a warning)
- `f.read()` without a size reads up to 64 KiB per call; `open()` modes must be string literals
- A `with` statement needs its context manager's class to be resolvable at compile time (an instantiation, a call with an annotated class return type, or a variable of known class type). `__exit__` runs on every way out: the normal path, a `return`/`break`/`continue`, and an exception leaving the block. Its return value never suppresses an exception, though, so returning `True` from `__exit__` does not swallow one the way Python's does
- Division by zero raises `ZeroDivisionError` (integer and float, `/`, `//`, and `%`), catchable like any other exception
- `**` computes by repeated multiplication: a fractional float exponent (`2.0 ** 0.5`) traps, since it needs exp/log this runtime does not carry, and a negative integer exponent traps because Python's answer is a float an int result cannot hold
- Sequence indexing is checked: a negative index counts from the end, an index outside the sequence raises `IndexError`, a missing dict key raises `KeyError`, and item assignment into a tuple or a string is a compile error. Slicing clamps instead of raising, as Python's does
- `int` is 32-bit and wraps on overflow rather than growing like CPython's, which is the one place waspy answers differently without saying so. See [Numbers](#numbers)
- Exceptions carry a type, not an object: `raise ValueError("message")` records the type and drops the message, and `except ValueError as e` binds the type's code rather than an exception instance. Matching is by exact type name (plus `Exception`/`BaseException`, which catch anything), so a user-defined exception's own base classes are not consulted. Runtime faults the compiler cannot turn into a raise, an out-of-range index or a division by zero, trap rather than raising, so `except ZeroDivisionError:` will not catch `1 // 0`
- F-string placeholders render through `str()`, so whatever `str()` cannot render cannot be interpolated: a float, a bool, or a collection in a placeholder is a compile error naming the type. Format specifiers (`f"{x:.2f}"`) and the `!r`/`!a` conversions are rejected too, rather than being dropped
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
- Module-level statements other than definitions. A WebAssembly module has no top-level run step, so only definitions run: `X = 1`, `X: int = 1`, `X = helper()`, `X = ClassName()`, `def`, `class`, and imports (a `try`/`except ImportError` guarded import included). A module-level loop, `if`, `try`, bare call statement, augmented assignment, tuple unpacking, or write through a subscript or attribute is rejected, because it would otherwise be compiled away and later reads would silently see the value from before it. Put the code in a function and call it, or drive it from `if __name__ == "__main__":`, which is recognized as the entry point

Set methods beyond `add`, `remove`, and `discard` (`union`, `intersection`, and friends) are not implemented. Receiver types are only known during code generation, so they are rejected there rather than by the parser's syntax pass, but it is still a compile error naming the method, the receiver's type, and the function it appears in. `remove` of a value the set does not hold traps at runtime, since there is no `KeyError` to raise; `discard` ignores the miss like Python's.

## Installation

```sh
cargo add waspy
```

Or add it to your `Cargo.toml`:

```toml
[dependencies]
waspy = "0.15.0"
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
- Growable sets (lists and dicts already reallocate when full) and hashed `dict` lookups (sets already use an open-addressing table)
- Garbage collection / reference counting for the bump-allocated heap

![waspy](./assets/waspy.png)

## License

[MIT](./LICENSE)
