use chakrapy::{compile_python_to_wasm_with_options, parser};
use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <python_file> [--no-optimize] [--debug]", args[0]);
        return Ok(());
    }

    let optimize = !args.contains(&"--no-optimize".to_string());
    let debug = args.contains(&"--debug".to_string());
    let python_file = &args[1];

    // Read the Python source file
    let source = fs::read_to_string(python_file)?;

    println!("Compiling {}...", python_file);

    if debug {
        println!("Source code:\n{}", source);
    }

    // Parse the Python code to check what features are used
    let ast = parser::parse_python(&source)?;
    println!("Successfully parsed Python code");
    println!("Found {} top-level statements", ast.len());

    let function_count = ast
        .iter()
        .filter(|stmt| matches!(stmt, rustpython_parser::ast::Stmt::FunctionDef(_)))
        .count();

    println!("Found {} function(s)", function_count);

    if debug {
        // Print the AST for debugging
        println!("AST Structure:");
        for (i, stmt) in ast.iter().enumerate() {
            match stmt {
                rustpython_parser::ast::Stmt::FunctionDef(func) => {
                    println!("  Function {}: {}", i, func.name);
                    println!(
                        "    Parameters: {:?}",
                        func.args
                            .args
                            .iter()
                            .map(|arg| arg.def.arg.to_string())
                            .collect::<Vec<_>>()
                    );
                    println!("    Body length: {}", func.body.len());
                }
                _ => println!("  Statement {}: {:?}", i, stmt),
            }
        }
    }

    // Start the compilation process
    let start = Instant::now();
    println!("Lowering AST to IR...");

    // First try compiling with optimization if requested
    let mut result_wasm = Vec::new();
    let mut success = false;
    let mut is_optimized = false;

    if optimize {
        println!("Compiling with optimization...");
        match compile_python_to_wasm_with_options(&source, true) {
            Ok(wasm) => {
                result_wasm = wasm;
                success = true;
                is_optimized = true;
                println!("Compilation with optimization successful!");
            }
            Err(err) => {
                println!("Optimization failed: {}", err);
                // Fall through to try without optimization
            }
        }
    }

    // If optimization failed or wasn't requested, try without optimization
    if !success {
        println!("Compiling without optimization...");
        match compile_python_to_wasm_with_options(&source, false) {
            Ok(wasm) => {
                result_wasm = wasm;
                success = true;
                is_optimized = false;
                println!("Compilation without optimization successful!");
            }
            Err(err) => {
                return Err(anyhow::anyhow!("Compilation failed: {}", err).into());
            }
        }
    }

    let duration = start.elapsed();
    println!("Compilation completed in {:.2?}", duration);

    // Generate the output filename
    let path = Path::new(python_file);
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let parent_dir = path.parent().unwrap_or(Path::new("."));

    let output_file = if is_optimized {
        parent_dir.join(format!("{}.wasm", stem))
    } else {
        parent_dir.join(format!("{}_unoptimized.wasm", stem))
    };

    // Write the WebAssembly to a file
    fs::write(&output_file, &result_wasm)?;

    let optimize_str = if is_optimized {
        "optimized"
    } else {
        "unoptimized"
    };
    println!(
        "Wrote {} WebAssembly to {}",
        optimize_str,
        output_file.display()
    );
    println!("Output size: {} bytes", result_wasm.len());

    // Generate a simple HTML test file if requested
    if args.contains(&"--html".to_string()) {
        let html_file = parent_dir.join(format!("{}_test.html", stem));
        let wasm_filename = output_file.file_name().unwrap().to_str().unwrap();
        let html = generate_html_test_file(stem, wasm_filename, &function_count);
        fs::write(&html_file, html)?;
        println!("Wrote test HTML to {}", html_file.display());
    }

    // Generate a simple Node.js test file if requested
    if args.contains(&"--node".to_string()) {
        let js_file = parent_dir.join(format!("{}_test.js", stem));
        let wasm_filename = output_file.file_name().unwrap().to_str().unwrap();
        let js = generate_node_test_file(stem, wasm_filename, &function_count);
        fs::write(&js_file, js)?;
        println!("Wrote test Node.js file to {}", js_file.display());
    }

    Ok(())
}

fn generate_html_test_file(
    module_name: &str,
    wasm_filename: &str,
    function_count: &usize,
) -> String {
    let mut html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>ChakraPy Test - {}</title>
    <style>
        body {{ font-family: sans-serif; margin: 20px; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; }}
        .function-test {{ margin-bottom: 20px; }}
        h2 {{ margin-top: 30px; }}
    </style>
</head>
<body>
    <h1>ChakraPy WebAssembly Test</h1>
    <p>Module: {}</p>
    <div id="container">
        <h2>Function Tests</h2>
"#,
        module_name, wasm_filename
    );

    // For simplicity, we'll just add a generic test UI
    html.push_str(
        r#"
        <div class="function-test">
            <h3>Test Functions</h3>
            <p>Select a function:
            <select id="function-select">
"#,
    );

    // We'll add "function1", "function2", etc. placeholders
    for i in 1..=*function_count {
        html.push_str(&format!(
            "                <option value=\"function{}\">{}</option>\n",
            i, i
        ));
    }

    html.push_str(
        r#"            </select>
            </p>
            <p>Arguments (comma separated): <input type="text" id="arguments" value="5"></p>
            <button id="run-test">Run Function</button>
            <div class="result" id="function-result">Result will appear here</div>
        </div>
    </div>

    <script>
        // Load the WebAssembly module
        (async () => {
            try {
                const response = await fetch('"#,
    );

    html.push_str(&wasm_filename);

    html.push_str(r#"');
                const bytes = await response.arrayBuffer();
                const { instance } = await WebAssembly.instantiate(bytes);
                
                // Display available functions
                const functions = Object.keys(instance.exports).filter(
                    name => typeof instance.exports[name] === 'function'
                );
                
                const functionSelect = document.getElementById('function-select');
                functionSelect.innerHTML = ''; // Clear placeholder options
                
                functions.forEach(name => {
                    const option = document.createElement('option');
                    option.value = name;
                    option.textContent = name;
                    functionSelect.appendChild(option);
                });
                
                // Add event listener to the test button
                document.getElementById('run-test').addEventListener('click', () => {
                    const functionName = functionSelect.value;
                    const argsString = document.getElementById('arguments').value;
                    const args = argsString.split(',').map(arg => parseInt(arg.trim()));
                    
                    try {
                        const result = instance.exports[functionName](...args);
                        document.getElementById('function-result').textContent = 
                            `${functionName}(${args.join(', ')}) = ${result}`;
                    } catch (error) {
                        document.getElementById('function-result').textContent = 
                            `Error: ${error.message}`;
                    }
                });
                
                console.log("Available functions:", functions);
            } catch (error) {
                console.error("Error loading WebAssembly:", error);
                document.body.innerHTML += `<div style="color: red">Error loading WebAssembly: ${error.message}</div>`;
            }
        })();
    </script>
</body>
</html>
"#);

    html
}

fn generate_node_test_file(
    _module_name: &str,
    wasm_filename: &str,
    _function_count: &usize,
) -> String {
    format!(
        r#"const fs = require('fs');
const path = require('path');

// Read the WebAssembly file
const wasmPath = path.join(__dirname, '{}');
const wasmBuffer = fs.readFileSync(wasmPath);

// Instantiate the WebAssembly module
WebAssembly.instantiate(wasmBuffer)
    .then(result => {{
        const instance = result.instance;
        
        // Get all exported functions
        const functions = Object.keys(instance.exports)
            .filter(name => typeof instance.exports[name] === 'function');
        
        console.log('Available functions:');
        functions.forEach(name => {{
            console.log(`- ${{name}}`);
        }});
        
        // Test with some example values
        if (functions.includes('factorial')) {{
            console.log(`\nTesting factorial function:`);
            for (let i = 0; i <= 5; i++) {{
                console.log(`factorial(${{i}}) = ${{instance.exports.factorial(i)}}`);
            }}
        }}
        
        if (functions.includes('fibonacci')) {{
            console.log(`\nTesting fibonacci function:`);
            for (let i = 0; i <= 10; i++) {{
                console.log(`fibonacci(${{i}}) = ${{instance.exports.fibonacci(i)}}`);
            }}
        }}
        
        if (functions.includes('max_num')) {{
            console.log(`\nTesting max_num function:`);
            console.log(`max_num(5, 10) = ${{instance.exports.max_num(5, 10)}}`);
            console.log(`max_num(10, 5) = ${{instance.exports.max_num(10, 5)}}`);
            console.log(`max_num(-5, -10) = ${{instance.exports.max_num(-5, -10)}}`);
        }}
        
        // You can add more specific tests for your functions here
        
    }})
    .catch(error => {{
        console.error('Error:', error);
    }});
"#,
        wasm_filename
    )
}
