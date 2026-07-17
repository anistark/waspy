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
├── examples/            - Example programs and test files
│   ├── output/          - Generated WebAssembly output
│   ├── *.py             - Python example files
│   └── *.rs             - Rust example programs
├── src/
│   ├── analysis/        - Project analysis tools
│   │   ├── imports.rs   - Import analysis
│   │   ├── metadata.rs  - Project metadata extraction
│   │   └── project.rs   - Project structure analysis
│   ├── compiler/        - WASM generation using wasm-encoder
│   │   ├── context.rs   - Compilation context
│   │   ├── expression.rs - Expression compilation
│   │   ├── function.rs  - Function compilation
│   │   └── module.rs    - Module compilation
│   ├── core/            - Core functionality
│   │   ├── config.rs    - Project configuration
│   │   ├── errors.rs    - Error handling
│   │   ├── options.rs   - Compiler options
│   │   └── parser.rs    - Python parsing using RustPython
│   ├── ir/              - Intermediate representation
│   │   ├── converter.rs - AST to IR conversion
│   │   ├── decorators.rs - Function decorators
│   │   ├── entry_points.rs - Entry point detection
│   │   └── types.rs     - IR data structures
│   ├── optimize/        - Optimization tools
│   │   └── wasm.rs      - WASM optimization using Binaryen
│   ├── utils/           - Utility functions
│   │   ├── fs.rs        - File system utilities
│   │   ├── logging.rs   - Logging utilities
│   │   └── paths.rs     - Path utilities
│   └── lib.rs           - Main library entry point
├── tests/               - Test suite
│   ├── integration/     - Integration tests
│   └── unit/            - Unit tests
├── Cargo.toml           - Project configuration
├── justfile             - Command runner configuration
└── README.md            - Project documentation
```

## Building and Running Examples

### Building the Project

Build the main project:
```sh
just build
```

Build the examples:
```sh
just build-examples
```

### Running Examples

Waspy includes several examples to demonstrate its functionality.

#### Run All Examples

```sh
just examples
```

#### Compile Specific Python Files

```sh
# Compile a single Python file
just compile examples/typed_demo.py

# Compile with optimization and metadata
just optimize examples/typed_demo.py

# Compile multiple files into one module
just compile-multi examples/output/combined.wasm examples/basic_operations.py examples/calculator.py

# Compile an entire project directory
just compile-project examples/calculator_project
```

#### Run Type System Demo

```sh
just run-typed-demo
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
- `just format-check` - Check formatting without changes
- `just lint` - Run linter
- `just docs` - Generate and open documentation
- `just docs-check` - Check documentation
- `just ci` - Run all CI checks locally
- `just dev` - Run the full development workflow
- `just compile <file>` - Compile a Python file to WebAssembly
- `just optimize <file>` - Compile a Python file with optimization

### CI Quality Checks

Before submitting a pull request, ensure your code passes all CI checks:

```sh
# Format your code
just format

# Check formatting
just format-check

# Run linting
just lint

# Run tests
just test

# Check documentation
just docs-check

# Run all CI checks locally
just ci

# Or run the full development workflow
just dev
```

All these checks run automatically on every pull request via GitHub Actions.

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
   - Use `just format` to format your code
   - Follow the [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)

2. **Linting**
   - Run `just lint` to check for common issues
   - Address all clippy warnings unless there's a good reason not to

3. **Documentation**
   - Add documentation comments to public APIs
   - Follow the [rustdoc conventions](https://doc.rust-lang.org/rustdoc/what-is-rustdoc.html)
   - If your change adds or completes a user-facing feature, update the development board (`docs/modules/index.html`). Tag the feature's version as `upcoming`; version tags on the board only ever name published releases, and maintainers swap `upcoming` for the real version number at release time

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

2. **Docs Sync**
   - In `docs/modules/index.html`, replace the `upcoming` version tags of features shipping in this release with the new version number. Features merged but not part of the release stay labeled `upcoming`; the board never shows a version that hasn't been published

3. **Publish**
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
