# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Standard Library Modules (Complete)**
  - **sys module**: System parameters and functions
    - Attributes: `argv`, `platform`, `version`, `maxsize`, `stdin`, `stdout`, `stderr`, `path`
  - **os module**: Operating system interface
    - Attributes: `name`, `sep`, `pathsep`, `linesep`, `devnull`, `curdir`, `pardir`, `extsep`
    - Functions: `getcwd`, `getenv`, `getpid`, `urandom`
  - **math module**: Mathematical functions and constants
    - Constants: `pi`, `e`, `tau`, `inf`, `nan`
    - Trigonometric: `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`
    - Hyperbolic: `sinh`, `cosh`, `tanh`
    - Exponential/Logarithmic: `exp`, `log`, `log10`, `log2`, `pow`
    - Rounding: `floor`, `ceil`, `trunc`, `round`
    - Utility: `sqrt`, `abs`, `fabs`, `copysign`, `fmod`, `remainder`, `degrees`, `radians`, `hypot`, `factorial`, `gcd`, `isnan`, `isinf`, `isfinite`
  - **random module**: Random number generation
    - Functions: `random`, `randint`, `randrange`, `uniform`, `choice`, `shuffle`, `sample`, `seed`, `getrandbits`, `gauss`, `normalvariate`, `expovariate`
  - **json module**: JSON encoding and decoding
    - Functions: `loads`, `dumps`, `load`, `dump`, `JSONEncoder`, `JSONDecoder`
  - **re module**: Regular expression operations
    - Functions: `compile`, `search`, `match`, `fullmatch`, `findall`, `finditer`, `split`, `sub`, `subn`, `escape`, `purge`
    - Flags: `IGNORECASE`, `MULTILINE`, `DOTALL`, `VERBOSE`, `ASCII` (and short forms: `I`, `M`, `S`, `X`, `A`)
  - **datetime module**: Date and time manipulation
    - Types: `datetime`, `date`, `time`, `timedelta`, `timezone`, `tzinfo`
    - Methods: `now`, `today`, `fromtimestamp`, `fromisoformat`, `strftime`, `strptime`, `replace`, `timestamp`, `isoformat`, `weekday`, `isoweekday`
    - Constants: `MINYEAR`, `MAXYEAR`
  - **collections module**: Specialized container datatypes
    - Functions: `namedtuple`, `deque`, `Counter`, `OrderedDict`, `defaultdict`, `ChainMap`, `UserDict`, `UserList`, `UserString`
  - **itertools module**: Iterator building blocks
    - Infinite iterators: `count`, `cycle`, `repeat`
    - Terminating iterators: `chain`, `compress`, `dropwhile`, `filterfalse`, `groupby`, `islice`, `starmap`, `takewhile`, `tee`, `zip_longest`
    - Combinatoric iterators: `product`, `permutations`, `combinations`, `combinations_with_replacement`
    - Additional: `accumulate`, `batched`, `pairwise`
  - **functools module**: Higher-order functions and operations on callable objects
    - Functions: `reduce`, `partial`, `partialmethod`, `wraps`, `update_wrapper`, `total_ordering`, `cmp_to_key`
    - Decorators: `lru_cache`, `cache`, `cached_property`, `singledispatch`, `singledispatchmethod`

- **Generator Functions & Iterators**
  - `yield` statement support in function bodies: `yield value`
  - Generator type system: `IRType::Generator[T]` for type tracking
  - Yield statement compilation to WASM instructions
  - Foundation for iterator protocol implementation
  - Support for generator expressions in comprehensions

- **Import System**
  - Import statement parsing: `import module` and `import module as alias`
  - From-import support: `from module import name1, name2` with aliases
  - Star imports: `from module import *` with detection and tracking
  - Conditional imports in try/except blocks with fallback tracking
  - Dynamic imports via `__import__(module_name)` function
  - Dynamic imports via `importlib.import_module(module_name)`
  - Module variable tracking and registration in IR
  - Module type system: `IRType::Module(name)` for imported modules
  - Import statement IR generation and WASM compilation

- **Functional Programming Features**
  - Lambda functions: Anonymous function support with `lambda x: x + 1` syntax
  - Parameter support in lambdas with type inference
  - Callable type tracking for function objects: `IRType::Callable { params, return_type }`
  - Closures: Variables captured from outer scope with `captured_vars` field
  - Foundation for higher-order functions (passing functions as arguments)

- **List Comprehensions**
  - List comprehension syntax: `[expr for var in iterable]`
  - Filter conditions in comprehensions: `[x for x in list if condition]`
  - Single generator comprehension support with proper iteration
  - Constant list literal optimization for comprehensions
  - Runtime support for variable-based iterables
  - Memory allocation for result lists via `allocate_list()` helper

- **Exception Handling Enhancements**
  - `raise` statement with exception type support
  - Exception type tracking and flag management in WASM execution
  - Multiple exception handler support (already present, verified working)
  - Exception propagation through try/except/finally blocks
  - Exception state preservation and restoration

- **Tuple Data Type**
  - Tuple literals with variable expressions: `(a, b, c)` and `(x + 1, y * 2)`
  - Tuple indexing with type tracking: `tuple[0]`, `tuple[1]`, etc.
  - Heterogeneous tuples with mixed types: `(42, "hello", 3.14)`
  - Empty tuples with type annotations: `empty: tuple[int] = ()`
  - Single-element tuples: `(value,)` with proper syntax
  - Proper type preservation for each element in the tuple
  - Memory layout: `[length:i32][elem0:i32][elem1:i32]...`

- **Range Function**
  - `range(stop)` - Single argument form
  - `range(start, stop)` - Two argument form
  - `range(start, stop, step)` - Full three argument form with custom step
  - Full integration with for loops
  - Range iteration support with step handling: `for i in range(0, 10, 2):`
  - Negative step support: `for i in range(10, 0, -1):`
  - Range object stored in memory: `[start:i32][stop:i32][step:i32][current:i32]`

### Changed
- Added `Yield { value }` statement variant for generator support
- Added `ImportModule { module_name, alias }` statement variant for module execution
- Added `Generator(Box<IRType>)` variant to `IRType` enum for generator type tracking
- Added `Lambda { params, body, captured_vars }` variant to `IRExpr` enum
- Added `Callable { params, return_type }` variant to `IRType` enum
- Updated `ListComp` handling to support filter conditions in comprehensions
- Added `allocate_list(element_count: u32)` helper method to `MemoryLayout`
- Removed error blocking for list comprehension filters (now supported)
- Enhanced type_to_string() function in both metadata.rs and lib.rs for Callable and Generator types
- Added `TupleLiteral(Vec<IRExpr>)` variant to `IRExpr` enum
- Added `RangeCall { start, stop, step }` variant to `IRExpr` enum
- Added `IRType::Range` to type system
- Enhanced for loop handler to support range iteration with proper step increments
- Extended compiler/function.rs to handle Yield and ImportModule statements

## [0.7.0](https://github.com/anistark/waspy/releases/tag/v0.7.0) - 2025-11-29

### Added
- **Bytes Type Support**
  - Bytes literals: `b"hello"` and `b'world'`
  - Bytes indexing (read & write): `bytes_var[i]` and `bytes_var[i] = value`
  - Bytes slicing: `bytes_var[start:end:step]` with proper bounds checking
  - Bytes concatenation with the `+` operator
  - Full WASM compilation support for binary data handling

- **Object-Oriented Programming**
  - Class definitions with full parsing, IR generation, and WASM compilation
  - Instance method definitions with implicit `self` parameter
  - Object instantiation via class constructor calls (e.g., `ClassName(args)`)
  - Automatic `__init__` method invocation during object creation
  - Method calls with proper dispatch to compiled methods (e.g., `obj.method()`)
  - Instance attribute access (getter): `obj.attr` returns field value
  - Instance attribute assignment (setter): `obj.attr = value` stores to memory
  - Per-instance field storage with calculated memory offsets
  - Memory layout extensions to support object heap allocation (starting at 64KB)
  - Qualified method export names (ClassName::method_name) for WASM exports
  - Class method compilation alongside module functions
  - Support for mixed instance variables and methods in class definitions
  - Proper type tracking with `IRType::Class(name)` throughout compilation

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
