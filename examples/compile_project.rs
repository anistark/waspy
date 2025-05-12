use chakrapy::compile_python_project;
use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;

/// Example of compiling a Python project to WebAssembly
///
/// This example demonstrates how to use the project compilation feature
/// to compile an entire Python project with multiple files and imports.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command-line arguments
    let args: Vec<String> = env::args().collect();

    let (project_path, output_path) = if args.len() >= 3 {
        // Use command-line arguments
        (Path::new(&args[1]), Path::new(&args[2]))
    } else {
        // Default to example project
        (
            Path::new("examples/calculator_project"),
            Path::new("examples/calculator_project.wasm"),
        )
    };

    println!("ChakraPy Project Compilation Example");
    println!("Project directory: {}", project_path.display());
    println!("Output file: {}", output_path.display());

    // Start timing
    let start_time = Instant::now();

    // Check if project directory exists
    if !project_path.exists() || !project_path.is_dir() {
        eprintln!(
            "Error: The project directory '{}' does not exist or is not a directory",
            project_path.display()
        );
        return Ok(());
    }

    // Compile the Python project to WebAssembly
    println!("Compiling project...");
    let wasm_binary = compile_python_project(project_path, true)?;

    // Report compilation time
    let duration = start_time.elapsed();
    println!("Compilation completed in {:.2?}", duration);

    // Create parent directories if needed
    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    // Write the WebAssembly to a file
    fs::write(output_path, &wasm_binary)?;
    println!("WebAssembly binary size: {} bytes", wasm_binary.len());
    println!(
        "Successfully wrote WebAssembly to {}",
        output_path.display()
    );

    // Print the project structure
    println!("\nProject structure:");
    print_dir_structure(project_path, 0)?;

    println!("\nCompilation successful!");
    Ok(())
}

/// Recursively print the directory structure for better visualization
fn print_dir_structure(dir: &Path, indent: usize) -> Result<(), Box<dyn std::error::Error>> {
    let prefix = "  ".repeat(indent);

    println!(
        "{}📂 {}",
        prefix,
        dir.file_name().unwrap_or_default().to_string_lossy()
    );

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Skip __pycache__ and hidden directories
            let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
            if !dir_name.starts_with("__pycache__") && !dir_name.starts_with('.') {
                print_dir_structure(&path, indent + 1)?;
            }
        } else if path.is_file() && path.extension().map_or(false, |ext| ext == "py") {
            println!(
                "{}📄 {}",
                "  ".repeat(indent + 1),
                path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }

    Ok(())
}
