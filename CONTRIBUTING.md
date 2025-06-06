# Contributing to Waspy

Thank you for your interest in contributing to Waspy! This document provides guidelines and instructions for contributing to the project.

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
   git clone https://github.com/anistark/waspy.git
   cd waspy
   ```

3. **Build the Project**
   ```sh
   just build
   ```

### Project Structure

Waspy follows a modular structure:

```
waspy/
├── src/
│   ├── lib.rs        - Main library entry point
│   ├── errors.rs     - Error handling
│   ├── parser.rs     - Python parsing using RustPython
│   ├── ir/           - Intermediate representation
│   │   ├── mod.rs    - IR module exports
│   │   ├── types.rs  - IR data structures
│   │   └── converter.rs - AST to IR conversion
│   ├── compiler/     - WASM generation using wasm-encoder
│   │   ├── mod.rs    - Compiler module exports
│   │   ├── context.rs - Compilation context
│   │   ├── expression.rs - Expression compilation
│   │   ├── function.rs - Function compilation
│   │   └── module.rs - Module compilation
│   └── optimizer.rs  - WASM optimization using Binaryen
├── examples/
│   ├── basic_operations.py  - Simple arithmetic operations
│   ├── control_flow.py      - Control flow examples 
│   ├── typed_demo.py        - Type system demonstration
│   ├── calculator.py        - Calculator using basic_operations.py functions
│   ├── simple_compiler.rs   - Basic compiler usage
│   ├── advanced_compiler.rs - Advanced compiler with options
│   └── multi_file_compiler.rs - Multi-file compilation
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

Waspy includes several examples to demonstrate its functionality:

#### Simple Compiler Example

Compiles a basic operations Python file to WebAssembly:

```sh
cargo run --example simple_compiler
```

#### Advanced Compiler Example

A more flexible compiler with various options:

```sh
# Compile with optimization (default)
cargo run --example advanced_compiler examples/typed_demo.py

# Without optimization
cargo run --example advanced_compiler examples/typed_demo.py --no-optimize

# Show function metadata
cargo run --example advanced_compiler examples/typed_demo.py --metadata

# Generate an HTML test harness
cargo run --example advanced_compiler examples/typed_demo.py --html
```

#### Multi-File Compiler Example

Compiles multiple Python files into a single WebAssembly module:

```sh
cargo run --example multi_file_compiler combined.wasm examples/basic_operations.py examples/calculator.py
```

You can also use the justfile to run all examples:
```sh
just examples
```

### Creating and Running Your Own Examples

Create a Python file with functions that use the supported features:

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

Then compile it:

```sh
cargo run --example advanced_compiler path/to/your_function.py
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

Waspy uses a `justfile` to manage common development tasks:

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

Waspy follows Rust's standard coding conventions:

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

5. **Error Handling**
   - Use the error handling system in `errors.rs`
   - Include context information where possible
   - Propagate errors using `?` operator with context

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
     
     // Call the max_num function
     console.log(instance.exports.max_num(42, 17)); // Should output 42
   });
   ```

3. **Browser**:
   You can use the HTML test files generated by the `advanced_compiler` or `multi_file_compiler` examples
   with the `--html` flag, or create your own HTML harness following those examples.

## Community

- **Issues**: Use [GitHub Issues](https://github.com/anistark/waspy/issues) for bug reports and feature requests
- **Discussions**: For questions and general discussion, use [GitHub Discussions](https://github.com/anistark/waspy/discussions)

Thank you for contributing to Waspy! 👋
