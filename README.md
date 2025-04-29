# ChakraPy

A Python to WebAssembly compiler written in Rust.

[![Crates.io Version](https://img.shields.io/crates/v/chakrapy)
](https://crates.io/crates/chakrapy) [![Crates.io Downloads](https://img.shields.io/crates/d/chakrapy)](https://crates.io/crates/chakrapy) [![Crates.io Downloads (latest version)](https://img.shields.io/crates/dv/chakrapy)](https://crates.io/crates/chakrapy) [![Open Source](https://img.shields.io/badge/open-source-brightgreen)](https://github.com/anistark/chakrapy) [![Contributors](https://img.shields.io/github/contributors/anistark/chakrapy)](https://github.com/anistark/chakrapy/graphs/contributors) ![maintenance-status](https://img.shields.io/badge/maintenance-actively--developed-brightgreen.svg)

## Overview

ChakraPy translates simple Python functions into WebAssembly. The current implementation supports basic integer arithmetic operations in single-function Python files.

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

- Compiles simple Python functions to WebAssembly
- Supports integer arithmetic operations (`+`, `-`, `*`, `/`)
- Function parameters
- Integer constants
- Automatic WebAssembly optimization using Binaryen

## Limitations

- Only supports a single function per file
- Only handles integer operations
- Single return statement required
- No control flow, loops, or complex features
- Limited error handling

## Usage

### Using the Library

```rust
use chakrapy::compile_python_to_wasm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let python_code = "def add(a, b):\n    return a + b";
    let wasm = compile_python_to_wasm(python_code)?;
    Ok(())
}
```

By default it'll create optimized wsam. But if you want to create unoptimized for further processing or testing, you can use `compile_python_to_wasm_with_options` instead.

### Running the Examples

ChakraPy includes examples to demonstrate its functionality:

#### Basic Example

Compiles a predefined addition function and compares optimized vs. unoptimized output:

```bash
cargo run --example compiler
```

#### Flexible Compiler

Compiles any Python file you specify:

```bash
# With optimization (default)
cargo run --example flexible_compiler -- examples/test_add.py

# Without optimization
cargo run --example flexible_compiler -- examples/test_add.py --no-optimize
```

Other examples:
```bash
cargo run --example flexible_compiler -- examples/test_sub.py
cargo run --example flexible_compiler -- examples/test_mul.py
```

### Creating Your Own Python Functions

Create a Python file with a single function that returns an integer expression:

```python
def my_function(a, b, c):
    return a * b + c
```

Then compile it:

```bash
cargo run --example flexible_compiler -- path/to/your_function.py
```

## Project Structure

```
chakrapy/
├── src/
│   ├── lib.rs        - Main library entry point
│   ├── parser.rs     - Python parsing using RustPython
│   ├── ir.rs         - Intermediate representation
│   ├── compiler.rs   - WASM generation using wasm-encoder
│   └── optimizer.rs  - WASM optimization using Binaryen
├── examples/
│   ├── compiler.rs              - Basic example compiler
│   ├── flexible_compiler.rs     - Command-line compiler
│   ├── test_add.py              - Addition test
│   ├── test_sub.py              - Subtraction test
│   └── test_mul.py              - Multiplication test
└── Cargo.toml        - Project configuration
```

## Optimization

ChakraPy uses the Binaryen library to optimize WebAssembly output:

- **Size Reduction**: The optimizer can significantly reduce the size of the generated WASM files
- **Performance Improvement**: Optimized code runs faster in WebAssembly environments
- **Configurable**: Optimization can be enabled/disabled and configured via API

The optimization settings can be adjusted in the `optimizer.rs` file. Current settings include:
- Optimization level: 3 (0-4 scale, with 4 being most aggressive)
- Shrink level: 1 (0-2 scale, with 2 being most aggressive for size)
- Inline optimization: Enabled with various size thresholds

## Testing Generated WebAssembly

To verify the generated WebAssembly, you can use:

1. **WebAssembly Binary Toolkit (WABT)**:
   ```bash
   # Validate the WebAssembly binary
   wasm-validate your_file.wasm
   
   # Convert to text format
   wasm2wat your_file.wasm -o your_file.wat
   ```

2. **Node.js**:
   ```js
   const fs = require('fs');
   const wasmBuffer = fs.readFileSync('your_file.wasm');
   WebAssembly.instantiate(wasmBuffer).then(result => {
     const func = result.instance.exports.your_function_name;
     console.log(func(5, 3)); // Example call
   });
   ```

## Future Improvements

- Support for more Python features (conditionals, loops)
- Multiple functions per file
- More data types (floats, strings, lists)
- Function imports/exports
- Memory management
- Standard library support
- Additional optimization passes

## License

[MIT](./LICENSE)
