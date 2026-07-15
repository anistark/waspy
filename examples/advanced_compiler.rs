//! Advanced Waspy Compiler Example
//!
//! This example demonstrates more advanced usage of Waspy,
//! including compiler options, function metadata extraction,
//! and generating HTML test harnesses.

use std::env;
use std::fs;
use std::path::Path;
use std::time::Instant;
use waspy::core::parser;
use waspy::{compile_python_file_with_options, CompilerOptions, Verbosity};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    // Parse command line arguments
    if args.len() < 2 {
        print_usage(&args[0]);
        return Ok(());
    }

    let python_file = &args[1];

    // Parse options
    let (options, driver) = parse_options(&args);

    // Create output directory
    fs::create_dir_all("examples/output")?;

    println!("Waspy Advanced Compiler");
    println!("======================\n");
    println!("Input file: {python_file}");

    // Read the Python source file
    let source = fs::read_to_string(python_file)?;

    // Extract and display function signatures if requested
    if driver.show_metadata {
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
    println!("- Verbosity: {:?}", options.verbosity);
    println!(
        "- HTML test harness: {}",
        if driver.generate_html { "yes" } else { "no" }
    );

    // Compile by path so imports of sibling user-written modules resolve
    // (each imported .py file is linked into the single output module).
    let start = Instant::now();
    let wasm = compile_python_file_with_options(python_file, &options)?;
    let duration = start.elapsed();

    println!("\n✅ Compilation completed in {duration:.2?}");

    // Generate output filename
    let path = Path::new(python_file);
    let stem = path.file_stem().unwrap().to_str().unwrap();
    let output_file = Path::new("examples/output").join(format!("{stem}.wasm"));

    // Write the WebAssembly to a file
    fs::write(&output_file, &wasm)?;
    println!("WebAssembly written to {}", output_file.display());

    // Report source vs output sizes so the compilation cost is visible at a
    // glance (issue: report file sizes after compilation).
    let source_size = source.len();
    let wasm_size = wasm.len();
    println!("\nFile sizes:");
    println!("  Python source: {}", format_size(source_size));
    println!("  WASM output:   {}", format_size(wasm_size));
    println!(
        "  Ratio:         {:.2}x {}",
        if source_size > 0 {
            wasm_size as f64 / source_size as f64
        } else {
            0.0
        },
        if wasm_size <= source_size {
            "(smaller than source)"
        } else {
            "(of source size)"
        }
    );

    // Generate HTML test harness if requested
    if driver.generate_html {
        let html_file = output_file.with_extension("html");
        let html =
            generate_html_test_file(stem, output_file.file_name().unwrap().to_str().unwrap());
        fs::write(&html_file, html)?;
        println!("HTML test harness written to {}", html_file.display());
    }

    println!("\nCompilation successful!");
    Ok(())
}

/// Driver-side switches that shape this example's output but are not
/// compiler options (the library neither prints metadata nor writes HTML).
#[derive(Default)]
struct DriverFlags {
    show_metadata: bool,
    generate_html: bool,
}

/// Render a byte count human-readably (e.g. "1.4 KiB (1433 bytes)").
fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB ({bytes} bytes)", bytes as f64 / 1024.0)
    } else {
        format!(
            "{:.2} MiB ({bytes} bytes)",
            bytes as f64 / (1024.0 * 1024.0)
        )
    }
}

fn print_usage(program_name: &str) {
    eprintln!("Waspy advanced compiler driver — compile a Python file to WebAssembly.");
    eprintln!();
    eprintln!("Usage: {program_name} <python_file> [options]");
    eprintln!();
    eprintln!("The entry file's imports of sibling user-written .py modules are");
    eprintln!("resolved from disk and linked into the single output module, which");
    eprintln!("is written to examples/output/<name>.wasm.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --no-optimize   Skip the Binaryen optimization pass (faster builds,");
    eprintln!("                  larger output; the module is valid either way)");
    eprintln!("  --metadata      Print the file's function signatures and AST summary");
    eprintln!("                  before compiling");
    eprintln!("  --html          Also write an HTML page next to the .wasm that loads");
    eprintln!("                  it and calls exported functions from the browser");
    eprintln!("  --verbose       Verbose compiler logging (per-stage progress)");
    eprintln!("  --debug         Debug logging (most detailed, includes IR dumps)");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  {program_name} examples/typed_demo.py");
    eprintln!("  {program_name} examples/typed_demo.py --metadata --html");
    eprintln!("  {program_name} examples/user_modules_app/main.py --verbose");
    eprintln!();
    eprintln!("Tip: `just compile <file>` and `just optimize <file>` wrap this driver.");
}

fn parse_options(args: &[String]) -> (CompilerOptions, DriverFlags) {
    let mut options = CompilerOptions::default();
    let mut driver = DriverFlags::default();
    let mut verbose = false;
    let mut debug = false;

    for arg in args.iter().skip(1) {
        match arg.as_str() {
            "--no-optimize" => options.optimize = false,
            "--metadata" => driver.show_metadata = true,
            "--html" => driver.generate_html = true,
            "--verbose" => verbose = true,
            "--debug" => debug = true,
            other => {
                if other.starts_with("--") {
                    eprintln!("Warning: unknown option `{other}` ignored (see --help)");
                }
            }
        }
    }

    // Set verbosity level based on flags
    options.verbosity = Verbosity::from_flags(verbose, debug);

    (options, driver)
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
            eprintln!("Error extracting metadata: {err}");
        }
    }

    // Print AST structure (simplified)
    match parser::parse_python(source) {
        Ok(ast) => {
            println!("AST Structure:");
            println!("-------------");
            let ast_len = ast.len();
            println!("• Found {ast_len} top-level statements");

            let function_count = ast
                .iter()
                .filter(|stmt| matches!(stmt, rustpython_parser::ast::Stmt::FunctionDef(_)))
                .count();

            println!("• Found {function_count} function definitions");

            // Count class definitions
            let class_count = ast
                .iter()
                .filter(|stmt| matches!(stmt, rustpython_parser::ast::Stmt::ClassDef(_)))
                .count();

            if class_count > 0 {
                println!("• Found {class_count} class definitions");
            }

            println!();
        }
        Err(err) => {
            eprintln!("Error parsing Python: {err}");
        }
    }

    Ok(())
}

fn generate_html_test_file(module_name: &str, wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>Waspy Test - {module_name}</title>
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
    <p>Module: <code>{wasm_filename}</code></p>
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
                const response = await fetch('{wasm_filename}');
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
"#
    )
}
