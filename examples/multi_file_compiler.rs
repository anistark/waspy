use waspy::compile_multiple_python_files;
use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;

/// Multi-file compiler example
///
/// This example demonstrates how to compile multiple Python files
/// into a single WebAssembly module, allowing for cross-file function calls.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get arguments from command line
    let args: Vec<String> = env::args().collect();

    if args.len() < 3 {
        eprintln!(
            "Usage: {} <output_file> <python_file1> [python_file2] ...",
            args[0]
        );
        eprintln!(
            "Example: {} combined.wasm examples/basic_operations.py examples/calculator.py",
            args[0]
        );
        return Ok(());
    }

    let output_path = &args[1];
    let input_files = &args[2..];

    println!("\n--- Multi-file WebAssembly Compilation ---");
    println!("Output file: {}", output_path);
    println!("Input files:");
    for (idx, file) in input_files.iter().enumerate() {
        println!("  {}. {}", idx + 1, file);
    }

    // Read all Python source files
    let mut file_data: Vec<(String, String)> = Vec::new();

    for file_path in input_files {
        let path_str = file_path.to_string();
        match fs::read_to_string(file_path) {
            Ok(source) => {
                file_data.push((path_str.clone(), source));
                println!("Successfully read {}", path_str);
            }
            Err(err) => {
                eprintln!("Error reading {}: {}", path_str, err);
                return Err(Box::new(err));
            }
        }
    }

    // Create the sources vector with references
    let sources: Vec<(&str, &str)> = file_data
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();

    // Compile all files into a single WASM module
    println!(
        "\nCompiling {} files into a single WebAssembly module...",
        sources.len()
    );
    let start_time = Instant::now();

    match compile_multiple_python_files(&sources, true) {
        Ok(wasm_binary) => {
            let duration = start_time.elapsed();

            let final_output = if !output_path.ends_with(".wasm") {
                format!("{}.wasm", output_path)
            } else {
                output_path.to_string()
            };

            fs::write(&final_output, &wasm_binary)?;

            println!("Compilation completed in {:.2?}", duration);
            println!("Successfully compiled to {}", final_output);
            println!("WebAssembly binary size: {} bytes", wasm_binary.len());

            // Generate HTML test file
            let parent = Path::new(&final_output).parent().unwrap_or(Path::new("."));
            let stem = Path::new(&final_output)
                .file_stem()
                .unwrap()
                .to_str()
                .unwrap();
            let html_path = parent.join(format!("{}_test.html", stem));

            let wasm_name = Path::new(&final_output)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            let html = generate_html_test_file(wasm_name);

            fs::write(&html_path, html)?;
            println!("Generated HTML test file: {}", html_path.display());
        }
        Err(e) => {
            eprintln!("Compilation error: {}", e);
        }
    }

    Ok(())
}

/// Generate an HTML test file for the compiled WebAssembly
fn generate_html_test_file(wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Waspy Multi-file Test</title>
    <style>
        body {{ font-family: sans-serif; margin: 20px; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; }}
        .function-test {{ margin-bottom: 20px; }}
        pre {{ background-color: #f8f8f8; padding: 10px; border-radius: 5px; }}
        h2 {{ margin-top: 30px; }}
        table {{ border-collapse: collapse; width: 100%; }}
        th, td {{ border: 1px solid #ddd; padding: 8px; text-align: left; }}
        th {{ background-color: #f2f2f2; }}
    </style>
</head>
<body>
    <h1>Waspy Multi-file WebAssembly Test</h1>
    <p>Module: <code>{}</code></p>
    
    <h2>Available Functions</h2>
    <div id="function-list">Loading...</div>
    
    <h2>Function Tests</h2>
    <div class="function-test">
        <p>
            Select a function:
            <select id="function-select"></select>
        </p>
        <p>
            Arguments (comma separated): 
            <input type="text" id="arguments" value="5, 3">
        </p>
        <button id="run-test">Run Function</button>
        <div class="result" id="function-result">Result will appear here</div>
    </div>
    
    <h2>Calculator Tests</h2>
    <div class="function-test">
        <h3>Basic Calculator</h3>
        <p>
            Operation: 
            <select id="calc-operation">
                <option value="add">Addition</option>
                <option value="subtract">Subtraction</option>
                <option value="multiply">Multiplication</option>
                <option value="divide">Division</option>
                <option value="modulo">Modulo</option>
            </select>
        </p>
        <p>
            First number: <input type="number" id="calc-a" value="10">
            Second number: <input type="number" id="calc-b" value="5">
        </p>
        <button id="calc-test">Calculate</button>
        <div class="result" id="calc-result">Result will appear here</div>
        
        <h3>Complex Calculations</h3>
        <button id="complex-calc">Run complex_calculation(10, 5)</button>
        <div class="result" id="complex-result">Result will appear here</div>
        
        <button id="apply-ops">Run apply_operations(10, 5)</button>
        <div class="result" id="apply-result">Result will appear here</div>
        
        <button id="factorial-test">Run calculate_factorial(5)</button>
        <div class="result" id="factorial-result">Result will appear here</div>
    </div>

    <script>
        // Load the WebAssembly module
        (async () => {{
            try {{
                const response = await fetch('{}');
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
                
                // Calculator tests
                if (instance.exports.calculate) {{
                    document.getElementById('calc-test').addEventListener('click', () => {{
                        const operation = document.getElementById('calc-operation').value;
                        const a = parseInt(document.getElementById('calc-a').value);
                        const b = parseInt(document.getElementById('calc-b').value);
                        
                        try {{
                            const result = instance.exports.calculate(operation, a, b);
                            document.getElementById('calc-result').textContent = 
                                `calculate("${{operation}}", ${{a}}, ${{b}}) = ${{result}}`;
                        }} catch (error) {{
                            document.getElementById('calc-result').textContent = 
                                `Error: ${{error.message}}`;
                        }}
                    }});
                }}
                
                // Complex calculation test
                if (instance.exports.complex_calculation) {{
                    document.getElementById('complex-calc').addEventListener('click', () => {{
                        try {{
                            const result = instance.exports.complex_calculation(10, 5);
                            document.getElementById('complex-result').textContent = 
                                `complex_calculation(10, 5) = ${{result}}`;
                        }} catch (error) {{
                            document.getElementById('complex-result').textContent = 
                                `Error: ${{error.message}}`;
                        }}
                    }});
                }}
                
                // Apply operations test
                if (instance.exports.apply_operations) {{
                    document.getElementById('apply-ops').addEventListener('click', () => {{
                        try {{
                            const result = instance.exports.apply_operations(10, 5);
                            document.getElementById('apply-result').textContent = 
                                `apply_operations(10, 5) = ${{result}}`;
                        }} catch (error) {{
                            document.getElementById('apply-result').textContent = 
                                `Error: ${{error.message}}`;
                        }}
                    }});
                }}
                
                // Factorial test
                if (instance.exports.calculate_factorial) {{
                    document.getElementById('factorial-test').addEventListener('click', () => {{
                        try {{
                            const result = instance.exports.calculate_factorial(5);
                            document.getElementById('factorial-result').textContent = 
                                `calculate_factorial(5) = ${{result}}`;
                        }} catch (error) {{
                            document.getElementById('factorial-result').textContent = 
                                `Error: ${{error.message}}`;
                        }}
                    }});
                }}
                
                console.log("WebAssembly module loaded successfully!");
            }} catch (error) {{
                console.error("Error loading WebAssembly:", error);
                document.body.innerHTML += `<div style="color: red">Error loading WebAssembly: ${{error.message}}</div>`;
            }}
        }})();
    </script>
</body>
</html>
"#,
        wasm_filename, wasm_filename
    )
}
