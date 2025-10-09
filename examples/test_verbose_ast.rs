//! Test AST logging in verbose mode
//!
//! This example demonstrates the AST output logging feature.

use std::fs;
use waspy::{compile_python_to_wasm_with_options, CompilerOptions, Verbosity};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Testing AST logging in verbose mode");
    println!("===================================\n");

    // Simple Python code for testing
    let python_code = r#"
def add(a: int, b: int) -> int:
    return a + b

def multiply(x: int, y: int) -> int:
    return x * y
"#;

    // Compile with verbose mode to see AST output
    let options = CompilerOptions {
        optimize: false,
        debug_info: true,
        generate_html: false,
        verbosity: Verbosity::Verbose,
        ..CompilerOptions::default()
    };

    println!("Compiling with verbose mode enabled...\n");

    let wasm = compile_python_to_wasm_with_options(python_code, &options)?;

    println!("\n✅ Compilation completed successfully!");
    println!("Output size: {} bytes", wasm.len());

    // Write output
    fs::create_dir_all("examples/output")?;
    fs::write("examples/output/test_verbose.wasm", &wasm)?;
    println!("WebAssembly written to examples/output/test_verbose.wasm");

    Ok(())
}
