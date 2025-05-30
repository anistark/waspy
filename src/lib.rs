//! Waspy: A Python to WebAssembly compiler written in Rust.
//!
//! Waspy translates Python functions into WebAssembly, allowing Python code
//! to run in browsers and other WebAssembly environments.
//!
//! # Overview
//!
//! The compiler follows a pipeline:
//! 1. Parse Python source code into an AST using RustPython parser
//! 2. Convert the AST to a custom Intermediate Representation (IR)
//! 3. Generate WebAssembly code from the IR
//! 4. Optionally optimize the WebAssembly binary
//!
//! # Example
//!
//! ```
//! use waspy::compile_python_to_wasm;
//!
//! fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let python_code = r#"
//!     def add(a: int, b: int) -> int:
//!         return a + b
//!     "#;
//!     
//!     let wasm = compile_python_to_wasm(python_code)?;
//!     // Use the WebAssembly binary
//!     Ok(())
//! }
//! ```

pub mod analysis;
pub mod compiler;
pub mod core;
pub mod ir;
pub mod optimize;
pub mod utils;

use crate::core::config::ProjectConfig;
pub use crate::core::options::CompilerOptions;
use crate::ir::{EntryPointInfo, IRType};
use anyhow::{anyhow, Context, Result};
use std::fs;
use std::path::Path;

/// Compile Python source code into a WASM binary using default options.
///
/// # Arguments
///
/// * `source` - Python source code to compile
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_python_to_wasm(source: &str) -> Result<Vec<u8>> {
    compile_python_to_wasm_with_options(source, &CompilerOptions::default())
}

/// Compile Python source code into a WASM binary with specified options.
///
/// # Arguments
///
/// * `source` - Python source code to compile
/// * `options` - Compiler options
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_python_to_wasm_with_options(
    source: &str,
    options: &CompilerOptions,
) -> Result<Vec<u8>> {
    // Parse Python to AST
    let ast = core::parser::parse_python(source).context("Failed to parse Python code")?;

    // Lower AST to IR
    let mut ir_module = ir::lower_ast_to_ir(&ast).context("Failed to convert Python AST to IR")?;

    // Process decorators
    let decorator_registry = ir::DecoratorRegistry::new();
    ir_module.functions = ir_module
        .functions
        .into_iter()
        .map(|func| {
            if !func.decorators.is_empty() {
                decorator_registry.apply_decorators(func)
            } else {
                func
            }
        })
        .collect();

    // Check for entry points
    if let Ok(Some(entry_point_info)) = ir::detect_entry_points(source, None) {
        // Add entry point support if detected
        ir::add_entry_point_to_module(&mut ir_module, &entry_point_info)?;
    }

    // Generate WASM binary
    let raw_wasm = compiler::compile_ir_module(&ir_module);

    // Optimize the WASM binary if requested
    if options.optimize {
        optimize::optimize_wasm(&raw_wasm).context("Failed to optimize WebAssembly binary")
    } else {
        Ok(raw_wasm)
    }
}

/// Compile multiple Python source files into a single WASM binary.
///
/// # Arguments
///
/// * `sources` - Array of (filename, source code) pairs
/// * `optimize` - Whether to optimize the output
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_multiple_python_files(sources: &[(&str, &str)], optimize: bool) -> Result<Vec<u8>> {
    let options = CompilerOptions {
        optimize,
        ..CompilerOptions::default()
    };

    compile_multiple_python_files_with_options(sources, &options)
}

/// Compile multiple Python source files with options.
///
/// # Arguments
///
/// * `sources` - Array of (filename, source code) pairs
/// * `options` - Compiler options
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_multiple_python_files_with_options(
    sources: &[(&str, &str)],
    options: &CompilerOptions,
) -> Result<Vec<u8>> {
    // Parse and convert each Python source to IR
    let mut combined_module = ir::IRModule::new();
    let mut function_names = std::collections::HashSet::new();
    let mut has_entry_point = false;
    let mut entry_point_info: Option<EntryPointInfo> = None;

    for (filename, source) in sources {
        // Skip incompatible files
        if utils::is_special_python_file(filename) {
            println!("Skipping special file: {}", filename);
            continue;
        }

        // Check for entry points
        if !has_entry_point {
            if let Ok(Some(info)) = ir::detect_entry_points(source, Some(Path::new(filename))) {
                has_entry_point = true;
                entry_point_info = Some(info);
                println!("Detected entry point in file: {}", filename);
            }
        }

        // Parse Python to AST
        let ast = match core::parser::parse_python(source) {
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

    // Process decorators on the combined module
    let decorator_registry = ir::DecoratorRegistry::new();
    combined_module.functions = combined_module
        .functions
        .into_iter()
        .map(|func| {
            if !func.decorators.is_empty() {
                decorator_registry.apply_decorators(func)
            } else {
                func
            }
        })
        .collect();

    // Add entry point if one was detected
    if has_entry_point {
        if let Some(info) = entry_point_info {
            ir::add_entry_point_to_module(&mut combined_module, &info)?;
        }
    }

    // Generate WASM binary from the combined module
    let raw_wasm = compiler::compile_ir_module(&combined_module);

    // Optimize the WASM binary
    if options.optimize {
        optimize::optimize_wasm(&raw_wasm).context("Failed to optimize WebAssembly binary")
    } else {
        Ok(raw_wasm)
    }
}

/// Compile multiple Python source files into a single WASM binary with config awareness.
///
/// # Arguments
///
/// * `sources` - Array of (filename, source code) pairs
/// * `optimize` - Whether to optimize the output
/// * `config` - Project configuration
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_multiple_python_files_with_config(
    sources: &[(&str, &str)],
    optimize: bool,
    config: &ProjectConfig,
) -> Result<Vec<u8>> {
    // Parse and convert each Python source to IR
    let mut combined_module = ir::IRModule::new();
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
        if core::config::is_config_file(filename) {
            println!("Skipping configuration file: {}", filename);
            continue;
        }

        // Skip incompatible files
        if utils::is_special_python_file(filename) {
            println!("Skipping special file: {}", filename);
            continue;
        }

        // Check for entry points
        if !has_entry_point {
            if let Ok(Some(info)) = ir::detect_entry_points(source, Some(Path::new(filename))) {
                has_entry_point = true;
                entry_point_info = Some(info);
                println!("Detected entry point in file: {}", filename);
            }
        }

        // Parse Python to AST
        let ast = match core::parser::parse_python(source) {
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
            ir::add_entry_point_to_module(&mut combined_module, &info)?;
        }
    }

    // Generate WASM binary from the combined module
    let raw_wasm = compiler::compile_ir_module(&combined_module);

    // Optimize the WASM binary
    if optimize {
        optimize::optimize_wasm(&raw_wasm).context("Failed to optimize WebAssembly binary")
    } else {
        Ok(raw_wasm)
    }
}

/// Compile a Python project directory to WebAssembly.
///
/// # Arguments
///
/// * `project_dir` - Path to project directory
/// * `optimize` - Whether to optimize the output
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_python_project<P: AsRef<Path>>(project_dir: P, optimize: bool) -> Result<Vec<u8>> {
    let options = CompilerOptions {
        optimize,
        ..CompilerOptions::default()
    };

    compile_python_project_with_options(project_dir, &options)
}

/// Compile a Python project with options.
///
/// # Arguments
///
/// * `project_dir` - Path to project directory
/// * `options` - Compiler options
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_python_project_with_options<P: AsRef<Path>>(
    project_dir: P,
    options: &CompilerOptions,
) -> Result<Vec<u8>> {
    // Load and analyze the project
    let project_dir = project_dir.as_ref();

    println!("Analyzing project structure...");

    // Load project configuration
    let config = core::config::load_project_config(project_dir)?;

    println!("Project Name: {}", config.name);
    println!("Project Version: {}", config.version);
    if let Some(description) = &config.description {
        println!("Description: {}", description);
    }
    if let Some(author) = &config.author {
        println!("Author: {}", author);
    }

    let files = utils::collect_compilable_python_files(project_dir)?;

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
            if let Ok(Some(info)) = ir::detect_entry_points(&content, Some(&main_py_path)) {
                entry_point_file = Some("__main__.py".to_string());
                entry_point_info = Some(info);
            }
        }
    }

    // If no __main__.py, check other files for entry points
    if entry_point_info.is_none() {
        for (path, content) in &files {
            if let Ok(Some(info)) = ir::detect_entry_points(content, Some(Path::new(path))) {
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
    let result = compile_multiple_python_files_with_config(&sources, options.optimize, &config)?;

    // If we found an entry point, we might need to add special handling here
    if entry_point_info.is_some() {
        // We've already integrated this in compile_multiple_python_files_with_config
        // But could add any additional entry point processing here
    }

    Ok(result)
}

/// Get metadata about a Python source file without compiling to WASM.
/// Returns a list of function signatures for documentation or analysis.
///
/// # Arguments
///
/// * `source` - Python source code
///
/// # Returns
///
/// List of function signatures
///
/// # Errors
///
/// Returns an error if parsing or IR conversion fails
pub fn get_python_file_metadata(
    source: &str,
) -> Result<Vec<analysis::metadata::FunctionSignature>> {
    // Parse Python to AST
    let ast = core::parser::parse_python(source).context("Failed to parse Python code")?;

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

        signatures.push(analysis::metadata::FunctionSignature {
            name: func.name.clone(),
            parameters: param_types,
            return_type: type_to_string(&func.return_type),
        });
    }

    Ok(signatures)
}

/// Get metadata about an entire Python project.
/// Returns a list of function signatures for all files.
///
/// # Arguments
///
/// * `project_dir` - Path to project directory
///
/// # Returns
///
/// List of (file path, function signatures) pairs
///
/// # Errors
///
/// Returns an error if parsing or IR conversion fails
pub fn get_python_project_metadata<P: AsRef<Path>>(
    project_dir: P,
) -> Result<Vec<(String, Vec<analysis::metadata::FunctionSignature>)>> {
    let project_dir = project_dir.as_ref();
    let files = utils::collect_compilable_python_files(project_dir)?;

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
pub fn type_to_string(ir_type: &IRType) -> String {
    match ir_type {
        IRType::Int => "int".to_string(),
        IRType::Float => "float".to_string(),
        IRType::Bool => "bool".to_string(),
        IRType::String => "str".to_string(),
        IRType::List(elem_type) => format!("List[{}]", type_to_string(elem_type)),
        IRType::Dict(key_type, val_type) => format!(
            "Dict[{}, {}]",
            type_to_string(key_type),
            type_to_string(val_type)
        ),
        IRType::Tuple(types) => {
            let inner = types
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("Tuple[{}]", inner)
        }
        IRType::Optional(inner) => format!("Optional[{}]", type_to_string(inner)),
        IRType::Union(types) => {
            let inner = types
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(" | ");
            format!("Union[{}]", inner)
        }
        IRType::Class(name) => name.clone(),
        IRType::Module(name) => format!("Module[{}]", name), // Add handling for Module type
        IRType::None => "None".to_string(),
        IRType::Any => "Any".to_string(),
        IRType::Unknown => "unknown".to_string(),
    }
}

pub use crate::analysis::metadata::FunctionSignature;
pub use crate::core::parser;
