//! Multi-file Waspy Compiler Example
//!
//! This example demonstrates how to compile multiple Python files
//! into a single WebAssembly module, allowing for cross-file function calls.

use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;
use waspy::{compile_multiple_python_files_with_options, CompilerOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get arguments from command line
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        print_usage(&args[0]);
        return Ok(());
    }

    let output_path = &args[1];
    let input_files = &args[2..];

    println!("\nWaspy Multi-file Compiler");
    println!("========================\n");
    println!("Output file: {output_path}");
    println!("Input files:");
    for (idx, file) in input_files.iter().enumerate() {
        let num = idx + 1;
        println!("  {num}. {file}");
    }

    // Create output directory if needed
    if let Some(parent) = Path::new(output_path).parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    // Read all Python source files
    let mut file_data: Vec<(String, String)> = Vec::new();

    for file_path in input_files {
        let path_str = file_path.to_string();
        match fs::read_to_string(file_path) {
            Ok(source) => {
                file_data.push((path_str.clone(), source));
                println!("Successfully read {path_str}");
            }
            Err(err) => {
                eprintln!("Error reading {path_str}: {err}");
                return Err(Box::new(err));
            }
        }
    }

    // Create the sources vector with references
    let sources: Vec<(&str, &str)> = file_data
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();

    // Set compiler options
    let options = CompilerOptions {
        optimize: true,
        debug_info: true,
        generate_html: true,
        ..CompilerOptions::default()
    };

    // Compile all files into a single WASM module
    let num_sources = sources.len();
    println!("\nCompiling {num_sources} files into a single WebAssembly module...");
    let start_time = Instant::now();

    let wasm_binary = compile_multiple_python_files_with_options(&sources, &options)?;
    let duration = start_time.elapsed();

    // Ensure output filename has .wasm extension
    let final_output = if !output_path.ends_with(".wasm") {
        format!("{output_path}.wasm")
    } else {
        output_path.to_string()
    };

    fs::write(&final_output, &wasm_binary)?;

    println!("\n✅ Compilation Results");
    println!("---------------------");
    println!("Compilation completed in {duration:.2?}");
    println!("Output file: {final_output}");
    println!("WebAssembly binary size: {} bytes", wasm_binary.len());

    // Generate HTML test file
    let parent = Path::new(&final_output).parent().unwrap_or(Path::new("."));
    let stem = Path::new(&final_output)
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap();
    let html_path = parent.join(format!("{stem}_test.html"));

    let wasm_name = Path::new(&final_output)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let html = generate_html_test_file(wasm_name);

    fs::write(&html_path, html)?;
    println!("HTML test file: {}", html_path.display());

    println!("\nMulti-file compilation successful!");
    Ok(())
}

fn print_usage(program_name: &str) {
    eprintln!("Usage: {program_name} <output_file> <python_file1> [python_file2] ...");
    eprintln!("Example: {program_name} examples/output/combined.wasm examples/basic_operations.py examples/calculator.py");
}

fn generate_html_test_file(wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Waspy Multi-file Test</title>
    <style>
        body {{ font-family: system-ui, sans-serif; margin: 0; padding: 20px; line-height: 1.5; max-width: 800px; margin: 0 auto; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; border-radius: 4px; font-family: monospace; white-space: pre-wrap; }}
        .function-test {{ margin-bottom: 20px; }}
        pre {{ background-color: #f8f8f8; padding: 10px; border-radius: 4px; overflow-x: auto; }}
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
    <h1>Waspy Multi-file WebAssembly Test</h1>
    <p>Module: <code>{wasm_filename}</code></p>
    
    <h2>Available Functions</h2>
    <div id="function-list">Loading...</div>
    
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
                
                // Generic function test
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
