# ChakraPy

A Python to WebAssembly compiler written in Rust.

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
         ↓ (optional: wasm-opt)
[Optimized .wasm]
         ↓
[Run/test in browser or server]
```

1. Python Source Code to AST: Uses `rustpython_parser` to parse Python code into an Abstract Syntax Tree (AST)
2. AST to Intermediate Representation (IR): Transforms the Python AST into a simplified custom IR
3. IR to WebAssembly Binary: Transforms your IR into actual WebAssembly bytecode
4. Optimization: The raw WebAssembly can be further optimized to get final `.wasm` file.

## Current Features

- Compiles simple Python functions to WebAssembly
- Supports integer arithmetic operations (`+`, `-`, `*`, `/`)
- Function parameters
- Integer constants

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
    let wasm_binary = compile_python_to_wasm(python_code)?;
    // Use the WebAssembly binary...
    Ok(())
}
```

### Running the Examples

ChakraPy includes examples to demonstrate its functionality:

#### Basic Example

Compiles a predefined addition function:

```bash
cargo run --example compiler
```

#### Flexible Compiler

Compiles any Python file you specify:

```bash
cargo run --example flexible_compiler -- examples/test_add.py
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
│   └── compiler.rs   - WASM generation using wasm-encoder
├── examples/
│   ├── compiler.rs              - Basic example compiler
│   ├── flexible_compiler.rs     - Command-line compiler
│   ├── test_add.py              - Addition test
│   ├── test_sub.py              - Subtraction test
│   └── test_mul.py              - Multiplication test
└── Cargo.toml        - Project configuration
```

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
   ```javascript
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

## License

[MIT]
