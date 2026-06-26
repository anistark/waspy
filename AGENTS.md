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
        ↓   core/parser.rs        — rustpython-parser → Python AST
[Python AST]
        ↓   ir/converter.rs       — lower_ast_to_ir() → IRModule
[IR Module]
        ↓   ir/decorators.rs      — DecoratorRegistry::apply_decorators()
        ↓   ir/entry_points.rs    — detect_entry_points() / add_entry_point_to_module()
[IR Module + entry points]
        ↓   compiler/module.rs     — compile_ir_module() → wasm-encoder → raw WASM
[Raw WASM binary]
        ↓   optimize/wasm.rs       — optimize_wasm() (Binaryen), only if options.optimize
[Optimized WASM binary]
```

### The Five Stages

| Stage | Module | Responsibility | Key entry point |
|-------|--------|----------------|-----------------|
| **1. Parse** | `src/core/parser.rs` | Python text → AST via `rustpython-parser` | `parse_python()` |
| **2. Lower (IR)** | `src/ir/converter.rs` | AST → typed `IRModule` (functions, classes, variables, imports, memory layout) | `lower_ast_to_ir()` |
| **3. Transform** | `src/ir/decorators.rs`, `src/ir/entry_points.rs` | Apply decorators; detect/inject `__main__` entry points | `DecoratorRegistry`, `detect_entry_points()` |
| **4. Codegen** | `src/compiler/` | IR → WASM bytecode via `wasm-encoder` | `compile_ir_module()` |
| **5. Optimize** | `src/optimize/wasm.rs` | Shrink/optimize WASM via Binaryen | `optimize_wasm()` |

### Critical Rules

1. **Respect stage boundaries.** The parser knows nothing about WASM. The IR knows nothing about `wasm-encoder`. Codegen consumes IR and never re-parses Python. Don't reach across stages or smuggle Python AST types into codegen.

2. **`IRType` is the type-system contract** (`src/ir/types.rs`). It is the single source of truth for types across IR and codegen. When you add a Python type, add the `IRType` variant first, then handle it in the converter, the compiler, and `type_to_string()` in `src/lib.rs` — all of them, or the build breaks.

3. **Memory layout is computed in the IR, consumed in codegen.** `MemoryLayout` (on `IRModule`) holds string/bytes offsets and the object heap layout. When merging modules (multi-file compilation), layouts are merged via `memory_layout.merge_from()`. Don't hardcode memory offsets in codegen — read them from the layout.

4. **Codegen must emit valid WASM.** The compiler reserves `SCRATCH_LOCALS` (`src/compiler/context.rs`) temporary locals per function for bookkeeping. If you emit instructions that use scratch locals beyond that budget, raise the constant. All memory access must be bounds-aware and use the layout offsets.

5. **Optimization is optional and last.** `optimize_wasm()` (Binaryen) runs only when `CompilerOptions::optimize` is set. Never make correctness depend on the optimizer — the unoptimized binary must already be valid and correct.

6. **stdlib modules are codegen shims, not a runtime.** `src/stdlib/` provides compile-time support for a fixed allowlist of Python modules (`is_stdlib_module()`). There is no Python interpreter at runtime — each supported module emits inline WASM. Adding a module means teaching the compiler to generate code for it.

---

## After Every Set of Changes

After completing any set of changes, **always** run these in order:

1. **`just format`** — Format all Rust code (`cargo fmt --all`).
2. **`just lint`** — Run clippy with zero-warnings enforcement (`-D warnings`).
3. **`just test`** — Run the full test suite (`cargo test --all-features`).

For a full local CI-equivalent check:

4. **`just ci`** — Runs format-check → lint → test (mirrors GitHub Actions).

Or run the complete dev workflow in one shot:

5. **`just dev`** — format → format-check → lint → build → test.

Do not consider a change complete until these pass cleanly.

### Additional Housekeeping

- **Update `CHANGELOG.md`** for any user-facing change (new feature, fix, breaking change).
- **Update `README.md`** when supported features, limitations, or the public API change — the README's "Current Features" / "Limitations" lists are user-facing.
- **Update `docs/` on every major change or feature update.** The `docs/` directory is the user-facing static HTML site (`index.html` landing page, `modules.html`, `changelog.html`, plus `assets/`). Whenever you add or change a feature, supported type, stdlib module, or any user-visible behaviour, reflect it in the relevant `docs/` page — keep `modules.html` in sync with what the compiler actually supports and `changelog.html` in sync with `CHANGELOG.md`. Don't let the published docs drift behind the code.
- **Prefer `just` commands** over raw `cargo` commands. The justfile handles sequencing, examples, and release steps correctly.
- **Prompt the user if `AGENTS.md` needs updating.** If your changes alter the pipeline, module layout, public API, supported types/stdlib modules, or conventions, tell the user: *"This change may require an update to AGENTS.md — would you like me to update it?"*

### Git Discipline

- **Do not run `git add` or `git commit` unless the user explicitly asks.** Stage and commit only on direct request.
- **When the user asks to commit**, review all staged/unstaged changes and prepare:
  - A **brief title** following conventional commits (`feat:`, `fix:`, `chore:`, etc.).
  - A **detailed description** summarizing what changed and why.
- **`plan/` is gitignored** — it's a local-only, internal directory for working and roadmap notes (each contributor may keep their own). Never try to commit it.

---

## Architecture

Waspy is a single Rust library crate (`crate-type = ["cdylib", "rlib"]`) with a static documentation site:

| Component | Language | Location | Purpose |
|-----------|----------|----------|---------|
| **Compiler library** | Rust | `src/` | Parser, IR, codegen, optimizer, stdlib shims |
| **Wasmrun plugin** | Rust | `src/wasmrun.rs` | Optional `wasm-plugin` feature — integrates Waspy into wasmrun |
| **Examples** | Rust + Python | `examples/` | Runnable compiler drivers (`.rs`) + Python test inputs (`.py`) |
| **Documentation** | Static HTML | `docs/` | Landing page + module/changelog pages (GitHub Pages) |

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
│   └── errors.rs           #   ChakraError enum (legacy name — see Gotchas), ErrorLocation
├── ir/                     # [Stage 2-3] Intermediate representation
│   ├── types.rs            #   ★ IRModule, IRFunction, IRStatement, IRExpr, IRType, MemoryLayout
│   ├── converter.rs        #   lower_ast_to_ir() — AST → IR
│   ├── decorators.rs       #   DecoratorRegistry — decorator application
│   └── entry_points.rs     #   detect_entry_points(), add_entry_point_to_module()
├── compiler/               # [Stage 4] IR → WASM (wasm-encoder)
│   ├── module.rs           #   compile_ir_module() — top-level codegen entry
│   ├── function.rs         #   Per-function codegen
│   ├── expression.rs       #   Expression codegen
│   └── context.rs          #   CompilationContext, SCRATCH_LOCALS, LocalInfo/FunctionInfo/ClassInfo
├── optimize/               # [Stage 5] WASM optimization
│   └── wasm.rs             #   optimize_wasm() — Binaryen
├── stdlib/                 # Compile-time stdlib shims (codegen, not a runtime)
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
- **Optional plugin via feature flag.** `wasmrun.rs` is gated behind `#[cfg(feature = "wasm-plugin")]`. Plugin metadata lives under `[package.metadata.wasm_plugin]` in `Cargo.toml`. The `wasmrun_plugin_create()` FFI symbol is the entry point wasmrun loads.

---

## Tech Stack & Tooling

| Tool / Crate | Purpose | Notes |
|------|---------|-------|
| **Rust** | Compiler library | Edition 2021, MSRV 1.70 (CI uses 1.88) |
| **Cargo** | Build system | `cargo build --release` |
| **Just** | Task runner | `justfile` — run `just` for available commands |
| **rustpython-parser** | Python parsing | Produces the Python AST (Stage 1) |
| **wasm-encoder** | WASM codegen | Emits the binary (Stage 4) |
| **binaryen** | WASM optimization | Native dependency; needs a C/C++ toolchain to build |
| **anyhow** | Error context | Public API return type |
| **thiserror** | Structured errors | `ChakraError` in `src/core/errors.rs`, `CompilationError` in plugin |
| **chrono** | Date/time | Backs the `datetime` stdlib shim |
| **regex** | Regex | Backs the `re` stdlib shim |
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
just ci              # format-check → lint → test (mirrors GitHub Actions)
just dev             # format → format-check → lint → build → test
just docs            # cargo doc --all-features --no-deps --open
just docs-check      # rustdoc with -D warnings
just clean           # cargo clean + remove examples/output
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

- **Unit tests** live inline as `#[cfg(test)]` modules alongside source (currently in `src/stdlib/re.rs` and `src/utils/logging.rs`). Add new tests next to the code they cover.
- **`tests/`** contains the test-layout scaffolding (`fixtures/`, `integration/`, `unit/`, `utils/`) for future integration tests.
- **Examples double as functional tests.** `just examples` exercises the full pipeline end-to-end against the Python files in `examples/`.
- Always run `cargo test --all-features` before committing.
- CI runs tests on `stable` and enforces zero clippy warnings on Rust 1.88: `cargo clippy --all-targets --all-features -- -D warnings`.

---

## Code Conventions

### Rust

- Standard Rust naming: `snake_case` for functions/variables, `PascalCase` for types.
- Public APIs get rustdoc comments (`///`) documenting Arguments, Returns, and Errors — match the existing style in `src/lib.rs`.
- The public API returns `anyhow::Result` with `.context("...")`. Structured compiler errors use `ChakraError` (`thiserror`) in `src/core/errors.rs` — add new variants there, include `ErrorLocation` where possible.
- Use the logging macros (`log_debug!`, `log_verbose!`, `log_info!`, `log_warn!`) for diagnostic output — never bare `println!` in library code (the metadata path is the only existing exception).
- Keep `#[allow(dead_code)]` annotated with a `// TODO:` comment explaining the plan (see `src/compiler/context.rs`).
- All memory operations in codegen must use offsets from `MemoryLayout` and stay within the scratch-local budget (`SCRATCH_LOCALS`).

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

---

## Important Files to Know

| File | Why It Matters |
|------|----------------|
| `Cargo.toml` | Version, dependencies, features, plugin metadata — start here |
| `src/lib.rs` | The entire public API and the pipeline orchestration |
| `src/ir/types.rs` | `IRType`, `IRModule`, `MemoryLayout` — the IR contract |
| `src/ir/converter.rs` | AST → IR lowering |
| `src/compiler/module.rs` | Top-level WASM codegen entry (`compile_ir_module`) |
| `src/compiler/context.rs` | `CompilationContext`, `SCRATCH_LOCALS` budget |
| `src/optimize/wasm.rs` | Binaryen optimization pass |
| `src/core/parser.rs` | Python parsing (rustpython) |
| `src/core/options.rs` | `CompilerOptions`, `Verbosity` |
| `src/core/errors.rs` | `ChakraError` (structured errors) |
| `src/stdlib/mod.rs` | stdlib module allowlist + attribute dispatch |
| `src/wasmrun.rs` | Wasmrun plugin integration (`wasm-plugin` feature) |
| `justfile` | All development task commands |
| `CHANGELOG.md` | Keep updated with every change |
| `CONTRIBUTING.md` | Contributor workflow and standards |

---

## Common Tasks for Agents

### Adding support for a new Python language feature

1. Confirm `rustpython-parser` produces the AST node you need (Stage 1 — usually no change here).
2. Lower it in `src/ir/converter.rs` into the appropriate `IRStatement`/`IRExpr` (add a variant in `src/ir/types.rs` if needed).
3. Implement codegen in `src/compiler/` (`expression.rs` / `function.rs`), using `CompilationContext` and `MemoryLayout`.
4. If it introduces a new type, add an `IRType` variant and handle it in the converter, the compiler, **and** `type_to_string()` in `src/lib.rs`.
5. Add an example `.py` under `examples/` and verify with `just compile examples/<file>.py`.
6. Add tests; run `just dev`.
7. Update README features/limitations and `CHANGELOG.md`.

### Adding a new IR type

1. Add the variant to `IRType` in `src/ir/types.rs`.
2. Produce it in `src/ir/converter.rs` (type inference / annotations).
3. Handle it in codegen (`src/compiler/`) — value representation, memory layout, WASM value type mapping.
4. Add a `type_to_string()` arm in `src/lib.rs` (the compiler won't build without it — the match is exhaustive).

### Adding a new stdlib module

1. Create `src/stdlib/<module>.rs` exposing `get_attribute()` (and any codegen helpers) following the existing modules.
2. Register it in `src/stdlib/mod.rs`: add to `is_stdlib_module()` and the `get_stdlib_attributes()` dispatch.
3. Wire the actual WASM emission for the module's functions into the compiler.
4. Add an example `.py` exercising it and a test.
5. Remember: these are **compile-time codegen shims**, not a runtime — each call must lower to inline WASM.

### Adding a compiler option

1. Add the field to `CompilerOptions` in `src/core/options.rs` (and update `Default`).
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
- **Binaryen is a native dependency.** The `binaryen` crate needs a C/C++ toolchain to build. Optimization failures may stem from the build environment, not your code.
- **Optimization is opt-in and must not affect correctness.** If a binary is only valid after `optimize_wasm()`, the codegen has a bug.
- **`SCRATCH_LOCALS` is a hard budget.** Codegen that overruns the reserved temporary locals will alias real variables and silently corrupt output. Raise the constant in `src/compiler/context.rs` if you need more.
- **Multi-file compilation de-duplicates by function name.** Duplicate function names across files are dropped with a warning (first one wins). Memory layouts are merged via `merge_from()` — don't assume single-file offset assumptions hold.
- **stdlib support is an allowlist.** Only modules in `is_stdlib_module()` are recognized; everything else is treated as user code or an unsupported import.
- **MSRV vs CI mismatch.** `Cargo.toml` declares MSRV 1.70 but CI builds and lints with Rust 1.88. Don't rely on >1.70 features without bumping the declared `rust-version`.
- **clippy must pass with zero warnings** — CI enforces `-D warnings` on `--all-targets --all-features`.
- **`examples/output/`, `examples/*.wasm`, `examples/*.html`, and `plan/` are gitignored.** Don't try to commit generated artifacts or the local planning directory.

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

**Python inputs (`.py`):** `typed_demo.py`, `basic_operations.py`, `calculator.py`, `control_flow.py`, `builtins.py`, plus `test_*.py` files exercising each stdlib module (`test_math.py`, `test_json.py`, `test_re.py`, `test_datetime.py`, `test_os.py`, `test_sys.py`, `test_logging.py`, `stdlib_all_modules.py`, …) and data-structure samples (`set_example.py`, `tuple_example.py`, `range_example.py`, `bytes_example.py`).

Use these for testing. `just examples` runs the full driver suite.
