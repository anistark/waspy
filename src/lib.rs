pub mod compiler;
pub mod config;
pub mod entry_points;
pub mod errors;
pub mod ir;
pub mod optimizer;
pub mod parser;
pub mod project;

use crate::config::{is_config_file, load_project_config, ProjectConfig};
use crate::entry_points::{add_entry_point_to_module, detect_entry_points, EntryPointInfo};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Compile Python source code into a WASM binary.
pub fn compile_python_to_wasm(source: &str) -> Result<Vec<u8>> {
    compile_python_to_wasm_with_options(source, true)
}

/// Compile Python source code into a WASM binary with options.
pub fn compile_python_to_wasm_with_options(source: &str, optimize: bool) -> Result<Vec<u8>> {
    // Parse Python to AST
    let ast = parser::parse_python(source).context("Failed to parse Python code")?;

    // Lower AST to IR
    let mut ir_module = ir::lower_ast_to_ir(&ast).context("Failed to convert Python AST to IR")?;

    // Check for entry points
    if let Ok(Some(entry_point_info)) = detect_entry_points(source, None) {
        // Add entry point support if detected
        add_entry_point_to_module(&mut ir_module, &entry_point_info)?;
    }

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
    let mut combined_module = crate::ir::IRModule::new();
    let mut function_names = std::collections::HashSet::new();
    let mut has_entry_point = false;
    let mut entry_point_info: Option<EntryPointInfo> = None;

    for (filename, source) in sources {
        // Skip incompatible files
        if is_special_python_file(filename) {
            println!("Skipping special file: {}", filename);
            continue;
        }

        // Check for entry points
        if !has_entry_point {
            if let Ok(Some(info)) = detect_entry_points(source, Some(Path::new(filename))) {
                has_entry_point = true;
                entry_point_info = Some(info);
                println!("Detected entry point in file: {}", filename);
            }
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
            println!("Skipping file with no functions: {}", filename);
            continue;
        }

        // Check for duplicate function names and add functions
        for func in ir_module.functions {
            if !function_names.insert(func.name.clone()) {
                println!(
                    "Warning: Duplicate function '{}' found in file: {}",
                    func.name, filename
                );
                // Skip the duplicate but continue processing
            } else {
                // Add the function
                combined_module.functions.push(func);
            }
        }

        // Add module-level variables and imports (might use these later)
        combined_module.variables.extend(ir_module.variables);
        combined_module.imports.extend(ir_module.imports);
        combined_module.classes.extend(ir_module.classes);
    }

    if combined_module.functions.is_empty() {
        return Err(anyhow!(
            "No valid functions found in any of the provided files"
        ));
    }

    // Add entry point if one was detected
    if has_entry_point {
        if let Some(info) = entry_point_info {
            add_entry_point_to_module(&mut combined_module, &info)?;
        }
    }

    // Generate WASM binary from the combined module
    let raw_wasm = compiler::compile_ir_module(&combined_module);

    // Optimize the WASM binary
    if optimize {
        optimizer::optimize_wasm(&raw_wasm).context("Failed to optimize WebAssembly binary")
    } else {
        Ok(raw_wasm)
    }
}

/// Compile multiple Python source files into a single WASM binary with config awareness.
pub fn compile_multiple_python_files_with_config(
    sources: &[(&str, &str)],
    optimize: bool,
    config: &ProjectConfig,
) -> Result<Vec<u8>> {
    // Parse and convert each Python source to IR
    let mut combined_module = crate::ir::IRModule::new();
    let mut function_names = std::collections::HashSet::new();
    let mut has_entry_point = false;
    let mut entry_point_info: Option<EntryPointInfo> = None;

    // Set project metadata if available
    if !config.name.is_empty() {
        combined_module
            .metadata
            .insert("project_name".to_string(), config.name.clone());
        combined_module
            .metadata
            .insert("project_version".to_string(), config.version.clone());

        if let Some(description) = &config.description {
            combined_module
                .metadata
                .insert("project_description".to_string(), description.clone());
        }

        if let Some(author) = &config.author {
            combined_module
                .metadata
                .insert("project_author".to_string(), author.clone());
        }
    }

    for (filename, source) in sources {
        // Skip incompatible files
        if is_config_file(filename) {
            println!("Skipping configuration file: {}", filename);
            continue;
        }

        // Skip incompatible files
        if is_special_python_file(filename) {
            println!("Skipping special file: {}", filename);
            continue;
        }

        // Check for entry points
        if !has_entry_point {
            if let Ok(Some(info)) = detect_entry_points(source, Some(Path::new(filename))) {
                has_entry_point = true;
                entry_point_info = Some(info);
                println!("Detected entry point in file: {}", filename);
            }
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
            println!("Skipping file with no functions: {}", filename);
            continue;
        }

        // Check for duplicate function names and add functions
        for func in ir_module.functions {
            if !function_names.insert(func.name.clone()) {
                println!(
                    "Warning: Duplicate function '{}' found in file: {}",
                    func.name, filename
                );
                // Skip the duplicate but continue processing
            } else {
                // Add the function
                combined_module.functions.push(func);
            }
        }

        // Add module-level variables and imports
        combined_module.variables.extend(ir_module.variables);
        combined_module.imports.extend(ir_module.imports);
        combined_module.classes.extend(ir_module.classes);

        // Add module-level metadata
        for (key, value) in ir_module.metadata {
            combined_module.metadata.insert(key, value);
        }
    }

    if combined_module.functions.is_empty() {
        return Err(anyhow!(
            "No valid functions found in any of the provided files"
        ));
    }

    // Add entry point if one was detected
    if has_entry_point {
        if let Some(info) = entry_point_info {
            add_entry_point_to_module(&mut combined_module, &info)?;
        }
    }

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
pub fn compile_python_project<P: AsRef<Path>>(project_dir: P, optimize: bool) -> Result<Vec<u8>> {
    // Load and analyze the project
    let project_dir = project_dir.as_ref();

    println!("Analyzing project structure...");

    // Load project configuration
    let config = load_project_config(project_dir)?;

    println!("Project Name: {}", config.name);
    println!("Project Version: {}", config.version);
    if let Some(description) = &config.description {
        println!("Description: {}", description);
    }
    if let Some(author) = &config.author {
        println!("Author: {}", author);
    }

    let files = collect_compilable_python_files(project_dir)?;

    if files.is_empty() {
        return Err(anyhow!("No compilable Python files found in the project"));
    }

    // Look for entry points in the project
    let mut entry_point_file = None;
    let mut entry_point_info = None;

    // First, check for __main__.py
    let main_py_path = project_dir.join("__main__.py");
    if main_py_path.exists() && main_py_path.is_file() {
        if let Ok(content) = fs::read_to_string(&main_py_path) {
            if let Ok(Some(info)) = detect_entry_points(&content, Some(&main_py_path)) {
                entry_point_file = Some("__main__.py".to_string());
                entry_point_info = Some(info);
            }
        }
    }

    // If no __main__.py, check other files for entry points
    if entry_point_info.is_none() {
        for (path, content) in &files {
            if let Ok(Some(info)) = detect_entry_points(content, Some(Path::new(path))) {
                entry_point_file = Some(path.clone());
                entry_point_info = Some(info);
                break;
            }
        }
    }

    if let Some(file) = &entry_point_file {
        println!("Found entry point in file: {}", file);
    }

    println!("Found {} compilable Python files", files.len());

    // Convert to the format expected by compile_multiple_python_files
    let sources: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, content)| (path.as_str(), content.as_str()))
        .collect();

    // Compile all files together
    let result = compile_multiple_python_files_with_config(&sources, optimize, &config)?;

    // If we found an entry point, we might need to add special handling here
    if entry_point_info.is_some() {
        // We've already integrated this in compile_multiple_python_files_with_config
        // But could add any additional entry point processing here
    }

    Ok(result)
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

        // Check for module-level code
        let has_module_level = has_module_level_code(&content);

        // With new IR support, we can handle some module-level code
        // but let's still skip complex cases
        if has_module_level && has_complex_module_level_code(&content) {
            println!("Skipping {} (complex module-level code)", path);
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
                    let rel_path = path
                        .strip_prefix(root_dir)
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
    let filename = Path::new(filename)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    filename.starts_with("__")
        || filename == "setup.py"
        || filename.contains("test")
        || filename.contains("config")
        || is_config_file(&filename)
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
    for line in content.lines().take(30) {
        // Check first 30 lines
        let line = line.trim();
        if line.starts_with("import ") || line.starts_with("from ") {
            // Complex import patterns
            if line.contains("*")
                || line.contains("(")
                || line.contains(")")
                || line.contains("try:")
                || line.contains("except")
            {
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

/// Check if a Python file has complex module-level code that we can't handle
fn has_complex_module_level_code(content: &str) -> bool {
    let mut in_function = false;
    let mut in_docstring = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with("#") {
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

        // Check for function/class start/end
        if trimmed.starts_with("def ") || trimmed.starts_with("class ") {
            in_function = true;
            continue;
        }

        if trimmed.starts_with("return") && !in_function {
            // Return statement outside of function
            return true;
        }

        // Check for complex module-level code
        if !in_function && !trimmed.starts_with("import ") && !trimmed.starts_with("from ") {
            // These are module-level assignments/operations we can't handle yet
            if trimmed.contains("if ")
                || trimmed.contains("for ")
                || trimmed.contains("while ")
                || trimmed.contains("with ")
                || trimmed.contains("try:")
                || trimmed.contains("except ")
                || trimmed.contains("lambda ")
                || trimmed.contains("yield ")
                || trimmed.contains("raise ")
            {
                return true;
            }

            // Function or method calls at module level
            if trimmed.contains("(") && trimmed.contains(")") && !trimmed.contains(" = ") {
                return true;
            }
        }
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
fn type_to_string(ir_type: &crate::ir::IRType) -> String {
    match ir_type {
        crate::ir::IRType::Int => "int".to_string(),
        crate::ir::IRType::Float => "float".to_string(),
        crate::ir::IRType::Bool => "bool".to_string(),
        crate::ir::IRType::String => "str".to_string(),
        crate::ir::IRType::List(elem_type) => format!("List[{}]", type_to_string(elem_type)),
        crate::ir::IRType::Dict(key_type, val_type) => format!(
            "Dict[{}, {}]",
            type_to_string(key_type),
            type_to_string(val_type)
        ),
        crate::ir::IRType::Tuple(types) => {
            let inner = types
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("Tuple[{}]", inner)
        }
        crate::ir::IRType::Optional(inner) => format!("Optional[{}]", type_to_string(inner)),
        crate::ir::IRType::Union(types) => {
            let inner = types
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(" | ");
            format!("Union[{}]", inner)
        }
        crate::ir::IRType::Class(name) => name.clone(),
        crate::ir::IRType::Module(name) => format!("Module[{}]", name), // Add handling for Module type
        crate::ir::IRType::None => "None".to_string(),
        crate::ir::IRType::Any => "Any".to_string(),
        crate::ir::IRType::Unknown => "unknown".to_string(),
    }
}

/// Function's signature
pub struct FunctionSignature {
    pub name: String,
    pub parameters: Vec<String>,
    pub return_type: String,
}
