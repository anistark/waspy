//! Simple Waspy Compiler Example
//!
//! This example demonstrates basic usage of Waspy to compile
//! a single Python file to WebAssembly.

use std::fs;
use std::path::Path;
use std::time::Instant;
use waspy::compile_python_to_wasm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Waspy Simple Compiler Example");
    println!("=============================\n");

    // Read the Python source file
    let python_file = Path::new("examples/basic_operations.py");
    let python_code = fs::read_to_string(python_file)?;

    // Display the Python code
    println!("Compiling Python code:");
    println!("---------------------");
    println!("{python_code}");
    println!("---------------------\n");

    // Compile with timing
    println!("Compiling to WebAssembly...");
    let start_time = Instant::now();
    let wasm = compile_python_to_wasm(&python_code)?;
    let compile_time = start_time.elapsed();

    // Write the WebAssembly output to a file
    let output_path = Path::new("examples/output/basic_operations.wasm");

    // Create directory if it doesn't exist
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(output_path, &wasm)?;

    // Print results
    println!("\n✅ Compilation Results");
    println!("---------------------");
    println!("Output file: {}", output_path.display());
    println!("Output size: {} bytes", wasm.len());
    println!("Compilation time: {compile_time:?}");

    println!("\nThe WebAssembly module contains these functions:");
    println!("• add(a: int, b: int) -> int");
    println!("• subtract(a: int, b: int) -> int");
    println!("• multiply(a: int, b: int) -> int");
    println!("• divide(a: int, b: int) -> int");
    println!("• modulo(a: int, b: int) -> int");
    println!("• combined_operation(a: int, b: int) -> int");

    println!("\nYou can run this WebAssembly in a browser or with a WebAssembly runtime.");
    Ok(())
}
