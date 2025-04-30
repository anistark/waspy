# ChakraPy

A Python to WebAssembly compiler written in Rust.

[![Crates.io Version](https://img.shields.io/crates/v/chakrapy)
](https://crates.io/crates/chakrapy) [![Crates.io Downloads](https://img.shields.io/crates/d/chakrapy)](https://crates.io/crates/chakrapy) [![Crates.io Downloads (latest version)](https://img.shields.io/crates/dv/chakrapy)](https://crates.io/crates/chakrapy) [![Open Source](https://img.shields.io/badge/open-source-brightgreen)](https://github.com/anistark/chakrapy) [![Contributors](https://img.shields.io/github/contributors/anistark/chakrapy)](https://github.com/anistark/chakrapy/graphs/contributors) ![maintenance-status](https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg)

## Overview

ChakraPy translates Python functions into WebAssembly. The current implementation supports basic arithmetic operations, control flow, and multiple functions in a single file.

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
- Control flow with if/else and while loops
- Variable declarations and assignments
- Function parameters and return statements
- Function calls between compiled functions
- Expanded type system: integers, floats, booleans, strings (basic)
- Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Boolean operators (`and`, `or`)
- Automatic WebAssembly optimization using Binaryen

## Limitations

- Limited standard library support
- Only basic memory management
- No complex data structures yet (lists, dicts, etc.)
- Limited error handling
- No closures or higher-order functions

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
    def add(a, b):
        return a + b
        
    def fibonacci(n):
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

For unoptimized WebAssembly (useful for debugging or further processing):

```rust
use chakrapy::compile_python_to_wasm_with_options;

let wasm = compile_python_to_wasm_with_options(python_code, false)?;
```

### Example Python Code

```python
def factorial(n):
    result = 1
    i = 1
    while i <= n:
        result = result * i
        i = i + 1
    return result

def max_num(a, b):
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

ChakraPy supports multiple function definitions in a single Python file:

- Each function is compiled to a separate WebAssembly function
- All functions are exported with their original names
- Functions can call other functions that are defined in the same file

### Control Flow

The compiler supports basic control flow constructs:

- **If/Else Statements**: Conditional execution using WebAssembly's block and branch instructions
- **While Loops**: Implemented using WebAssembly's loop and branch instructions
- **Comparison Operators**: All standard Python comparison operators

### Variable Support

ChakraPy handles variables through WebAssembly locals:

- Local variables are allocated in the function's local variable space
- Assignment statements modify these locals
- Variables are statically typed based on their usage (currently defaulting to `i32`)

### Type System

The type system currently includes:

- **Integers**: Mapped to WebAssembly's `i32` type
- **Floats**: Supported as `f64` with conversion to `i32` when necessary
- **Booleans**: Represented as `i32` (`0` for `false`, `1` for `true`)
- **Strings**: Basic support for string literals (memory-based)

## Contributing

Contributions are welcome! See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to get started.

## Roadmap

- Complete support for all Python data types (lists, dicts, sets, etc.)
- Classes and object-oriented programming features
- Exception handling
- More comprehensive standard library support
- Memory management improvements
- Dynamic typing support
- Modules and imports
- Optimization improvements specific to Python patterns
- Type inference and annotation support

## License

[MIT](./LICENSE)
