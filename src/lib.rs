//! Waspy: A Python to WebAssembly compiler written in Rust.
//!
//! Waspy translates Python functions into WebAssembly, allowing Python code
//! to run in browsers and other WebAssembly environments.

pub mod analysis;
pub mod compiler;
pub mod core;
pub mod ir;
pub mod optimize;
pub mod utils;

// WASM plugin integration
#[cfg(feature = "wasm-plugin")]
pub mod wasmrun;

#[cfg(feature = "wasm-plugin")]
pub use wasmrun::{WaspyBuilder, WaspyPlugin};

use crate::core::config::ProjectConfig;
pub use crate::core::options::{CompilerOptions, Verbosity};
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
    // Initialize logging with the specified verbosity
    utils::logging::init(options.verbosity);

    log_debug!("Starting compilation with options: {:?}", options);

    // Parse Python to AST
    log_verbose!("Parsing Python source code...");
    let ast = core::parser::parse_python(source).context("Failed to parse Python code")?;
    log_debug!("Successfully parsed Python AST");

    // Lower AST to IR
    log_verbose!("Converting AST to intermediate representation...");
    let mut ir_module = ir::lower_ast_to_ir(&ast).context("Failed to convert Python AST to IR")?;
    log_debug!(
        "Generated IR module with {} functions",
        ir_module.functions.len()
    );

    // Process decorators
    log_verbose!("Processing decorators...");
    let decorator_registry = ir::DecoratorRegistry::new();
    ir_module.functions = ir_module
        .functions
        .into_iter()
        .map(|func| {
            if !func.decorators.is_empty() {
                log_debug!("Applying decorators to function: {}", func.name);
                decorator_registry.apply_decorators(func)
            } else {
                func
            }
        })
        .collect();
    log_verbose!("{:#?}", ir_module);
    // Check for entry points
    log_verbose!("Detecting entry points...");
    if let Ok(Some(entry_point_info)) = ir::detect_entry_points(source, None) {
        log_debug!("Found entry point: {:?}", entry_point_info);
        // Add entry point support if detected
        ir::add_entry_point_to_module(&mut ir_module, &entry_point_info)?;
    }

    // Generate WASM binary
    log_verbose!("Generating WebAssembly binary...");
    let raw_wasm = compiler::compile_ir_module(&ir_module);
    log_debug!("Generated WASM binary: {} bytes", raw_wasm.len());

    // Optimize the WASM binary if requested
    if options.optimize {
        log_verbose!("Optimizing WebAssembly binary...");
        let optimized =
            optimize::optimize_wasm(&raw_wasm).context("Failed to optimize WebAssembly binary")?;
        log_debug!(
            "Optimized WASM binary: {} bytes (saved {} bytes)",
            optimized.len(),
            raw_wasm.len() as i64 - optimized.len() as i64
        );
        Ok(optimized)
    } else {
        log_debug!("Skipping optimization");
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
            log_verbose!("Skipping special file: {filename}");
            continue;
        }

        // Check for entry points
        if !has_entry_point {
            if let Ok(Some(info)) = ir::detect_entry_points(source, Some(Path::new(filename))) {
                has_entry_point = true;
                entry_point_info = Some(info);
                log_debug!("Detected entry point in file: {filename}");
            }
        }

        log_debug!("Processing file: {filename}");

        // Parse Python to AST
        let ast = match core::parser::parse_python(source) {
            Ok(ast) => ast,
            Err(e) => {
                log_warn!("Failed to parse {filename}: {e}");
                continue;
            }
        };

        // Lower AST to IR
        let ir_module = match ir::lower_ast_to_ir(&ast) {
            Ok(module) => module,
            Err(e) => {
                log_warn!("Failed to convert {filename} to IR: {e}");
                continue;
            }
        };

        // Skip if no functions
        if ir_module.functions.is_empty() {
            log_verbose!("Skipping file with no functions: {filename}");
            continue;
        }

        log_debug!(
            "Found {} functions in {filename}",
            ir_module.functions.len()
        );

        // Check for duplicate function names and add functions
        for func in ir_module.functions {
            if !function_names.insert(func.name.clone()) {
                log_warn!(
                    "Duplicate function '{}' found in file: {}",
                    func.name,
                    filename
                );
                // Skip the duplicate but continue processing
            } else {
                log_debug!("Adding function: {}", func.name);
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
            log_verbose!("Skipping configuration file: {filename}");
            continue;
        }

        // Skip incompatible files
        if utils::is_special_python_file(filename) {
            log_verbose!("Skipping special file: {filename}");
            continue;
        }

        // Check for entry points
        if !has_entry_point {
            if let Ok(Some(info)) = ir::detect_entry_points(source, Some(Path::new(filename))) {
                has_entry_point = true;
                entry_point_info = Some(info);
                log_debug!("Detected entry point in file: {filename}");
            }
        }

        log_debug!("Processing file: {filename}");

        // Parse Python to AST
        let ast = match core::parser::parse_python(source) {
            Ok(ast) => ast,
            Err(e) => {
                log_warn!("Failed to parse {filename}: {e}");
                continue;
            }
        };

        // Lower AST to IR
        let ir_module = match ir::lower_ast_to_ir(&ast) {
            Ok(module) => module,
            Err(e) => {
                log_warn!("Failed to convert {filename} to IR: {e}");
                continue;
            }
        };

        // Skip if no functions
        if ir_module.functions.is_empty() {
            log_verbose!("Skipping file with no functions: {filename}");
            continue;
        }

        log_debug!(
            "Found {} functions in {filename}",
            ir_module.functions.len()
        );

        // Check for duplicate function names and add functions
        for func in ir_module.functions {
            if !function_names.insert(func.name.clone()) {
                log_warn!(
                    "Duplicate function '{}' found in file: {}",
                    func.name,
                    filename
                );
                // Skip the duplicate but continue processing
            } else {
                log_debug!("Adding function: {}", func.name);
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
    // Initialize logging with the specified verbosity
    utils::logging::init(options.verbosity);

    // Load and analyze the project
    let project_dir = project_dir.as_ref();

    log_info!("Analyzing project structure...");
    log_debug!("Project directory: {}", project_dir.display());

    // Load project configuration
    let config = core::config::load_project_config(project_dir)?;

    log_info!("Project Name: {}", config.name);
    log_info!("Project Version: {}", config.version);
    if let Some(description) = &config.description {
        log_verbose!("Description: {description}");
    }
    if let Some(author) = &config.author {
        log_verbose!("Author: {author}");
    }

    let files = utils::collect_compilable_python_files(project_dir)?;

    if files.is_empty() {
        return Err(anyhow!("No compilable Python files found in the project"));
    }

    // Look for entry points in the project
    let mut entry_point_file = None;
    let mut entry_point_info = None;

    log_verbose!("Searching for entry points...");
    // First, check for __main__.py
    let main_py_path = project_dir.join("__main__.py");
    if main_py_path.exists() && main_py_path.is_file() {
        log_debug!("Checking __main__.py for entry point");
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
            log_debug!("Checking {} for entry point", path);
            if let Ok(Some(info)) = ir::detect_entry_points(content, Some(Path::new(path))) {
                entry_point_file = Some(path.clone());
                entry_point_info = Some(info);
                break;
            }
        }
    }

    if let Some(file) = &entry_point_file {
        log_info!("Found entry point in file: {file}");
    } else {
        log_debug!("No entry point detected");
    }

    log_info!("Found {} compilable Python files", files.len());
    log_debug!("Files: {:?}", files.keys().collect::<Vec<_>>());

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
                println!("Warning: Failed to extract metadata from {path}: {e}");
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
            format!("Tuple[{inner}]")
        }
        IRType::Optional(inner) => format!("Optional[{}]", type_to_string(inner)),
        IRType::Union(types) => {
            let inner = types
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(" | ");
            format!("Union[{inner}]")
        }
        IRType::Class(name) => name.clone(),
        IRType::Module(name) => format!("Module[{name}]"), // Add handling for Module type
        IRType::None => "None".to_string(),
        IRType::Any => "Any".to_string(),
        IRType::Unknown => "unknown".to_string(),
    }
}

pub use crate::analysis::metadata::FunctionSignature;
pub use crate::core::parser;
