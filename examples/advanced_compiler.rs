//! Advanced Waspy Compiler Example
//!
//! This example demonstrates more advanced usage of Waspy,
//! including compiler options, function metadata extraction,
//! and generating HTML test harnesses.

use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;
use waspy::{compile_python_to_wasm_with_options, parser, CompilerOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // Parse command line arguments
    if args.len() < 2 {
        print_usage(&args[0]);
        return Ok(());
    }

    let python_file = &args[1];

    // Parse options
    let options = parse_options(&args);

    // Create output directory
    fs::create_dir_all("examples/output")?;

    println!("Waspy Advanced Compiler");
    println!("======================\n");
    println!("Input file: {}", python_file);

    // Read the Python source file
    let source = fs::read_to_string(python_file)?;

    // Extract and display function signatures if requested
    if options.include_metadata {
        display_function_metadata(&source)?;
    }

    // Compile the Python code
    println!("\nCompiling with the following options:");
    println!(
        "- Optimization: {}",
        if options.optimize {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "- Debug info: {}",
        if options.debug_info {
            "included"
        } else {
            "excluded"
        }
    );
    println!(
        "- HTML test harness: {}",
        if options.generate_html { "yes" } else { "no" }
    );

    let start = Instant::now();
    let wasm = compile_python_to_wasm_with_options(&source, &options)?;
    let duration = start.elapsed();

    println!("\n✅ Compilation completed in {:.2?}", duration);

    // Generate output filename
    let path = Path::new(python_file);
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let output_file = Path::new("examples/output").join(format!("{}.wasm", stem));

    // Write the WebAssembly to a file
    fs::write(&output_file, &wasm)?;
    println!("WebAssembly written to {}", output_file.display());
    println!("Output size: {} bytes", wasm.len());

    // Generate HTML test harness if requested
    if options.generate_html {
        let html_file = output_file.with_extension("html");
        let html =
            generate_html_test_file(stem, output_file.file_name().unwrap().to_str().unwrap());
        fs::write(&html_file, html)?;
        println!("HTML test harness written to {}", html_file.display());
    }

    println!("\nCompilation successful!");
    Ok(())
}

fn print_usage(program_name: &str) {
    eprintln!("Usage: {} <python_file> [options]", program_name);
    eprintln!("Options:");
    eprintln!("  --no-optimize     Disable WebAssembly optimization");
    eprintln!("  --debug-info      Include debug information");
    eprintln!("  --metadata        Show function signatures and metadata");
    eprintln!("  --html            Generate an HTML test harness");
    eprintln!("  --entry-point=NAME Set a specific entry point function");
    eprintln!("\nExample:");
    eprintln!(
        "  {} examples/typed_demo.py --metadata --html",
        program_name
    );
}

fn parse_options(args: &[String]) -> CompilerOptions {
    let mut options = CompilerOptions::default();

    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--no-optimize" => options.optimize = false,
            "--debug-info" => options.debug_info = true,
            "--metadata" => options.include_metadata = true,
            "--html" => options.generate_html = true,
            _ => {
                // Check for --entry-point=NAME format
                if arg.starts_with("--entry-point=") {
                    if let Some(name) = arg.strip_prefix("--entry-point=") {
                        options.entry_point = Some(name.to_string());
                    }
                }
            }
        }
    }

    options
}

fn display_function_metadata(source: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Extract function signatures
    match waspy::get_python_file_metadata(source) {
        Ok(signatures) => {
            println!("\nFunction Signatures:");
            println!("-------------------");
            for (i, sig) in signatures.iter().enumerate() {
                println!(
                    "{}. def {}({}) -> {}",
                    i + 1,
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

    // Print AST structure (simplified)
    match parser::parse_python(source) {
        Ok(ast) => {
            println!("AST Structure:");
            println!("-------------");
            println!("• Found {} top-level statements", ast.len());

            let function_count = ast
                .iter()
                .filter(|stmt| matches!(stmt, rustpython_parser::ast::Stmt::FunctionDef(_)))
                .count();

            println!("• Found {} function definitions", function_count);

            // Count class definitions
            let class_count = ast
                .iter()
                .filter(|stmt| matches!(stmt, rustpython_parser::ast::Stmt::ClassDef(_)))
                .count();

            if class_count > 0 {
                println!("• Found {} class definitions", class_count);
            }

            println!();
        }
        Err(err) => {
            eprintln!("Error parsing Python: {}", err);
        }
    }

    Ok(())
}

fn generate_html_test_file(module_name: &str, wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Waspy Test - {}</title>
    <style>
        body {{ font-family: system-ui, sans-serif; margin: 0; padding: 20px; line-height: 1.5; max-width: 800px; margin: 0 auto; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; border-radius: 4px; font-family: monospace; white-space: pre-wrap; }}
        .function-test {{ margin-bottom: 20px; }}
        h2 {{ margin-top: 30px; color: #2b6cb0; }}
        button {{ background-color: #4299e1; color: white; border: none; padding: 8px 16px; border-radius: 4px; cursor: pointer; }}
        button:hover {{ background-color: #3182ce; }}
        select, input {{ padding: 8px; border: 1px solid #cbd5e0; border-radius: 4px; margin-right: 8px; }}
    </style>
</head>
<body>
    <h1>Waspy WebAssembly Test</h1>
    <p>Module: <code>{}</code></p>
    <div id="container">
        <h2>Function Tests</h2>
        <div class="function-test">
            <h3>Test Functions</h3>
            <p>
                <label for="function-select">Select a function:</label>
                <select id="function-select"></select>
            </p>
            <p>
                <label for="arguments">Arguments (comma separated):</label>
                <input type="text" id="arguments" value="5" style="width: 200px;">
                <button id="run-test">Run Function</button>
            </p>
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
                document.body.innerHTML += `<div style="color: red; padding: 20px; background: #fed7d7; margin-top: 20px; border-radius: 4px;">
                    <h3>Error Loading WebAssembly</h3>
                    <p>${{error.message}}</p>
                </div>`;
            }}
        }})();
    </script>
</body>
</html>
"#,
        module_name, wasm_filename, wasm_filename
    )
}
