# ChakraPy Justfile
# Install just with: cargo install just

# Default recipe to run when just is called without arguments
default:
    @just --list

# Build the project
build:
    cargo build --release

# Build examples
build-examples:
    cargo build --examples

# Run all examples
examples:
    cargo run --example compiler
    cargo run --example flexible_compiler -- examples/test_add.py
    cargo run --example flexible_compiler -- examples/test_sub.py
    cargo run --example flexible_compiler -- examples/test_mul.py

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

# Publish the crate to crates.io
publish:
    cargo publish

# Clean the project
clean:
    cargo clean

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
    cargo run --example flexible_compiler -- {{file}}

# Compile a specific Python file and show size optimization
optimize file:
    @echo "Compiling {{file}} with optimization..."
    @cargo run --example flexible_compiler -- {{file}}
