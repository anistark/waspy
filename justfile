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
    cargo test

# Format the code
format:
    @echo "Formatting code..."
    @cargo fmt

# Run clippy linter with fix suggestions
lint:
    cargo clippy --all-targets --all-features -- -D warnings

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

# Generate documentation
docs:
    cargo doc --no-deps --open

# Complete development workflow: format, lint, build, and test
dev: format lint build test
    @echo "Development checks completed successfully!"

# Prepare for release: format, lint, build, test, and check if ready to publish
prepare-release: format lint build test check-publish
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
