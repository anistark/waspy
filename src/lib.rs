//! Waspy: A Python to WebAssembly compiler written in Rust.
//!
//! Waspy translates Python functions into WebAssembly, allowing Python code
//! to run in browsers and other WebAssembly environments.

pub mod analysis;
pub mod compiler;
pub mod core;
pub mod ir;
pub mod optimize;
pub mod stdlib;
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

        // Merge this file's string/bytes layout into the combined module.
        combined_module
            .memory_layout
            .merge_from(&ir_module.memory_layout);
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

        // Merge this file's string/bytes layout into the combined module.
        combined_module
            .memory_layout
            .merge_from(&ir_module.memory_layout);

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
        IRType::Module(name) => format!("Module[{name}]"),
        IRType::Bytes => "bytes".to_string(),
        IRType::Set(elem_type) => format!("Set[{}]", type_to_string(elem_type)),
        IRType::Range => "range".to_string(),
        IRType::None => "None".to_string(),
        IRType::Any => "Any".to_string(),
        IRType::Unknown => "unknown".to_string(),
        IRType::Callable { .. } => "Callable".to_string(),
        IRType::Generator(yield_type) => format!("Generator[{}]", type_to_string(yield_type)),
        IRType::Datetime => "datetime.datetime".to_string(),
        IRType::Date => "datetime.date".to_string(),
        IRType::Time => "datetime.time".to_string(),
        IRType::Timedelta => "datetime.timedelta".to_string(),
    }
}

pub use crate::analysis::metadata::FunctionSignature;
pub use crate::core::parser;

#[cfg(test)]
mod collection_tests {
    use super::*;

    use wasmi::{Engine, Linker, Module, Store};

    /// Compile (unoptimized) and instantiate the module, returning the wasmi
    /// instance + store so a test can call exported functions. Instantiation
    /// validates types and stack balance, so this also guards the codegen bugs
    /// that previously produced invalid modules.
    fn instantiate(source: &str) -> (wasmi::Instance, Store<()>) {
        let options = CompilerOptions {
            optimize: false,
            ..CompilerOptions::default()
        };
        let wasm = compile_python_to_wasm_with_options(source, &options).expect("compilation");
        let engine = Engine::default();
        let module = Module::new(&engine, &wasm[..]).expect("valid wasm module");
        let mut store = Store::new(&engine, ());
        let instance = Linker::<()>::new(&engine)
            .instantiate(&mut store, &module)
            .expect("instantiation")
            .start(&mut store)
            .expect("start");
        (instance, store)
    }

    fn call_i32(source: &str, func: &str) -> i32 {
        let (instance, mut store) = instantiate(source);
        instance
            .get_typed_func::<(), i32>(&store, func)
            .expect("exported i32 fn")
            .call(&mut store, ())
            .expect("call")
    }

    fn call_i32_arg(source: &str, func: &str, arg: i32) -> i32 {
        let (instance, mut store) = instantiate(source);
        instance
            .get_typed_func::<i32, i32>(&store, func)
            .expect("exported i32 fn")
            .call(&mut store, arg)
            .expect("call")
    }

    #[test]
    fn float_list_roundtrips() {
        // Reads the float element back and compares (returns 1 on match). The
        // value is stored as f32; 2.5 is exact, so equality holds.
        let src = "def f() -> int:\n    xs = [1.5, 2.5, 3.5]\n    if xs[1] == 2.5:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn float_tuple_roundtrips() {
        let src = "def f() -> int:\n    t = (1.25, 2.75)\n    if t[1] == 2.75:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn int_list_indexing_returns_element() {
        // Previously returned a constant 0: the untyped local lost its type.
        let src = "def f() -> int:\n    xs = [10, 20, 30]\n    return xs[1]\n";
        assert_eq!(call_i32(src, "f"), 20);
    }

    #[test]
    fn distinct_collections_do_not_alias() {
        // Both lists previously shared one address (base + local_count*100).
        let src =
            "def f() -> int:\n    a = [1, 2, 3]\n    b = [10, 20, 30]\n    return a[0] + b[0]\n";
        assert_eq!(call_i32(src, "f"), 11);
    }

    #[test]
    fn nested_collections_do_not_alias() {
        let src = "def f() -> int:\n    m = [[1, 2], [3, 4]]\n    return m[0][1] + m[1][0]\n";
        assert_eq!(call_i32(src, "f"), 5);
    }

    #[test]
    fn print_of_collection_element_is_valid() {
        // print() returns nothing; a stray drop would underflow and fail to
        // instantiate. Just ensure it builds and instantiates.
        instantiate("def f():\n    xs = [1, 2, 3]\n    print(xs[0])\n");
    }

    #[test]
    fn range_for_loop_iterates() {
        // for-over-range: previously the loop's iterator locals were added
        // after the function's locals were fixed (out-of-range), and the range
        // object's fields were stored with reversed operands, so the loop ran
        // zero times. Sum 0..5 (with step) to exercise both.
        let sum =
            "def f() -> int:\n    t = 0\n    for i in range(5):\n        t = t + i\n    return t\n";
        assert_eq!(call_i32(sum, "f"), 10);
        let step = "def f() -> int:\n    t = 0\n    for i in range(0, 10, 2):\n        t = t + i\n    return t\n";
        assert_eq!(call_i32(step, "f"), 20);
    }

    #[test]
    fn nested_range_loops_use_distinct_iterators() {
        let src = "def f() -> int:\n    s = 0\n    for i in range(3):\n        for j in range(4):\n            s = s + 1\n    return s\n";
        assert_eq!(call_i32(src, "f"), 12);
    }

    #[test]
    fn try_except_finally_is_valid_and_runs() {
        // try/except/finally previously emitted an extra End that closed the
        // function frame early ("body shorter than given size").
        let src = "def f(x: int) -> int:\n    try:\n        return x + 1\n    except ValueError:\n        return -1\n    finally:\n        x = x + 100\n";
        assert_eq!(call_i32_arg(src, "f", 5), 6);
    }

    #[test]
    fn nested_try_except_is_valid() {
        let src = "def f(x: int) -> int:\n    try:\n        try:\n            return x + 5\n        except KeyError:\n            return -2\n    except ValueError:\n        return -1\n";
        assert_eq!(call_i32_arg(src, "f", 5), 10);
    }

    #[test]
    fn int_plus_float_coerces() {
        // a (int) + b (float) widens the int to f64; the result equals 3.5.
        let src = "def f() -> int:\n    a = 2\n    b = 1.5\n    if (a + b) == 3.5:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn boolean_and_or_short_circuit() {
        // and/or now yield an i32 result from their if/else instead of an
        // empty block type.
        let and =
            "def f(a: int) -> int:\n    if (a > 0) and (a < 10):\n        return 1\n    return 0\n";
        assert_eq!(call_i32_arg(and, "f", 5), 1);
        assert_eq!(call_i32_arg(and, "f", 20), 0);
        let or =
            "def f(a: int) -> int:\n    if (a < 0) or (a > 100):\n        return 1\n    return 0\n";
        assert_eq!(call_i32_arg(or, "f", -1), 1);
        assert_eq!(call_i32_arg(or, "f", 50), 0);
    }

    #[test]
    fn unannotated_float_local_in_mixed_function() {
        // `result` is an unannotated float local (f64) living alongside the int
        // local `i`; both the type inference and index-order local layout must
        // be right for this to validate and compute 2**10.
        let src = "def f() -> int:\n    result = 1.0\n    i = 0\n    while i < 10:\n        result = result * 2.0\n        i = i + 1\n    if result == 1024.0:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn module_level_float_constant_is_inlined() {
        // A module-level float constant used in arithmetic; emitting it at its
        // natural type (not the caller's expectation) keeps it an f64.
        let src = "PI = 2.5\ndef f() -> int:\n    if (PI * 4.0) == 10.0:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn int_and_float_conversions() {
        // int() truncates a float; float() widens an int.
        let src = "def f() -> int:\n    return int(3.7) + int(float(2))\n";
        assert_eq!(call_i32(src, "f"), 5);
    }

    #[test]
    fn math_float_constant_local() {
        // `math.pi`/`math.tau` are f64 stdlib constants; their locals must be
        // f64 (previously an f64 store landed in an i32 slot, which failed
        // validation and aborted Binaryen during optimization).
        let src = "import math\ndef f() -> int:\n    pi = math.pi\n    tau = math.tau\n    if tau > pi:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn unannotated_function_returning_float() {
        // An unannotated function that returns a float gets an f64 result type
        // inferred from its body, so a caller sees an f64 (previously the i32
        // result signature mismatched the f64 return value).
        let src = "import math\ndef get_pi():\n    pi = math.pi\n    return pi\ndef f() -> int:\n    if get_pi() > 3.0:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn min_and_max_reduce() {
        // The reduction previously left the if/else stack unbalanced.
        let src = "def lo() -> int:\n    return min(5, 3, 8, 1, 9)\ndef hi() -> int:\n    return max(5, 3, 8, 1, 9)\n";
        assert_eq!(call_i32(src, "lo"), 1);
        assert_eq!(call_i32(src, "hi"), 9);
    }

    #[test]
    fn os_path_submodule_attribute_is_valid() {
        // os.path.<attr> previously fell through to a stray drop that
        // underflowed the stack; it now resolves the submodule attribute.
        instantiate("import os\ndef f():\n    print(\"sep:\", os.path.sep)\n");
    }
}
