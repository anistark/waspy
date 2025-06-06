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
    cargo run --example advanced_compiler examples/basic_operations.py
    cargo run --example advanced_compiler examples/control_flow.py --metadata
    cargo run --example advanced_compiler examples/typed_demo.py --html
    
    @echo "\nRunning multi-file compiler example..."
    cargo run --example multi_file_compiler examples/combined.wasm examples/basic_operations.py examples/calculator.py
    
    @echo "\nRunning project compilation example..."
    cargo run --example compile_project examples/calculator_project
    
    @echo "\nRunning module-level code example..."
    cargo run --example compile_module_level

# Run tests
test:
    cargo test

# Format the code
format:
    cargo fmt --all

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
    rm examples/*.wasm || true
    rm examples/*.html || true

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
    cargo run --example advanced_compiler {{file}}

# Compile a specific Python file and show size optimization
optimize file:
    @echo "Compiling {{file}} with optimization..."
    @cargo run --example advanced_compiler {{file}} --metadata

# Compile multiple Python files to a single WebAssembly module
compile-multi output file1 file2:
    cargo run --example multi_file_compiler {{output}} {{file1}} {{file2}}

# Compile a Python project directory to WebAssembly
compile-project dir output="project.wasm":
    @echo "Compiling project {{dir}} to {{output}}..."
    @cargo run --example compile_project {{dir}} {{output}}

# Run project metadata analysis without compilation
project-info dir:
    @echo "Analyzing project {{dir}}..."
    @cargo run --example project_metadata {{dir}}

# Extract functions from Python files
extract-functions dir output="extracted_functions.py":
    @echo "Extracting functions from Python files in {{dir}}..."
    @cargo run --example extract_functions {{dir}} {{output}}

# Compile extracted functions to WebAssembly
compile-extracted file output="extracted_functions.wasm":
    @echo "Compiling extracted functions from {{file}}..."
    @cargo run --example advanced_compiler {{file}} {{output}} --html

# Compile module-level code example
compile-module-level:
    @echo "Compiling module-level code example..."
    @cargo run --example compile_module_level
