use chakrapy::{compile_python_to_wasm, get_python_file_metadata};
use std::fs;
use std::path::Path;
use std::time::Instant;

/// Example of compiling a file with module-level code
///
/// This example demonstrates the enhanced capabilities of ChakraPy
/// to handle module-level variables and class definitions.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let python_file = Path::new("examples/module_level_demo.py");
    let output_path = Path::new("examples/module_level_demo.wasm");
    let html_path = Path::new("examples/module_level_demo.html");

    println!("ChakraPy Module-Level Compilation Test");
    println!("Input file: {}", python_file.display());
    println!("Output file: {}", output_path.display());

    // Read the Python source file
    let source = fs::read_to_string(python_file)?;

    // Show metadata
    println!("\nFile metadata:");
    match get_python_file_metadata(&source) {
        Ok(signatures) => {
            for sig in signatures {
                println!(
                    "- def {}({}) -> {}",
                    sig.name,
                    sig.parameters.join(", "),
                    sig.return_type
                );
            }
        }
        Err(e) => {
            println!("Error extracting metadata: {}", e);
        }
    }

    // Start the compilation process
    println!("\nCompiling...");
    let start = Instant::now();

    let wasm = compile_python_to_wasm(&source)?;

    let duration = start.elapsed();
    println!("Compilation completed in {:.2?}", duration);

    // Write the WebAssembly to a file
    fs::write(output_path, &wasm)?;
    println!("Wrote WebAssembly to {}", output_path.display());
    println!("Output size: {} bytes", wasm.len());

    // Generate an HTML test file
    let html = generate_html_test_file(output_path.file_name().unwrap().to_str().unwrap());
    fs::write(html_path, html)?;
    println!("Wrote test HTML to {}", html_path.display());

    println!("\nCompilation successful!");
    Ok(())
}

/// Generate a simple HTML test file for trying out the WebAssembly
fn generate_html_test_file(wasm_filename: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
    <title>ChakraPy Module-Level Demo</title>
    <style>
        body {{ font-family: system-ui, sans-serif; margin: 0; padding: 20px; line-height: 1.6; }}
        .container {{ max-width: 1200px; margin: 0 auto; }}
        h1, h2, h3 {{ margin-top: 2rem; color: #333; }}
        .card {{ background: #fff; border-radius: 8px; box-shadow: 0 2px 8px rgba(0,0,0,0.1); padding: 20px; margin-bottom: 20px; }}
        .result {{ margin-top: 10px; padding: 10px; background-color: #f5f5f5; border-radius: 4px; font-family: monospace; }}
        pre {{ background-color: #f8f8f8; padding: 10px; border-radius: 5px; overflow-x: auto; }}
        .tabs {{ display: flex; margin-bottom: -1px; }}
        .tab {{ padding: 10px 20px; cursor: pointer; border: 1px solid #ddd; background: #f5f5f5; border-radius: 8px 8px 0 0; margin-right: 5px; }}
        .tab.active {{ background: white; border-bottom: 1px solid white; }}
        .tab-content {{ display: none; border: 1px solid #ddd; padding: 20px; border-radius: 0 8px 8px 8px; }}
        .tab-content.active {{ display: block; }}
        button {{ background: #4CAF50; color: white; border: none; padding: 10px 15px; border-radius: 4px; cursor: pointer; font-size: 14px; }}
        button:hover {{ background: #45a049; }}
        .button-row {{ display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 10px; }}
    </style>
</head>
<body>
    <div class="container">
        <h1>ChakraPy Module-Level Demo</h1>
        
        <div class="card">
            <h2>Module-Level Variables Test</h2>
            <p>Click the buttons below to test functions that use module-level variables:</p>
            
            <div class="button-row">
                <button onclick="testFunction('get_pi')">Get PI</button>
                <button onclick="testFunction('is_debug_mode')">Check Debug Mode</button>
                <button onclick="testFunction('get_message')">Get Message</button>
                <button onclick="testFunction('get_version')">Get Version</button>
            </div>
            
            <div class="result" id="module-result">Results will appear here</div>
        </div>
        
        <div class="card">
            <h2>Circle Calculations</h2>
            <p>Calculate properties of a circle using PI:</p>
            
            <div>
                Radius: <input type="number" id="radius-input" value="5" style="width: 100px;">
                <button onclick="calculateCircle()">Calculate</button>
            </div>
            
            <div class="result" id="circle-result">Results will appear here</div>
        </div>
        
        <div class="card">
            <h2>Custom Function Test</h2>
            <p>Test any exported function:</p>
            
            <div>
                <select id="function-select">
                    <option value="">Select a function</option>
                </select>
                
                <input type="text" id="arguments" placeholder="Arguments (comma separated)" style="width: 300px;">
                <button onclick="runCustomTest()">Run Function</button>
            </div>
            
            <div class="result" id="custom-result">Results will appear here</div>
        </div>
    </div>

    <script>
        let wasmInstance = null;
        let availableFunctions = [];
        
        // Load the WebAssembly module
        async function loadWasm() {{
            try {{
                const response = await fetch('{wasm_filename}');
                const bytes = await response.arrayBuffer();
                const {{ instance }} = await WebAssembly.instantiate(bytes);
                
                wasmInstance = instance;
                availableFunctions = Object.keys(instance.exports)
                    .filter(name => typeof instance.exports[name] === 'function');
                
                // Populate function selector
                const selector = document.getElementById('function-select');
                selector.innerHTML = '<option value="">Select a function</option>';
                
                availableFunctions.forEach(name => {{
                    const option = document.createElement('option');
                    option.value = name;
                    option.textContent = name;
                    selector.appendChild(option);
                }});
                
                console.log("WebAssembly module loaded successfully!");
                console.log("Available functions:", availableFunctions);
            }} catch (error) {{
                console.error("Error loading WebAssembly:", error);
                document.body.innerHTML += `
                    <div style="color: white; background: #d9534f; padding: 20px; margin-top: 20px; border-radius: 5px;">
                        <h3>Error Loading WebAssembly</h3>
                        <p>${{error.message}}</p>
                    </div>`;
            }}
        }}
        
        function testFunction(name) {{
            if (!wasmInstance) {{
                document.getElementById('module-result').textContent = 'WebAssembly not loaded yet!';
                return;
            }}
            
            try {{
                const result = wasmInstance.exports[name]();
                document.getElementById('module-result').textContent = `${{name}}() = ${{result}}`;
            }} catch (error) {{
                document.getElementById('module-result').textContent = `Error: ${{error.message}}`;
            }}
        }}
        
        function calculateCircle() {{
            if (!wasmInstance) {{
                document.getElementById('circle-result').textContent = 'WebAssembly not loaded yet!';
                return;
            }}
            
            try {{
                const radius = parseFloat(document.getElementById('radius-input').value);
                const area = wasmInstance.exports.calculate_circle_area(radius);
                const circumference = wasmInstance.exports.calculate_circle_circumference(radius);
                
                document.getElementById('circle-result').textContent = 
                    `Circle with radius ${{radius}}:\\n` +
                    `  Area = ${{area}}\\n` +
                    `  Circumference = ${{circumference}}`;
            }} catch (error) {{
                document.getElementById('circle-result').textContent = `Error: ${{error.message}}`;
            }}
        }}
        
        function runCustomTest() {{
            if (!wasmInstance) {{
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
                // Try to parse as number
                const num = parseFloat(trimmed);
                return isNaN(num) ? trimmed : num;
            }}) : [];
            
            try {{
                const result = wasmInstance.exports[functionName](...args);
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
"#,
        wasm_filename = wasm_filename
    )
}
