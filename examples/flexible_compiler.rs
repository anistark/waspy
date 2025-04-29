use std::env;
use std::fs;
use std::path::Path;
use chakrapy::compile_python_to_wasm_with_options;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the Python file path from command line arguments
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        println!("Usage: cargo run --example flexible_compiler -- <python_file_path>");
        println!("Example: cargo run --example flexible_compiler -- examples/test_add.py");
        println!("The compiler will generate both optimized and unoptimized versions for comparison");
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
    
    // Generate both optimized and unoptimized versions to compare
    println!("\n=== Compiling without optimization ===");
    let start_time_unopt = std::time::Instant::now();
    let unoptimized_wasm = compile_python_to_wasm_with_options(&python_code, false)?;
    let unopt_time = start_time_unopt.elapsed();
    
    println!("\n=== Compiling with optimization ===");
    let start_time_opt = std::time::Instant::now();
    let optimized_wasm = compile_python_to_wasm_with_options(&python_code, true)?;
    let opt_time = start_time_opt.elapsed();
    
    // Generate output paths
    let file_stem = python_file.file_stem().unwrap_or_default();
    let unopt_path = python_file.with_file_name(format!("{}.raw.wasm", file_stem.to_string_lossy()));
    let opt_path = python_file.with_file_name(format!("{}.opt.wasm", file_stem.to_string_lossy()));
    
    // Write both versions
    fs::write(&unopt_path, &unoptimized_wasm)?;
    fs::write(&opt_path, &optimized_wasm)?;
    
    // Print comparison information
    println!("\n=== Compilation Results ===");
    println!("Unoptimized output: {}", unopt_path.display());
    println!("  - Size: {} bytes", unoptimized_wasm.len());
    println!("  - Compilation time: {:?}", unopt_time);
    
    println!("Optimized output: {}", opt_path.display());
    println!("  - Size: {} bytes", optimized_wasm.len());
    println!("  - Compilation time: {:?}", opt_time);
    
    let size_reduction = 100.0 * (1.0 - (optimized_wasm.len() as f64 / unoptimized_wasm.len() as f64));
    println!("Size reduction: {:.2}%", size_reduction);
    
    Ok(())
}