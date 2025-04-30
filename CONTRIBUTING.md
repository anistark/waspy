# Contributing to ChakraPy

Thank you for your interest in contributing to ChakraPy! This document provides guidelines and instructions for contributing to the project.

![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=for-the-badge&logo=rust&logoColor=white) ![Python](https://img.shields.io/badge/python-3670A0?style=for-the-badge&logo=python&logoColor=ffdd54)

## Table of Contents

- [Getting Started](#getting-started)
  - [Development Environment](#development-environment)
  - [Project Structure](#project-structure)
- [Building and Running Examples](#building-and-running-examples)
  - [Building the Project](#building-the-project)
  - [Running Examples](#running-examples)
  - [Creating and Running Your Own Examples](#creating-and-running-your-own-examples)
- [Development Workflow](#development-workflow)
  - [Making Changes](#making-changes)
  - [Testing](#testing)
  - [Using Justfile Commands](#using-justfile-commands)
- [Pull Request Process](#pull-request-process)
- [Coding Standards](#coding-standards)
- [Release Process](#release-process)
- [Testing Generated WebAssembly](#testing-generated-webassembly)
- [Community](#community)

## Getting Started

### Development Environment

1. **Prerequisites**
   - [Rust](https://www.rust-lang.org/tools/install)
   - [Just](https://github.com/casey/just#installation)
   - [Git](https://git-scm.com/downloads)
   - [Python](https://www.python.org/downloads/)

2. **Clone the Repository**
   ```sh
   git clone https://github.com/anistark/chakrapy.git
   cd chakrapy
   ```

3. **Build the Project**
   ```sh
   just build
   ```

### Project Structure

ChakraPy follows a modular structure:

```
chakrapy/
├── src/
│   ├── lib.rs        - Main library entry point
│   ├── parser.rs     - Python parsing using RustPython
│   ├── ir.rs         - Intermediate representation
│   ├── compiler.rs   - WASM generation using wasm-encoder
│   └── optimizer.rs  - WASM optimization using Binaryen
├── examples/
│   └── ...           - All examples here
└── Cargo.toml        - Project configuration
```

## Building and Running Examples

### Building the Project

Build the main project:
```sh
cargo build --release
```

Build the examples:
```sh
cargo build --examples
```

Or use the justfile shortcuts:
```sh
just build
just build-examples
```

### Running Examples

ChakraPy includes examples to demonstrate its functionality:

#### Basic Example

Compiles a predefined addition function and compares optimized vs. unoptimized output:

```sh
cargo run --example compiler
```

#### Flexible Compiler

Compiles any Python file you specify:

```sh
# With optimization (default)
cargo run --example flexible_compiler -- examples/test_add.py

# Without optimization
cargo run --example flexible_compiler -- examples/test_add.py --no-optimize
```

Other examples:
```sh
cargo run --example flexible_compiler -- examples/test_sub.py
cargo run --example flexible_compiler -- examples/test_mul.py
cargo run --example flexible_compiler -- examples/test_control_flow.py
```

You can also use the justfile to run all examples:
```sh
just examples
```

### Creating and Running Your Own Examples

Create a Python file with functions that use the supported features:

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

Then compile it:

```sh
cargo run --example flexible_compiler -- path/to/your_function.py
```

Or use the justfile shortcut:
```sh
just compile path/to/your_function.py
```

## Development Workflow

### Making Changes

1. **Create a Branch**
   ```sh
   git checkout -b feature/your-feature-name
   ```

2. **Implement Your Changes**
   - Follow the [Coding Standards](#coding-standards)
   - Keep changes focused on a specific feature or bugfix
   - Do not try to group together too many various things in single PR.

3. **Run the Development Workflow**
   ```sh
   just dev
   ```
   This will format your code, run the linter, build the project, and run tests.

### Testing

1. **Write Tests**
   - Add tests for new features or bug fixes
   - Ensure existing tests pass with your changes

2. **Run Tests**
   ```sh
   just test
   ```

3. **Run Examples**
   ```sh
   just examples
   ```

### Using Justfile Commands

ChakraPy uses a `justfile` to manage common development tasks:

- `just` - Show available commands
- `just build` - Build the project
- `just test` - Run tests
- `just format` - Format the code
- `just lint` - Run linter
- `just dev` - Run the full development workflow
- `just compile <file>` - Compile a Python file to WebAssembly
- `just optimize <file>` - Compile a Python file with optimization

## Pull Request Process

1. **Create a Pull Request**
   - Make sure your branch is up to date with the main branch
   - Create a pull request with a clear title and description
   - Reference any related issues

2. **Code Review**
   - Address feedback from maintainers
   - Make requested changes

3. **Merge**
   - Once approved, your pull request will be merged by a maintainer
   - Delete your branch after merging

## Coding Standards

ChakraPy follows Rust's standard coding conventions:

1. **Code Formatting**
   - Use `cargo fmt` or `just format` to format your code
   - Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)

2. **Linting**
   - Run `cargo clippy` or `just lint` to check for common issues
   - Address all clippy warnings unless there's a good reason not to

3. **Documentation**
   - Add documentation comments to public APIs
   - Follow the [rustdoc conventions](https://doc.rust-lang.org/rustdoc/what-is-rustdoc.html)

4. **Commit Messages**
   - Write clear, concise commit messages
   - Start with a short summary line
   - Use the imperative mood ("Add feature" not "Added feature")

## Release Process

Releases are handled by the maintainers. The process is:

1. **Version Bump**
   - Update version in `Cargo.toml`

2. **Publish**
   - Run `just publish` to publish to crates.io and create a GitHub release

## Testing Generated WebAssembly

To verify the generated WebAssembly, you can use:

1. **WebAssembly Binary Toolkit (WABT)**:
   ```sh
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
     // Access any exported function
     const instance = result.instance;
     
     // Call the factorial function
     console.log(instance.exports.factorial(5)); // Should output 120
     
     // Call the fibonacci function
     console.log(instance.exports.fibonacci(10)); // Should output 55
     
     // Call the max_num function
     console.log(instance.exports.max_num(42, 17)); // Should output 42
   });
   ```

3. **Browser**:
   ```html
   <!DOCTYPE html>
   <html>
   <head>
     <title>ChakraPy WASM Test</title>
   </head>
   <body>
     <script>
       (async () => {
         const response = await fetch('your_file.wasm');
         const bytes = await response.arrayBuffer();
         const { instance } = await WebAssembly.instantiate(bytes);
         
         // Call the exported functions
         console.log(instance.exports.factorial(5));
         console.log(instance.exports.fibonacci(10));
       })();
     </script>
   </body>
   </html>
   ```

## Community

- **Issues**: Use [GitHub Issues](https://github.com/anistark/chakrapy/issues) for bug reports and feature requests
- **Discussions**: For questions and general discussion, use [GitHub Discussions](https://github.com/anistark/chakrapy/discussions)

Thank you for contributing to ChakraPy! 👋
