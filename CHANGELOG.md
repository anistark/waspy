# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Control Flow Features - Full Implementation**
  - `for` loop iteration over lists and strings with proper element assignment
    - Allocates iterator state: pointer, counter, and length tracking
    - Loads list length from memory and iterates through indexed elements
    - Fallback support for integer-based counting
  - `try`/`except`/`finally` exception handling
    - Exception flag and type tracking using special local variables
    - Typed exception handlers (ZeroDivisionError, ValueError, TypeError, KeyError, IndexError, AttributeError, RuntimeError)
    - Bare except clause for catching all exceptions
    - Exception handler variable assignment (`except Error as e:`)
    - Multiple exception handler matching with type-based dispatch
    - Finally blocks execute regardless of exceptions
  - `with` statement context manager support
    - Context expression evaluation and variable binding (`with expr as var:`)
    - Exception state preservation across with blocks
    - Proper exception flag initialization and restoration

- **Built-In Functions Implementation**
  - `len()` function: Full support for strings, lists, and dictionaries
    - For strings: Returns length from stack (offset, length) pair
    - For lists/dicts: Loads length from first 4 bytes in memory
  - `print()` function: Proper argument handling with type-aware stack cleanup
    - Handles string pairs (offset, length) separately from scalar types
  - `min()` and `max()` functions: Multiple argument support
    - Iterative comparison with conditional branch logic
    - Stack-based implementation with temporary local variables
  - `sum()` function: Partial implementation with iterable and start value support
  - Comprehensive test file: `examples/builtins.py` demonstrating all functions

- **Collections Support**
  - List literals with memory allocation: `[1, 2, 3]`
  - List indexing (read & write): `list[i]` and `list[i] = value`
  - List methods: `.append(value)`, `.pop([index])`, `.clear()`, `.insert(index, value)`
  - List search methods: `.index(value)` with linear search returning -1 if not found, `.count(value)`
  - Dictionary literals with memory allocation: `{"key": value}`
  - Dictionary indexing (read & write): `dict[key]` and `dict[key] = value`
  - Efficient memory allocation strategy: List ptr + 4 + (index * 4)
  - Type tracking for `List[T]` and `Dict[K, V]`

### Fixed
- ✓ List indexing implementation in compiler/expression.rs
- ✓ Dictionary indexing implementation in compiler/expression.rs
- ✓ List assignment support via new `IndexAssign` IR statement
- ✓ Dictionary assignment support via `IndexAssign` IR statement
- ✓ List method call dispatch in emit_list_method_call function

### Changed
- Restructured IR types to support `IndexAssign` statement for subscript assignments
- Enhanced converter.rs to handle subscript assignments in AST to IR conversion
- Updated modules.html documentation to reflect completed Collections module

## [0.6.3](https://github.com/anistark/waspy/releases/tag/v0.6.3) - 2025-11-15

### Added
- Documentation updates for module development board
- Verbose AST and IR logging example entries
- Configurable logging options for verbose and debug modes
- IRModule logging for better development experience
- GitHub Actions CI workflows
- Documentation website with module development status board

### Fixed
- Packaging files organization
- Format and linting issues
- rustfmt.toml configuration for stable Rust
- Binaryen upgrade compatibility
- Documentation links

### Changed
- WebAssembly compilation pattern improvements
- Removed cross-platform matrix testing at this stage
- Updated WASM compilation with FFI for wasmrun

## [0.6.2](https://github.com/anistark/waspy/releases/tag/v0.6.2) - 2025-09-25

### Added
- wasmrun plugin integration for WASM runtime execution

### Fixed
- Cargo build after upgrading binaryen

## [0.6.1](https://github.com/anistark/waspy/releases/tag/v0.6.1) - 2025-08-04

### Added
- Documentation homepage
- Module development status page with Kanban board visualization
- Interactive development board for tracking feature status

### Fixed
- Documentation links and navigation

## [0.6.0](https://github.com/anistark/waspy/releases/tag/v0.6.0) - 2025-06-24

### Added
- **Decorator Support**
  - @memoize decorator for function result caching
  - @debug decorator for logging function calls
  - @timer decorator for performance measurement
  - Custom decorator registration mechanism

- **Raise Statement Parsing**
  - Exception raising syntax support (parsing only)

- **Verbose Logging**
  - AST log in verbose mode for debugging

### Changed
- Project renamed from ChakraPy to Waspy (2025-06-06)
- Code refactoring for better maintainability
- Linting and formatting improvements

### Fixed
- Linting errors across codebase

## [0.5.0](https://github.com/anistark/waspy/releases/tag/v0.5.0) - 2025-05-31

### Added
- **Multi-File Project Support**
  - Multi-file compilation to single WASM module
  - Dependency analysis with circular dependency detection
  - Entry point detection (`__main__.py` and `if __name__ == "__main__"`)
  - Configuration file parsing (setup.py, pyproject.toml, __init__.py)

- **Import System**
  - Import syntax parsing (all types: `import`, `from ... import`, star imports)
  - Conditional imports in try/except blocks
  - Dynamic imports using `__import__()` and `importlib.import_module()`
  - Dynamic import expression handling

### Changed
- Module variable support with operator identification
- Project structure reorganization

## [0.4.0](https://github.com/anistark/waspy/releases/tag/v0.4.0) - 2025-05-01

### Added
- **Complete Core Language Features**
  - Arithmetic operations: `+`, `-`, `*`, `/`, `%`, `//`, `**`
  - Comparison operations: `==`, `!=`, `<`, `<=`, `>`, `>=`
  - Boolean operations: `and`, `or`, `not` with short-circuit evaluation
  - Bitwise operations: `&`, `|`, `^`, `<<`, `>>`, `~`

- **Control Flow**
  - `if`/`elif`/`else` statements with proper branching
  - `while` loops with exit conditions
  - Comparison and boolean logic operations

- **Functions**
  - Function definitions with parameters
  - Type annotations for parameters and return types
  - Function calls between compiled functions
  - Multiple functions per module support
  - Augmented assignment operations: `+=`, `-=`, `*=`, `/=`, `%=`, `//=`, `**=`

- **Type System**
  - Basic types: `int`, `float`, `bool`, `str`
  - Type annotations and inference
  - Type coercion between compatible types
  - Support for generic types: `List[T]`, `Dict[K,V]`, `Tuple[T,...]`
  - Union and Optional types: `Union[T,U]`, `Optional[T]`
  - Custom class type annotations

- **Variables & Assignment**
  - Variable declarations and assignments
  - Attribute assignment: `obj.attr = value`
  - Augmented assignment operations
  - Type inference from usage patterns

## [0.3.0](https://github.com/anistark/waspy/releases/tag/v0.3.0) - 2025-05-02

### Added
- **String Operations (Complete Implementation)**
  - String literals and constants
  - String indexing with positive and negative indices
  - String slicing: `str[start:end:step]` with bounds checking
  - String concatenation with the `+` operator
  - Compile-time concatenation optimization for constants

- **String Methods (20+ methods implemented)**
  - Case conversion: `.upper()`, `.lower()`, `.capitalize()`, `.title()`
  - Whitespace handling: `.strip()`, `.lstrip()`, `.rstrip()`
  - Test methods: `.isdigit()`, `.isalpha()`, `.isalnum()`, `.isspace()`, `.isupper()`, `.islower()`
  - Search methods: `.find()`, `.index()`, `.count()`, `.startswith()`, `.endswith()`
  - Transform methods: `.replace()`, `.split()`, `.join()`
  - Layout methods: `.ljust()`, `.rjust()`, `.center()`

- **String Formatting**
  - `.format()` method with support for `{}`, `{0}`, `{name}` placeholders
  - `%` string formatting with `%s`, `%d`, `%f`, `%x`, `%o`, `%%` specifiers
  - f-string support with constant and dynamic variable interpolation
  - Compile-time optimization for constant strings

### Changed
- Code modularization and refactoring
- Code organization improvements

## [0.2.0](https://github.com/anistark/waspy/releases/tag/v0.2.0) - 2025-04-29

### Added
- **Project Management Infrastructure**
  - Multi-function support in single module
  - Operator categorization and labeling
  - Build system with category updates

- **Documentation & Examples**
  - Basic project documentation
  - Example compilation workflows

### Changed
- Category organization and labeling system
- Base compiler architecture improvements

## [0.1.0](https://github.com/anistark/waspy/releases/tag/v0.1.0) - 2025-04-28

### Added
- **Initial Compiler Implementation**
  - Base WebAssembly code generation
  - Python to WASM compilation pipeline
  - AST parsing and IR conversion
  - Function compilation from Python to WASM instructions
  - Basic operator support and execution

- **Development Tools**
  - Error handling system with structured error types
  - WebAssembly optimization using Binaryen
  - Metadata extraction from compiled modules
  - Compiler context management for local variables
  - Memory layout management for string storage

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines on contributing to Waspy.

## License

See LICENSE file for details.
