# Waspy Justfile
# Install just with: cargo install just

# Get version from Cargo.toml
version := `grep -m 1 'version = ' Cargo.toml | cut -d '"' -f 2`

# Repository information
repo := `if git remote -v >/dev/null 2>&1; then git remote get-url origin | sed -E 's/.*github.com[:/]([^/]+)\/([^/.]+).*/\1\/\2/'; else echo "anistark/waspy"; fi`

# Default recipe to display help information
default:
    @just --list
    @echo "\nCurrent version: {{version}}"

# Build the project
build:
    cargo build --release

# Build examples
build-examples:
    cargo build --examples

# Run all examples
examples:
    @echo "Running simple compiler example..."
    cargo run --example simple_compiler
    
    @echo "\nRunning advanced compiler examples..."
    cargo run --example advanced_compiler examples/typed_demo.py
    cargo run --example advanced_compiler examples/typed_demo.py --metadata
    cargo run --example advanced_compiler examples/typed_demo.py --html
    
    @echo "\nRunning multi-file compiler example..."
    cargo run --example multi_file_compiler examples/output/combined.wasm examples/basic_operations.py examples/calculator.py
    
    @echo "\nRunning project compiler example..."
    cargo run --example project_compiler examples/calculator_project examples/output/project.wasm
    
    @echo "\nRunning type system demo..."
    cargo run --example typed_demo

# Run tests
test:
    cargo test --all-features

# Format the code
format:
    @echo "Formatting code..."
    @cargo fmt --all

# Check formatting without making changes
format-check:
    @echo "Checking code formatting..."
    @cargo fmt --all -- --check

# Run clippy linter
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Build the docs-site playground: waspy compiled to wasm32 (without the
# Binaryen feature) plus its wasm-bindgen glue, into docs/playground/compiler/.
# Needs `rustup target add wasm32-unknown-unknown` and the wasm-bindgen CLI at
# the version pinned in playground/Cargo.toml.
playground:
    #!/usr/bin/env bash
    set -euo pipefail
    want=$(sed -nE 's/^wasm-bindgen = "=([0-9.]+)"/\1/p' playground/Cargo.toml)
    have=$(wasm-bindgen --version 2>/dev/null | cut -d ' ' -f 2 || true)
    if [ "$have" != "$want" ]; then
      echo "wasm-bindgen CLI $want is required (found: ${have:-none})."
      echo "Install it with: cargo install wasm-bindgen-cli --version $want --locked"
      exit 1
    fi
    cargo build --release --locked --target wasm32-unknown-unknown --manifest-path playground/Cargo.toml
    rm -rf docs/playground/compiler
    wasm-bindgen --target web --no-typescript --out-dir docs/playground/compiler --out-name waspy \
        playground/target/wasm32-unknown-unknown/release/waspy_playground.wasm
    ls -lh docs/playground/compiler

# Build the playground and serve the docs site at http://localhost:8000/playground/
playground-serve port="8000": playground
    python3 -m http.server {{port}} --directory docs

# Compile every playground example with the built bundle and check the
# answers against CPython
playground-verify:
    node scripts/verify_playground.mjs

# Lint the playground crate (it builds for wasm32 only)
playground-lint:
    cargo fmt --manifest-path playground/Cargo.toml -- --check
    cargo clippy --locked --target wasm32-unknown-unknown --manifest-path playground/Cargo.toml -- -D warnings

# Fix lint issues automatically where possible
lint-fix:
    cargo clippy --all-targets --all-features --fix -- -D warnings

# Check if the crate is ready for publishing
check-publish:
    cargo publish --dry-run

# Create a GitHub release with an optional custom title
github-release title="":
    #!/usr/bin/env bash
    set -euo pipefail
    
    VERSION="{{version}}"
    # Use provided title or default if none provided
    RELEASE_TITLE="${title:-v$VERSION}"
    
    echo "Creating GitHub release for v$VERSION..."
    git tag -a "v$VERSION" -m "Release v$VERSION"
    git push origin "v$VERSION"
    
    if command -v gh >/dev/null 2>&1; then
      echo "Creating GitHub release using the GitHub CLI..."
      gh release create "v$VERSION"
    else
      ENCODED_TITLE=$(echo "$RELEASE_TITLE" | sed 's/ /%20/g')
      echo "GitHub CLI not found. Please install it or create the release manually at:"
      echo "https://github.com/{{repo}}/releases/new?tag=v$VERSION&title=$ENCODED_TITLE"
    fi

# Publish the crate to crates.io
publish: prepare-release
    cargo publish
    @just github-release

# Clean the project
clean:
    cargo clean
    rm -rf examples/output || true

# Deep clean: build artifacts plus every generated example output (.wasm/.html)
clean-all: clean
    #!/usr/bin/env bash
    set -euo pipefail
    find examples -name "*.wasm" -delete
    find examples -name "*.html" -delete
    echo "Removed generated example artifacts."

# Compile every bundled example through the real driver (multi-file examples
# via their entry file), then run the end-to-end programs under Node and
# wasmtime. Fails on the first broken one.
verify-examples: verify-runtime
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p examples/output
    cargo build --quiet --example advanced_compiler
    for f in examples/*.py; do
      case "$(basename "$f")" in
        calculator.py) continue;;  # multi-file only: needs basic_operations.py (covered below)
      esac
      echo "== $f"
      cargo run --quiet --example advanced_compiler "$f" >/dev/null
    done
    echo "== examples/user_modules_app/main.py"
    cargo run --quiet --example advanced_compiler examples/user_modules_app/main.py >/dev/null
    echo "== examples/library_project/main.py"
    cargo run --quiet --example advanced_compiler examples/library_project/main.py >/dev/null
    echo "== examples/basic_operations.py + examples/calculator.py (multi-file)"
    cargo run --quiet --example multi_file_compiler examples/output/verify_combined.wasm \
        examples/basic_operations.py examples/calculator.py >/dev/null
    echo "== examples/calculator_project (project directory)"
    cargo run --quiet --example project_compiler examples/calculator_project \
        examples/output/verify_project.wasm >/dev/null
    echo ""
    echo "All examples compiled successfully."

# Run the end-to-end programs under real runtimes, not only the test harness.
# The integration suite asserts the same results with wasmi in-process; this
# asserts them under the engines a user ships against. Needs `node` and
# `wasmtime` on PATH.
verify-runtime:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "== building the end-to-end programs with their runtime checks"
    cargo run --quiet --example verify_runtime
    echo ""
    echo "== node"
    node scripts/verify_runtime_node.mjs
    echo ""
    echo "== wasmtime"
    python3 scripts/verify_runtime_wasmtime.py
    echo ""
    echo "Every end-to-end program answers correctly under both runtimes."

# Compile-time and module-size baseline for the end-to-end programs, median
# of nine release-build runs each, optimized and unoptimized. Prints a table
# ready to paste into CHANGELOG.md. Report the machine alongside the numbers:
# they are wall clock, so they only mean anything next to the hardware.
benchmark:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --quiet --release --example benchmark
    echo "machine: $(uname -sm), $(sysctl -n machdep.cpu.brand_string 2>/dev/null || \
        grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | xargs || echo unknown)"
    echo ""
    ./target/release/examples/benchmark

# Serve the docs website locally (set host to 0.0.0.0 or a tailscale IP to share over the network)
docs port="8000" host="0.0.0.0":
    python3 -m http.server {{port}} --bind {{host}} --directory docs

# Generate rustdoc API documentation and open in browser
docs-rs:
    cargo doc --all-features --no-deps --open

# Check documentation build
docs-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --all-features --no-deps

# Complete development workflow: format, lint, build, and test
dev: format format-check lint build test
    @echo "Development checks completed successfully!"

# CI check - runs exactly what CI runs (format-check, lint, test)
ci: format-check lint test
    @echo "CI checks completed successfully!"

# Prepare for release: format, lint, build, test, and check if ready to publish
prepare-release: format format-check lint build test check-publish
    @echo "Release preparation completed successfully!"

# Compile a specific Python file to WebAssembly
compile file:
    @mkdir -p examples/output
    cargo run --example advanced_compiler {{file}}

# Compile a specific Python file and show size optimization
optimize file:
    @mkdir -p examples/output
    @echo "Compiling {{file}} with optimization..."
    @cargo run --example advanced_compiler {{file}} --metadata

# Compile multiple Python files to a single WebAssembly module
compile-multi output file1 file2:
    @mkdir -p examples/output
    cargo run --example multi_file_compiler {{output}} {{file1}} {{file2}}

# Compile a Python project directory to WebAssembly
compile-project dir output="examples/output/project.wasm":
    @mkdir -p examples/output
    @echo "Compiling project {{dir}} to {{output}}..."
    @cargo run --example project_compiler {{dir}} {{output}}

# Run the type system demo
run-typed-demo:
    @mkdir -p examples/output
    @echo "Running type system demo..."
    @cargo run --example typed_demo

# Create necessary directory structure for a new example
setup-example name:
    @mkdir -p examples/output
    @echo "Setting up a new example: {{name}}"
    @touch examples/{{name}}.rs
    @touch examples/{{name}}.py
    @echo "Created example files:\n- examples/{{name}}.rs\n- examples/{{name}}.py"
    @echo "Don't forget to update the justfile and README.md with the new example!"
