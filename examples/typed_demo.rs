//! Type System and Module-Level Code Example
//!
//! This example demonstrates Waspy's type system and handling of module-level code.

use std::fs;
use std::path::Path;
use std::time::Instant;
use waspy::{compile_python_to_wasm_with_options, CompilerOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Waspy Type System Demo");
    println!("===================\n");

    // Create output directory
    fs::create_dir_all("examples/output")?;

    // Read the Python file
    let python_file = Path::new("examples/typed_demo.py");
    let source = fs::read_to_string(python_file)?;

    println!("Analyzing Python code with type annotations...");

    // Extract and display function signatures
    match waspy::get_python_file_metadata(&source) {
        Ok(signatures) => {
            println!("\nDetected Functions:");
            println!("-----------------");
            for (i, sig) in signatures.iter().enumerate() {
                println!(
                    "{}. def {}({}) -> {}",
                    i + 1,
                    sig.name,
                    sig.parameters.join(", "),
                    sig.return_type
                );
            }

            // Highlight type conversions
            println!("\nType Conversion Functions:");
            println!("------------------------");
            for sig in &signatures {
                if sig.name.contains("to_") {
                    println!(
                        "• {}: {} -> {}",
                        sig.name,
                        extract_param_type(&sig.parameters),
                        sig.return_type
                    );
                }
            }
        }
        Err(err) => {
            eprintln!("Error extracting metadata: {err}");
        }
    }

    // Compile with options
    let options = CompilerOptions {
        optimize: true,
        debug_info: true,
        generate_html: true,
        include_metadata: true,
        ..CompilerOptions::default()
    };

    println!("\nCompiling with type system support...");
    let start = Instant::now();
    let wasm = compile_python_to_wasm_with_options(&source, &options)?;
    let duration = start.elapsed();

    // Write the WebAssembly to a file
    let output_file = Path::new("examples/output/typed_demo.wasm");
    fs::write(output_file, &wasm)?;

    // Generate HTML test file
    let html_file = output_file.with_extension("html");
    let wasm_name = output_file.file_name().unwrap().to_str().unwrap();
    let html = generate_html_test_file("Type System Demo", wasm_name);
    fs::write(&html_file, html)?;

    println!("\n✅ Compilation Results");
    println!("---------------------");
    println!("Compilation completed in {duration:.2?}");
    println!("WebAssembly binary size: {} bytes", wasm.len());
    println!("Output file: {}", output_file.display());
    println!("HTML test file: {}", html_file.display());

    println!("\nType system demonstration compiled successfully!");
    Ok(())
}

/// Extract parameter type from a parameter string like "a: int"
fn extract_param_type(params: &[String]) -> String {
    if params.is_empty() {
        return "void".to_string();
    }

    let param = &params[0];
    if let Some(pos) = param.find(':') {
        if pos + 1 < param.len() {
            return param[pos + 1..].trim().to_string();
        }
    }

    "unknown".to_string()
}

/// Generate an HTML test file for the type system demo
fn generate_html_test_file(title: &str, wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>{title} - Waspy</title>
    <style>
        body {{ font-family: system-ui, sans-serif; margin: 0; padding: 20px; line-height: 1.5; max-width: 800px; margin: 0 auto; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; border-radius: 4px; font-family: monospace; white-space: pre-wrap; }}
        .card {{ background: white; border-radius: 8px; box-shadow: 0 2px 8px rgba(0,0,0,0.1); padding: 20px; margin-bottom: 20px; }}
        h2 {{ margin-top: 30px; color: #2b6cb0; }}
        h3 {{ color: #4a5568; }}
        button {{ background-color: #4299e1; color: white; border: none; padding: 8px 16px; border-radius: 4px; cursor: pointer; margin-right: 8px; }}
        button:hover {{ background-color: #3182ce; }}
        select, input {{ padding: 8px; border: 1px solid #cbd5e0; border-radius: 4px; margin-right: 8px; }}
        .button-row {{ display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 10px; }}
    </style>
</head>
<body>
    <h1>{title}</h1>
    <p>This demonstration showcases Waspy's type system capabilities.</p>
    
    <div class="card">
        <h2>Type Conversion Tests</h2>
        <p>Test various type conversions between integers and floats:</p>
        
        <div class="button-row">
            <button onclick="testFunction('int_to_float', 42)">Convert int to float</button>
            <button onclick="testFunction('float_to_int', 3.14)">Convert float to int</button>
            <button onclick="testFunction('add_integers', 5, 7)">Add integers</button>
            <button onclick="testFunction('add_floats', 2.5, 3.5)">Add floats</button>
            <button onclick="testFunction('mixed_types', 10, 0.5)">Mixed types</button>
        </div>
        
        <div class="result" id="conversion-result">Results will appear here</div>
    </div>
    
    <div class="card">
        <h2>Boolean Operations Test</h2>
        <p>Test boolean operations and comparisons:</p>
        
        <div class="button-row">
            <button onclick="testFunction('bool_operations', true, false)">AND/OR Operations</button>
            <button onclick="testFunction('comparisons', 10, 5)">Comparison Operators</button>
        </div>
        
        <div class="result" id="boolean-result">Results will appear here</div>
    </div>
    
    <div class="card">
        <h2>Custom Function Test</h2>
        <p>Test any exported function:</p>
        
        <div>
            <select id="function-select">
                <option value="">Select a function</option>
            </select>
            
            <input type="text" id="arguments" placeholder="Arguments (comma separated)" style="width: 250px;">
            <button onclick="runCustomTest()">Run Function</button>
        </div>
        
        <div class="result" id="custom-result">Results will appear here</div>
    </div>

    <script>
        let instance = null;
        
        async function loadWasm() {{
            try {{
                const response = await fetch('{wasm_filename}');
                const bytes = await response.arrayBuffer();
                const result = await WebAssembly.instantiate(bytes);
                instance = result.instance;
                
                // Populate function selector
                const functions = Object.keys(instance.exports)
                    .filter(name => typeof instance.exports[name] === 'function');
                
                const selector = document.getElementById('function-select');
                selector.innerHTML = '<option value="">Select a function</option>';
                
                functions.forEach(name => {{
                    const option = document.createElement('option');
                    option.value = name;
                    option.textContent = name;
                    selector.appendChild(option);
                }});
                
                console.log("Available functions:", functions);
            }} catch (error) {{
                console.error("Error loading WebAssembly:", error);
                document.body.innerHTML += `<div style="color: red; padding: 20px; background: #fed7d7; margin-top: 20px; border-radius: 4px;">
                    <h3>Error Loading WebAssembly</h3>
                    <p>${{error.message}}</p>
                </div>`;
            }}
        }}
        
        function testFunction(name, ...args) {{
            if (!instance) {{
                document.getElementById('conversion-result').textContent = 'WebAssembly not loaded yet!';
                document.getElementById('boolean-result').textContent = 'WebAssembly not loaded yet!';
                return;
            }}
            
            try {{
                const result = instance.exports[name](...args);
                
                // Update the appropriate result div
                let resultElement;
                if (['int_to_float', 'float_to_int', 'add_integers', 'add_floats', 'mixed_types'].includes(name)) {{
                    resultElement = document.getElementById('conversion-result');
                }} else if (['bool_operations', 'comparisons'].includes(name)) {{
                    resultElement = document.getElementById('boolean-result');
                }} else {{
                    resultElement = document.getElementById('custom-result');
                }}
                
                resultElement.textContent = `${{name}}(${{args.join(', ')}}) = ${{result}}`;
            }} catch (error) {{
                document.getElementById('conversion-result').textContent = `Error: ${{error.message}}`;
            }}
        }}
        
        function runCustomTest() {{
            if (!instance) {{
                document.getElementById('custom-result').textContent = 'WebAssembly not loaded yet!';
                return;
            }}
            
            const functionName = document.getElementById('function-select').value;
            if (!functionName) {{
                document.getElementById('custom-result').textContent = 'Please select a function!';
                return;
            }}
            
            const argsString = document.getElementById('arguments').value;
            const args = argsString ? argsString.split(',').map(arg => {{
                const trimmed = arg.trim();
                if (trimmed === 'true') return true;
                if (trimmed === 'false') return false;
                const num = parseFloat(trimmed);
                return isNaN(num) ? trimmed : num;
            }}) : [];
            
            try {{
                const result = instance.exports[functionName](...args);
                document.getElementById('custom-result').textContent = 
                    `${{functionName}}(${{args.join(', ')}}) = ${{result}}`;
            }} catch (error) {{
                document.getElementById('custom-result').textContent = `Error: ${{error.message}}`;
            }}
        }}
        
        // Load the WebAssembly module when the page loads
        document.addEventListener('DOMContentLoaded', loadWasm);
    </script>
</body>
</html>
"#
    )
}
