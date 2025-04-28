use std::env;
use std::fs;
use std::path::Path;
use chakrapy::compile_python_to_wasm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the Python file path from command line arguments
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        println!("Usage: cargo run --example flexible_compiler -- <python_file_path>");
        println!("Example: cargo run --example flexible_compiler -- examples/test_add.py");
        return Ok(());
    }
    
    let python_file = Path::new(&args[1]);
    
    // Check if the file exists
    if !python_file.exists() {
        return Err(format!("File not found: {}", python_file.display()).into());
    }
    
    // Read the Python file
    let python_code = fs::read_to_string(python_file)?;
    
    println!("Compiling Python code from {}:\n{}", python_file.display(), python_code);
    
    // Compile it to WASM
    let wasm_binary = compile_python_to_wasm(&python_code)?;
    
    // Generate output path
    let file_stem = python_file.file_stem().unwrap_or_default();
    let output_path = python_file.with_file_name(format!("{}.wasm", file_stem.to_string_lossy()));
    
    fs::write(&output_path, &wasm_binary)?;
    
    println!("Successfully compiled Python to WebAssembly!");
    println!("Output written to {}", output_path.display());
    
    // Print the size of the generated WASM file
    let wasm_size = fs::metadata(&output_path)?.len();
    println!("WASM file size: {} bytes", wasm_size);
    
    Ok(())
}