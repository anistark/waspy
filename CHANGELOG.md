# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [Unreleased]

### Added
- Coverage audit: every bundled `examples/*.py` (plus the multi-file `examples/user_modules_app/` and the project-directory `examples/calculator_project/`) now has an integration test that compiles it, instantiates the WASM with `wasmi`, and asserts concrete runtime results (`tests/integration/coverage.rs`), alongside operator-level unit suites for arithmetic, comparisons, boolean/bitwise logic, conversions, and augmented assignment (`tests/unit/basics.rs`) and for error quality (`tests/unit/errors.rs`). The integration harness gained untyped-call helpers (f64/mixed signatures), a disk-based entry-file compile path, and an in-memory `waspy_host` filesystem that drives `examples/file_io.py` end to end. The suite runs as an explicit named gate in the test workflow
- Early validation of unsupported Python syntax: `parse_python` now walks the AST before lowering and rejects `async def`/`async for`/`async with`/`await`, `match`, `global`, `nonlocal`, `del`, `assert`, `type` aliases, `except*`, `from module import *`, `*args`/`**kwargs`/keyword-only parameters, class keywords (metaclasses), and loop `else:` clauses — each with the construct named, its line/column, the enclosing function, and a workaround hint, instead of failing deep in codegen with an AST debug dump or compiling silently wrong code (loop `else` bodies were previously dropped on the floor). `min()`/`max()` over a single iterable argument (previously a stub that always produced 0) are rejected the same way until implemented
- Python syntax errors now report line and column (computed from the parser's byte offset) with the parser's own message; `ChakraError`'s located variants render their position in `Display`
- `sum()` over lists and tuples computes a real result — the previous codegen was a stub that always produced 0. The emitted loop walks the collection's `[len][slot...]` layout, accumulates at f64 width for float lists (all-float tuples included), and honors the optional start argument
- New `examples/algorithms.py` (gcd, primality, digit math, Collatz, Newton's sqrt) and `examples/calculator_project/` (the project-compilation demo the justfile referenced but which was never committed); `examples/builtins.py`, `bytes_example.py`, `range_example.py`, `set_example.py`, and `tuple_example.py` gained assertable functions so the harness checks real values
- justfile recipes: `verify-examples` (compile every bundled example through the real drivers, including the multi-file and project paths), `benchmark` (wall-clock compile times, release build), and `clean-all` (build artifacts plus generated example outputs); `just compile` now reports Python-source vs WASM-output sizes after compilation
- User-defined module imports ([#41](https://github.com/anistark/waspy/issues/41)): programs import their own `.py` files, not just the bundled stdlib. A new entry point `compile_python_file` (and `compile_python_file_with_options`) resolves an entry file's imports against its directory — `import mod` finds `mod.py` or `mod/__init__.py`, `import pkg.mod` finds `pkg/mod.py` — transitively through each resolved module's own imports, and links everything into the single output WASM module. Within any multi-file compilation (`compile_multiple_python_files*`, `compile_python_project*`, and the new resolver), all import forms now resolve across files: `from mod import f` calls the merged function, `from mod import f as g` binds the alias, `import mod` / `import mod as m` make namespace calls (`mod.f(...)`), namespace constants (`mod.CONST`, inlined like any module-level variable), and namespace class instantiation (`mod.ClassName(...)`) work, with a local variable of the same name shadowing the module binding. Import-resolved module files bypass the special-file skip, so a genuine local module named e.g. `config.py` (or a package `__init__.py`) still links
- Module caching ([#41](https://github.com/anistark/waspy/issues/41)): a module imported through several paths is compiled and merged exactly once. The import resolver visits each module a single time (a diamond `app -> util/helper -> shared` links one copy of `shared`), and the multi-file merge skips a filename it has already processed, so re-imports never duplicate functions or module state
- File I/O through a documented host interface ([#25](https://github.com/anistark/waspy/issues/25)): `open(path[, mode])`, `f.read([n])`, `f.write(s)`, `f.close()`, `f.flush()`, and `with open(...) as f:` (desugared during lowering to open/body/close, sidestepping the unsupported general context-manager path, #5). The web target has no filesystem, so file operations compile to calls into four imported host functions — WASM module `waspy_host`: `open(path_ptr, path_len, flags) -> fd`, `read(fd, buf, len) -> n` (0 = EOF), `write(fd, buf, len) -> n`, `close(fd) -> status` — that the embedder provides; mode strings are folded to flag bits at compile time (`r`=1, `w`=2, `a`=4, `b`=8, `+`=16). `read()` fills a fresh length-prefixed heap blob and returns it as a regular string (default cap 64 KiB per call; `read(n)` caps at `n`); a new `IRType::File` types handles so method calls dispatch statically. The import section is emitted **only when the program calls `open()`** — everything else keeps instantiating with an empty import object, and the wasmi test suite ships a reference host implementation over an in-memory filesystem (`examples/file_io.py` shows the JS equivalent)
- Conditional imports in `try`/`except` verified end to end ([#4](https://github.com/anistark/waspy/issues/4)): an import inside `try`/`except ImportError` parses, resolves, and its members are usable afterwards, asserted by a runtime test
- `examples/user_modules_app/` (namespace calls, aliases, a module constant, a class imported across modules, and a shared module imported twice) and `examples/file_io.py` (write/read round trip, nested `with open`, append mode), both compiled and executed against Node as part of verification
- `just compile <file>` (the `advanced_compiler` example) and the wasmrun plugin's single-file build now compile by path, so an entry file's local imports resolve automatically

### Changed
- **Breaking (pre-1.0):** `CompilerOptions` now carries exactly the options the pipeline honors — `optimize` and `verbosity`. The five removed fields (`debug_info`, `max_memory`, `entry_point`, `generate_html`, `include_metadata`) were never read by any compilation stage: linear memory is sized automatically from the module's data and grows on demand, entry points are auto-detected, and metadata printing/HTML harness generation are driver concerns (the example drivers and the wasmrun plugin keep their behavior through their own flags)
- **Breaking (pre-1.0):** the `waspy::parser` crate-root re-export is gone; use `waspy::core::parser` (the root surface no longer leaks rustpython AST types). Crate-level rustdoc now states the stable public API — the crate-root exports — with a compilable quick-start example, and marks the pipeline modules as implementation detail
- Augmented assignment on f64 locals (`x += 2.0`, `x /= 2.0`, …) emitted i32 arithmetic on f64 operands and produced invalid WASM; it now selects the instruction width from the local's type and coerces the operand, and the bitwise/shift augmented operators (`&=`, `|=`, `^=`, `<<=`, `>>=`) gained real implementations instead of a placeholder that zeroed the target
- `examples/stdlib_test.py` restructured from a script with no functions (which the per-file driver rejected) into module-level imports plus a `main()` entry point; the multi-file and project example drivers print real usage text
- Cargo.toml metadata says what Waspy is (a Python-to-WebAssembly compiler, not an interpreter); README documents the supported Python subset and the explicitly-rejected constructs in one authoritative place, and the docs site's feature/status claims are synced with the compiler (i32/f64 value types, `sum()` status, `with` statement coverage, 0.20.0 hardening entries)
- The three multi-file compilation entry points share one merge implementation; project compilation (`compile_python_project*` / `compile_multiple_python_files_with_config`) now applies function decorators like every other path (it previously skipped decorator processing) and merges per-file IR metadata consistently
- A source file containing only module-level constants (no functions) now contributes its variables to a multi-file merge instead of being skipped
- Generators actually run ([#6](https://github.com/anistark/waspy/issues/6), [#45](https://github.com/anistark/waspy/issues/45)), replacing the placeholder that dropped every yielded value. A generator function is rewritten during IR lowering into a resumable state machine: a synthesized state class holds the resume point, the `send()` value, and every parameter and local as instance fields (so all live state survives suspension in linear memory), and a `__step` method dispatches the original body — flattened into basic blocks — on the stored resume point inside a trampoline loop. `yield v` stores the next block id and returns `v`; the next request re-enters at the stored block. Calling the generator function returns a fresh suspended generator object (a plain heap instance), so several instances of one generator advance independently. Yields work inside `while` loops, `for`-over-range/list/tuple loops, and conditionals; a generator yielding float values produces f64s end to end; `return` (or falling off the end) marks the generator exhausted and raises `StopIteration`
- `yield from` delegates to an inner iterable — a range, a list, or another generator — and generators compose (a generator can drive another generator in its own `for` loop, including recursively)
- The full generator protocol: `for x in gen(...)` drives a generator to exhaustion, `next(g)` pulls one value, `x = yield v` resumes with the value passed by `g.send(v)` (0 when resumed by plain `next`), and `g.close()` finalizes the generator so later requests raise `StopIteration`. Iteration is desugared at the IR level into an explicit `__next__` drive loop with static `Class::method` dispatch; exhaustion crosses the call boundary through a dedicated StopIteration flag (a new WASM global) set by `raise StopIteration` and read-and-cleared by a codegen intrinsic
- Custom iterator protocol ([#40](https://github.com/anistark/waspy/issues/40)): a user class implementing `__iter__`/`__next__` iterates in a `for` loop, with `raise StopIteration` in `__next__` ending the loop — the same drive-loop machinery generators use. `__iter__` is honored when present (its declared return class types the iterator); a class with only `__next__` iterates itself
- Unsupported generator shapes fail loudly at compile time instead of miscompiling: `yield`/`return` inside `try`/`with` (suspension cannot re-enter a protected frame) and generator *methods* (`yield` inside a class method) are rejected with clear errors. Known subset limits: generator locals bound by tuple unpacking or by a yield-free `for` loop live in WASM locals and don't survive across a `yield`; `send()` on an unprimed generator starts it like `next()`; `close()` skips `GeneratorExit`/`finally` semantics
- `examples/generators.py`, asserted end to end by the integration suite: while/range/conditional suspension, early `return`, `yield from` over a range and a generator, generator-consuming-generator, manual `next()`, `send()` accumulation, `close()`, a user `Countdown` iterator class, and `break` out of a drive loop
- Tuple targets in `for` statements: `for a, b in pairs` binds a hidden loop variable and unpacks it per iteration (star targets included, reusing the extended-unpacking machinery), and the iterator-shaped builtins desugar during lowering — `for i, x in enumerate(xs[, start])` threads an explicit counter alongside the driven iterable, `for a, b, ... in zip(s0, s1, ...)` drives the first sequence and indexes the rest with a shared counter (stopping at the shortest), and `for k, v in d.items()` (plus single-target `.keys()` / `.values()`) walks the dict's entry slots positionally through two new codegen intrinsics. All of it composes with generators: an `enumerate` or `items()` loop containing `yield` suspends and resumes with its counters and dict pointer preserved in the generator state. `examples/loop_unpacking.py` asserts each shape at runtime
- `len()` of a value whose type codegen can't resolve (e.g. a collection read back out of an instance field) now reads the count word from the pointer instead of answering a constant 0, and instance fields initialized with collection literals or `range()` keep their collection type ([#44](https://github.com/anistark/waspy/issues/44)), replacing the placeholder that evaluated the iterable and yielded an empty list. List (`[x * 2 for x in xs if cond]`), set (`{x % 3 for x in xs}`, deduped at construction via a runtime-built open-addressing hash table), and dict (`{k: v for k, v in items}`, including tuple-unpacking targets) comprehensions all build their result at runtime in a fresh `__alloc` block — the element count depends on iterable lengths and filters, so capacity is computed first (the iterable's length, or a counting pre-pass over the outer generators when there are several; with multiple generators an inner iterable expression is therefore evaluated once per outer iteration in both passes) and the final count is written back after the fill loops run. Iterables can be lists, tuples, or ranges (ascending and descending, with runtime trip-count math; strings and sets as comprehension iterables are a follow-up); filters compose per generator; float elements round-trip as f64 slots. Multiple generators (`[x for row in m for x in row]`) and nesting (a comprehension as another's element or iterable, tracked by a per-function nesting depth that keys the reserved helper locals) work, as does a comprehension as a `for` statement's iterable. Generator expressions lower as list comprehensions (every consumer here drains them eagerly). Python 3 scoping is honored: comprehension variables are renamed to unique names during lowering, so they never clobber (or leak into) same-named function locals
- Comprehension results adopt their concrete element types on assignment (e.g. `List(Float)`, `List(List(Int))`), so indexing a float or nested result loads the right width — the scan pass can only type them as collection-of-Unknown, which previously stuck
- Full closure variable capture ([#43](https://github.com/anistark/waspy/issues/43)), replacing the stub that compiled every lambda to the constant `1`. A whole-module finalize pass lifts each lambda into a real function whose trailing `__env` parameter carries a heap environment `[table_slot][captured...]`; free variables are detected by scope analysis (parameters, nested-lambda parameters, and comprehension targets bind; module functions/classes/variables, stdlib modules, and builtins resolve globally) and captured into the environment at creation. Closure values dispatch through a funcref table with `call_indirect` (one signature per arity), so closures are first-class: returned from functions (`make_adder(5)` works), passed as arguments and called through untyped parameters, stored in collections, nested (`lambda x: lambda y: x + y` — the inner closure captures the outer's parameter), and defined at module level. Capture is by value at creation time — a captured variable mutated after the closure is created keeps its old value inside the closure (Python's late-binding cells are a follow-up), and float captures read as 0 for now. Calling a subscripted expression directly (`fs[0](x)`) is not lowerable yet; bind it to a local first
- Extended (starred) unpacking ([#24](https://github.com/anistark/waspy/issues/24)): `a, *b, c = xs` binds scalars positionally from the front and back and collects the middle slice into a fresh runtime list with one `memory.copy` (slots are contiguous), so `len(b)`, indexing, and iteration work on the starred target. The star can sit anywhere (`*xs, last` / `first, *rest`), tuples and lists both unpack, and an exact-fit unpack leaves the starred list empty rather than trapping
- `examples/comprehensions.py`, `examples/closures.py`, and `examples/extended_unpacking.py`, each asserted end to end by the integration suite (filters, multi-generator flattening, comprehension-in-comprehension, set membership on comprehension results, per-iteration freshness inside loops; capture independence between closures from one factory, zero-argument closures, closures built inside a comprehension; star-position variants and empty middles)

## [0.12.0](https://github.com/anistark/waspy/releases/tag/v0.12.0) - 2026-07-12

### Added
- `@dataclass` ([#18](https://github.com/anistark/waspy/issues/18)): a class decorated with `@dataclass` (or `@dataclasses.dataclass`) gets `__init__`, `__eq__`, and `__repr__` generated from its annotated fields during IR conversion, so the regular field-discovery and instantiation machinery applies unchanged. The constructor takes one parameter per field with field defaults honored — a call site that omits trailing arguments has the defaults spliced in by a new whole-module post-pass (`src/ir/finalize.rs`), which also fills parameter defaults for ordinary function calls (previously an omitted default underflowed the stack into invalid WASM) and rejects a construction missing a required argument. Python's dataclass rules are enforced at compile time: a field without a default may not follow one with a default, mutable defaults (list/dict/set literals) are rejected, and `dataclasses.field(...)` / `@dataclass(...)` with arguments fail loudly as unsupported. A method the user writes in the class body wins over the generated one; `ClassVar`-annotated names stay class variables
- `==` / `!=` between class instances now dispatches to `__eq__` when the left operand's class defines or inherits one (generated or hand-written) — the two instance pointers already on the stack are exactly the `(self, other)` argument pair, keeping dispatch static like the rest of the object model. Dataclass equality therefore compares field values; classes without `__eq__` keep pointer identity
- The generated `__repr__` renders `Name(field=value, ...)` at runtime: a new runtime helper `__i32_to_str(value) -> offset` (emitted alongside `__alloc`/`__alloc_obj`) renders an i32 as decimal digits in a length-prefixed `__alloc` blob, and `str(x)` on runtime `int`/`bool` values now compiles to it (previously `str` was an unknown builtin yielding garbage). String fields are spliced in quoted, like Python's repr; a dataclass with a float field skips `__repr__` generation (no f64 formatter yet)
- Abstract base classes ([#13](https://github.com/anistark/waspy/issues/13)): a class deriving from `abc.ABC` (directly or transitively) that still has unimplemented `@abstractmethod` methods rejects instantiation at compile time with Python's "Can't instantiate abstract class" TypeError message — including a subclass that fails to implement an inherited abstract method. Concrete methods on the ABC are inherited normally, `isinstance` works against the abstract base, and the `ABC` base is a marker that neither occupies the single-inheritance slot nor contributes layout. `abc` and `dataclasses` are recognized stdlib modules (compile-time only, no runtime surface)
- String/bytes-typed instance fields now work end to end: `self.text = "..."` narrows the `(offset, length)` pair to the offset word its 8-byte slot holds (previously the store left an extra value on the stack — invalid WASM), and reading the field rebuilds the pair from the blob's length prefix. Call results were fixed the same way across every call path (user functions, methods, property getters, `super()` calls): a `str`-returning callee leaves a single offset word, and the caller now recovers the length via `load(offset - 4)` instead of misreading the stack
- `pass` compiles as the no-op it is (e.g. an `@abstractmethod` body); previously any function containing it failed conversion
- `examples/oop_dataclasses.py` (construction with and without defaults, override of a default, `==`/`!=` by value, `__repr__` round-tripped byte-for-byte including a negative int and a quoted string field) and `examples/oop_abc.py` (concrete subclass instantiation, inherited concrete method, abstract-method dispatch, `isinstance` against the ABC); the integration suite asserts each runtime result plus the six rejection errors (default ordering, mutable default, `field(...)`, missing required argument, abstract class, abstract subclass)
- Method kinds beyond plain instance methods, dispatched statically by a per-method kind recorded at class registration: `@staticmethod` ([#17](https://github.com/anistark/waspy/issues/17)) takes no implicit argument and is callable on the class (`Counter.add(a, b)`) or on an instance (whose pointer is dropped); `@classmethod` ([#16](https://github.com/anistark/waspy/issues/16)) receives the class implicitly — call sites push the class id as `cls`, and inside the body `cls(...)`, `cls.method(...)`, and `cls.var` resolve statically to the defining class (consistent with the object model's no-vtable dispatch), enabling the classmethod factory pattern; `@property` with `@<name>.setter` ([#10](https://github.com/anistark/waspy/issues/10)) compiles `obj.attr` reads to the getter, `obj.attr = v` assignments to the setter, and `obj.attr OP= v` to a getter-then-setter chain, instead of direct field access — a property's name never occupies a field slot (the setter body assigns the real backing field, e.g. `self._attr`)
- Conflicting or unsupported method decorator stacks fail compilation with a clear error instead of silently mis-dispatching: combining two kinds (e.g. `@staticmethod` + `@classmethod`), a `@<name>.setter`/`@<name>.getter` whose name doesn't match its method, a setter with no matching `@property` getter (it could never be reached), and property deleters (unsupported)
- `examples/oop_method_kinds.py` covering a static method called on the class and on an instance, a classmethod factory via `cls(...)`, a classmethod called through an instance, property reads (stored and computed), a property setter, and augmented assignment through a property; the integration suite asserts each runtime result plus the three rejection errors
- Single class inheritance with method resolution ([#9](https://github.com/anistark/waspy/issues/9)): a subclass extends one base class, inheriting its fields and methods. The base's fields are laid out as a prefix of the subclass instance (identical offsets), so a base method reading `self.x` works unchanged on a subclass instance; the subclass's own fields append after the base's size. An inherited method dispatches to the base's already-compiled WASM function (no duplication); a method redefined in the subclass overrides it at call sites typed as the subclass. Dispatch remains fully static — a base method calling `self.helper()` internally resolves to the base's `helper` even on a subclass that overrides it (true virtual dispatch through `self` would need a vtable and stays out of scope). Multiple inheritance is rejected with a clear compile error instead of silently mislaying fields
- `super().__init__(...)` and `super().method(...)` dispatch statically to the immediate base class of the enclosing method's class, passing `self` (local 0) through, so construction and behavior chain across multi-level hierarchies (each class's method table already contains its base's fully resolved entries)
- `isinstance(obj, ClassName)` and `issubclass(Sub, Base)` over the user class hierarchy. Every instance now carries its class id in the tag word at offset 0 (the slot every layout already reserved), stamped by a new runtime helper `__alloc_obj(size, class_id)` that wraps `__alloc` — keeping the instantiation sequence stack-only. `isinstance` compares the runtime tag against the target class and all its subclasses, so it answers correctly even when the static type is a base class (e.g. a factory annotated `-> Animal` returning a `Dog`); `issubclass` folds to a compile-time constant. Checks against built-in types (`isinstance(x, int)`) are a follow-up
- `examples/oop_inheritance.py` covering method override, an inherited method reading a base-prefix field, `super().__init__` chaining, `super().method()` reaching the base implementation past an override, a two-level `Puppy -> Dog -> Animal` hierarchy, `isinstance` across the hierarchy and via a base-typed factory, and compile-time `issubclass`; the integration suite asserts each runtime result plus the multiple-inheritance rejection
- Heap-allocated, multi-instance objects: `ClassName(...)` now calls the runtime bump allocator (`__alloc(instance_size)`) and returns a distinct pointer per instantiation, replacing the fixed compile-time address that limited every class to a single live instance. Multiple instances of one class coexist with independent field state, and instances are first-class values — passable as arguments, returnable from factory functions, storable in collections (the slot holds the instance pointer, consistent with the string/bytes convention), and mutable through the shared pointer. `__init__` is compiled to return `self` so the instantiation sequence is stack-only and nested instantiations compose; classes without `__init__` allocate a zeroed instance directly. Objects are not reclaimed (the bump allocator has no `free`) and `__del__` is not invoked; GC is tracked post-1.0
- `examples/oop_objects.py` covering two independently mutated instances, a fresh zeroed instance per call, a factory return, an instance mutated through a function argument, instances stored in a list/tuple/dict and read back live, augmented assignment on a field (`self.value += n`), and per-instance f64 fields; the integration suite asserts each runtime result
- Integration test harness (`tests/integration/`, `tests/utils/`, registered as the `integration_examples` test target): compiles every `examples/*.py`, validates and instantiates the module with `wasmi`, and asserts runtime results (e.g. `break`/`continue` and multi-file cross-calls). The sweep immediately surfaced the two parameter/`raise` fixes below
- `examples/nested_collections.py`, covering nested list-of-lists indexing, a per-iteration list literal that escapes its loop, float dict/set values, and lossless f64 round-trip through list/dict/tuple slots, `in`, and float-list iteration; the integration suite asserts each result
- Non-lossy f64 collection layout: every collection element now occupies an 8-byte slot (the count header stays 4 bytes), so float members of lists, dicts, sets and tuples round-trip with full f64 precision instead of being narrowed to f32 (~7 significant digits). The change spans every access path — literals, indexing, `dict[key]` lookup/assign, set dedup, `in`/`not in`, `for` iteration (a float list literal binds an f64 loop variable), and the list/tuple methods (`append`, `pop`, `extend`, `insert`, `remove`, `index`, `count`). Slot address arithmetic is type-independent; only the load/store/compare width is chosen by element type. Binding floats from a `for`/tuple-unpack over a *variable* (rather than a literal) remains a follow-up
- Sets are now an open-addressing hash table (linear probing) instead of a linear array, so membership (`in`/`not in`) and construction dedup are amortised constant time rather than `O(n)`/`O(n²)` scans. Layout is `[count:i32][cap:i32]` followed by `cap` buckets of `[state:i32][_pad][value:8 bytes]`, with `cap` a power of two kept above the member count so a probe always terminates at an empty bucket; the member count stays at offset 0 so `len()` is unchanged. The whole region is zeroed (`memory.fill`) on construction, which also clears stale bucket state when a set literal is rebuilt each iteration of an enclosing loop. Float members are hashed by folding both halves of their f64 bit pattern and compared at full width. Lists keep their linear `in` scan

### Fixed
- A quoted forward-reference annotation (`def create(...) -> "Counter":`) now resolves to the class type instead of `Any`, so a value returned under one is typed as its class — previously a method call on such a value hit the unknown-object path, which drops two stack values and produced invalid WASM
- A string/bytes argument to a class constructor (`ClassName("text")`) is now narrowed to its single offset word like any other user-function argument; previously the full `(offset, length)` pair was pushed against `__init__`'s one parameter slot, leaving an extra value on the stack (invalid WASM)
- Float `dict` *keys* now match at full f64 width on both lookup and index-assign. Previously the key expression was coerced to `int` and compared with `i32.eq`, so `{1.5: ...}[1.5]` never matched — and because `1.5` and `2.5` share their low 32 bits, distinct float keys were also indistinguishable. The key path is now width-aware (mirroring the value path): the index is hinted with the container's key type, a float key needle is kept in a dedicated second f64 scratch so it can coexist with a float value, and the search compares (and the append stores) at `f64` width
- A collection literal built inside a loop reused one compile-time region every iteration, so per-iteration lists/dicts/sets/tuples that escaped the loop all aliased the last iteration's data ([#14](https://github.com/anistark/waspy/issues/14)). Inside a loop the literal is now built into that shared template region and then copied (`memory.copy`) into a fresh runtime `__alloc` block, so each iteration's collection gets its own region; outside a loop the unique compile-time region is still used directly. Nested literals compose — an inner literal stores its own runtime pointer into the outer template before the outer region is copied out
- Float values in `dict`, float members of a `set`, and float elements of lists/tuples now keep full f64 precision (see the 8-byte slot layout above); previously they were stored as f32, and a float-valued `dict` or `list[i] = <float>` index-assign could even emit a wrong-width store that overflowed into the next slot
- `raise ExceptionType(arg)` (e.g. `raise ValueError("msg")`) emitted the constructor call and left its argument on the stack, producing invalid WASM ("values remaining on stack"); the raised exception is now resolved to its integer type code by name, shared with the `except` handler dispatch so the two cannot diverge
- String/bytes function parameters now form a complete `(offset, length)` pair. Referencing a `str`/`bytes` parameter (e.g. `op == "add"`) pushed only its offset, so consumers like `==` underflowed the stack into invalid WASM; the length is recovered from the blob prefix (`load(offset - 4)`) when there is no companion length local. Passing a string/bytes value as an argument to a user function now narrows it to the single offset word each parameter slot holds (the callee recovers the length), fixing an out-of-bounds read that previously passed the length as the offset

## [0.11.0](https://github.com/anistark/waspy/releases/tag/v0.11.0) - 2026-06-27

### Added
- `break` and `continue` statements in `for` and `while` loops. Each loop body is wrapped in an inner block so `continue` falls through to the iterator step, and a block-depth counter plus loop-context stack compute the correct branch depth so break/continue nested inside `if`/`try` frames or nested loops target the right loop ([#23](https://github.com/anistark/waspy/issues/23))
- Runtime bump allocator (`__alloc(size) -> ptr`) over a mutable global heap pointer that grows linear memory on demand (no `free`), emitted after the user functions; backs runtime-built strings/bytes such as concatenation results

### Fixed
- `for x in <list>` read each element from `ptr + i*4` with load offset 0, loading the leading length word as element 0 and dropping the last element; the load now uses a `+4` offset so iteration sees the real elements
- Reading a string/bytes value out of a list or tuple rebuilds its `(offset, length)` pair instead of emitting invalid WASM (a `local.set` stack underflow): each interned string/bytes blob is laid out as `[len:i32][bytes][nul?]` with its recorded offset pointing past the prefix, so the length is recovered via `load(offset - 4)`; the stored slot stays the interned offset, so offset-based membership and set de-duplication are unchanged
- String/bytes concatenation (`+`) copies both operands into a freshly allocated, length-prefixed blob via `memory.copy`, replacing the placeholder that returned the left operand's offset with the combined length (correct only when literals happened to be adjacent in memory)
- A string/bytes function return yields the value's offset rather than its length (a length-prefixed blob lets the caller recover the length via `load(offset - 4)`)
- Binaryen optimization runs with the bulk-memory feature enabled so the `memory.copy` emitted by concatenation survives optimization instead of aborting it

## [0.10.0](https://github.com/anistark/waspy/releases/tag/v0.10.0) - 2026-06-15

### Fixed
- Compiled modules are valid and runnable: Code section before Data, per-function scratch locals, corrected `while` exit test, bare `list`/`dict`/`set`/`tuple` annotations, and `MemoryLayout` propagated to codegen ([#78](https://github.com/anistark/waspy/pull/78))
- Collection runtime: `dict[key] = value` update/append, set de-duplication, `in`/`not in` for sets and lists, and `len()` for sets/tuples/bytes ([#78](https://github.com/anistark/waspy/pull/78))
- Collections no longer alias in memory — each list/set/tuple/dict/range literal gets its own region via a compile-time bump allocator; distinct and nested literals previously shared one address (Issue #14)
- Float elements in lists/tuples round-trip correctly (stored as f32 in the one-word slot), and unannotated collection locals keep their element type so `xs[i]` no longer returns `0`
- `for ... in range(...)` iterates (ascending): iterator locals are reserved up front (nested loops get distinct locals) and range fields use the correct store operand order
- `try`/`except`/`finally` compiles to a valid module: exception-state locals reserved in the scan, and balanced control flow (was emitting one `End` too many)
- Numeric typing: `and`/`or` yield an `i32` result, mixed int/float arithmetic widens the int operand to `f64`, unannotated float locals infer as `f64`, and locals are declared in index order
- Module-level variables (e.g. `PI = 3.14159`) resolve inside functions by inlining their initializer
- `int()` / `float()` actually convert (truncate / widen); `min()` / `max()` reduce correctly; `os.path` submodule attributes (e.g. `os.path.sep`) resolve
- Expression statements no longer emit a stray `drop` after `print()`
- Descending `range()` loops iterate: the for-range break test is now step-sign-aware (stop once `current >= stop` ascending, or `current <= stop` descending), and integer unary negation is corrected — `-x` previously evaluated to `x` ([#84](https://github.com/anistark/waspy/pull/84))
- String/bytes locals round-trip: each carries a companion length local so the `(offset, length)` pair survives assignment, `len()` keeps the length, bytes constants are written to the data section, and `x[a:b]` slicing lowers correctly via a branchless clamp ([#85](https://github.com/anistark/waspy/pull/85))
- Float-aware classes: instance fields are typed (f64 for float fields), `self` resolves as its class, constructor/method arguments coerce to declared parameter types (int literals widen to f64), and augmented field assignment (`self.x *= f`) and class-variable reads (`ClassName.var`) work ([#86](https://github.com/anistark/waspy/pull/86))
- Float return types are resolved up front — inferred from `return` statements when unannotated — and float-valued stdlib-constant locals (e.g. `pi = math.pi`) are typed `f64`, fixing an f64-into-i32 mismatch that aborted the optimizer ([#87](https://github.com/anistark/waspy/pull/87))

## [0.9.0](https://github.com/anistark/waspy/releases/tag/v0.9.0) - 2025-12-14

### Added
- **Regular Expression (re) Module Runtime Implementation** (Issue #30)
  - Full runtime support using the `regex` crate
  - Core functions: `re.compile()`, `re.search()`, `re.match()`, `re.fullmatch()`
  - Multi-match functions: `re.findall()`, `re.finditer()`
  - String manipulation: `re.split()`, `re.sub()`, `re.subn()`
  - Utility functions: `re.escape()`, `re.purge()`
  - **Match object support** with methods:
    - `group()` - returns matched string
    - `groups()` - returns tuple of captured groups
    - `start()`, `end()` - match position indices
    - `span()` - returns (start, end) tuple
  - **Regex flags support**:
    - `re.IGNORECASE` / `re.I` - case-insensitive matching
    - `re.MULTILINE` / `re.M` - multi-line mode (^ and $ match line boundaries)
    - `re.DOTALL` / `re.S` - dot matches newlines
    - `re.VERBOSE` / `re.X` - verbose patterns with comments
    - `re.ASCII` / `re.A` - ASCII-only matching
    - `re.UNICODE` / `re.U` - Unicode matching (default)
  - Compile-time pattern evaluation for constant string patterns

- **Datetime Module Runtime Implementation** (Issue #32)
  - Full runtime support for `datetime` module with chrono backend
  - Constructors: `datetime.datetime()`, `datetime.date()`, `datetime.time()`, `datetime.timedelta()`
  - Class methods: `datetime.datetime.now()`, `datetime.datetime.today()`, `datetime.date.today()`
  - Factory methods: `fromtimestamp()`, `fromisoformat()`, `strptime()`
  - Instance methods: `strftime()`, `isoformat()`, `replace()`, `timestamp()`, `weekday()`, `isoweekday()`
  - **Date arithmetic operations**:
    - `datetime + timedelta` → datetime
    - `datetime - timedelta` → datetime
    - `datetime - datetime` → timedelta
    - `date + timedelta` → date
    - `date - timedelta` → date
    - `date - date` → timedelta
    - `timedelta + timedelta` → timedelta
    - `timedelta - timedelta` → timedelta
  - New dedicated IRType variants: `Datetime`, `Date`, `Time`, `Timedelta`
  - Datetime represented as 7 i32s: (year, month, day, hour, minute, second, microsecond)
  - Date represented as 3 i32s: (year, month, day)
  - Time represented as 4 i32s: (hour, minute, second, microsecond)
  - Timedelta represented as 3 i32s: (days, seconds, microseconds)
  - Test suite in `examples/test_datetime.py`

- **Logging Module** (Issue #34)
  - Complete implementation of Python's `logging` standard library module
  - Log level constants: `DEBUG`, `INFO`, `WARNING`, `ERROR`, `CRITICAL`, `NOTSET`
  - Aliases: `WARN` (for `WARNING`), `FATAL` (for `CRITICAL`)
  - Logging functions: `debug()`, `info()`, `warning()`, `error()`, `critical()`, `exception()`, `log()`
  - Configuration: `basicConfig()`, `setLevel()`, `disable()`
  - Logger management: `getLogger()`
  - Handler support: `addHandler()`, `removeHandler()`
  - Classes: `Logger`, `Handler`, `StreamHandler`, `FileHandler`, `Formatter`, `Filter`, `LogRecord`
  - Test suite in `examples/test_logging.py`

- **JSON Module Runtime Implementation** (Issue #31)
  - Implemented runtime support for `json.dumps()` - serialize Python objects to JSON strings
  - Implemented runtime support for `json.loads()` - parse JSON strings to Python objects
  - Added runtime support for `json.load()` and `json.dump()` for file-based operations
  - Added support for `JSONEncoder` and `JSONDecoder` classes
  - Comprehensive test suite in `examples/test_json.py` covering:
    - Basic type serialization (dict, list, string, int, bool, None)
    - Deserialization of JSON strings
    - Nested data structures
    - All major json module functions
- **Complete System Calls Implementation** (Issue #27)
  - Functional `os` module method calls (`os.getcwd()`, `os.getenv()`, `os.getpid()`, `os.urandom()`)
  - Full `os.path` module support with working method calls
    - Path manipulation: `os.path.join()`, `os.path.basename()`, `os.path.dirname()`, `os.path.abspath()`
    - Path inspection: `os.path.exists()`, `os.path.isfile()`, `os.path.isdir()`
    - Path operations: `os.path.split()`, `os.path.splitext()`
  - Stdlib module method call handling in compiler for `sys`, `os`, and `os.path` modules
  - WASM-appropriate implementations with platform limitations for web environments
  - Comprehensive test suite in `examples/test_system_calls.py`

### Fixed
- Fixed json module functions that were previously only stubs without runtime implementation
- JSON module now properly compiles and executes in WASM environment
- Fixed `os` module functions that were previously only stubs
- Fixed `os.path` sub-module access that was returning only attributes, not callable functions
- Fixed stdlib method call compilation to properly handle module and sub-module method invocations

## [0.8.0](https://github.com/anistark/waspy/releases/tag/v0.8.0) - 2025-12-07

### Added
- **Standard Library Modules (Complete)**
  - **sys module**: System parameters and functions
    - Attributes: `argv`, `platform`, `version`, `maxsize`, `stdin`, `stdout`, `stderr`, `path`
  - **os module**: Operating system interface
    - Attributes: `name`, `sep`, `pathsep`, `linesep`, `devnull`, `curdir`, `pardir`, `extsep`, `environ`
    - Functions: `getcwd`, `getenv`, `getpid`, `urandom`
    - **os.path submodule**: Path manipulation functions
      - Functions: `join`, `exists`, `isfile`, `isdir`, `basename`, `dirname`, `abspath`, `split`, `splitext`
      - Attributes: `sep`, `pathsep`, `curdir`, `pardir`
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
    - Implementation: Uses `serde_json` for compile-time JSON operations
    - Runtime JSON parsing infrastructure in place
  - **re module**: Regular expression operations
    - Functions: `compile`, `search`, `match`, `fullmatch`, `findall`, `finditer`, `split`, `sub`, `subn`, `escape`, `purge`
    - Flags: `IGNORECASE`, `MULTILINE`, `DOTALL`, `VERBOSE`, `ASCII` (and short forms: `I`, `M`, `S`, `X`, `A`)
  - **datetime module**: Date and time manipulation using chrono crate
    - Types: `datetime`, `date`, `time`, `timedelta`, `timezone`, `tzinfo`
    - Methods: `now`, `today`, `fromtimestamp`, `fromisoformat`, `strftime`, `strptime`, `replace`, `timestamp`, `isoformat`, `weekday`, `isoweekday`
    - Constants: `MINYEAR`, `MAXYEAR`
    - Implementation: Uses `chrono` crate for compile-time datetime operations
    - Compile-time helpers: `datetime_now_utc()`, `datetime_now_local()`, `date_today()`, `datetime_from_timestamp()`, `datetime_from_iso()`, `datetime_to_iso()`, `datetime_strftime()`
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
