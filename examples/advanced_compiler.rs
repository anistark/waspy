use waspy::{compile_python_to_wasm_with_options, get_python_file_metadata, parser};
use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;

/// Advanced compiler example with options
///
/// This example demonstrates more advanced usage of Waspy,
/// including command-line options, metadata extraction, and
/// optimization control.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!(
            "Usage: {} <python_file> [--no-optimize] [--metadata] [--html]",
            args[0]
        );
        eprintln!("Options:");
        eprintln!("  --no-optimize    Disable WebAssembly optimization");
        eprintln!("  --metadata       Show function signatures and metadata");
        eprintln!("  --html           Generate an HTML test file");
        return Ok(());
    }

    let python_file = &args[1];
    let optimize = !args.contains(&"--no-optimize".to_string());
    let show_metadata = args.contains(&"--metadata".to_string());
    let generate_html = args.contains(&"--html".to_string());

    // Read the Python source file
    let source = fs::read_to_string(python_file)?;

    println!("Compiling {}...", python_file);

    // If metadata is requested, extract and display function signatures
    if show_metadata {
        match get_python_file_metadata(&source) {
            Ok(signatures) => {
                println!("\n--- Function Signatures ---");
                for sig in signatures {
                    println!(
                        "def {}({}) -> {}",
                        sig.name,
                        sig.parameters.join(", "),
                        sig.return_type
                    );
                }
                println!();
            }
            Err(err) => {
                eprintln!("Error extracting metadata: {}", err);
            }
        }

        // Print the AST structure
        match parser::parse_python(&source) {
            Ok(ast) => {
                println!("--- AST Structure ---");
                println!("Found {} top-level statements", ast.len());

                let function_count = ast
                    .iter()
                    .filter(|stmt| matches!(stmt, rustpython_parser::ast::Stmt::FunctionDef(_)))
                    .count();

                println!("Found {} function(s)\n", function_count);
            }
            Err(err) => {
                eprintln!("Error parsing Python: {}", err);
            }
        }
    }

    // Start the compilation process
    let start = Instant::now();

    println!("Compiling with optimization: {}", optimize);
    let result = compile_python_to_wasm_with_options(&source, optimize);

    match result {
        Ok(wasm) => {
            let duration = start.elapsed();
            println!("Compilation completed in {:.2?}", duration);

            // Generate the output filename
            let path = Path::new(python_file);
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let parent_dir = path.parent().unwrap_or(Path::new("."));

            let optimize_suffix = if optimize { "" } else { "_unoptimized" };
            let output_file = parent_dir.join(format!("{}{}.wasm", stem, optimize_suffix));

            // Write the WebAssembly to a file
            fs::write(&output_file, &wasm)?;

            println!("Wrote WebAssembly to {}", output_file.display());
            println!("Output size: {} bytes", wasm.len());

            // Generate a simple HTML test file if requested
            if generate_html {
                let html_file = parent_dir.join(format!("{}_test.html", stem));
                let wasm_filename = output_file.file_name().unwrap().to_str().unwrap();
                let html = generate_html_test_file(stem, wasm_filename);
                fs::write(&html_file, html)?;
                println!("Wrote test HTML to {}", html_file.display());
            }
        }
        Err(err) => {
            eprintln!("Compilation error: {}", err);
        }
    }

    Ok(())
}

/// Generate a simple HTML test harness for trying out the WebAssembly
fn generate_html_test_file(module_name: &str, wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Waspy Test - {}</title>
    <style>
        body {{ font-family: sans-serif; margin: 20px; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; }}
        .function-test {{ margin-bottom: 20px; }}
        h2 {{ margin-top: 30px; }}
    </style>
</head>
<body>
    <h1>Waspy WebAssembly Test</h1>
    <p>Module: {}</p>
    <div id="container">
        <h2>Function Tests</h2>
        <div class="function-test">
            <h3>Test Functions</h3>
            <p>Select a function:
            <select id="function-select"></select>
            </p>
            <p>Arguments (comma separated): <input type="text" id="arguments" value="5"></p>
            <button id="run-test">Run Function</button>
            <div class="result" id="function-result">Result will appear here</div>
        </div>
    </div>

    <script>
        // Load the WebAssembly module
        (async () => {{
            try {{
                const response = await fetch('{}');
                const bytes = await response.arrayBuffer();
                const {{ instance }} = await WebAssembly.instantiate(bytes);
                
                // Display available functions
                const functions = Object.keys(instance.exports).filter(
                    name => typeof instance.exports[name] === 'function'
                );
                
                const functionSelect = document.getElementById('function-select');
                
                functions.forEach(name => {{
                    const option = document.createElement('option');
                    option.value = name;
                    option.textContent = name;
                    functionSelect.appendChild(option);
                }});
                
                // Add event listener to the test button
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
                
                console.log("Available functions:", functions);
            }} catch (error) {{
                console.error("Error loading WebAssembly:", error);
                document.body.innerHTML += `<div style="color: red">Error loading WebAssembly: ${{error.message}}</div>`;
            }}
        }})();
    </script>
</body>
</html>
"#,
        module_name, wasm_filename, wasm_filename
    )
}
