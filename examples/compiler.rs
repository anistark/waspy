use std::fs;
use std::path::Path;
use chakrapy::compile_python_to_wasm;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Path to our Python test file (inside examples directory)
    let python_file = Path::new("examples/test_add.py");
    
    // Read the Python file
    let python_code = fs::read_to_string(python_file)?;
    
    println!("Compiling Python code:\n{}", python_code);
    
    // Compile it to WASM
    let wasm_binary = compile_python_to_wasm(&python_code)?;
    
    // Write the WASM output to a file
    let output_path = Path::new("examples/output.wasm");
    fs::write(output_path, &wasm_binary)?;
    
    println!("Successfully compiled Python to WebAssembly!");
    println!("Output written to {}", output_path.display());
    
    // Print the size of the generated WASM file
    let wasm_size = fs::metadata(output_path)?.len();
    println!("WASM file size: {} bytes", wasm_size);
    
    Ok(())
}