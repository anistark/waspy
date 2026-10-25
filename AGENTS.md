# AGENTS.md — AI Coding Agent Instructions for Waspy

> Instructions for Claude Code, pi, Cursor, Copilot, and other AI coding agents working on this project.

---

## Project Overview

**Waspy** is a Python-to-WebAssembly compiler written in Rust. It parses Python source, lowers it to an intermediate representation (IR), generates a WebAssembly binary, and optimizes it. It is shipped as a **Rust library** (and as an optional [wasmrun](https://github.com/anistark/wasmrun) compilation plugin), not as a standalone CLI binary.

- **Repository:** https://github.com/anistark/waspy
- **Crate:** https://crates.io/crates/waspy
- **License:** MIT
- **Minimum Rust Version:** 1.70 (declared in `Cargo.toml`)
- **CI / Recommended Rust Version:** 1.88 (what GitHub Actions builds and lints with)

---

## ⚠️ The Compilation Pipeline — Read This First

Waspy is a **single linear compiler pipeline**. Every public entry point in `src/lib.rs` flows through the same stages. Understand this pipeline before touching anything — most changes belong to exactly one stage, and breaking the contract between stages breaks the whole compiler.

```
[Python source code]
        ↓   core/parser.rs        : rustpython-parser → Python AST
[Python AST]
        ↓   ir/converter.rs       : lower_ast_to_ir() → IRModule, then the module
        ↓                            passes, in this order:
        ↓                              ir/logging/            lower_logging
        ↓                              ir/re/                 lower_re
        ↓                              ir/decorate.rs         apply_user_decorators
        ↓                              ir/metaclasses.rs      apply_metaclasses
        ↓                              ir/finalize.rs         lift_nested_functions
        ↓                              ir/partials.rs         lower_partials
        ↓                              ir/datetime/           lower_datetime
        ↓                              ir/finalize.rs         resolve_keyword_arguments
        ↓                              ir/context_managers.rs desugar_with_statements
        ↓                              ir/itertools.rs        lower_itertools
        ↓                              ir/collections/        lower_collections
        ↓                              ir/generators.rs       transform_generators
        ↓                              ir/finalize.rs         finalize_module
[IR Module]
        ↓   ir/decorators.rs      : DecoratorRegistry::apply_decorators()
        ↓   ir/entry_points.rs    : detect_entry_points() / add_entry_point_to_module()
[IR Module + entry points]
        ↓   compiler/module.rs     : compile_ir_module(): appends __module_init for
        ↓                            non-constant module/class definitions, then
        ↓                            wasm-encoder → raw WASM (start section runs the init)
[Raw WASM binary]
        ↓   optimize/wasm.rs       : optimize_wasm() (Binaryen), only if options.optimize
        ↓                            and the default `optimize` feature is on
[Optimized WASM binary]
```

### The Five Stages

| Stage | Module | Responsibility | Key entry point |
|-------|--------|----------------|-----------------|
| **1. Parse** | `src/core/parser.rs` | Python text → AST via `rustpython-parser` | `parse_python()` |
| **2. Lower (IR)** | `src/ir/converter.rs` + the module passes | AST → typed `IRModule` (functions, classes, variables, imports, memory layout), then the passes that desugar what codegen does not see: the standard-library modules written out in IR (`logging`, `re`, `functools.partial`, `datetime`, `itertools`, `collections`), user decorators, metaclasses, nested functions, keyword arguments, `with`, generators | `lower_ast_to_ir()` |
| **3. Transform** | `src/ir/decorators.rs`, `src/ir/entry_points.rs` | Apply decorators; detect/inject `__main__` entry points | `DecoratorRegistry`, `detect_entry_points()` |
| **4. Codegen** | `src/compiler/` | IR → WASM bytecode via `wasm-encoder`, plus the runtime helpers and WASI imports the program uses, validated before it is returned | `compile_ir_module()` |
| **5. Optimize** | `src/optimize/wasm.rs` | Shrink/optimize WASM via Binaryen | `optimize_wasm()` |

### Critical Rules

1. **Respect stage boundaries.** The parser knows nothing about WASM. The IR knows nothing about `wasm-encoder`. Codegen consumes IR and never re-parses Python. Don't reach across stages or smuggle Python AST types into codegen.

2. **`IRType` is the type-system contract** (`src/ir/types.rs`). It is the single source of truth for types across IR and codegen. When you add a Python type, add the `IRType` variant first, then handle it in the converter, the compiler, and `type_to_string()` in `src/lib.rs` — all of them, or the build breaks.

3. **Memory layout is computed in the IR, consumed in codegen.** `MemoryLayout` (on `IRModule`) holds string/bytes offsets and the object heap layout. When merging modules (multi-file compilation), layouts are merged via `memory_layout.merge_from()`. Don't hardcode memory offsets in codegen — read them from the layout.

4. **Codegen must emit valid WASM, and scratch locals do not survive nested emission.** The compiler reserves `SCRATCH_LOCALS` (`src/compiler/context.rs`) temporary locals per function for straight-line bookkeeping. Anything that must stay alive while another expression is emitted (a container pointer while its key and value are emitted, a search value while its container is, a literal's block while its elements are, a virtual call's receiver while its arguments are) goes in a **held slot** from `ctx.hold()` / `ctx.release_held()` (or `hold_f64()` for floats), never in `temp_local + N`. Nested emission runs arbitrary codegen over the scratch run, so a value parked there is silently overwritten. Held slots are locals, so they are also private to each activation, which is what keeps recursion from overwriting them. All memory access must be bounds-aware and use the layout offsets.

5. **Optimization is optional and last.** `optimize_wasm()` (Binaryen) runs only when `CompilerOptions::optimize` is set. Never make correctness depend on the optimizer — the unoptimized binary must already be valid and correct.

6. **stdlib modules are compiled, not interpreted.** There is no Python interpreter at runtime. `src/stdlib/` holds the allowlist of modules (`is_stdlib_module()`) and folds their constants. A module's functions are implemented one of two ways: as codegen in `src/compiler/stdlib/` (inline WASM and runtime helpers: `math`, `random`, `os`, `json`, `functools`), or as classes and functions written out in IR by a module pass (`src/ir/datetime/`, `src/ir/logging/`, `src/ir/re/`, `src/ir/collections/`, `src/ir/itertools.rs`, `src/ir/partials.rs`), which are then compiled like the program's own code. Choose IR when the module is mostly objects and algorithms CPython writes in Python; choose codegen when values' static types decide the code (`json.dumps` of a declared type).

7. **A construct either gives Python's answer or fails loudly.** This is the correctness rule the project is built on, and it outranks coverage: a construct the compiler does not implement must be *rejected*, never accepted and ignored. Never emit a placeholder value for something unhandled: an `I32Const(0)` in a `_ =>` arm is a silently wrong answer. Use `CompilationContext::report` (which collects errors while the whole module is still walked) for a codegen-stage refusal, or return an `unsupported_feature` error from the converter for one the IR can see. The single documented exception is integer width: `int` is an i32 and wraps, which the website's numbers page states (`docs/src/docs/numbers.md`).

8. **The module passes run in a fixed order, and each relies on the ones before it.** `logging` and `re` go first: `logging` names each record's function from the program's own function names, before decorators and lifting rename them, and both rewrite calls the later passes then see as ordinary method calls. User decorators and metaclasses next (both read nested `def`s and `super()` calls as written, and specialize functions per use), then nested functions are lifted, then `functools.partial` becomes closures over the lifted functions, then `datetime`'s classes are made (their constructor calls take keywords), then keyword arguments are placed (the later passes' expression walkers have wildcard arms that would skip an `IRExpr::Keyword`), then `with` is desugared (before generators, so a `with` inside a generator is a `try`/`finally` the generator pass already handles), then `itertools` and `collections` synthesize their generators and classes, then generators become state classes. A new pass goes where what it consumes has been produced and what it produces is still handled.

---

## After Every Set of Changes

After completing any set of changes, **always** run these in order:

1. **`just format`** — Format all Rust code (`cargo fmt --all`).
2. **`just lint`** — Run clippy with zero-warnings enforcement (`-D warnings`).
3. **`just test`** — Run the full test suite (`cargo test --all-features`).

For a full local CI-equivalent check:

4. **`just ci`**: runs format-check → lint → test → board-check (mirrors GitHub Actions).

Or run the complete dev workflow in one shot:

5. **`just dev`** — format → format-check → lint → build → test.

Do not consider a change complete until these pass cleanly.

### Additional Housekeeping

- **Update `CHANGELOG.md`** for any user-facing change (new feature, fix, breaking change).
- **Keep `README.md` short**: installation, a quick start, and links to the docs site and `CONTRIBUTING.md`. Update it only when installation or the quick-start API changes. Supported features, limitations, the library API in detail, and the host interface live in the website's documentation (`docs/src/docs/`).
- **Update the website on every major change or feature update.** `docs/` is the user-facing website, an Eleventy + Tailwind CSS project (see "The website" below). Whenever you add or change a feature, supported type, stdlib module, or any user-visible behaviour, reflect it in the relevant documentation page under `docs/src/docs/` (`python-subset.md`, `numbers.md`, `limitations.md`, `not-supported.md`, `library.md`, `running.md`), and keep the modules board (`docs/src/_data/modules.json`) in sync with what the compiler actually supports. The changelog page renders `CHANGELOG.md` at build time. Don't let the published docs drift behind the code.
- **Version tags on the modules board name published releases only.** On the modules board (`docs/src/_data/modules.json`), a feature's `version` field is the release that shipped it. Work that is merged but not yet released is tagged `"upcoming"`, never a future or internal/planned version number. When a release ships, replace the `upcoming` tags of the features it contains with the actual release version.
- **Prefer `just` commands** over raw `cargo` commands. The justfile handles sequencing, examples, and release steps correctly.
- **Prompt the user if `AGENTS.md` needs updating.** If your changes alter the pipeline, module layout, public API, supported types/stdlib modules, or conventions, tell the user: *"This change may require an update to AGENTS.md — would you like me to update it?"*

### Git Discipline

- **Do not run `git add` or `git commit` unless the user explicitly asks.** Stage and commit only on direct request.
- **When the user asks to commit**, review all staged/unstaged changes and prepare:
  - A **brief title** following conventional commits (`feat:`, `fix:`, `chore:`, etc.).
  - A **detailed description** summarizing what changed and why.
- **`plan/` is gitignored** — it's a local-only directory for each contributor's own working notes. Never try to commit it, and don't document its contents here.

---

## Architecture

Waspy is a single Rust library crate (`crate-type = ["cdylib", "rlib"]`) with a static documentation site:

| Component | Language | Location | Purpose |
|-----------|----------|----------|---------|
| **Compiler library** | Rust | `src/` | Parser, IR, codegen, optimizer, stdlib shims |
| **Wasmrun plugin** | Rust | `src/wasmrun.rs` | Optional `wasm-plugin` feature — integrates Waspy into wasmrun |
| **Examples** | Rust + Python | `examples/` | Runnable compiler drivers (`.rs`) + Python test inputs (`.py`) |
| **Website** | Eleventy + Tailwind CSS | `docs/` | Landing page, documentation, modules board, paper, and playground (GitHub Pages) |
| **Playground compiler** | Rust | `playground/` | wasm-bindgen wrapper exposing `compile`/`signatures` to the docs playground; builds waspy for wasm32 without Binaryen |

### Source Layout (`src/`)

```
src/
├── lib.rs                  # ★ Public API: compile_python_to_wasm[_with_options],
│                            #   compile_multiple_python_files*, compile_python_project*,
│                            #   get_python_*_metadata, type_to_string
├── wasmrun.rs              # [wasm-plugin feature] Wasmrun plugin: WaspyPlugin, WaspyBuilder,
│                            #   wasmrun_plugin_create() FFI export
├── core/                   # Core compiler primitives
│   ├── parser.rs           #   [Stage 1] Python → AST (rustpython-parser)
│   ├── config.rs           #   ProjectConfig, load_project_config(), is_config_file()
│   ├── options.rs          #   CompilerOptions, Verbosity
│   ├── comments.rs         #   Comment recovery for the python.comments custom section
│   ├── unicode.rs          #   CPython's character classes and case mappings (compile-time folding)
│   ├── unicode_tables.rs   #   Generated by scripts/gen_unicode_tables.py from CPython; do not edit
│   └── errors.rs           #   ChakraError enum (legacy name, see Gotchas), ErrorLocation
├── ir/                     # [Stage 2-3] Intermediate representation
│   ├── types.rs            #   ★ IRModule, IRFunction, IRStatement, IRExpr, IRType, MemoryLayout
│   ├── converter.rs        #   lower_ast_to_ir() — AST → IR
│   ├── decorators.rs       #   DecoratorRegistry: the built-in method kinds and decorators
│   ├── decorate.rs         #   User-written decorators, specialized per decorated function
│   ├── metaclasses.rs      #   Metaclasses, specialized per class they make
│   ├── generators.rs       #   Generators (functions and methods) to state classes; iterator consumption (for, comprehensions, zip)
│   ├── build.rs            #   Short IR builders the module passes write their code with (v, int, call, mcall, if_, ...)
│   ├── infer.rs            #   Types the IR shows before codegen, for passes that synthesize code
│   ├── partials.rs         #   functools.partial and cmp_to_key, as closures over the bound arguments
│   ├── itertools.rs        #   itertools, each call a generator synthesized for it
│   ├── collections/        #   namedtuple / NamedTuple, Counter, defaultdict, OrderedDict, deque, ChainMap, specialized per element type (spec.rs names them, ctor.rs types their calls)
│   ├── datetime/           #   timedelta, date, time, datetime, timezone (calendar.rs, classes.rs, time.rs, text.rs: strftime, ISO parsing, strptime)
│   ├── logging/            #   Loggers, handlers, Formatter (format.rs compiles each format), Filter, LogRecord, the module's state
│   ├── re/                 #   parse.rs (port of re/_parser.py), compile.rs (to a program), vm.rs (the backtracking matcher), classes.rs (Pattern, Match)
│   ├── context_managers.rs #   `with` desugared to __enter__/__exit__ calls
│   ├── finalize.rs         #   Whole-module checks and rewrites (nested def lifting, keyword arguments, abstract classes, call-site defaults)
│   └── entry_points.rs     #   detect_entry_points(), add_entry_point_to_module()
├── compiler/               # [Stage 4] IR → WASM (wasm-encoder)
│   ├── module.rs           #   compile_ir_module(): class layout (MRO, fields), globals, module init, assembly
│   ├── function.rs         #   Per-function codegen
│   ├── expression.rs       #   Expression codegen
│   ├── equality.rs         #   Value equality, ordering, and hashing (==, <, in, sets, dict keys)
│   ├── operators.rs        #   Binary operators on non-numbers: sequence + and *, set and dict operators, set comparisons
│   ├── dunders.rs          #   Rewrites at codegen: special methods, method variants and keywords, views, optional receivers, logging calls
│   ├── asm.rs              #   A structured builder for runtime helpers written as code
│   ├── libm.rs             #   musl's libm, ported over asm.rs, for math
│   ├── stdlib/             #   Codegen for the stdlib functions: math, random, os, json, functools
│   ├── sets.rs             #   Sets laid out and probed as CPython's are (iteration order is CPython's)
│   ├── closures.rs         #   Types a lifted closure's untyped parameters and captured cells from its calls
│   ├── exceptions.rs       #   Exceptions as objects: the builtin hierarchy, EXC_VALUE_GLOBAL, str/repr/args
│   ├── host.rs             #   The WASI preview 1 imports (print, input, argv, exit, file I/O)
│   ├── runtime.rs          #   Runtime helpers, built only when called (ctx.helper(Helper::...))
│   ├── format.rs           #   The format spec mini-language and float rendering, as helpers
│   ├── float_parse.rs      #   float() of a string, correctly rounded, as helpers
│   ├── unicode.rs          #   The unicode tables in memory, and the helpers that read them
│   ├── validate.rs         #   Validates every generated module before it is returned
│   └── context.rs          #   CompilationContext, SCRATCH_LOCALS, held slots, the runtime globals, LocalInfo/FunctionInfo/ClassInfo
├── optimize/               # [Stage 5] WASM optimization
│   └── wasm.rs             #   optimize_wasm() — Binaryen
├── stdlib/                 # The stdlib allowlist and its constants (the functions are in compiler/stdlib/ and the ir/ module passes)
│   ├── mod.rs              #   is_stdlib_module(), get_stdlib_attributes() dispatch
│   ├── sys.rs  os.rs  math.rs  random.rs  json.rs  re.rs
│   └── datetime.rs  collections.rs  itertools.rs  functools.rs  logging.rs
├── analysis/               # Project/source analysis (non-codegen)
│   ├── imports.rs          #   Import analysis
│   ├── metadata.rs         #   FunctionSignature extraction
│   └── project.rs          #   Project structure analysis
└── utils/                  # Shared utilities
    ├── fs.rs               #   collect_compilable_python_files(), is_special_python_file()
    ├── logging.rs          #   log_debug!/log_verbose!/log_info!/log_warn! macros + init()
    └── paths.rs            #   Path helpers
```

### Key Architectural Patterns

- **Library, not a binary.** There is no `main.rs` or `[[bin]]`. Compilation is driven by the public functions in `src/lib.rs` or by the example programs in `examples/`. To "run" the compiler during development, use a `just compile …` recipe (which invokes an example).
- **Single IR, two compile paths.** `compile_python_to_wasm*` compiles one source string; `compile_multiple_python_files*` / `compile_python_project*` merge several files into one `IRModule` (de-duplicating function names, merging memory layouts) before codegen.
- **Verbosity-gated logging.** Use the `log_debug!`, `log_verbose!`, `log_info!`, `log_warn!` macros (from `src/utils/logging.rs`), initialized via `utils::logging::init(verbosity)`. Output is gated by `Verbosity` (`Quiet`/`Normal`/`Verbose`/`Debug`).
- **Error handling is two-tier.** The public API returns `anyhow::Result` with `.context(...)`. The custom `thiserror` enum `ChakraError` (in `src/core/errors.rs`) carries structured, located compiler errors. The plugin path uses its own `CompilationError`/`CompilationResult` in `src/wasmrun.rs`.
- **Binaryen is a default feature.** `optimize` (on by default) gates the `binaryen` dependency and the real `optimize_wasm()`. Without it, `optimize_wasm()` returns its input unchanged, which is how `playground/` builds waspy for `wasm32-unknown-unknown`. Code in `src/` must compile with `--no-default-features` (CI lints it that way).
- **Optional plugin via feature flag.** `wasmrun.rs` is gated behind `#[cfg(feature = "wasm-plugin")]`. Plugin metadata lives under `[package.metadata.wasm_plugin]` in `Cargo.toml`. The `wasmrun_plugin_create()` FFI symbol is the entry point wasmrun loads.

---

### The website (`docs/`)

An Eleventy 3 + Tailwind CSS v4 project, deployed to GitHub Pages under `/waspy/`.

```
docs/
├── package.json, package-lock.json   # npm dependencies (commit the lock file)
├── eleventy.config.js                # pathPrefix, Tailwind via PostCSS, esbuild bundles, markdown, filters
├── lib/                              # the shared markdown-it instance and Prism highlighting
└── src/                              # input; output goes to docs/_site/ (gitignored)
    ├── _data/                        # site.js (version read from Cargo.toml), docsNav.js, modules.json (the board), changelog.js (renders CHANGELOG.md)
    ├── _includes/                    # layouts/base.njk, layouts/docs.njk, partials (header, footer, icons)
    ├── assets/css/main.css           # the design system: theme tokens, components, utilities
    ├── assets/js/                    # main.js (every page), home.js
    ├── docs/                         # /docs/: getting started, library, subset, numbers, limitations, not supported, running, programs, changelog
    ├── modules/                      # the development board page and its script
    ├── playground/                   # the playground page, playground.js (CodeMirror 6), examples.js, wasi.js, compiler/ (built)
    ├── paper/                        # the paper and waspy.pdf
    └── index.njk, 404.njk, redirects.njk
```

- **Colors are semantic tokens** (`bg-surface`, `text-ink-1`, `border-line`, `text-honey`, ...) defined in `main.css` that switch with `data-theme` on `<html>`, so a page needs no `dark:` variants. Reuse the components there (`glass`, `card-link`, `btn-primary`, `prose-waspy`, `code-window`, ...) rather than restyling.
- **Write root-relative URLs** (`/docs/`): Eleventy's HtmlBasePlugin adds the `/waspy/` prefix.
- **Browser scripts are bundled by esbuild** from the entry points listed in `eleventy.config.js`; a page loads its own through `scripts:` front matter.
- **A new documentation page** is a Markdown file in `docs/src/docs/` plus an entry in `docs/src/_data/docsNav.js`. Markdown there runs through Nunjucks first, so `{{ site.version }}` works and a literal `{{`, `{%`, or `{#` needs escaping.
- **Moving a page** means adding its old path to `docs/src/redirects.njk`.

## Tech Stack & Tooling

| Tool / Crate | Purpose | Notes |
|------|---------|-------|
| **Rust** | Compiler library | Edition 2021, MSRV 1.70 (CI uses 1.88) |
| **Cargo** | Build system | `cargo build --release` |
| **Just** | Task runner | `justfile` — run `just` for available commands |
| **rustpython-parser** | Python parsing | Produces the Python AST (Stage 1) |
| **wasm-encoder** | WASM codegen | Emits the binary (Stage 4) |
| **binaryen** | WASM optimization | Native dependency; needs a C/C++ toolchain to build. Behind the default `optimize` feature |
| **wasm-bindgen** | Playground JS glue | Only in `playground/`, pinned exactly (`=0.2.x`) to match the CLI in CI |
| **Eleventy** | Website generator | `docs/`: Nunjucks layouts, Markdown documentation, data files |
| **Tailwind CSS** | Website styles | v4 through PostCSS; tokens and components in `docs/src/assets/css/main.css` |
| **esbuild** | Website scripts | Bundles the browser scripts, CodeMirror 6 in the playground included |
| **anyhow** | Error context | Public API return type |
| **thiserror** | Structured errors | `ChakraError` in `src/core/errors.rs`, `CompilationError` in plugin |
| **chrono** | Date/time | Used by the legacy helpers in `src/stdlib/datetime.rs`; the `datetime` module itself is IR (`src/ir/datetime/`) |
| **regex** | Regex | Compile-time folding of constant `re.sub` calls (`stdlib::re::fold_sub`); matching at run time is `src/ir/re/` |
| **libm** (dev) | Reference math | What the `math` port (`compiler/libm.rs`) is checked against, bit for bit |
| **serde** / **serde_json** | Serialization | Plugin metadata (`wasm-plugin` feature) |
| **log** | Logging facade | Wrapped by `utils/logging.rs` macros |
| **clippy** | Linting | Enforced: `-D warnings` (zero-warnings policy); see `clippy.toml` |
| **cargo fmt** | Formatting | See `rustfmt.toml` |

---

## Build & Development

### Quick Commands

```sh
just build           # cargo build --release
just build-examples  # cargo build --examples
just test            # cargo test --all-features
just format          # cargo fmt --all
just format-check    # cargo fmt --all -- --check
just lint            # cargo clippy --all-targets --all-features -- -D warnings
just lint-fix        # clippy --fix where possible
just ci              # format-check → lint → test → board-check (mirrors GitHub Actions)
just board-check     # every done feature on the board (docs/src/_data/modules.json) is backed by a passing test in tests/integration/board.rs
just dev             # format → format-check → lint → build → test
just docs-rs         # cargo doc --all-features --no-deps --open
just docs-check      # rustdoc with -D warnings
just docs            # serve the website with live reload at localhost:8000/waspy/ (npm ci on first run)
just docs-build      # build the website into docs/_site/, as CI does
just playground      # build waspy to wasm32 into docs/src/playground/compiler/ (needs wasm32 target + pinned wasm-bindgen CLI)
just playground-verify  # compile every playground example with that build, check against CPython
just playground-serve   # build, then serve the website at localhost:8000/waspy/
just playground-lint    # fmt + clippy for the playground crate (wasm32)
just clean           # cargo clean + remove examples/output and docs/_site
```

### Compiling Python (development)

Because there's no CLI binary, compilation runs through example drivers:

```sh
just compile examples/typed_demo.py                 # advanced_compiler driver
just optimize examples/typed_demo.py                # compile + show size metadata
just compile-multi out.wasm file1.py file2.py       # merge files into one module
just compile-project examples/calculator_project    # compile a project directory
just run-typed-demo                                  # type system demo
just examples                                        # run the full example suite
```

### Building from Source

```sh
cargo build --release                # Library build
cargo build --all-features           # Include the wasm-plugin feature
cargo build --examples               # Build example drivers
cargo test                           # Run tests
cargo test test_name                 # Run a specific test
cargo run --example advanced_compiler examples/typed_demo.py
```

---

## Library API Reference

The public surface lives in `src/lib.rs`:

```rust
// Single-source compilation
compile_python_to_wasm(source: &str) -> Result<Vec<u8>>
compile_python_to_wasm_with_options(source: &str, opts: &CompilerOptions) -> Result<Vec<u8>>

// Entry-file compilation with user-module resolution (imports of sibling
// .py files are resolved from disk, transitively, each compiled once)
compile_python_file<P: AsRef<Path>>(path: P, optimize: bool) -> Result<Vec<u8>>
compile_python_file_with_options<P>(path, opts: &CompilerOptions) -> Result<Vec<u8>>

// Multi-file compilation (merged into one module)
compile_multiple_python_files(sources: &[(&str, &str)], optimize: bool) -> Result<Vec<u8>>
compile_multiple_python_files_with_options(sources, opts) -> Result<Vec<u8>>
compile_multiple_python_files_with_config(sources, optimize, config: &ProjectConfig) -> Result<Vec<u8>>

// Project-directory compilation
compile_python_project<P: AsRef<Path>>(dir: P, optimize: bool) -> Result<Vec<u8>>
compile_python_project_with_options<P>(dir, opts) -> Result<Vec<u8>>

// Metadata (no codegen)
get_python_file_metadata(source) -> Result<Vec<FunctionSignature>>
get_python_project_metadata<P>(dir) -> Result<Vec<(String, Vec<FunctionSignature>)>>

// Helper
type_to_string(ir_type: &IRType) -> String
```

---

## Testing

- **`tests/`** holds the suite, each file its own test target declared in `Cargo.toml`. `tests/utils/harness.rs` is the shared harness (`try_compile`, `instantiate`, `call_i32`, `call_str`, ...): it compiles a source string, instantiates the module under `wasmi`, and calls an export, so a test asserts a **runtime result** rather than a successful compilation.
  - `unit/basics.rs`: operators, conversions, integer width.
  - `unit/errors.rs`: located errors and what must be refused.
  - `unit/memory_safety.rs`: growth, aliasing, checked indexing, validation.
  - `unit/miscompiles.rs`: regressions for constructs that once compiled "successfully" and answered wrong. **This is where a silent-miscompile fix goes.**
  - `unit/language.rs`: nested functions and `nonlocal`, star parameters, and class objects, with the cases around them that must be refused rather than answered wrong.
  - `unit/stdlib.rs`: the standard library at run time, mostly whole programs (`prints` / `streams` compare a `__main__` block's stdout and stderr with CPython's); the `math` port is checked bit for bit against the `libm` crate.
  - `integration/examples.rs`, `integration/coverage.rs`: every `examples/*.py` compiled, instantiated, and asserted; `integration/wasmrun_plugin.rs` does the same through the plugin (needs `--features wasm-plugin`).
  - `integration/board.rs`: one test per feature on the development board (`docs/src/_data/modules.json`), in a module per card. Each feature names its test with a `"test": "card::name"` field. A done feature's test must pass; an open feature may have one, marked `#[ignore]`, asserting the answer it will give once it lands. `just board-check` (`scripts/board_check.mjs`) fails when a done feature names no passing test, an open feature's test passes, or a test backs no feature, so **moving a feature on the board means adding or un-ignoring its test in the same change**, and adding a feature means adding its test.
- **Every expected value is what CPython answers for the same source.** Run the Python first and assert its answer, so a failure means a divergence from the reference implementation rather than a change in what the compiler happens to do. Do not assert what the compiler currently does.
- **Unit tests may also live inline** as `#[cfg(test)]` modules alongside source (`src/stdlib/re.rs`, `src/utils/logging.rs`) when they cover something with no Python-level surface.
- **Examples double as functional tests.** `just verify-examples` compiles every bundled example, and depends on `just verify-runtime`, which runs the end-to-end programs under Node and the `wasmtime` CLI (both optimization levels) against checker suffixes in `tests/fixtures/runtime/`.
- Always run `cargo test --all-features` before committing.
- **The playground examples are checked too.** `docs/src/playground/examples.js` holds the examples the page ships; `just playground-verify` (`scripts/verify_playground.mjs`) compiles each with the wasm32 build, calls the functions listed in the script, and requires CPython's answer. Adding an example means adding its calls there, and an example must avoid constructs the compiler refuses (except the one meant to show a refusal).
- CI runs tests on `stable` and enforces zero clippy warnings on Rust 1.88: `cargo clippy --all-targets --all-features -- -D warnings`. There are three named gates: the test binaries, the board check, and the runtime verification above.
- Harness helpers worth knowing: `dedent` (write a test's Python indented in a raw string), `call_i32_traps` (assert a call traps, how a fault CPython raises for and nothing here catches surfaces), and `run_main(source, stdin, args)` (run a program's `__main__` block as a WASI command and get its stdout, stderr, and exit code, to compare with CPython's).
- **The harness is a WASI host.** `Host` (`tests/utils/harness.rs`) implements the preview 1 functions a module may import, with stdin, argv, and captured output; `link_host` registers them, and the deprecated `waspy_host` file imports alongside, over the same files. Its file system is in memory (`Host::files`): one preopened directory `.` at fd 3, files from fd 4, so `open()` tests need no disk, and `instantiate_with_host_fs` / `call_host_fs_i32` keep the files across calls on one instance. A new WASI import means implementing it here, in `docs/src/playground/wasi.js` (which `scripts/verify_runtime_node.mjs` also uses), and checking it under the `wasmtime` CLI.

---

## Code Conventions

### Rust

- Standard Rust naming: `snake_case` for functions/variables, `PascalCase` for types.
- Public APIs get rustdoc comments (`///`) documenting Arguments, Returns, and Errors — match the existing style in `src/lib.rs`.
- The public API returns `anyhow::Result` with `.context("...")`. Structured compiler errors use `ChakraError` (`thiserror`) in `src/core/errors.rs` — add new variants there, include `ErrorLocation` where possible.
- Use the logging macros (`log_debug!`, `log_verbose!`, `log_info!`, `log_warn!`) for diagnostic output — never bare `println!` in library code (the metadata path is the only existing exception).
- Keep `#[allow(dead_code)]` annotated with a `// TODO:` comment explaining the plan (see `src/compiler/context.rs`).
- All memory operations in codegen must use offsets from `MemoryLayout` and stay within the scratch-local budget (`SCRATCH_LOCALS`). A value that must outlive a nested `emit_expr` goes in a held slot (critical rule 4).

### Commit Messages

Follow conventional commits (matches the existing history):

```
feat: description          # New feature
fix: description           # Bug fix
chore: description         # Maintenance, deps, CI
docs: description          # Documentation only
refactor: description      # Code restructuring
test: description          # Adding/fixing tests
```

### Branching

- `main` — stable release branch.
- `feature/*` — feature branches (per `CONTRIBUTING.md`).
- `fix/*` — bug fix branches.
- `docs/*` — documentation branches.
- PRs are reviewed and merged by maintainers.

---

## Versioning & Release

- Version source of truth: `Cargo.toml` (`version = "X.Y.Z"`).
- Follow [Semantic Versioning](https://semver.org/).
- `just prepare-release` runs format → lint → build → test → `cargo publish --dry-run`.
- `just publish` publishes to crates.io and then `just github-release` tags and creates the GitHub release.
- Keep `CHANGELOG.md` updated with every meaningful change.
- **On release, retag the docs modules board.** Replace every `"upcoming"` version tag in `docs/src/_data/modules.json` for features shipping in the release with the new version number (see the housekeeping rule above; the board never names an unreleased version).

---

## Important Files to Know

| File | Why It Matters |
|------|----------------|
| `Cargo.toml` | Version, dependencies, features, plugin metadata — start here |
| `src/lib.rs` | The entire public API and the pipeline orchestration |
| `src/ir/types.rs` | `IRType`, `IRModule`, `MemoryLayout` — the IR contract |
| `src/ir/converter.rs` | AST → IR lowering; the decorator allowlist and `@singledispatch`/`@total_ordering` lowering; the module pass order (end of `lower_ast_to_ir`) |
| `src/ir/decorate.rs`, `src/ir/metaclasses.rs` | User decorators and metaclasses, specialized at compile time; `is_internal_function` |
| `src/compiler/host.rs` | The WASI imports, including file I/O and the deprecated `waspy_host` interface |
| `src/compiler/runtime.rs` | The runtime helpers (`Helper`), built only when a program calls them |
| `src/compiler/module.rs` | Top-level WASM codegen entry (`compile_ir_module`) |
| `src/compiler/context.rs` | `CompilationContext`, `SCRATCH_LOCALS` budget, held slots, the runtime globals, the MRO (`c3_linearization`) and `check_class_store` |
| `src/optimize/wasm.rs` | Binaryen optimization pass |
| `src/core/parser.rs` | Python parsing (rustpython) |
| `src/core/options.rs` | `CompilerOptions`, `Verbosity` |
| `src/core/errors.rs` | `ChakraError` (structured errors) |
| `src/stdlib/mod.rs` | stdlib module allowlist + attribute dispatch |
| `src/compiler/dunders.rs` | The codegen-time rewrites: special methods, method variants and keywords, views, optional receivers, logging calls |
| `src/ir/build.rs` | The IR builders every module pass writes with |
| `src/wasmrun.rs` | Wasmrun plugin integration (`wasm-plugin` feature) |
| `justfile` | All development task commands |
| `CHANGELOG.md` | Keep updated with every change |
| `CONTRIBUTING.md` | Contributor workflow and standards |

---

## Common Tasks for Agents

### Adding support for a new Python language feature

1. Confirm `rustpython-parser` produces the AST node you need (Stage 1 — usually no change here).
2. Lower it in `src/ir/converter.rs` into the appropriate `IRStatement`/`IRExpr` (add a variant in `src/ir/types.rs` if needed). Prefer desugaring into constructs that already compile, as `assert` (an `If` that raises `AssertionError`) and `del x[i]` (a `__delitem__` / `__delslice__` method call, `DEL_ITEM_METHOD` / `DEL_SLICE_METHOD`) do. A new variant has to be handled by every IR walker, and several of them have wildcard arms that would skip it silently: `rename_vars` (converter), `collect_free_names` and `visit_expr` (`finalize.rs`), `for_each_child` and `expr_has_float` (`generators.rs`), `scan_expr_locals` and `infer_value_type` (`compiler/function.rs`), `scan_expr_calls`, `expr_uses_open` and `infer_field_value_type` (`compiler/module.rs`), and `infer_key_body` and `key_needs_param_type` (`compiler/expression.rs`); a new statement that binds names also needs `walk_targets` (`ir/decorate.rs`), which the decorator and metaclass passes rename through. `IRExpr::Conditional` is the most recent example to copy.
3. Implement codegen in `src/compiler/` (`expression.rs` / `function.rs`), using `CompilationContext` and `MemoryLayout`. If the expression opens a WASM block or loop around nested emission, bracket that emission with `ctx.enter_expr_blocks(n)` / `ctx.exit_expr_blocks(n)` (see the gotcha on raising inside expressions).
4. If it introduces a new type, add an `IRType` variant and handle it in the converter, the compiler, **and** `type_to_string()` in `src/lib.rs`.
5. Add an example `.py` under `examples/` and verify with `just compile examples/<file>.py`.
6. Add tests; run `just dev`.
7. Update the documentation pages' subset and limitations (`docs/src/docs/`), the modules board, and `CHANGELOG.md`.

### Adding a new IR type

1. Add the variant to `IRType` in `src/ir/types.rs`.
2. Produce it in `src/ir/converter.rs` (type inference / annotations).
3. Handle it in codegen (`src/compiler/`) — value representation, memory layout, WASM value type mapping.
4. Add a `type_to_string()` arm in `src/lib.rs` (the compiler won't build without it — the match is exhaustive).

### Adding a new stdlib module

1. Create `src/stdlib/<module>.rs` exposing `get_attribute()` (its constants), following the existing modules.
2. Register it in `src/stdlib/mod.rs`: add to `is_stdlib_module()` and the `get_stdlib_attributes()` dispatch.
3. Implement its functions either as codegen in `src/compiler/stdlib/` (answer `None` for what is not implemented, and the call site refuses it), or as a module pass under `src/ir/` that writes CPython's classes and functions out in IR (see the gotcha "Standard-library modules written in IR follow one shape"). Place a new pass by rule 8.
4. Add tests in `tests/unit/stdlib.rs` whose expected output is CPython 3.13's, and a board test per card.

### Adding a decorator or a builtin function

1. Decide first whether it changes what the program *computes*. If it does, it must be implemented or refused; there is no third option (see critical rule 7).
2. A decorator: add the name to `classify_function_decorator` in `src/ir/converter.rs` (module-level functions), to the class-decorator loop in `process_class_definition`, and/or to the match in `ir::decorators::method_kind` (methods), depending on where it can appear. `FunctionDecorator::Inert` is for one with no observable effect; anything else needs a lowering. A decorator the program defines itself goes through `ir::decorate` instead (`FunctionDecorator::User`), so a new shape of user decorator is taught there.
3. A builtin: lower it in `src/ir/converter.rs` if it desugars (like `bool()` or the empty collection constructors), otherwise add an arm to the builtin match in the `FunctionCall` case of `src/compiler/expression.rs`. The `_ =>` arm at the end reports an error, so a name you don't add is refused, which is the intended default.
4. Keep the stack balanced on the refusal path: drop each argument (twice for a string/bytes pair) and push a placeholder of the right shape before calling `ctx.report`, so the module still validates and one run reports every offending call.
5. Add a regression test asserting the *runtime result* against CPython's answer for the same source, in `tests/unit/miscompiles.rs`, plus an example if it is user-facing.

### Adding a construct that compares, hashes, or stores values

1. Equality or ordering between values: call `crate::compiler::equality::{eq_unsupported, order_unsupported}` and refuse on `Some`, then `emit_values_eq` / `emit_values_order` with the values in held slots.
2. A new container that searches or hashes: go through `emit_slot_eq_needle` and `emit_set_hash`, and check `hash_unsupported` for keys and members.
3. A new literal or runtime-built object: allocate with `__alloc` per evaluation (see `emit_literal_block`), never at a compile-time address.
4. A new place a value is stored into a typed slot (an argument, a field, a return, a collection element, a local): call `ctx.check_class_store(&value_type, &slot_type, what)` once the value is emitted, so an instance of an unrelated class is refused instead of dispatched as the slot's class, and `ctx.check_untyped_store` where the slot may be untyped (see the untyped-place gotcha). A type inferred for a place that holds instances takes their nearest common base (`ctx.common_base`, or `common_class_base` before classes are registered), never the first instance's class.
5. Test with recursion and with a second call of the same function, not just one call, since sharing across calls and activations is the failure mode.

### Adding a compiler option

1. Add the field to `CompilerOptions` in `src/core/options.rs` (and update `Default`). A struct literal that lists every field stops compiling, so the crate's own examples, doc examples, and tests build options with `..CompilerOptions::default()`; keep it that way, and note the break in `CHANGELOG.md`.
2. Thread it through the relevant `*_with_options` functions in `src/lib.rs`.
3. Honor it in the appropriate stage (e.g. codegen or optimize).
4. Add tests.

### Adding a new error variant

1. Add the variant to `ChakraError` in `src/core/errors.rs` with a `#[error("...")]` message.
2. Include `ErrorLocation` (file/line/column/function) where the information is available.
3. Surface it through the `anyhow` chain with `.context(...)` at the call site.

### Working on the wasmrun plugin

1. Plugin code is in `src/wasmrun.rs`, gated behind the `wasm-plugin` feature.
2. Build/test it with `--all-features` (or `--features wasm-plugin`).
3. Keep `[package.metadata.wasm_plugin]` in `Cargo.toml` in sync with the plugin's declared capabilities/extensions/entry files.
4. The `wasmrun_plugin_create()` `#[no_mangle]` export is the ABI boundary — don't change its signature without coordinating with wasmrun.

---

## Gotchas & Pitfalls

- **There is no CLI binary.** Waspy is a library; "running" it means calling the API or invoking an example via `just`. Don't add ad-hoc `main`-style code to `src/`.
- **`ChakraError` is a legacy name.** The custom error enum kept its old name from when the project was called "Chakra." It is the current error type — don't be confused by the name, and don't rename it casually (it's part of the public surface via `core::errors`).
- **`type_to_string()` is an exhaustive match.** Adding an `IRType` variant without updating `src/lib.rs` is a compile error — fix it there too.
- **Binaryen is a native dependency.** The `binaryen` crate needs a C/C++ toolchain to build. Optimization failures may stem from the build environment, not your code. It is behind the default `optimize` feature, so don't use it outside `src/optimize/wasm.rs`.
- **The website is built and deployed by CI, not served from the branch.** `.github/workflows/docs.yml` builds `playground/`, runs `just playground-verify`, builds the site with `npm run build` in `docs/`, and uploads `docs/_site/` to GitHub Pages on every push to `main`. `docs/_site/`, `docs/node_modules/`, and `docs/src/playground/compiler/` are build outputs and gitignored; never commit them, but do commit `docs/package-lock.json`. The `wasm-bindgen` crate pin in `playground/Cargo.toml` and the CLI version and checksum in the workflow move together.
- **Optimization is opt-in and must not affect correctness.** If a binary is only valid after `optimize_wasm()`, the codegen has a bug.
- **`SCRATCH_LOCALS` is a hard budget.** Codegen that overruns the reserved temporary locals will alias real variables and silently corrupt output. Raise the constant in `src/compiler/context.rs` if you need more. `HELD_LOCALS` / `HELD_F64_LOCALS` are the budget for held slots; exceeding them is reported, not miscompiled.
- **Every evaluation of a collection literal is a fresh `__alloc` block.** A literal builds straight into its own block, whose pointer lives in a held slot while the elements are emitted. Don't give anything a function can build a compile-time address: a function body runs once per call, so a static region would be one object shared by every call, and a recursive call would overwrite it mid-build.
- **Module-level and class-level definitions that are not plain constants live in WASM globals.** `compile_ir_module` appends a synthesized `__module_init` (`MODULE_INIT_FN`) that assigns each one in source order (class variables first, as `Class.var`), compiles it before every other function so each global's type is known, and runs it from the start section through a `() -> ()` wrapper that traps on a pending exception. A read is `global.get` (`ctx.module_global_index`, `ctx.module_global_types`); plain constants are still inlined. Globals start at `FIRST_MODULE_GLOBAL` (5); the ones below it are the runtime's: the allocator, StopIteration, the pending exception's class id (`EXC_TYPE_GLOBAL`), the call depth (`CALL_DEPTH_GLOBAL`), and the pending exception object (`EXC_VALUE_GLOBAL`, `src/compiler/exceptions.rs`). A runtime global added below it moves `FIRST_MODULE_GLOBAL` up. A class variable assigned through its class anywhere (`Counter.count += 1`) is always a global, so every reader, through the class or through an instance, sees the write; one that is also assigned as `self.<name>` gets an instance field too, which shadows it for that instance as in CPython. Don't inline a non-constant module definition at a read site: that made every read a new object. A name declared `global` in any function (`IRStatement::Global`, collected by `function::global_declarations`) is always a WASM global, even when its definition is a constant, and is kept out of the constant-folding map so definitions computed from it keep their import-time value. In a function, `ctx.declared_globals` holds the declared names and `assigns_module_global` routes an assignment to `global.set`; a rebinding keeps the type the module init recorded, and a global bound by a loop, unpacking or `with` is refused.
- **Compare values through `src/compiler/equality.rs`, never by word.** `emit_values_eq` / `emit_values_order` / `emit_value_hash` (with the `*_unsupported` checks first) are the one implementation of `==`, ordering, and hashing; `emit_slot_eq_needle` and `emit_set_hash` route through them, so `in`, `index`, `count`, sets, and dict keys agree. Comparing two collection pointers or two string offsets with `i32.eq` is identity, which is a wrong answer for `==`. A user `__eq__` that can raise is refused inside these sequences, because the unwind check after a call assumes no extra blocks are open.
- **Multi-file compilation shares one flat namespace.** Two files defining the same function name is a hard error naming both files, never a silent pick of one. Memory layouts are merged via `merge_from()`, so don't assume single-file offset assumptions hold. Anything else that merges per file (`IRModule::dispatch_tables`, variables, imports, classes) has to be extended in the merge loop in `src/lib.rs` too.
- **Decorators are an allowlist, and so are called names.** `src/ir/converter.rs` classifies every decorator on a function (`classify_function_decorator`) and class, and `ir::decorators::method_kind` classifies every one on a method; anything not on the list is a compile error. Supported: the method kinds (`@staticmethod`/`@classmethod`/`@property`/`@<name>.setter`), `@abstractmethod`, `@dataclass`, `@functools.total_ordering`, `@functools.singledispatch` with `@f.register`, the caching decorators (`@lru_cache`/`@cache`/`@wraps`, accepted because they change nothing a compiled module can observe), and a decorator that is a function of this module (`FunctionDecorator::User`). Likewise, a call to a name that is neither a compiled function nor a builtin codegen implements is refused, not compiled to 0. Adding a decorator or builtin means adding it to the allowlist *and* implementing it, never just the former.
- **A place that holds instances is typed as their nearest common base.** A method call dispatches from the receiver's static class (virtually only when something below that class overrides the method), so a list, dict value, local, or field holding a `Circle` and a `Rect` must be typed `Shape`, or `Rect` runs `Circle`'s method (#128). Literals unify in `literal_element_type` (`src/compiler/expression.rs`), reassigned locals and collection writes in the local pre-pass (`widen_class_local` / `widen_class_element` in `src/compiler/function.rs`), and fields in the class layout pass in `src/compiler/module.rs`. Whatever a widening misses is caught by `check_class_store` at the store and refused. Types are per function, not per program point, so a widened local only exposes the base's fields and methods.
- **Iterators are consumed through the IR, not by codegen.** `src/ir/generators.rs` rewrites a `for` over a generator or `__next__` class into a `__next__` loop, and a comprehension's first iterable into a call to a synthesized `__drain_<Class>` helper (`DRAIN_FN_PREFIX`, kept out of exports and function metadata) that returns a list. Codegen only ever iterates collections and ranges and refuses anything else, so a new construct that iterates must go through this pass or refuse an iterator. A `zip()` lowers to per-round pulls into value temporaries (`ZIP_VALUE_PREFIX`), binding the targets only once every iterable has produced, so an early stop leaves the targets as CPython does. An iterator in a comprehension's inner `for` clause is refused, since inner iterables are evaluated twice (to size the result, then to fill it).
- **`@singledispatch` dispatch is static.** `IRModule::dispatch_tables` (`IRDispatch` in `src/ir/types.rs`) carries each base function's registered arms; the converter fills it, `compile_ir_module` copies it onto `CompilationContext`, and the `FunctionCall` arm in `src/compiler/expression.rs` picks the arm from the *static* IR type of the first argument. There is no runtime type tag on scalars, so an argument whose type is `Unknown` is refused rather than guessed.
- **stdlib support is an allowlist.** Only modules in `is_stdlib_module()` are recognized; everything else is treated as a user-written module: multi-file compilation links it into the single output module (namespace calls, aliases, and constants resolve statically), and `compile_python_file` resolves it from disk next to the entry file.
- **A stdlib function is implemented, or refused by name.** A module constant is read (string constants are folded to interned literals in the converter), and `from math import pi` reads through `ctx.stdlib_imports`. A call the codegen modules or the IR passes do not implement reaches one path in `src/compiler/expression.rs` (`stdlib_call_name`), which refuses it naming the dotted function; a refusal never names a planned release version. Implementing a stdlib function means replacing that refusal for it, never adding a placeholder value. Constant `re.sub`/`re.escape` calls are still folded in the converter (`fold_re_call`) where the `regex` crate and CPython provably agree, and left to the run-time engine otherwise.
- **Standard-library modules written in IR follow one shape.** A pass runs only when the module imports its module, and finds the program's names for it (`import logging as lg`, `from re import compile`) through a `Bindings` of the imports. Its classes are named `module.Class` (`datetime.date`, `logging.Logger`, `re.Match`, so `type(x)` prints `<class 're.Match'>`, `build_class_name` in `exceptions.rs`), and annotations name them through the module (`type_annotation_to_ir_type` maps `datetime.X`, `logging.X`, `re.X`). Its functions take a prefix (`__datetime_`, `__logging_`, `__re_`, ...) listed in `is_internal_function`, so they stay out of exports and metadata. It interns every string its code uses (`memory_layout.add_string`). Module state lives in class variables of a class the pass puts first in `module.classes` (`logging._State`, `re._Cache`), since class variables are initialized before the program's own definitions. A class whose instances CPython makes immutable carries `FROZEN_VAR`, so assigning a field is refused.
- **A method can have variants by its argument's type**, named `method<encoded type>` (`collections::spec::variant`): `setLevel<int>` and `setLevel<str>`, `__sub__<datetime.timedelta>`. Code generation picks the variant for the argument's static type (`dunders::rewrite_variant`, `rewrite_binary` for operators), an int taking a float variant when there is no int one. It is how a builtin method that accepts several types is written in IR.
- **Hidden names carry facts between passes and codegen.** The `collections` markers (`<view>`, `<data>`, `<from>`, `<delegates>`, `<frozen>`), and keywords no Python call can spell: the converter adds `__log_line` to calls of logging methods (from the parser's line table, `core::parser::line_of`), the logging pass adds `__log_func` and pre-lowers a literal message's `%` formatting into `__log_text` over `__log_arg<k>` placeholders, and codegen consumes them on a `logging.Logger` receiver (`dunders::log_parts`) and strips them from any other. A logging call that is a statement evaluates its arguments, then formats the message only if the logger is enabled for the level.
- **Method calls take keywords and defaults at codegen.** The receiver's class decides which method is called, so `dunders::place_method_keywords` places keywords by name and `fill_method_defaults` fills constant defaults; `super().m(...)` is placed against the method it reaches (`place_super_arguments`).
- **An optional string or instance is used as CPython uses the value.** `Optional[str]` is a single word, the string's offset or 0. Converting it to `str` checks for None (it traps); `x == s` compares the text (`__optional_text_eq`); `+`, `len()`, `int()`, and `float()` raise CPython's `TypeError` on None; a method call or attribute read on an `Optional[str]` or `Optional[Class]` goes through `__some`, which raises `AttributeError` (or the error a third argument names, as `x[k]`'s `TypeError`). Because these checks raise, a function that reads an optional reference and uses it counts as one that can raise (`may_deref_none` in `module.rs`).
- **`re` patterns are compiled with the program.** `src/ir/re/parse.rs` is a port of CPython 3.13's parser (its `re.error` messages and positions are reported at compile time), `compile.rs` turns the parse into a program, and `vm.rs` is the backtracking matcher, written in IR, whose repeats follow `sre`'s `MAX_UNTIL`/`MIN_UNTIL`. Each distinct pattern and flags is one `re._Cache` class variable; a pattern is an instance of `_Pattern0`, `_Pattern1`, or `_PatternN` by its group count, because `findall` and `split` answer different types. A pattern must be a literal, a module constant, or a compiled pattern held in a module variable.
- **The `__main__` block is the entry point.** In the module that is `__main__`, the converter lowers the block's body to the function `MAIN_BLOCK_FN` (`src/ir/entry_points.rs`, internal), declaring `global` the module variables it assigns, and `_start` (`host::build_start`) runs it, or `main()` for a `__main__.py` or CLI script without a block. A module with neither is a library with no `_start`. Every definition is made before `_start` runs, so a block that reaches a name defined after it is refused (`check_main_block_order`). An imported module's block is skipped.
- **`__name__` is the module's.** The converter lowers it to `"__main__"` in the entry file and to the module's dotted name in an imported one (`lower_ast_to_ir_named`, which the multi-file path in `lib.rs` uses).
- **An untyped place holds one word, read as an int.** An unannotated parameter or return (`IRType::Unknown`) and the elements of a bare `list`/`set`/`dict` parameter or return (`IRType::Any`) take only ints, bools, None, and functions; `check_untyped_store` refuses anything else at the argument, return, field, or collection write, because a string loses its length and a float its width on the way. A bare annotation on a variable or field is different: its elements infer from the value (`elements_from_value` in the converter). `len()` of an untyped value is refused for the same reason.
- **Keyword arguments are `IRExpr::Keyword`.** The converter keeps them after the positional arguments; `finalize::resolve_keyword_arguments` places them by name for this module's functions and constructors, before the generator and `with` passes (whose expression walkers have wildcard arms). One that reaches codegen is refused, never dropped.
- **Every call to a module function is checked against its signature.** `finalize::place_keywords` runs for every call to this module's functions and constructors, with or without keywords, so a surplus positional argument or a missing one is a TypeError at compile time. Keyword-only parameters are `IRParam::keyword_only`, appended after the positional ones; they are accepted on functions and `__init__`, and refused on other methods and on lambdas. A class with no `__init__` along its chain is not checked.
- **An exception raised inside an expression must know how many blocks the expression opened.** `emit_exception_transfer` (`src/compiler/function.rs`) branches to the enclosing `try` handler by block depth, and an expression that wraps nested emission in its own WASM blocks (the right operand of `and`/`or`, a comprehension's loops and filters, a conditional expression's arms) adds to that depth. Each one records it with `ctx.enter_expr_blocks(n)` / `ctx.exit_expr_blocks(n)` (`ctx.expr_block_depth`), and a new one must too. Forgetting it branches one frame short: a raising comprehension inside a `try` loops forever, and a raising `and` operand produces an invalid module.
- **Conditional expressions are typed by unifying their arms.** `conditional_type` (`src/compiler/expression.rs`) dry-runs each arm with `probe_type` into a scratch function, then takes the common class base, makes `Optional` from a class and None, takes the element type of the non-empty arm for an empty collection, and widens int to float only when float is expected. Arms with no shared type are refused rather than coerced, so `1 if c else 2.5` is an error where nothing expects a float.
- **A None-valued call pushes nothing; a None value pushes 0.** Builtins and user functions declared `-> None` leave nothing on the stack (`finish_user_call` drops the callee's word), so an expression statement drops nothing after them. Where such a call's value is used, emit it with `emit_value`, which pushes the word 0. `x is None` compares words, so it is refused where None and 0 share one (untyped values).
- **An optional number is boxed.** `Optional[int]`, `Optional[bool]`, and `Optional[float]` are a pointer to a heap cell holding the number, with None the null pointer, so None is told apart from 0. `emit_expr` (a wrapper around `emit_expr_inner` in `src/compiler/expression.rs`) does the conversions at the boundary from the expected type: it boxes a number going into an optional slot (`emit_box`) and unboxes an optional going into a plain number slot (`emit_unbox`), which traps on None, since CPython would pass the None on and no catchable error here is its answer. Arithmetic or a comparison on an optional number raises CPython's `TypeError` when it is None (`unbox_operands`). `boxed_scalar` says whether a type is one; anything that reads an optional's word as a number (a store, `str()`, a dict value) has to go through these or it reads an address.
- **A program imports only the WASI functions it uses.** `src/compiler/host.rs` imports from `wasi_snapshot_preview1`, each function on first use (`print` imports `fd_write`, `input` `fd_read`, `sys.argv` the args functions, a raising `__main__` `proc_exit`, `open()` `path_open`, the prestat functions, and `fd_close`), so a program that does none of these imports nothing and must stay that way. `open()` resolves a path against the host's preopens as wasi-libc does and raises CPython's `OSError` subclass, with its POSIX errno and message, for the WASI errno it gets. The `waspy_host` imports (`open`/`read`/`write`/`close`) are the deprecated alternative behind `CompilerOptions::waspy_host_file_io` (carried to codegen as the `FILE_IO_HOST_KEY` metadata); under it a failed open answers -1. A program with a `__main__` block is a WASI command exporting `_start`.
- **Classes are values: `IRType::ClassRef`.** A class name used as a value, `type(x)`, `x.__class__`, and a `type[A]` annotation are a `ClassRef(A)`: the class or a subclass, held as its class id, compared by identity. Its class variables are read by selecting on the id (`emit_class_var_by_tag`), and `str()` gives `<class 'mod.A'>` from the class's module (`CLASS_MODULE_KEY` metadata for a class merged from another file). Calling one held in a variable is refused. `check_class_store` refuses a `ClassRef` stored where an instance is expected, and the reverse.
- **Classes take several bases, along CPython's MRO.** `ClassInfo::mro` is the C3 linearization (`c3_linearization` in `src/compiler/context.rs`; an inconsistent order is CPython's `TypeError`), and inheritance of methods, fields, and class variables (`class_var_owner`) follows it. `is_class_or_subclass` and `common_base` are MRO-based, and `super()` resolves to the next class in the instance's MRO (`super_targets` / `emit_cooperative_super_call`), so a diamond's `__init__` chain runs each class once. Classes of one multiple-inheritance group share field slots, so a field has one offset whichever base reads it.
- **User decorators and metaclasses are specialized at compile time.** There are no function or class objects created at run time, so `ir::decorate` instantiates a decorator's body once per decorated function (the original kept under `DECORATED_FN_PREFIX`, the decorator's variables becoming per-application globals, its other statements run by the module init at the `def`'s position), and `ir::metaclasses` specializes a metaclass's `__new__` / `__init__` / `__call__` per class it makes (`METACLASS_FN_PREFIX`), with the attributes they give the class becoming its class variables (`IRClass::deferred_vars`, defined at `IRClass::position`). A shape either pass cannot specialize statically (a decorator on a method or that is a class, the `namespace` argument, `__prepare__`, calling a metaclass directly) is refused, not approximated.
- **Synthesized functions stay out of the public surface.** `ir::is_internal_function` (`src/ir/decorate.rs`) names the prefixes of functions the passes create (`DRAIN_FN_PREFIX`, `DECORATED_FN_PREFIX`, `GEN_METHOD_PREFIX`, `METACLASS_FN_PREFIX`); exports and function metadata skip them. A pass that synthesizes a function gives it a prefix and adds it there.
- **A `str` is a sequence of code points.** Strings are stored as UTF-8 with a byte length, but `len`, indexing, slicing, `find`, padding, and iteration count characters, as CPython does. A string method is ported from CPython's `unicodeobject.c` twice over one set of tables: `core::unicode` for folding on a literal, and `compiler::unicode` (the tables laid out in memory) for run time, so both give CPython's answer. `src/core/unicode_tables.rs` is generated by `scripts/gen_unicode_tables.py`; regenerate it, never edit it.
- **Runtime helpers are built on demand.** Code that needs a shared routine (float formatting and parsing, string case mapping, file I/O, class names) asks `ctx.helper(Helper::...)` for its function index; once every function is compiled, the module assembler builds each requested helper (`src/compiler/runtime.rs`, which may request others) and appends it. A program that never calls one carries none of it. Prefer a helper over inlining a long sequence at every site.
- **MSRV vs CI mismatch.** `Cargo.toml` declares MSRV 1.70 but CI builds and lints with Rust 1.88. Don't rely on >1.70 features without bumping the declared `rust-version`.
- **clippy must pass with zero warnings** — CI enforces `-D warnings` on `--all-targets --all-features`.
- **`examples/output/`, `examples/*.wasm`, `examples/*.html`, and `plan/` are gitignored.** Don't try to commit generated artifacts or local notes.

---

## Examples

The `examples/` directory holds both Rust **driver programs** and Python **input files**:

**Driver programs (`.rs`):**

- `simple_compiler.rs` — minimal single-source compilation
- `advanced_compiler.rs` — full-featured driver (used by `just compile`/`just optimize`), supports `--metadata` and `--html`
- `multi_file_compiler.rs` — merge multiple files into one module
- `project_compiler.rs` — compile a project directory
- `typed_demo.rs` — type system demonstration
- `verbose_debug_demo.rs` — verbosity/logging demonstration
- `plugin_test.rs`, `html_test.rs` — require the `wasm-plugin` feature

**Python inputs (`.py`):** `typed_demo.py`, `basic_operations.py`, `calculator.py`, `control_flow.py`, `builtins.py`, plus `test_*.py` files exercising each stdlib module's constants (`test_math.py`, `test_re.py`, `test_datetime.py`, `test_os.py`, `test_sys.py`, `test_logging.py`, `stdlib_all_modules.py`, …) and data-structure samples (`set_example.py`, `tuple_example.py`, `range_example.py`, `bytes_example.py`).

Use these for testing. `just examples` runs the full driver suite.
