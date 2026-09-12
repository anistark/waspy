# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [0.16.0](https://github.com/anistark/waspy/releases/tag/v0.16.0) - 2026-09-12

### Added
- String methods work on a string built at runtime, not only on a literal. `upper`, `lower`, `strip`, `lstrip`, `rstrip`, `capitalize`, `title`, `find`, `index`, `count`, `startswith`, `endswith`, `replace`, `split`, `join`, `ljust`, `rjust`, `center`, and the `isdigit`/`isalpha`/`isalnum`/`isspace`/`isupper`/`islower` predicates are all emitted as real byte loops now. Every one of them was a stub that returned the receiver unchanged, or 0, or `false`, while reporting a successful compilation: `"ALPHA".lower()` on a variable answered `"ALPHA"`, `",".join(parts)` answered `","`, `text.find(sub)` always answered -1, and `text.split(sep)` returned a region whose length word was garbage. A *constant* receiver was folded correctly during lowering, which is exactly why no feature test caught any of it. A byte at or above 0x80 traps in the case transforms rather than passing through unchanged, since CPython case-folds the whole of Unicode and answering `"CAFÉ".lower() == "cafÉ"` would be the same class of silent wrong answer
- `str.format()` interpolates runtime values. A literal template now lowers to the same `+` chain an f-string does, reusing that path, so `"{} items".format(n)` renders `n` instead of answering the empty string. Automatic (`{}`) and positional (`{0}`) fields are supported; a named field, a format specifier, and a conversion are rejected with a hint rather than ignored. Constant arguments fold through the shared renderer, so `"{}".format(True)` is `True` and not Rust's `true`
- Dicts have methods: `get(key[, default])`, `keys()`, `values()`, and `items()`. There was no dict arm in method dispatch at all, so every one of them was reported as unsupported. `counts[w] = counts.get(w, 0) + 1`, the idiomatic accumulator, works, and `.items()` is a value rather than only something a `for` loop can desugar
- `in` searches the containers it could not before. `key in some_dict`, `x in some_tuple`, and `sub in text` each answered a constant `False` for anything the container actually held, because only lists and sets were considered searchable. Dict keys are scanned at their own two-slot stride, tuples share the list scan, and a substring test runs the same search `find` does. A container that still cannot be searched is a compile error instead of a constant
- `list.sort()` and `list.reverse()` reorder the list. Neither name appeared anywhere in the compiler, so both fell through to a default arm that dropped the receiver and pushed 0, which meant `[3, 1, 2].sort()` left the list untouched and reported success. `sort` is an in-place insertion sort over int, float, and bool elements and takes `reverse=True`; `reverse` swaps whole slots, so it is correct for any element width
- List slicing returns a real list. `xs[:n]` computed a length, dropped it, and pushed a null pointer, and its clamp pushed a value from both arms of an `if` typed as returning nothing, so the module did not validate. Indices follow Python: a negative one counts from the end, both ends clamp into range, and the result is a fresh region, so growing it never touches the list it came from. A step other than 1 is rejected rather than silently ignored
- `examples/text_report.py` and `examples/library_project/`, the second and third of the 0.16.0 end-to-end programs, joining the shopping cart. The text report is a word-frequency tool: it tokenizes a block of prose by stripping punctuation and case-folding each word, counts them into a dict keyed by those runtime strings, ranks by count with alphabetical tie-breaking, and formats a report with a fixed-point mean. It exercises string methods, dict accumulation, sorting, comprehensions, and f-strings all at once, which is why it found a dozen defects nothing else had. The library project is a book catalogue split across four modules with the domain class shared between them, compiled from its entry file with imports resolved from disk. Both are asserted against what CPython prints for the same source, and both were checked under `wasmi`, Node, and `wasmtime`
- `examples/shopping_cart.py`, the first of the 0.16.0 end-to-end programs. It is a whole program rather than a feature demo: a `Cart` holding a list of `Item` instances, an `add` method that constructs an item and appends it to a field, a `total` method that calls each element's own method, float money arithmetic, and a plain rules function over the result. Nothing in it was chosen to show off a compiler feature, which is the point: the first time this program was compiled it hit three defects in a row (a Binaryen process abort, a silently wrong `0.0`, and a trap) that no single-feature test had caught, because they lived in the composition of features that each worked alone. Four asserted tests in `tests/integration/coverage.rs` check its results against what CPython answers for the same source, including both discount-tier boundaries and the empty-cart case, and the compiled module was checked to give identical answers under `wasmi`, Node, and `wasmtime`
- `just verify-runtime`, which runs the end-to-end programs under Node and `wasmtime` rather than only under the test harness, and `just verify-examples` now depends on it. The integration suite asserts these programs' results with `wasmi` inside the test process, which leaves the engines a user actually ships against unchecked. Each program is now compiled twice, unoptimized and Binaryen-optimized, with a checker suffix from `tests/fixtures/runtime/` appended: the suffix adds `runtime_check_*` functions that compare the program's own results against what CPython answers and return 1 or 0, so a runner only has to call every such export. Comparing inside the module is what lets the `wasmtime` CLI, which can invoke an export but cannot read the module's memory, check the same returned strings Node does. A `runtime_check_negative_*` function must answer 0, so a comparison that regressed to a constant `True` is caught rather than papered over. Both runners are a required gate in CI, and `verify-examples` also compiles `examples/library_project/`, which it had been skipping
- A "what a program can look like" page in the docs, at `docs/programs/`. One complete working program, taken unedited from `examples/`, and a plain list of the shapes that do not compile yet, each tagged with what happens when you try: refused at compile time, answers differently from CPython, or has a supported way to write the same thing. The README's limitations stay the exhaustive statement; this is the page to read in a minute to tell whether your code is in scope
- `tests/unit/miscompiles.rs`, 36 regression tests for the silent miscompiles the whole-program work turned up, over method dispatch, constant folding, and error surfacing. Every expected value is what CPython answers for the same source, so a failure is a divergence from the reference implementation rather than a change in what the compiler happens to do. `tests/unit/memory_safety.rs` holds the earlier round of the same exercise
- `tests/integration/wasmrun_plugin.rs`, which takes the end-to-end programs through the wasmrun plugin's own `WasmBuilder` rather than the library API, asserts the module lands where `BuildResult` says it does, and then runs it and checks the same answers the library path is checked against. `examples/plugin_test.rs` covered the plugin before this, but it printed its findings: a failed build printed a cross and the process still exited 0, and the module was never instantiated. It fails properly now, and both plugin optimization levels are checked
- `sorted(iterable[, key=...][, reverse=...])` sorts. It fell through to the generic builtin path and returned a value with nothing to do with sorting, while reporting success: `sorted([3, 1, 2])[0]` answered 2. It is a stable insertion sort over a copy now (the original is untouched, as in Python), ordering ints, floats, strings, and tuples, the last compared lexicographically member by member. `key=` is applied once per element rather than on every comparison. A key whose result type cannot be determined, or a sequence whose element type is unknown, is a compile error rather than a comparison of raw slot words, which for a list of tuples would compare pointers and leave the list in allocation order
- Fixed-point formatting in f-strings: `f"{value:.2f}"`. The specifier used to be rejected outright, so a program could not print money. The integer and fractional parts are handled separately (no 64-bit arithmetic needed), the fraction is rounded to nearest with ties to even the way IEEE-754 and CPython both do, and a fraction that rounds up carries into the integer part. Every other specifier (widths, alignment, separators) is still refused rather than silently ignored

### Changed
- **`list.index(value)` traps when the value is absent, instead of answering -1.** CPython raises `ValueError`; nothing in a compiled module can carry one, so it traps, the way `set.remove` of a missing member already does. Answering -1 is `str.find`'s contract, not `list.index`'s, and a caller who then indexes with it reads the wrong element. Nothing can be relying on the old behaviour, because `index` never once returned a correct position (see `### Fixed`)
- **Indexing a lambda's parameter, measuring it, or calling a method on it is a compile error** ([#115](https://github.com/anistark/waspy/issues/115)). A lambda's parameters carry no annotation, so a lifted lambda's parameter is untyped and code generation reads it as a bare word: `(lambda kv: kv[1])((1, 5))` answered 0 where CPython answers 5, and `(lambda w: len(w))("abcd")` answered 1684234849, which is the four bytes read as an integer. Both compiled and reported success, which the roadmap had recorded as a loud refusal it turned out not to be. Typing a lambda's parameters from its call sites is a typing pass of its own, so until then this is refused rather than miscompiled. Arithmetic and comparison on a parameter are untouched, so `sorted(xs, key=lambda v: 0 - v)` still works; anything more wants a named `def`, whose parameters can be annotated
- **A list method called with the wrong number of arguments is a compile error.** Every arm read the arguments it needed and ignored the rest, so `xs.append(3, 4)` compiled and appended only the 3, and `xs.clear(9)` dropped its argument on the floor. CPython raises `TypeError` for both. The optional forms Python accepts still compile: `pop()`, `pop(i)`, `sort()`, and `sort(reverse=True)`

### Fixed
- **A parse or lowering failure no longer disappears into a warning.** Both the file and the multi-file entry points logged the failure and skipped the file, which is how every user compiles. On a single file the located message with its hint was replaced by "No valid functions found in any of the provided files"; in a project the build *succeeded* with that module missing, so one unsupported construct anywhere in an imported module took every other function in it down and a call across the import answered 0 with nothing said. A typo in any imported module reached this. Both are errors now, naming the file and carrying the real cause
- **`list.pop(i)` removes the element at `i`.** The index was evaluated, stored, and used only for the load: the length was decremented and the tail left where it was, so `[9, 3, 2].pop(0)` answered 9 (right) and left `[9, 3]` behind where CPython leaves `[3, 2]`. Every element above the popped one moves down a slot now, whole slots at a time so a float or a string relocates intact, and a position the list does not have traps instead of reading a neighbouring slot. This is the mirror of the `insert` defect below: the argument was read and discarded
- **`list.index(value)` answers the position it found.** The search loop pushed the matching position and then branched out of the enclosing block, and a `br` to a label whose result type is empty discards whatever sits above it, so the position was thrown away and execution fell through to the `-1` after the loop. `[2, 3].index(2)` answered -1 for a value the list held, and reported success. `count` shares the same scan and was always right, which is why nothing caught it
- **`"a b a".split(" ")` returns a list.** A constant separator on a *literal* receiver folded into a string spelling the result the way Python's `repr` does, so the answer was the 15-byte `['a', 'b', 'a']`: `len()` answered 15 instead of 3, iterating it walked characters, and using one as a dict key trapped. There is no list constant to fold into, so the fold is gone and the runtime implementation is the only path, which is the one `s.split(sep)` on a variable always took and always got right
- **f-string `.0f` rounds ties to even.** Ties-to-even needs the parity of the digit being rounded into, and at precision zero that digit is the integer part's last one. Splitting the value and rounding the fraction alone lost it, and rounding 0.5 to nearest-even is 0 whatever sits to its left, so `f"{3.5:.0f}"` answered `3` where CPython answers `4`. `2.5` and `4.5` were right only because their integer part was already even. Every other precision was, and stays, correct
- **An unknown string method names the method instead of failing validation.** It dropped the receiver's `(offset, length)` and pushed nothing, so the module failed WebAssembly validation and the error blamed code generation rather than the Python source. The list and dict receivers already reported theirs
- **`list.insert(i, value)` honours its position.** The index was evaluated and discarded, so every insertion appended the value. It is now normalized and clamped like Python's, and the existing tail shifts right before the new value is stored.
- **`/` on two integers is true division.** Python 3's `/` yields a float, so `7 / 2` is 3.5; this emitted an integer divide and answered 3, silently truncating wherever the result was used and failing WebAssembly validation wherever a float was expected. `//` is unchanged and remains the floor division that keeps an integer. A `return` now also converts its value to the function's declared return type, which is what lets `-> int` accept a true-division result
- **Iterating a dict read whatever the stride landed on.** `for k in d` was only implemented for lists and strings, so a dict fell through to a branch that walked it at the wrong width: a three-key dict counted 131072 iterations, and looking a key up inside the loop trapped. Keys are walked at the entry stride now, and the loop variable takes the dict's key type
- **Indexing a tuple always reported the first member's type.** `pairs[0][1]` on a `(int, str)` came back typed `int`, so concatenating two of them compiled as integer addition and the sum was then read back as a string offset. The index decides the type now; a computed index into a tuple whose members differ is a compile error rather than a guess
- **A comprehension over a call bound untyped targets.** `[f"{w}" for c, w in top_words(...)]` did not consult the callee's declared return type, so a string member rendered as its pointer
- Dict keys compare by content, not by where they happen to live. A string slot holds only the blob's offset, so comparing slots as words compared *identity*: a key built at runtime never matched an equal key already in the dict, and `counts[word]` with a word from `split()` could not find its own entry. Strings now compare byte for byte wherever a slot is matched, which fixes `in` over lists and sets at the same time
- A string value is an `(offset, length)` pair everywhere. A string parameter arrived as the offset alone, and a `for` target or comprehension target binding a string element had no companion length local at all, so reading one pushed a single word where the rest of code generation expects two. `len(w)` on a loop variable answered whatever the companion happened to hold, a placeholder rendered the pointer as a number, `sum(len(w) for w in words)` answered garbage, and using such a value as a dict key dropped the offset and kept whatever was underneath it. Loop and comprehension targets also take the element's type now, so a string method can be called on one
- Truthiness follows Python. A `str` is an `(offset, length)` pair, so testing one directly left the offset on the stack and the module failed to validate inside a loop; a collection is a region pointer, which is never null, so `if xs:` on an empty list, dict, set, or tuple answered `True`. One helper now reduces a value to Python's truth value for `if`, `while`, and `not`
- Keyword arguments are no longer dropped on the floor. Only a call's positional arguments were read, so `xs.sort(reverse=True)` reached code generation as a bare `sort()` and would have sorted the wrong way round without saying so. `sort`'s `reverse` is lowered to a positional argument and every other keyword is rejected with a hint
- A method the compiler does not implement is a compile error rather than a no-op. An unknown *list* method was silently discarded, which is what let `sort` and `reverse` do nothing; an unknown *string* method emitted a module that failed to validate. Both now report the method, the receiver, and the enclosing function. The list hint names `reverse` and `sort` now that both are implemented.
- Two modules defining the same function name is an error. Merged modules share one flat namespace, so the second definition was dropped with a warning while compilation reported success, and every call to either one reached the first: `alpha.rate()` and `beta.rate()` both answered alpha's value. Resolving this properly means qualifying names by module and rewriting call sites per file's imports, so for now it is refused instead of miscompiled


### Performance baseline

First recorded baseline, so later releases have something to compare against.
Median of nine release-build compilations each on an Apple M1 (Darwin arm64),
via `just benchmark`. These are wall-clock numbers from one machine: the point
is to catch an order-of-magnitude regression, not to defend a percentage.

| Program | Source | Compile | Module | Compile (opt) | Module (opt) |
| --- | --- | --- | --- | --- | --- |
| `shopping_cart` | 2.0 KiB | 0.2 ms | 1.4 KiB | 1.2 ms | 1014 B |
| `text_report` | 3.4 KiB | 0.4 ms | 6.5 KiB | 5.8 ms | 5.7 KiB |
| `library_project` | 3.3 KiB | 0.5 ms | 3.6 KiB | 2.7 ms | 3.1 KiB |
| `nested_collections` | 4.7 KiB | 0.5 ms | 13.6 KiB | 4.1 ms | 12.7 KiB |

The first three are the end-to-end programs; `nested_collections` is the
feature example that produces the largest module, as a second data point.
Binaryen dominates the optimized column, costing 5 to 15 times the rest of the
pipeline while taking 7 to 27% off the module, so the unoptimized compile is
the number to watch for a codegen regression.

## [0.15.0](https://github.com/anistark/waspy/releases/tag/v0.15.0) - 2026-09-07

### Added
- Generated WebAssembly is validated before anything else touches it. Code generation could emit a module that does not validate (an unbalanced stack, a local index past the function's local vector, a type mismatch); handing one to Binaryen aborted the whole process with `UNREACHABLE executed at bits.h:436` and no file, line, or function to go on, and skipping optimization just handed the caller a binary every runtime rejects. Every compilation path now runs the module through a validator first (`waspy::compiler::validate_wasm`) and reports a normal `ChakraError` carrying the validator's message, the byte offset, and the name of the exported function whose body contains that offset, stating plainly that it is a compiler bug rather than a problem with the Python source. Validation runs whether or not optimization is enabled, so a successful compilation always means a module a runtime will accept
- Module-level statements whose effects the compiler silently dropped are now rejected up front ([#3](https://github.com/anistark/waspy/issues/3)). A WebAssembly module has no top-level run step, and Waspy compiles module-level variables by inlining their initializers into the functions that read them, so only definitions can run: `X = 1`, `X: int = 1`, `X = helper()`, `X = ClassName()`, `def`, `class`, and imports. Everything else written at module level compiled "successfully" and then did nothing at all, so the program read the value from before the statement and returned a silently wrong answer: a module-level `for` loop accumulating into a global returned the initial value, a `while` loop and a non-guard `if` the same, `ITEMS.append(3)` left the list at its literal length, `TOTAL += 2` left `TOTAL` unchanged, `D[1] = 9` and `O.n = 9` were dropped, and `A, B = 1, 2` read back garbage. Each of those shapes now fails at the front door with the statement named, its line and column, and a hint pointing at the two things that do work (move it into a function, or drive it from `if __name__ == "__main__":`, which is recognized as the entry point). The entry-point guard and the `try`/`except ImportError` guarded-import idiom stay allowed, since neither carries a runtime effect to drop
- An error channel in code generation. `compile_function` and `compile_body` returned `()` and expression codegen runs behind a shared `&CompilationContext`, so a construct codegen could not generate code for had no way to say so and could only emit a runtime trap. The context now carries an error sink (`CompilationContext::report`), the whole module is still walked so one compile run surfaces every such call, and `compile_ir_module` then fails with the collected errors instead of returning a module that dies at runtime. The first construct moved across is a method call on a receiver kind with no method support (set mutation, `s.add(...)` above all): it is now a compile error naming the method, the receiver's type, and the function it appears in, where before it compiled "successfully" and trapped. `compiler::compile_ir_module` returns `Result<Vec<u8>, ChakraError>` accordingly (an implementation-detail module; the crate-root API is unchanged)
- Set mutation: `s.add(v)`, `s.remove(v)`, and `s.discard(v)` work at runtime, where before they compiled to a trap. The set's open-addressing table gained a `used` counter (occupied buckets plus tombstones) and an 8-byte-aligned 16-byte header; `add` rehashes into a fresh, larger `__alloc` block when the load factor would pass 1/2, re-inserting every live bucket (which also compacts the tombstones away) and writing the new pointer back through the variable or instance field the set was reached through, the same reserve-and-rebind shape lists and dicts use. `remove`/`discard` leave a tombstone rather than an empty bucket, so members whose probe ran past the removed one stay reachable, and membership now checks a bucket's state before comparing its value, so a removed member does not come back to life through the value left behind. Float members hash and compare at f64 width throughout. `remove` of a value the set does not hold traps (Python raises `KeyError`, and a compiled module has no exception value to raise); `discard` ignores the miss, as Python's does
- `finally` and `__exit__` run on the non-local exit paths. A `return`, `break`, or `continue` leaving a `with` block or a `try`/`finally` jumped straight past the cleanup, which code generation only emits after the block, so the cleanup was silently skipped: a `with` left by `break` never called `__exit__`, and a `try`/`finally` left by `return` never ran its `finally`. Both constructs are now lowered by the one bottom-up walk in `ir::context_managers`, which inserts a copy of the cleanup ahead of every exit that leaves the block (a `break` or `continue` belonging to a loop written *inside* the block is left alone, since it does not leave). Because both use the same walk, cleanups nest in Python's order in either direction: a `try`/`finally` inside a `with` runs its `finally` before `__exit__`, and a `with` inside a `try`/`finally` runs `__exit__` first. The exception path is covered too, by the exception work below: `with` lowers onto `try`/`finally`, whose propagation path runs the cleanup
- Collection literals that mix float elements with int or bool ones (`[1, 2.5]`, `(1, 2.5)`, `{1: 1.5, 2: 2}`, `{1, 2.5}`) are rejected at compile time. Every element occupies one 8-byte slot and the collection reads them all at a single width, so one of the two element types came back as garbage; since the module-validation pass above, they failed WASM validation with a message blaming code generation instead. They are now a proper compile error through the codegen error channel, naming the literal kind and the function. Bools next to ints are unaffected (both are word-width, and Python calls bool an int), and `examples/tuple_example.py`, which carried a mixed `(42, "hello", 3.14)` tuple, demonstrates a float tuple and the restriction instead
- Indexing is normalized and bounds-checked. `xs[-1]` is Python for "the last element", but with the count word at offset 0 it computed an address *before* the region and read that count back as though it were an element; an index past the end read, or wrote, whatever followed the region; a dict read for a key the dict did not hold answered 0, which is a real value and indistinguishable from a stored one; assigning into a tuple left the stack unbalanced (so the module failed validation while the compiler reported success); and assigning into a string was dropped on the floor. Every index is now folded against the length and checked: a negative one counts from the end, out of range raises `IndexError`, a missing key raises `KeyError`, and item assignment into a tuple or a string is a compile error naming the type, the way Python raises `TypeError`. Both exceptions are catchable and both travel out of calls, since a function that indexes counts as one that can raise. Closure environments share the collection slot layout but hold a dispatch table slot where a collection holds its length, so reading a capture became its own IR node (`IRExpr::EnvRead`) instead of an index that would be checked against that word
- Division by zero raises `ZeroDivisionError`. Integer division and modulo trapped, which is loud but uncatchable and not what Python does, and float division answered `inf` silently. The divisor is checked first now (a nonzero literal divisor still compiles to a bare divide, so `n // 2` is unchanged), and the raise is catchable and travels out of calls like any other
- Growing a collection no longer moves it. Elements now live in a block the region's header points at, so `append`, `extend`, `insert`, `dict[key] = value` for a new key, and `set.add` reallocate that block and leave the collection itself where it is. Growth used to reallocate the whole region and rebind the one variable or field the collection was reached through, which left every other name for it holding a stale pointer: a list grown inside a function it was *passed* to silently lost everything added after it outgrew its original block (elements that fitted were visible, so it looked intermittent), and one reached by indexing another collection had nowhere to rebind and trapped instead. Both work now, as do two locals naming the same list, a list inside a dict, and a set or dict grown through a parameter. The write-back machinery is gone with it
- Closures capture the variable, not its value. A lambda copied its captured variables into the environment when it was created, so a captured variable reassigned afterwards kept its old value inside the closure, and closures made in a loop each froze a different iteration's value; Python's see the variable itself. A captured variable now lives in a one-slot heap cell that the enclosing function and every closure over it share: the enclosing function reads and writes through the cell, the environment carries the cell's *pointer*, and a lifted lambda binds that pointer from its environment and reads through it too, which is what makes nested closures share a cell all the way down. A loop variable is mirrored into its cell on each pass, so closures made in a loop all see the final value, as Python's do
- `str()` of an integer renders its digits. The IR converter erased the call and left the argument in its place, so `str(123)` *was* the integer 123: `len(str(n))` answered 0, comparing the result against a string literal never matched, and concatenating it built the wrong string, all silently. The call survives to code generation now and goes through the `__i32_to_str` renderer, including for a value whose type codegen cannot resolve (a single i32 word like any other). What it cannot render is a compile error rather than an empty string: a bool would come out as `1` rather than Python's `True`, a float needs a formatter this runtime does not carry, and a collection needs its elements rendered too
- Three arithmetic helpers wrote to WASM locals 0, 1, and 2 outright, which are a function's first parameters. `a ** b` clobbered `a`, so reading it afterwards gave the wrong value, and in a function whose first locals were not the width the helper assumed the module failed to validate. They use dedicated scratch locals now, and the bugs they were hiding are fixed with them: float modulo subtracted the wrong way round (`3.5 % 2.0` answered -1.5), and float power used `return` for its special cases, which returned from the *enclosing function* rather than the expression, and answered the base itself for a fractional exponent. Float power now computes an integral exponent by repeated multiplication (a negative one through the reciprocal, with `0.0 ** -n` raising `ZeroDivisionError`) and traps on a fractional one, which needs exp/log this runtime does not have; integer power traps on a negative exponent, whose Python answer is a float an i32 result cannot hold
- **F-strings interpolate every placeholder.** An f-string holding a value kept only its *first* piece: `f"{n} items"` compiled to the bare integer `n` typed as a `str`, so `len()` of it answered 0 and comparing it to a literal never matched, and `f"Total: {n}"` compiled to the label `"Total: "` with the value missing. Both reported a successful compilation. Only an f-string with no placeholders at all, or one whose placeholders were all constants, came out right. An f-string now lowers the way Python defines it: each placeholder renders its value with `str()`, the pieces are concatenated left to right, and adjacent literal text is merged so each interned label stays a single blob. Constant placeholders still fold into the literal at compile time, and they fold the way Python spells them, so `f"{True}"` is `True` rather than Rust's `true` and `f"{3.0}"` keeps its decimal point. A float constant folds only over the range where Rust and Python write the same digits (positionally, no exponent); past it Python switches to `1e+16` where Rust spells the number out, so the placeholder is reported rather than folded to something else. What a placeholder cannot render is a compile error rather than a wrong string: a format specifier (`f"{x:.2f}"`) and the `!r`/`!a` conversions change what is printed and have no implementation here, so both are rejected (`!s` is accepted, being what a bare placeholder already does), and a value `str()` cannot render, a float, a bool, or a collection, is reported by name. `examples/fstrings.py` demonstrates the working shapes, with asserted results in the coverage suite and the truncation cases pinned in `tests/unit/memory_safety.rs`

### Changed
- Documented that `int` is a 32-bit two's-complement integer, in a new "Numbers" section in `README.md`, next to the type list, and on `IRType::Int` itself. CPython's `int` is arbitrary precision, so arithmetic that leaves the range -2147483648..2147483647 wraps here instead of growing: `1000000 * 1000000` answers -727379968 and `2 ** 40` answers 0. This is the one place the compiler answers differently without saying so, and it is now part of the definition of the subset rather than an unstated property of the backend. Checking every add, subtract, multiply, and power for overflow and trapping was the alternative; it costs instructions on the hottest path in any program and rejects code that relies on wrapping, and a 64-bit integer would only move the boundary. `tests/unit/basics.rs` pins the wrapping behaviour so it stays deliberate
- **Exceptions transfer control.** `raise` used to set a per-function flag that the *end* of the enclosing `try` body read to pick a handler, so the statements after a `raise` still ran, a `raise` inside a loop kept looping, a handler for another type swallowed the exception, an uncaught `raise` was ignored and the function returned normally, and nothing crossed a call boundary. Only one shape behaved like Python, by accident. Now the pending exception's type lives in a module global, a `raise` branches out of the block it is in (to the enclosing `try`'s handler dispatch, or out of the function), and any call to a function that can raise is followed by a check that keeps unwinding when one is pending. A handler whose type matches clears the exception and runs; one that does not match passes it on, so it keeps travelling outward through as many frames as it takes. `finally` runs on the way past, including while an exception is propagating, and `with` now lowers onto `try`/`finally`, so `__exit__` runs when an exception leaves the block too (including one raised inside a call the body made). An exception nothing catches unwinds out of the program and traps, instead of handing the host a default value: a call-depth global tells a propagating frame whether anything below it will check
- Exception handlers match more like Python's. `except (A, B):` was read as a bare `except:` (the IR held one optional type name, and a tuple is not a name), so it caught exceptions it should have passed on; handlers now carry every name they list and match any of them. `except Exception:` and `except BaseException:` catch anything, since in Python everything derives from them. Exception types outside the built-in table, a user-defined exception class above all, get a stable code derived from the name instead of sharing one generic code, so two different user exceptions no longer catch each other. Known limits, documented in `README.md`: exceptions carry a type and not an object (the message in `raise ValueError("...")` is dropped, and `except ... as e` binds the type code), matching does not consult a user exception's base classes, and runtime faults such as an out-of-range index or `1 // 0` trap rather than raising, so they cannot be caught

## [0.14.0](https://github.com/anistark/waspy/releases/tag/v0.14.0) - 2026-08-13

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
- Python comments are preserved in the generated WebAssembly. The parser discards them, so a scanner recovers them from the source (skipping `#` inside strings and docstrings, single- and triple-quoted), the IR module carries them, and the backend emits them as a `python.comments` custom section: UTF-8 text, one `file:line:text` entry per line. Custom sections carry no code, so nothing about how the module runs changes, and the section survives the Binaryen optimization pass. `waspy::core::comments::comments_from_wasm` reads them back out of a compiled binary
- Runnable rustdoc examples on every crate-root entry point: single-source, multi-file, entry-file, project-directory, metadata, and `type_to_string`, including the option-carrying variants and the on-disk paths (import resolution, `pyproject.toml` metadata). Each is a doctest, so the documented usage is checked by the test suite
- `examples/context_managers.py`, asserted end to end by the integration suite: entering and exiting, the `as` binding, an early `return` from the body, nested managers, re-entry in a loop, and a subclass inheriting the protocol
- `tests/unit/memory_safety.rs`: regression coverage for the constructs that used to compile "successfully" and then misbehave at runtime (context managers, float-list iteration, list and dict growth past a literal's size, the unbalanced-drop set method call, and element types for collection fields holding instances), wired into the named CI gate alongside the error-message suite

### Changed
- **Breaking:** `CompilerOptions` now carries exactly the options the pipeline honors — `optimize` and `verbosity`. The five removed fields (`debug_info`, `max_memory`, `entry_point`, `generate_html`, `include_metadata`) were never read by any compilation stage: linear memory is sized automatically from the module's data and grows on demand, entry points are auto-detected, and metadata printing/HTML harness generation are driver concerns (the example drivers and the wasmrun plugin keep their behavior through their own flags)
- **Breaking:** the `waspy::parser` crate-root re-export is gone; use `waspy::core::parser` (the root surface no longer leaks rustpython AST types). Crate-level rustdoc now states the stable public API — the crate-root exports — with a compilable quick-start example, and marks the pipeline modules as implementation detail
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

### Fixed
- `with` over a user-written context manager ([#5](https://github.com/anistark/waspy/issues/5)) compiles and runs. It previously reported a successful compilation and emitted a module that failed WASM validation (`invalid local index`), because the code generator added locals after the function's local vector was fixed and never called either protocol method. A whole-module IR pass (`ir::context_managers`) now rewrites `with expr as name:` into `__enter__`/body/`__exit__` over ordinary assignments, running after the class table is complete so the synthesized locals are typed from the real `__enter__` return type (an f64-returning `__enter__` included). Nested blocks, managers inherited from a base class, re-entering the same manager in a loop, and an inline `with ClassName(...) as x:` all work, and an early `return` from the body runs `__exit__` before leaving (the returned expression is evaluated into a temporary first, so it still sees the state from inside the block). A class missing `__enter__` or `__exit__`, or a context expression whose class cannot be resolved at compile time, is now a located compile error instead of a broken module. `with open(...)` keeps its existing lowering. Known limits: an exception or a `break` leaving the body skips `__exit__`, and `__exit__`'s return value never suppresses an exception
- Iterating a float list held in a variable (`for x in xs`) bound the loop variable as an i32 and produced garbage values; only float *literal* iterables worked. Collection literals now carry their element type through the assignment scan, and an element-less annotation (`xs: list = [1.5, 2.5]`) adopts the element type of the value it is assigned, so the loop variable is typed f64 and the elements read back at full width. A mixed-type literal stays untyped rather than mis-typing part of it
- Growing a list past the size of its literal overwrote whatever collection sat next in linear memory: `a = [1, 2]; b = [100, 200]; a.append(3); a.append(4)` left `b[0]` reading 4. Collection regions now carry a capacity alongside their length, and `append`, `extend`, and `insert` reserve room before writing, reallocating into a fresh `__alloc` block (capacity doubling, with a floor of 4) and copying the live elements when the region is full. The grown pointer is written back through the variable or instance field the list was reached through, so `xs = []` filled in a loop and `self.items.append(v)` both work. Where there is nowhere to write the new pointer back (a list reached by indexing another collection, or a temporary), the grow path traps rather than writing out of bounds
- Adding a key to a dict past its literal's entry count had the same effect on the neighbouring collection; `dict[key] = value` for a new key now reserves an entry through the same path, so `d = {}` filled in a loop works and float values survive the move. Updating a key the dict already holds still writes in place
- A method call on a receiver kind with no method support, `set.add(...)` above all, emitted two `drop` instructions for a one-word receiver, so the module failed WASM validation while the compiler reported success. The drop count now matches what the receiver left on the stack, and the call compiles to a trap: set mutation is still unimplemented, and failing loudly at runtime beats silently doing nothing. Receiver types are only known during code generation, so this cannot yet be a compile-time rejection
- The empty-tuple literal copied only 4 of its header bytes when rebuilt inside a loop
- A collection field started empty (`self.items = []`, the ordinary way to begin one) carried no element type, so iterating it bound an untyped element: `for it in self.items: it.price` read 0 and calling a method on the element trapped. Class building now also collects the element types a class puts into its *own* collection fields through `append`/`insert`, threading a local environment through each method body so the two-step form (`item = Item(...)` then `self.items.append(item)`) resolves as well as the direct `self.items.append(Item(...))`, and fills in an element-less field from that evidence. A field initialized with a literal of instances (`self.ps = [P(1.5), P(2.5)]`) and a field assigned an instance (`self.origin = Point()`) now record the class too. With this, the first realistic whole program compiled against waspy (a shopping-cart domain: classes holding a list of instances, float money arithmetic, methods calling methods) returns the same values CPython does

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
