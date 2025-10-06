//! Wasmrun plugin integration for Waspy
//!
//! This module provides the plugin interface for integrating Waspy as a wasmrun plugin.

use serde::{Deserialize, Serialize};
use std::time::Duration;

// Plugin trait definitions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum PluginType {
    Builtin,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCapabilities {
    pub compile_wasm: bool,
    pub compile_webapp: bool,
    pub live_reload: bool,
    pub optimization: bool,
    pub custom_targets: Vec<String>,
    pub supported_languages: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PluginSource {
    CratesIo { name: String, version: String },
    Git { url: String, rev: Option<String> },
    Local { path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub extensions: Vec<String>,
    pub entry_files: Vec<String>,
    pub plugin_type: PluginType,
    pub source: Option<PluginSource>,
    pub dependencies: Vec<String>,
    pub capabilities: PluginCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OptimizationLevel {
    Debug,
    Release,
    Size,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    pub input: String,
    pub output_dir: String,
    pub optimization: OptimizationLevel,
    pub target_type: String,
    pub verbose: bool,
    pub watch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub output_path: String,
    pub language: String,
    pub optimization_level: OptimizationLevel,
    pub build_time: Duration,
    pub file_size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum CompilationError {
    #[error("Build failed for {language}: {reason}")]
    BuildFailed { language: String, reason: String },

    #[error("Tool execution failed - {tool}: {reason}")]
    ToolExecutionFailed { tool: String, reason: String },

    #[error("Invalid configuration: {reason}")]
    InvalidConfiguration { reason: String },
}

pub type CompilationResult<T> = std::result::Result<T, CompilationError>;

// Plugin traits
pub trait Plugin {
    fn info(&self) -> &PluginInfo;
    fn can_handle_project(&self, project_path: &str) -> bool;
    fn get_builder(&self) -> Box<dyn WasmBuilder>;
}

pub trait WasmBuilder {
    fn can_handle_project(&self, project_path: &str) -> bool;
    fn build(&self, config: &BuildConfig) -> CompilationResult<BuildResult>;
    fn check_dependencies(&self) -> Vec<String>;
    fn validate_project(&self, project_path: &str) -> CompilationResult<()>;
    fn clean(&self, project_path: &str) -> std::result::Result<(), Box<dyn std::error::Error>>;
    fn clone_box(&self) -> Box<dyn WasmBuilder>;
    fn language_name(&self) -> &str;
    fn entry_file_candidates(&self) -> &[&str];
    fn supported_extensions(&self) -> &[&str];
}

// Waspy-specific implementations
pub struct WaspyBuilder;

impl WasmBuilder for WaspyBuilder {
    fn language_name(&self) -> &str {
        "python"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["py"]
    }

    fn entry_file_candidates(&self) -> &[&str] {
        &["main.py", "__main__.py", "app.py", "src/main.py"]
    }

    fn check_dependencies(&self) -> Vec<String> {
        // Waspy is self-contained - no external dependencies!
        vec![]
    }

    fn can_handle_project(&self, project_path: &str) -> bool {
        let path = std::path::Path::new(project_path);

        // Single Python file
        if path.is_file() {
            return path.extension().is_some_and(|ext| ext == "py");
        }

        // Directory with Python files
        if path.is_dir() {
            // Check for common Python entry points
            for candidate in self.entry_file_candidates() {
                if path.join(candidate).exists() {
                    return true;
                }
            }

            // Check for any .py files
            if let Ok(entries) = std::fs::read_dir(project_path) {
                for entry in entries.flatten() {
                    if let Some(extension) = entry.path().extension() {
                        if extension == "py" {
                            return true;
                        }
                    }
                }
            }
        }

        false
    }

    fn validate_project(&self, project_path: &str) -> CompilationResult<()> {
        if !self.can_handle_project(project_path) {
            return Err(CompilationError::BuildFailed {
                language: "python".to_string(),
                reason: format!("No Python files found in '{project_path}'"),
            });
        }
        Ok(())
    }

    fn build(&self, config: &BuildConfig) -> CompilationResult<BuildResult> {
        use crate::{
            compile_python_project_with_options, compile_python_to_wasm_with_options,
            CompilerOptions,
        };

        let start_time = std::time::Instant::now();

        // Convert BuildConfig to CompilerOptions
        let compiler_options = CompilerOptions {
            optimize: matches!(
                config.optimization,
                OptimizationLevel::Release | OptimizationLevel::Size
            ),
            debug_info: config.verbose,
            generate_html: config.target_type == "html",
            include_metadata: true,
            ..CompilerOptions::default()
        };

        // Determine if input is file or directory
        let input_path = std::path::Path::new(&config.input);

        let wasm_bytes = if input_path.is_file() {
            // Single Python file
            let source = std::fs::read_to_string(&config.input).map_err(|e| {
                CompilationError::BuildFailed {
                    language: "python".to_string(),
                    reason: format!("Failed to read file: {e}"),
                }
            })?;

            compile_python_to_wasm_with_options(&source, &compiler_options)
        } else {
            // Python project directory
            compile_python_project_with_options(&config.input, &compiler_options)
        }
        .map_err(|e| CompilationError::BuildFailed {
            language: "python".to_string(),
            reason: e.to_string(),
        })?;

        // Generate output filename
        let output_name = input_path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
            + ".wasm";

        let output_path = std::path::Path::new(&config.output_dir).join(output_name);

        // Create output directory
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CompilationError::BuildFailed {
                language: "python".to_string(),
                reason: format!("Failed to create output directory: {e}"),
            })?;
        }

        // Write WASM file
        std::fs::write(&output_path, &wasm_bytes).map_err(|e| CompilationError::BuildFailed {
            language: "python".to_string(),
            reason: format!("Failed to write output file: {e}"),
        })?;

        // Generate HTML file if html target
        if config.target_type == "html" && compiler_options.generate_html {
            let html_file = output_path.with_extension("html");
            let wasm_name = output_path.file_name().unwrap().to_str().unwrap();
            let html_content = generate_html_test_file(wasm_name);
            std::fs::write(&html_file, html_content).map_err(|e| {
                CompilationError::BuildFailed {
                    language: "python".to_string(),
                    reason: format!("Failed to write HTML file: {e}"),
                }
            })?;
        }

        let build_time = start_time.elapsed();
        let file_size = wasm_bytes.len() as u64;

        Ok(BuildResult {
            output_path: output_path.to_string_lossy().to_string(),
            language: "python".to_string(),
            optimization_level: config.optimization.clone(),
            build_time,
            file_size,
        })
    }

    fn clean(&self, project_path: &str) -> std::result::Result<(), Box<dyn std::error::Error>> {
        // Clean any waspy-generated files
        let dist_dir = std::path::Path::new(project_path).join("dist");
        if dist_dir.exists() {
            std::fs::remove_dir_all(dist_dir)?;
        }

        // Clean __pycache__ directories
        let pycache_dir = std::path::Path::new(project_path).join("__pycache__");
        if pycache_dir.exists() {
            std::fs::remove_dir_all(pycache_dir)?;
        }

        // Clean any .wasm files in the project directory
        if let Ok(entries) = std::fs::read_dir(project_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if let Some(extension) = path.extension() {
                    if extension == "wasm" || extension == "html" {
                        if let Err(e) = std::fs::remove_file(&path) {
                            eprintln!("Warning: Failed to clean {}: {e}", path.display());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn clone_box(&self) -> Box<dyn WasmBuilder> {
        Box::new(WaspyBuilder)
    }
}

pub struct WaspyPlugin {
    info: PluginInfo,
}

impl WaspyPlugin {
    pub fn new() -> Self {
        Self {
            info: PluginInfo {
                name: "waspy".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: "Python to WebAssembly compiler with advanced type support"
                    .to_string(),
                author: "Kumar Anirudha".to_string(),
                extensions: vec!["py".to_string()],
                entry_files: vec![
                    "main.py".to_string(),
                    "__main__.py".to_string(),
                    "app.py".to_string(),
                    "src/main.py".to_string(),
                ],
                plugin_type: PluginType::External,
                source: Some(PluginSource::CratesIo {
                    name: "waspy".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                }),
                dependencies: vec![], // Self-contained!
                capabilities: PluginCapabilities {
                    compile_wasm: true,
                    compile_webapp: false,
                    live_reload: false,
                    optimization: true,
                    custom_targets: vec!["wasm".to_string(), "html".to_string()],
                    supported_languages: Some(vec!["python".to_string()]),
                },
            },
        }
    }
}

impl Default for WaspyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for WaspyPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn can_handle_project(&self, project_path: &str) -> bool {
        WaspyBuilder.can_handle_project(project_path)
    }

    fn get_builder(&self) -> Box<dyn WasmBuilder> {
        Box::new(WaspyBuilder)
    }
}

/// Generate HTML test file for webapp targets
fn generate_html_test_file(wasm_filename: &str) -> String {
    let html = r#"<!DOCTYPE html>
<html>
<head>
    <title>Waspy Python WebAssembly</title>
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
        .success {{ color: #38a169; }}
        .error {{ color: #e53e3e; }}
    </style>
</head>
<body>
    <h1>🐍 Waspy Python → WebAssembly</h1>
    <p>WebAssembly Module: <code>{wasm_filename}</code></p>
    <p class="success">✅ Compiled with Waspy - Python to WebAssembly compiler</p>

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

    <h2>About Waspy</h2>
    <p>Waspy is a Python to WebAssembly compiler that supports:</p>
    <ul>
        <li>✅ Type annotations and advanced type system</li>
        <li>✅ Multi-file project compilation</li>
        <li>✅ Self-contained output (no external dependencies)</li>
        <li>✅ Control flow (if/else, while loops)</li>
        <li>✅ Mathematical operations and functions</li>
        <li>✅ Boolean and comparison operations</li>
    </ul>

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
                    ['#', 'Function Name', 'Type'].forEach(text => {{
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
                        nameCell.style.fontFamily = 'monospace';
                        row.appendChild(nameCell);

                        const typeCell = document.createElement('td');
                        typeCell.textContent = 'Python Function';
                        typeCell.style.color = '#38a169';
                        row.appendChild(typeCell);

                        table.appendChild(row);
                    }});

                    functionListDiv.innerHTML = '';
                    functionListDiv.appendChild(table);
                }} else {{
                    functionListDiv.innerHTML = '<p class="error">No functions found in the WebAssembly module.</p>';
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

                    if (!functionName) {{
                        document.getElementById('function-result').innerHTML = '<span class="error">Please select a function</span>';
                        return;
                    }}

                    // Parse arguments
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
                        document.getElementById('function-result').innerHTML =
                            `<span class="success">✅ ${{functionName}}(${{args.join(', ')}}) = ${{result}}</span>`;
                    }} catch (error) {{
                        document.getElementById('function-result').innerHTML =
                            `<span class="error">❌ Error: ${{error.message}}</span>`;
                    }}
                }});

                console.log("✅ Waspy WebAssembly module loaded successfully!");
                console.log("Available functions:", functions);
            }} catch (error) {{
                console.error("❌ Error loading WebAssembly:", error);
                document.body.innerHTML += `<div style="color: red; padding: 20px; background: #fed7d7; margin-top: 20px; border-radius: 4px;">
                    <h3>❌ Error Loading WebAssembly</h3>
                    <p>${{error.message}}</p>
                    <p>Make sure the .wasm file is in the same directory as this HTML file.</p>
                </div>`;
            }}
        }})();
    </script>
</body>
</html>
"#;

    html.replace("{wasm_filename}", wasm_filename)
}

// Plugin entry point for wasmrun
#[no_mangle]
pub extern "C" fn wasmrun_plugin_create() -> *mut std::ffi::c_void {
    let plugin = Box::new(WaspyPlugin::new());
    Box::into_raw(plugin) as *mut std::ffi::c_void
}

// Plugin factory function for library usage
pub fn create_plugin() -> Box<dyn Plugin> {
    Box::new(WaspyPlugin::new())
}

// ============================================================================
// FFI Compilation Functions
// ============================================================================

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

/// FFI-safe result structure for compilation
#[repr(C)]
pub struct WaspyCompileResult {
    pub success: bool,
    pub wasm_data: *mut u8,
    pub wasm_len: usize,
    pub error_message: *mut c_char,
}

/// Compile Python source code to WASM via FFI
///
/// # Safety
/// - `source_ptr` must be a valid null-terminated C string
/// - `optimize` should be 0 for debug, 1 for release
/// - Caller must free the returned wasm_data using `waspy_free_wasm_data`
/// - Caller must free error_message using `waspy_free_error_message`
#[no_mangle]
pub unsafe extern "C" fn waspy_compile_python(
    source_ptr: *const c_char,
    optimize: c_int,
) -> WaspyCompileResult {
    // Validate input
    if source_ptr.is_null() {
        return WaspyCompileResult {
            success: false,
            wasm_data: std::ptr::null_mut(),
            wasm_len: 0,
            error_message: CString::new("Source pointer is null").unwrap().into_raw(),
        };
    }

    // Convert C string to Rust string
    let source = match CStr::from_ptr(source_ptr).to_str() {
        Ok(s) => s,
        Err(e) => {
            return WaspyCompileResult {
                success: false,
                wasm_data: std::ptr::null_mut(),
                wasm_len: 0,
                error_message: CString::new(format!("Invalid UTF-8 in source: {e}"))
                    .unwrap()
                    .into_raw(),
            };
        }
    };

    // Set up compiler options
    let options = crate::CompilerOptions {
        optimize: optimize != 0,
        debug_info: optimize == 0,
        generate_html: false,
        include_metadata: true,
        ..crate::CompilerOptions::default()
    };

    // Compile the Python source
    match crate::compile_python_to_wasm_with_options(source, &options) {
        Ok(wasm_bytes) => {
            let len = wasm_bytes.len();
            let mut boxed_data = wasm_bytes.into_boxed_slice();
            let ptr = boxed_data.as_mut_ptr();
            std::mem::forget(boxed_data); // Prevent deallocation

            WaspyCompileResult {
                success: true,
                wasm_data: ptr,
                wasm_len: len,
                error_message: std::ptr::null_mut(),
            }
        }
        Err(e) => WaspyCompileResult {
            success: false,
            wasm_data: std::ptr::null_mut(),
            wasm_len: 0,
            error_message: CString::new(format!("Compilation failed: {e}"))
                .unwrap()
                .into_raw(),
        },
    }
}

/// Compile a Python project directory to WASM via FFI
///
/// # Safety
/// - `project_path_ptr` must be a valid null-terminated C string
/// - `optimize` should be 0 for debug, 1 for release
/// - Caller must free the returned wasm_data using `waspy_free_wasm_data`
/// - Caller must free error_message using `waspy_free_error_message`
#[no_mangle]
pub unsafe extern "C" fn waspy_compile_project(
    project_path_ptr: *const c_char,
    optimize: c_int,
) -> WaspyCompileResult {
    // Validate input
    if project_path_ptr.is_null() {
        return WaspyCompileResult {
            success: false,
            wasm_data: std::ptr::null_mut(),
            wasm_len: 0,
            error_message: CString::new("Project path pointer is null")
                .unwrap()
                .into_raw(),
        };
    }

    // Convert C string to Rust string
    let project_path = match CStr::from_ptr(project_path_ptr).to_str() {
        Ok(s) => s,
        Err(e) => {
            return WaspyCompileResult {
                success: false,
                wasm_data: std::ptr::null_mut(),
                wasm_len: 0,
                error_message: CString::new(format!("Invalid UTF-8 in path: {e}"))
                    .unwrap()
                    .into_raw(),
            };
        }
    };

    // Set up compiler options
    let options = crate::CompilerOptions {
        optimize: optimize != 0,
        debug_info: optimize == 0,
        generate_html: false,
        include_metadata: true,
        ..crate::CompilerOptions::default()
    };

    // Compile the Python project
    match crate::compile_python_project_with_options(project_path, &options) {
        Ok(wasm_bytes) => {
            let len = wasm_bytes.len();
            let mut boxed_data = wasm_bytes.into_boxed_slice();
            let ptr = boxed_data.as_mut_ptr();
            std::mem::forget(boxed_data); // Prevent deallocation

            WaspyCompileResult {
                success: true,
                wasm_data: ptr,
                wasm_len: len,
                error_message: std::ptr::null_mut(),
            }
        }
        Err(e) => WaspyCompileResult {
            success: false,
            wasm_data: std::ptr::null_mut(),
            wasm_len: 0,
            error_message: CString::new(format!("Project compilation failed: {e}"))
                .unwrap()
                .into_raw(),
        },
    }
}

/// Free WASM data allocated by waspy_compile_python or waspy_compile_project
///
/// # Safety
/// - `data` must be a pointer previously returned by a waspy_compile_* function
/// - `len` must be the length previously returned by a waspy_compile_* function
/// - Must only be called once per allocation
#[no_mangle]
pub unsafe extern "C" fn waspy_free_wasm_data(data: *mut u8, len: usize) {
    if !data.is_null() && len > 0 {
        let _ = Box::from_raw(std::slice::from_raw_parts_mut(data, len));
    }
}

/// Free error message allocated by waspy compilation functions
///
/// # Safety
/// - `error_message` must be a pointer previously returned by a waspy_compile_* function
/// - Must only be called once per allocation
#[no_mangle]
pub unsafe extern "C" fn waspy_free_error_message(error_message: *mut c_char) {
    if !error_message.is_null() {
        let _ = CString::from_raw(error_message);
    }
}
