pub mod compiler;
pub mod errors;
pub mod ir;
pub mod optimizer;
pub mod parser;
pub mod project;

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::collections::HashMap;
use std::fs;

/// Compile Python source code into a WASM binary.
pub fn compile_python_to_wasm(source: &str) -> Result<Vec<u8>> {
    compile_python_to_wasm_with_options(source, true)
}

/// Compile Python source code into a WASM binary with options.
pub fn compile_python_to_wasm_with_options(source: &str, optimize: bool) -> Result<Vec<u8>> {
    // Parse Python to AST
    let ast = parser::parse_python(source).context("Failed to parse Python code")?;

    // Lower AST to IR
    let ir_module = ir::lower_ast_to_ir(&ast).context("Failed to convert Python AST to IR")?;

    // Generate WASM binary
    let raw_wasm = compiler::compile_ir_module(&ir_module);

    // Optimize the WASM binary
    if optimize {
        optimizer::optimize_wasm(&raw_wasm).context("Failed to optimize WebAssembly binary")
    } else {
        Ok(raw_wasm)
    }
}

/// Compile multiple Python source files into a single WASM binary.
pub fn compile_multiple_python_files(sources: &[(&str, &str)], optimize: bool) -> Result<Vec<u8>> {
    // Parse and convert each Python source to IR
    let mut all_functions = Vec::new();
    let mut function_names = std::collections::HashSet::new();

    for (filename, source) in sources {
        // Skip incompatible files
        if is_special_python_file(filename) {
            println!("Skipping special file: {}", filename);
            continue;
        }

        // Check if file contains function definitions
        if !contains_function_definitions(source) {
            println!("Skipping file with no functions: {}", filename);
            continue;
        }

        // Parse Python to AST
        let ast = match parser::parse_python(source) {
            Ok(ast) => ast,
            Err(e) => {
                println!("Warning: Failed to parse {}: {}", filename, e);
                continue;
            }
        };

        // Lower AST to IR
        let ir_module = match ir::lower_ast_to_ir(&ast) {
            Ok(module) => module,
            Err(e) => {
                println!("Warning: Failed to convert {} to IR: {}", filename, e);
                continue;
            }
        };

        // Skip if no functions
        if ir_module.functions.is_empty() {
            println!("Skipping file with no valid functions: {}", filename);
            continue;
        }

        // Check for duplicate function names and add functions
        for func in ir_module.functions {
            if !function_names.insert(func.name.clone()) {
                println!("Warning: Duplicate function '{}' found in file: {}", func.name, filename);
                // Skip the duplicate but continue processing
            } else {
                // Add the function (not a reference)
                all_functions.push(func);
            }
        }
    }

    if all_functions.is_empty() {
        return Err(anyhow!("No valid functions found in any of the provided files"));
    }

    // Create a combined IR module
    let combined_module = ir::IRModule {
        functions: all_functions,
    };

    // Generate WASM binary from the combined module
    let raw_wasm = compiler::compile_ir_module(&combined_module);

    // Optimize the WASM binary
    if optimize {
        optimizer::optimize_wasm(&raw_wasm).context("Failed to optimize WebAssembly binary")
    } else {
        Ok(raw_wasm)
    }
}

/// Compile a Python project directory to WebAssembly
pub fn compile_python_project<P: AsRef<Path>>(
    project_dir: P,
    optimize: bool,
) -> Result<Vec<u8>> {
    // Load and analyze the project
    let project_dir = project_dir.as_ref();
    
    println!("Analyzing project structure...");
    let files = collect_compilable_python_files(project_dir)?;
    
    if files.is_empty() {
        return Err(anyhow!("No compilable Python files found in the project"));
    }
    
    println!("Found {} compilable Python files", files.len());
    
    // Convert to the format expected by compile_multiple_python_files
    let sources: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();
    
    // Compile all files together
    compile_multiple_python_files(&sources, optimize)
        .context("Failed to compile Python project")
}

/// Collect Python files that can be compiled to WebAssembly
fn collect_compilable_python_files(dir: &Path) -> Result<HashMap<String, String>> {
    let mut files = HashMap::new();
    
    // Files to exclude
    let exclude_files = vec![
        "__init__.py",
        "__about__.py",
        "__version__.py",
        "__main__.py",
        "setup.py",
    ];
    
    // Directories to exclude
    let exclude_dirs = vec![
        "venv",
        "env",
        ".venv",
        ".env",
        ".git",
        "__pycache__",
        "node_modules",
        "site-packages",
        "dist",
        "build",
        "tests",
        "docs",
    ];
    
    // Recursively collect Python files
    collect_python_files_recursive(dir, dir, &mut files, &exclude_files, &exclude_dirs)?;
    
    // Further filter files based on content
    let mut compilable_files = HashMap::new();
    
    for (path, content) in files {
        // Skip files without function definitions
        if !contains_function_definitions(&content) {
            println!("Skipping {} (no functions)", path);
            continue;
        }
        
        // Skip files with import errors or other issues
        if has_complex_imports(&content) {
            println!("Skipping {} (complex imports)", path);
            continue;
        }
        
        // Skip files with module-level code that's not a function definition
        if has_module_level_code(&content) {
            println!("Skipping {} (module-level code)", path);
            continue;
        }
        
        // Add compilable file
        compilable_files.insert(path, content);
    }
    
    Ok(compilable_files)
}

/// Recursively collect Python files from a directory
fn collect_python_files_recursive(
    root_dir: &Path,
    current_dir: &Path,
    files: &mut HashMap<String, String>,
    exclude_files: &[&str],
    exclude_dirs: &[&str],
) -> Result<()> {
    for entry in fs::read_dir(current_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        if path.is_dir() {
            // Check if directory should be excluded
            if let Some(dir_name) = path.file_name() {
                let dir_name = dir_name.to_string_lossy();
                if exclude_dirs.iter().any(|&d| dir_name == d) || dir_name.starts_with('.') {
                    continue;
                }
            }
            
            // Recursively scan subdirectory
            collect_python_files_recursive(root_dir, &path, files, exclude_files, exclude_dirs)?;
        } else if path.is_file() && path.extension().map_or(false, |ext| ext == "py") {
            // Check if file should be excluded
            if let Some(file_name) = path.file_name() {
                let file_name = file_name.to_string_lossy();
                if exclude_files.iter().any(|&f| file_name == f) {
                    continue;
                }
            }
            
            // Read file content
            match fs::read_to_string(&path) {
                Ok(content) => {
                    // Use relative path as key
                    let rel_path = path.strip_prefix(root_dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    
                    files.insert(rel_path, content);
                }
                Err(e) => {
                    println!("Warning: Failed to read {}: {}", path.display(), e);
                }
            }
        }
    }
    
    Ok(())
}

/// Check if a file is a special Python file that's not suitable for compilation
fn is_special_python_file(filename: &str) -> bool {
    let filename = Path::new(filename).file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    
    filename.starts_with("__") || 
    filename == "setup.py" || 
    filename.contains("test") ||
    filename.contains("config")
}

/// Check if a Python file contains function definitions
fn contains_function_definitions(content: &str) -> bool {
    for line in content.lines() {
        if line.trim().starts_with("def ") {
            return true;
        }
    }
    false
}

/// Check if a Python file has complex imports that might not be supported
fn has_complex_imports(content: &str) -> bool {
    for line in content.lines().take(30) { // Check first 30 lines
        let line = line.trim();
        if line.starts_with("import ") || line.starts_with("from ") {
            // Complex import patterns
            if line.contains("*") || line.contains(" as ") || 
               line.contains("(") || line.contains(")") ||
               line.contains("try:") || line.contains("except") {
                return true;
            }
        }
    }
    false
}

/// Check if a Python file has module-level code (outside functions)
fn has_module_level_code(content: &str) -> bool {
    let mut in_function = false;
    let mut in_docstring = false;
    let mut last_line_blank = true;
    
    for line in content.lines() {
        let trimmed = line.trim();
        
        // Skip empty lines and comments
        if trimmed.is_empty() {
            last_line_blank = true;
            continue;
        }
        if trimmed.starts_with("#") {
            continue;
        }
        
        // Check for docstrings
        if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
            in_docstring = !in_docstring;
            continue;
        }
        
        // Skip if in docstring
        if in_docstring {
            continue;
        }
        
        // Check for function definition
        if trimmed.starts_with("def ") {
            in_function = true;
            last_line_blank = false;
            continue;
        }
        
        // Check for class definition
        if trimmed.starts_with("class ") {
            in_function = false;
            last_line_blank = false;
            continue;
        }
        
        // Check for end of function/class
        if last_line_blank && !trimmed.starts_with(" ") && !trimmed.starts_with("\t") {
            in_function = false;
        }
        
        // Check for module-level code
        if !in_function && !trimmed.starts_with("import ") && !trimmed.starts_with("from ") {
            // Allow some common module-level declarations
            if !trimmed.starts_with("__") && !trimmed.contains(" = ") {
                return true;
            }
        }
        
        last_line_blank = false;
    }
    
    false
}

/// Get metadata about a Python source file without compiling to WASM.
/// Returns a list of function signatures for documentation or analysis.
pub fn get_python_file_metadata(source: &str) -> Result<Vec<FunctionSignature>> {
    // Parse Python to AST
    let ast = parser::parse_python(source).context("Failed to parse Python code")?;

    // Lower AST to IR
    let ir_module = ir::lower_ast_to_ir(&ast).context("Failed to convert Python AST to IR")?;

    // Extract function signatures
    let mut signatures = Vec::new();
    for func in &ir_module.functions {
        let param_types: Vec<String> = func
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, type_to_string(&p.param_type)))
            .collect();

        signatures.push(FunctionSignature {
            name: func.name.clone(),
            parameters: param_types,
            return_type: type_to_string(&func.return_type),
        });
    }

    Ok(signatures)
}

/// Get metadata about an entire Python project.
/// Returns a list of function signatures for all files.
pub fn get_python_project_metadata<P: AsRef<Path>>(
    project_dir: P,
) -> Result<Vec<(String, Vec<FunctionSignature>)>> {
    let project_dir = project_dir.as_ref();
    let files = collect_compilable_python_files(project_dir)?;
    
    let mut all_metadata = Vec::new();
    
    for (path, content) in files {
        match get_python_file_metadata(&content) {
            Ok(signatures) => {
                if !signatures.is_empty() {
                    all_metadata.push((path, signatures));
                }
            }
            Err(e) => {
                println!("Warning: Failed to extract metadata from {}: {}", path, e);
            }
        }
    }
    
    Ok(all_metadata)
}

/// Convert IR type to string
fn type_to_string(ir_type: &ir::IRType) -> String {
    match ir_type {
        ir::IRType::Int => "int".to_string(),
        ir::IRType::Float => "float".to_string(),
        ir::IRType::Bool => "bool".to_string(),
        ir::IRType::String => "str".to_string(),
        ir::IRType::List(elem_type) => format!("List[{}]", type_to_string(elem_type)),
        ir::IRType::Dict(key_type, val_type) => format!(
            "Dict[{}, {}]",
            type_to_string(key_type),
            type_to_string(val_type)
        ),
        ir::IRType::None => "None".to_string(),
        ir::IRType::Any => "Any".to_string(),
        ir::IRType::Unknown => "unknown".to_string(),
    }
}

/// Function's signature
pub struct FunctionSignature {
    pub name: String,
    pub parameters: Vec<String>,
    pub return_type: String,
}
