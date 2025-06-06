// A minimal example to demonstrate entry point support

use waspy::compile_python_to_wasm;
use std::fs;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Waspy Minimal Entry Point Demo");
    println!("--------------------------------");

    // Create a very minimal Python file that should parse and compile
    let python_code = r#"
def add(a, b):
    c = a + b
    return c

def main():
    result = add(5, 7)
    return result

if __name__ == "__main__":
    main()
"#;

    println!("Created minimal Python file with entry point");

    // Write the file to disk
    let file_path = Path::new("examples/minimal_entry_point.py");
    fs::write(file_path, python_code)?;

    println!("Compiling to WebAssembly...");

    // Try to compile the file
    match compile_python_to_wasm(python_code) {
        Ok(wasm) => {
            let output_path = file_path.with_extension("wasm");
            fs::write(&output_path, &wasm)?;
            println!("✅ Successfully compiled to {}", output_path.display());
            println!("   WebAssembly size: {} bytes", wasm.len());

            // Create a simple HTML test file
            let html_content = format!(
                r#"<!DOCTYPE html>
<html>
<head>
    <title>Minimal Entry Point Test</title>
    <style>
        body {{ font-family: system-ui, sans-serif; margin: 20px; }}
        pre {{ background: #f5f5f5; padding: 10px; border-radius: 5px; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f0f0f0; }}
    </style>
</head>
<body>
    <h1>Minimal Entry Point Test</h1>
    
    <h2>Source Code</h2>
    <pre>{}</pre>
    
    <h2>Test Functions</h2>
    <button id="run-add">Test add(3, 4)</button>
    <div class="result" id="add-result">Result will appear here</div>
    
    <button id="run-main">Run main()</button>
    <div class="result" id="main-result">Result will appear here</div>
    
    <script>
        // Load the WebAssembly module
        (async () => {{
            try {{
                const response = await fetch('minimal_entry_point.wasm');
                const bytes = await response.arrayBuffer();
                const {{ instance }} = await WebAssembly.instantiate(bytes);
                
                console.log("Available functions:", Object.keys(instance.exports).filter(
                    name => typeof instance.exports[name] === 'function'
                ));
                
                document.getElementById('run-add').addEventListener('click', () => {{
                    try {{
                        const result = instance.exports.add(3, 4);
                        document.getElementById('add-result').textContent = `add(3, 4) = ${{result}}`;
                    }} catch (error) {{
                        document.getElementById('add-result').textContent = `Error: ${{error.message}}`;
                    }}
                }});
                
                document.getElementById('run-main').addEventListener('click', () => {{
                    try {{
                        const result = instance.exports.main();
                        document.getElementById('main-result').textContent = `main() = ${{result}}`;
                    }} catch (error) {{
                        document.getElementById('main-result').textContent = `Error: ${{error.message}}`;
                    }}
                }});
            }} catch (error) {{
                console.error("Error loading WebAssembly:", error);
                document.body.innerHTML += `<div style="color: red">Error loading WebAssembly: ${{error.message}}</div>`;
            }}
        }})();
    </script>
</body>
</html>
"#,
                python_code
            );

            let html_path = file_path.with_extension("html");
            fs::write(&html_path, html_content)?;
            println!("✅ Created HTML test file: {}", html_path.display());
        }
        Err(e) => {
            println!("❌ Compilation failed: {}", e);
        }
    }

    println!("\nDemo completed");
    Ok(())
}
