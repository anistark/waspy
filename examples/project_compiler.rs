//! Python Project Waspy Compiler
//!
//! This example demonstrates how to compile an entire Python project
//! with multiple files and dependencies into a single WebAssembly module.

use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;
use waspy::{compile_python_project_with_options, CompilerOptions};

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
            Path::new("examples/output/calculator_project.wasm"),
        )
    };

    println!("Waspy Project Compiler");
    println!("=====================\n");
    println!("Project directory: {}", project_path.display());
    println!("Output file: {}", output_path.display());

    // Set compiler options
    let options = CompilerOptions {
        optimize: true,
        debug_info: true,
        generate_html: true,
        include_metadata: true,
        ..CompilerOptions::default()
    };

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
    println!("\nAnalyzing project structure...");
    println!("Compiling project files...");
    let wasm_binary = compile_python_project_with_options(project_path, &options)?;

    // Report compilation time
    let duration = start_time.elapsed();
    println!("\n✅ Compilation completed in {duration:.2?}");

    // Create parent directories if needed
    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    // Write the WebAssembly to a file
    fs::write(output_path, &wasm_binary)?;
    println!("WebAssembly binary size: {} bytes", wasm_binary.len());
    println!("Output file: {}", output_path.display());

    // Generate HTML test file if requested
    if options.generate_html {
        let html_file = output_path.with_extension("html");
        let wasm_name = output_path.file_name().unwrap().to_str().unwrap();
        let html = generate_html_test_file(wasm_name);
        fs::write(&html_file, html)?;
        println!("HTML test file: {}", html_file.display());
    }

    // Print the project structure
    println!("\nProject structure:");
    print_dir_structure(project_path, 0)?;

    println!("\nProject compilation successful!");
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
        } else if path.is_file() && path.extension().is_some_and(|ext| ext == "py") {
            println!(
                "{}📄 {}",
                "  ".repeat(indent + 1),
                path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    }

    Ok(())
}

fn generate_html_test_file(wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Waspy Project Test</title>
    <style>
        body {{ font-family: system-ui, sans-serif; margin: 0; padding: 20px; line-height: 1.5; max-width: 800px; margin: 0 auto; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; border-radius: 4px; font-family: monospace; white-space: pre-wrap; }}
        .function-test {{ margin-bottom: 20px; }}
        h2 {{ margin-top: 30px; color: #2b6cb0; }}
        table {{ border-collapse: collapse; width: 100%; margin: 20px 0; }}
        th, td {{ border: 1px solid #ddd; padding: 8px; text-align: left; }}
        th {{ background-color: #f0f0f0; }}
        button {{ background-color: #4299e1; color: white; border: none; padding: 8px 16px; border-radius: 4px; cursor: pointer; }}
        button:hover {{ background-color: #3182ce; }}
        select, input {{ padding: 8px; border: 1px solid #cbd5e0; border-radius: 4px; margin-right: 8px; }}
    </style>
</head>
<body>
    <h1>Waspy Project Test</h1>
    <p>WebAssembly Module: <code>{wasm_filename}</code></p>
    
    <h2>Available Functions</h2>
    <div id="function-list">Loading functions...</div>
    
    <h2>Function Tester</h2>
    <div class="function-test">
        <p>
            <label for="function-select">Select a function:</label>
            <select id="function-select"></select>
        </p>
        <p>
            <label for="arguments">Arguments (comma separated):</label>
            <input type="text" id="arguments" value="5, 3" style="width: 200px;">
            <button id="run-test">Run Function</button>
        </p>
        <div class="result" id="function-result">Result will appear here</div>
    </div>

    <script>
        // Load the WebAssembly module
        (async () => {{
            try {{
                const response = await fetch('{wasm_filename}');
                const bytes = await response.arrayBuffer();
                const {{ instance }} = await WebAssembly.instantiate(bytes);
                
                // Get all exported functions
                const functions = Object.keys(instance.exports)
                    .filter(name => typeof instance.exports[name] === 'function');
                
                // Display function list
                const functionListDiv = document.getElementById('function-list');
                if (functions.length > 0) {{
                    const table = document.createElement('table');
                    const headerRow = document.createElement('tr');
                    ['#', 'Function Name'].forEach(text => {{
                        const th = document.createElement('th');
                        th.textContent = text;
                        headerRow.appendChild(th);
                    }});
                    table.appendChild(headerRow);
                    
                    functions.forEach((name, index) => {{
                        const row = document.createElement('tr');
                        
                        const indexCell = document.createElement('td');
                        indexCell.textContent = index + 1;
                        row.appendChild(indexCell);
                        
                        const nameCell = document.createElement('td');
                        nameCell.textContent = name;
                        row.appendChild(nameCell);
                        
                        table.appendChild(row);
                    }});
                    
                    functionListDiv.innerHTML = '';
                    functionListDiv.appendChild(table);
                }} else {{
                    functionListDiv.textContent = 'No functions found in the WebAssembly module.';
                }}
                
                // Populate function selector
                const functionSelect = document.getElementById('function-select');
                functions.forEach(name => {{
                    const option = document.createElement('option');
                    option.value = name;
                    option.textContent = name;
                    functionSelect.appendChild(option);
                }});
                
                // Function test handler
                document.getElementById('run-test').addEventListener('click', () => {{
                    const functionName = functionSelect.value;
                    const argsString = document.getElementById('arguments').value;
                    
                    // Try to parse as numbers first, but keep strings if that fails
                    const args = argsString.split(',').map(arg => {{
                        const trimmed = arg.trim();
                        // Check if it's a quoted string
                        if ((trimmed.startsWith('"') && trimmed.endsWith('"')) || 
                            (trimmed.startsWith("'") && trimmed.endsWith("'"))) {{
                            return trimmed.substring(1, trimmed.length - 1);
                    }}
                        // Try to parse as number
                        const num = Number(trimmed);
                        return isNaN(num) ? trimmed : num;
                    }});
                    
                    try {{
                        const result = instance.exports[functionName](...args);
                        document.getElementById('function-result').textContent = 
                            `${{functionName}}(${{args.join(', ')}}) = ${{result}}`;
                    }} catch (error) {{
                        document.getElementById('function-result').textContent = 
                            `Error: ${{error.message}}`;
                    }}
                }});
                
                console.log("WebAssembly module loaded successfully!");
            }} catch (error) {{
                console.error("Error loading WebAssembly:", error);
                document.body.innerHTML += `<div style="color: red; padding: 20px; background: #fed7d7; margin-top: 20px; border-radius: 4px;">
                    <h3>Error Loading WebAssembly</h3>
                    <p>${{error.message}}</p>
                </div>`;
            }}
        }})();
    </script>
</body>
</html>
"#
    )
}
