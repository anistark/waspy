use chakrapy::compile_python_to_wasm;
use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Path to our Python test file (inside examples directory)
    let python_file = Path::new("examples/test_add.py");

    // Read the Python file
    let python_code = fs::read_to_string(python_file)?;

    println!("Compiling Python code:\n{}", python_code);

    let start_time_opt = std::time::Instant::now();
    let optimized_wasm = compile_python_to_wasm(&python_code)?;
    let opt_time = start_time_opt.elapsed();

    // Write the WASM outputs to files
    let opt_path = Path::new("examples/output.wasm");

    fs::write(opt_path, &optimized_wasm)?;

    // Print comparison information
    println!("\n=== Compilation Results ===");

    println!("Output: {}", opt_path.display());
    println!("  - Size: {} bytes", optimized_wasm.len());
    println!("  - Compilation time: {:?}", opt_time);

    Ok(())
}
