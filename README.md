# ChakraPy

A Python to WebAssembly compiler written in Rust.

[![Crates.io Version](https://img.shields.io/crates/v/chakrapy)
](https://crates.io/crates/chakrapy) [![Crates.io Downloads](https://img.shields.io/crates/d/chakrapy)](https://crates.io/crates/chakrapy) [![Crates.io Downloads (latest version)](https://img.shields.io/crates/dv/chakrapy)](https://crates.io/crates/chakrapy) [![Open Source](https://img.shields.io/badge/open-source-brightgreen)](https://github.com/anistark/chakrapy) [![Contributors](https://img.shields.io/github/contributors/anistark/chakrapy)](https://github.com/anistark/chakrapy/graphs/contributors) ![maintenance-status](https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg)

## Overview

ChakraPy translates Python functions into WebAssembly. The implementation supports basic arithmetic operations, control flow, and multiple functions with enhanced type support.

### Compilation Pipeline

```
[Python Source Code]
         ↓ (rustpython_parser)
[Python AST]
         ↓ (ir module)
[Custom IR (functions, ops)]
         ↓ (wasm-encoder)
[Raw WASM binary]
         ↓ (binaryen optimizer)
[Optimized .wasm]
         ↓
[Run/test in browser or server]
```

## Current Features

- Compiles Python functions to WebAssembly
- Supports multiple functions in a single file
- Compile multiple files into a single WebAssembly module
- Control flow with if/else and while loops
- Variable declarations and assignments
- Type annotations for function parameters and return values
- Function parameters and return statements
- Function calls between compiled functions
- Expanded type system: integers, floats, booleans, strings (basic)
- Arithmetic operations (`+`, `-`, `*`, `/`, `%`, `//`, `**`)
- Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Boolean operators (`and`, `or`)
- Improved error handling with detailed error messages
- Automatic WebAssembly optimization using Binaryen

## Limitations

- Limited standard library support
- Only basic memory management
- No complex data structures yet (limited support for lists, dicts)
- No closures or higher-order functions
- No exception handling

## Installation

```sh
cargo add chakrapy
```

## Usage

### Using the Library

```rust
use chakrapy::compile_python_to_wasm;

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
    // You can also import your python file code and parse it here
    
    let wasm = compile_python_to_wasm(python_code)?;
    // Write to file or use the WebAssembly binary
    Ok(())
}
```

For multiple files compilation:

```rust
use chakrapy::compile_multiple_python_files;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let sources = vec![
        ("math.py", "def add(a: int, b: int) -> int:\n    return a + b"),
        ("main.py", "def compute(x: int) -> int:\n    return add(x, 10)")
    ];
    
    let wasm = compile_multiple_python_files(&sources, true)?;
    // Write combined module to file
    Ok(())
}
```

For unoptimized WebAssembly (useful for debugging or further processing):

```rust
use chakrapy::compile_python_to_wasm_with_options;

let wasm = compile_python_to_wasm_with_options(python_code, false)?;
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

ChakraPy supports multiple function definitions:

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
- **Strings**: Basic support for string literals (memory-based)
- **Type Coercion**: Automatic conversion between compatible types when needed

### Control Flow

The compiler supports basic control flow constructs:

- **If/Else Statements**: Conditional execution using WebAssembly's block and branch instructions
- **While Loops**: Implemented using WebAssembly's loop and branch instructions
- **Comparison Operators**: All standard Python comparison operators
- **Boolean Operators**: Support for `and` and `or` with short-circuit evaluation

### Variable Support

ChakraPy handles variables through WebAssembly locals:

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

ChakraPy includes several examples to demonstrate its functionality:

```sh
# Basic compiler example
cargo run --example compiler

# Flexible compiler with type support
cargo run --example flexible_compiler -- examples/typed_example.py

# Type system demonstration
cargo run --example type_demo -- examples/typed_example.py

# Multi-file compilation
cargo run --example multi_function -- output.wasm examples/math_functions.py examples/calculator.py
```

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to get started.

## Roadmap

- Complete support for all Python data types (lists, dicts, sets, etc.)
- Classes and object-oriented programming features
- Exception handling
- More comprehensive standard library support
- Memory management improvements
- Modules and imports
- Optimization improvements specific to Python patterns
- Enhanced type inference

## License

[MIT](./LICENSE)
