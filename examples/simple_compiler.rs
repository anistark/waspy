use chakrapy::compile_python_to_wasm;
use std::fs;
use std::path::Path;
use std::time::Instant;

/// Simple compiler example
///
/// This example demonstrates the basic use of ChakraPy to compile
/// a Python file to WebAssembly.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let python_file = Path::new("examples/basic_operations.py");

    // Read the Python file
    let python_code = fs::read_to_string(python_file)?;

    println!("Compiling Python code:");
    println!("----------------------");
    println!("{}", python_code);
    println!("----------------------");

    // Record compilation time
    let start_time = Instant::now();

    // Compile the Python code to optimized WebAssembly
    let wasm = compile_python_to_wasm(&python_code)?;

    let compile_time = start_time.elapsed();

    // Write the WASM output to a file
    let output_path = Path::new("examples/basic_operations.wasm");
    fs::write(output_path, &wasm)?;

    // Print results information
    println!("\n=== Compilation Results ===");
    println!("Output file: {}", output_path.display());
    println!("Output size: {} bytes", wasm.len());
    println!("Compilation time: {:?}", compile_time);

    println!("\nSuccessfully compiled the Python code to WebAssembly.");
    println!("The WebAssembly module contains these exported functions:");
    println!("  - add(a: i32, b: i32) -> i32");
    println!("  - subtract(a: i32, b: i32) -> i32");
    println!("  - multiply(a: i32, b: i32) -> i32");
    println!("  - divide(a: i32, b: i32) -> i32");
    println!("  - modulo(a: i32, b: i32) -> i32");
    println!("  - combined_operation(a: i32, b: i32) -> i32");

    Ok(())
}
