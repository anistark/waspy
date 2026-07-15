//! Waspy: A Python to WebAssembly compiler written in Rust.
//!
//! Waspy translates a typed subset of Python into a standalone WebAssembly
//! module, allowing Python code to run in browsers and other WebAssembly
//! environments. There is no interpreter or runtime dependency: each
//! supported construct compiles to inline WASM, and the produced module's
//! exported functions are the compiled Python functions.
//!
//! # Quick start
//!
//! ```
//! use waspy::compile_python_to_wasm;
//!
//! let source = "def add(a: int, b: int) -> int:\n    return a + b\n";
//! let wasm: Vec<u8> = compile_python_to_wasm(source)?;
//! assert_eq!(&wasm[0..4], b"\0asm");
//! # anyhow::Ok(())
//! ```
//!
//! # Public API
//!
//! The stable surface of this crate is the set of items exported from the
//! crate root:
//!
//! - **Single source**: [`compile_python_to_wasm`],
//!   [`compile_python_to_wasm_with_options`]
//! - **Entry file with user-module imports resolved from disk**:
//!   [`compile_python_file`], [`compile_python_file_with_options`]
//! - **Several sources merged into one module**:
//!   [`compile_multiple_python_files`],
//!   [`compile_multiple_python_files_with_options`],
//!   [`compile_multiple_python_files_with_config`]
//! - **Project directory**: [`compile_python_project`],
//!   [`compile_python_project_with_options`]
//! - **Metadata without codegen**: [`get_python_file_metadata`],
//!   [`get_python_project_metadata`], [`FunctionSignature`]
//! - **Configuration**: [`CompilerOptions`], [`Verbosity`]
//! - **Helpers**: [`type_to_string`]
//!
//! All compile entry points return `anyhow::Result<Vec<u8>>`; the byte vector
//! is a complete, validated WebAssembly binary. Structured compiler errors
//! (with source locations where available) travel in the error chain as
//! [`core::errors::ChakraError`] values.
//!
//! The pipeline modules ([`core`], [`ir`], [`compiler`], [`optimize`],
//! [`stdlib`], [`analysis`], [`utils`]) are exposed for advanced embedding
//! and inspection, but their contents are implementation detail and may
//! change between minor versions; depend on the crate-root exports.

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
    compile_merged_sources(sources, options, None, true)
}

/// Merge several Python sources into one IR module and compile it — the
/// shared implementation behind every multi-file entry point.
///
/// Each file is parsed and lowered once; a filename appearing twice is
/// skipped (a module imported through several paths is compiled and its
/// module-level state merged exactly once, #41). Functions are de-duplicated
/// by name (first definition wins, with a warning), and string/bytes layouts
/// are merged. With `skip_special` set, files matching
/// [`utils::is_special_python_file`] are ignored — the behavior of the
/// directory-scanning entry points; import-resolved module files bypass it so
/// a genuine local module named e.g. `config.py` still links.
fn compile_merged_sources(
    sources: &[(&str, &str)],
    options: &CompilerOptions,
    config: Option<&ProjectConfig>,
    skip_special: bool,
) -> Result<Vec<u8>> {
    // Parse and convert each Python source to IR
    let mut combined_module = ir::IRModule::new();
    let mut function_names = std::collections::HashSet::new();
    let mut seen_files = std::collections::HashSet::new();
    let mut has_entry_point = false;
    let mut entry_point_info: Option<EntryPointInfo> = None;

    // Set project metadata if available
    if let Some(config) = config {
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
    }

    for (filename, source) in sources {
        // A module reached through several import paths is merged once (#41).
        if !seen_files.insert(filename.to_string()) {
            log_verbose!("Skipping already-merged file: {filename}");
            continue;
        }

        // Skip incompatible files
        if core::config::is_config_file(filename) {
            log_verbose!("Skipping configuration file: {filename}");
            continue;
        }

        if skip_special && utils::is_special_python_file(filename) {
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

        // Skip files that contribute nothing (a constants-only module still
        // carries variables worth merging).
        if ir_module.functions.is_empty()
            && ir_module.classes.is_empty()
            && ir_module.variables.is_empty()
        {
            log_verbose!("Skipping file with no compilable definitions: {filename}");
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
    let options = CompilerOptions {
        optimize,
        ..CompilerOptions::default()
    };
    compile_merged_sources(sources, &options, Some(config), true)
}

/// Compile a Python entry file together with the user-written modules it
/// imports (#41), using default options.
///
/// Imports are resolved relative to the entry file's directory: `import mod`
/// finds `mod.py` (or `mod/__init__.py`), `import pkg.mod` finds
/// `pkg/mod.py`, transitively through each resolved module's own imports.
/// Every resolved module is compiled and linked into the single output WASM
/// module exactly once, however many import paths reach it.
///
/// # Arguments
///
/// * `path` - Path to the entry `.py` file
/// * `optimize` - Whether to optimize the output
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if the entry file or a resolved module cannot be read, or
/// if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_python_file<P: AsRef<Path>>(path: P, optimize: bool) -> Result<Vec<u8>> {
    let options = CompilerOptions {
        optimize,
        ..CompilerOptions::default()
    };
    compile_python_file_with_options(path, &options)
}

/// Compile a Python entry file together with the user-written modules it
/// imports (#41), with the specified options. See [`compile_python_file`].
///
/// # Arguments
///
/// * `path` - Path to the entry `.py` file
/// * `options` - Compiler options
///
/// # Returns
///
/// WebAssembly binary as a byte vector
///
/// # Errors
///
/// Returns an error if the entry file or a resolved module cannot be read, or
/// if parsing, IR conversion, or WebAssembly generation fails
pub fn compile_python_file_with_options<P: AsRef<Path>>(
    path: P,
    options: &CompilerOptions,
) -> Result<Vec<u8>> {
    utils::logging::init(options.verbosity);

    let path = path.as_ref();
    let entry_source = fs::read_to_string(path)
        .with_context(|| format!("Failed to read Python file: {}", path.display()))?;

    // Resolve every user-written module reachable from the entry file; each
    // appears once (module caching), in discovery order after the entry file
    // so the entry file wins name collisions and entry-point detection.
    let modules = analysis::imports::resolve_user_modules(path, &entry_source)?;
    let mut sources: Vec<(String, String)> = vec![(path.display().to_string(), entry_source)];
    for (module_name, file_path, source) in modules {
        log_debug!(
            "Resolved user module '{module_name}' -> {}",
            file_path.display()
        );
        sources.push((file_path.display().to_string(), source));
    }

    let source_refs: Vec<(&str, &str)> = sources
        .iter()
        .map(|(f, s)| (f.as_str(), s.as_str()))
        .collect();

    // Resolved module files bypass the special-file skip: the import already
    // vouches for them (a local module legitimately named `config.py` or a
    // package `__init__.py` must still link).
    compile_merged_sources(&source_refs, options, None, false)
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
                log_warn!("Failed to extract metadata from {path}: {e}");
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
        IRType::File => "file".to_string(),
        IRType::Datetime => "datetime.datetime".to_string(),
        IRType::Date => "datetime.date".to_string(),
        IRType::Time => "datetime.time".to_string(),
        IRType::Timedelta => "datetime.timedelta".to_string(),
    }
}

pub use crate::analysis::metadata::FunctionSignature;

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

    /// A rectangle class with float fields, used by the class+float tests. Each
    /// test function returns 1 when the float computation matches its expected
    /// value (the wasmi build in use can't return an f64 directly).
    const RECT_SRC: &str = "class Rectangle:\n    default_width = 10\n    default_height = 5\n    def __init__(self, width: float, height: float):\n        self.width = width\n        self.height = height\n    def area(self) -> float:\n        return self.width * self.height\n    def perimeter(self) -> float:\n        return 2 * (self.width + self.height)\n    def scale(self, factor: float) -> None:\n        self.width *= factor\n        self.height *= factor\n";

    #[test]
    fn class_float_fields_and_methods() {
        // Float instance fields are stored/loaded as f64, and a method returning
        // a float computes correctly (previously fields read as i32 and the f64
        // return mismatched).
        let area = format!(
            "{RECT_SRC}def f() -> int:\n    r = Rectangle(10.0, 5.0)\n    if r.area() == 50.0:\n        return 1\n    return 0\n"
        );
        assert_eq!(call_i32(&area, "f"), 1);
        // `2 * (a + b)` keeps the float result instead of truncating it to int.
        let perim = format!(
            "{RECT_SRC}def f() -> int:\n    r = Rectangle(10.0, 5.0)\n    if r.perimeter() == 30.0:\n        return 1\n    return 0\n"
        );
        assert_eq!(call_i32(&perim, "f"), 1);
    }

    #[test]
    fn class_int_args_coerced_to_float() {
        // Int literals passed to float constructor parameters widen to f64.
        let src = format!(
            "{RECT_SRC}def f() -> int:\n    r = Rectangle(3, 4)\n    if r.area() == 12.0:\n        return 1\n    return 0\n"
        );
        assert_eq!(call_i32(&src, "f"), 1);
    }

    #[test]
    fn class_augmented_field_assign() {
        // `self.width *= factor` performs an f64 load/mul/store (was a no-op).
        let src = format!(
            "{RECT_SRC}def f() -> int:\n    r = Rectangle(2.0, 3.0)\n    r.scale(2.0)\n    if r.area() == 24.0:\n        return 1\n    return 0\n"
        );
        assert_eq!(call_i32(&src, "f"), 1);
    }

    #[test]
    fn class_variable_access() {
        // `ClassName.classvar` reads the class-level variable's value (10 * 5).
        let src = format!(
            "{RECT_SRC}def f() -> int:\n    r = Rectangle(Rectangle.default_width, Rectangle.default_height)\n    if r.area() == 50.0:\n        return 1\n    return 0\n"
        );
        assert_eq!(call_i32(&src, "f"), 1);
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
    fn string_read_back_from_list_has_length() {
        // A string element stores only its offset; reading it back rebuilds the
        // (offset, length) pair from the blob's length prefix. Previously this
        // dropped the length, so binding it to a local emitted invalid WASM
        // ("not enough arguments on the stack for local.set").
        let src =
            "def f() -> int:\n    xs = [\"alpha\", \"beta\", \"gamma\"]\n    w = xs[1]\n    return len(w)\n";
        assert_eq!(call_i32(src, "f"), 4);
    }

    #[test]
    fn string_read_back_from_tuple_has_length() {
        let src =
            "def f() -> int:\n    t = (\"one\", \"three\", \"x\")\n    w = t[1]\n    return len(w)\n";
        assert_eq!(call_i32(src, "f"), 5);
    }

    #[test]
    fn string_read_back_preserves_identity() {
        // The recovered offset is the interned one, so membership (offset
        // comparison) still finds the value pulled back out of the list.
        let src = "def f() -> int:\n    xs = [\"alpha\", \"beta\", \"gamma\"]\n    w = xs[1]\n    if w in xs:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn string_membership_unaffected_by_read_back() {
        // Reading strings out of collections must not regress offset-based
        // membership/dedup, which the length-prefix layout leaves untouched.
        let present =
            "def f() -> int:\n    xs = [\"a\", \"b\", \"c\"]\n    if \"b\" in xs:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(present, "f"), 1);
        let absent =
            "def f() -> int:\n    xs = [\"a\", \"b\", \"c\"]\n    if \"z\" in xs:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(absent, "f"), 0);
        let set_dedup =
            "def f() -> int:\n    s = {\"x\", \"y\", \"x\", \"z\"}\n    return len(s)\n";
        assert_eq!(call_i32(set_dedup, "f"), 3);
    }

    #[test]
    fn string_concatenation_round_trips() {
        // Real runtime concatenation: `__alloc` a fresh blob and copy both
        // operands in (was a placeholder that aliased the left operand).
        let len = "def f() -> int:\n    a = \"foo\"\n    b = \"barbar\"\n    return len(a + b)\n";
        assert_eq!(call_i32(len, "f"), 9);
        // The concatenated string round-trips through a list slot too.
        let via_list = "def f() -> int:\n    a = \"foo\"\n    b = \"bar\"\n    xs = [a + b]\n    w = xs[0]\n    return len(w)\n";
        assert_eq!(call_i32(via_list, "f"), 6);
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
    fn descending_range_for_loop_iterates() {
        // range(start, stop, -step): the loop's break test was ascending-only
        // (current >= stop), so a descending range exited immediately. The step
        // also relied on integer unary negation, which evaluated `-x` as `x`.
        let down = "def f() -> int:\n    t = 0\n    for i in range(10, 0, -1):\n        t = t + i\n    return t\n";
        assert_eq!(call_i32(down, "f"), 55);
        let neg = "def f() -> int:\n    t = 0\n    for i in range(20, 5, -3):\n        t = t + i\n    return t\n";
        assert_eq!(call_i32(neg, "f"), 70);
        let empty = "def f() -> int:\n    t = 0\n    for i in range(0, 5, -1):\n        t = t + i\n    return t\n";
        assert_eq!(call_i32(empty, "f"), 0);
    }

    #[test]
    fn integer_unary_negation() {
        // `-x` previously emitted `operand - 0`, leaving the value unchanged.
        let src = "def f() -> int:\n    x = 7\n    return -x\n";
        assert_eq!(call_i32(src, "f"), -7);
    }

    #[test]
    fn nested_range_loops_use_distinct_iterators() {
        let src = "def f() -> int:\n    s = 0\n    for i in range(3):\n        for j in range(4):\n            s = s + 1\n    return s\n";
        assert_eq!(call_i32(src, "f"), 12);
    }

    #[test]
    fn bytes_local_round_trips() {
        // A string/bytes value is an (offset, length) pair, but a local holds
        // one word; without a companion length local the offset was dropped, so
        // indexing read from offset 0 and len() returned 0.
        let idx = "def f() -> int:\n    b = b\"hello\"\n    return b[0]\n";
        assert_eq!(call_i32(idx, "f"), 104); // 'h'
        let idx1 = "def f() -> int:\n    b = b\"hello\"\n    return b[1]\n";
        assert_eq!(call_i32(idx1, "f"), 101); // 'e'
        let length = "def f() -> int:\n    b = b\"hello\"\n    return len(b)\n";
        assert_eq!(call_i32(length, "f"), 5);
    }

    #[test]
    fn string_local_len() {
        // len() of a string local previously kept the offset, not the length.
        let src = "def f() -> int:\n    s = \"hello\"\n    return len(s)\n";
        assert_eq!(call_i32(src, "f"), 5);
    }

    #[test]
    fn bytes_slicing_round_trips() {
        // `Expr::Slice` now lowers, and the slice codegen is branchless so it
        // validates. Slices share the source bytes' backing memory.
        let mid = "def f() -> int:\n    b = b\"hello\"\n    s = b[1:4]\n    return s[0]\n";
        assert_eq!(call_i32(mid, "f"), 101); // b"ell"[0] == 'e'
        let mid_len = "def f() -> int:\n    b = b\"hello\"\n    s = b[1:4]\n    return len(s)\n";
        assert_eq!(call_i32(mid_len, "f"), 3);
        let open_end = "def f() -> int:\n    b = b\"hello\"\n    return len(b[2:])\n";
        assert_eq!(call_i32(open_end, "f"), 3);
        let open_start = "def f() -> int:\n    b = b\"hello\"\n    return len(b[:3])\n";
        assert_eq!(call_i32(open_start, "f"), 3);
        let negative = "def f() -> int:\n    b = b\"hello\"\n    s = b[-2:]\n    return s[0]\n";
        assert_eq!(call_i32(negative, "f"), 108); // b"lo"[0] == 'l'
    }

    #[test]
    fn bytes_concatenation_round_trips() {
        let src =
            "def f() -> int:\n    a = b\"ab\"\n    c = b\"cd\"\n    d = a + c\n    return d[3]\n";
        assert_eq!(call_i32(src, "f"), 100); // 'd'
        let len = "def f() -> int:\n    a = b\"ab\"\n    c = b\"cd\"\n    return len(a + c)\n";
        assert_eq!(call_i32(len, "f"), 4);
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

    #[test]
    fn string_equality_compares_contents() {
        // `==`/`!=` on str/bytes previously fell through to the integer path,
        // which compared only the top word (the right operand's length) and
        // stranded the left pair, so even equal strings compared unequal (#90).
        let eq =
            "def f() -> int:\n    a = \"hello\"\n    b = \"hello\"\n    if a == b:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(eq, "f"), 1);
        let ne_same =
            "def f() -> int:\n    a = \"hello\"\n    b = \"hello\"\n    if a != b:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(ne_same, "f"), 0);
        let neq =
            "def f() -> int:\n    a = \"hello\"\n    b = \"world\"\n    if a == b:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(neq, "f"), 0);
        // Different lengths short-circuit before the byte loop.
        let diff_len =
            "def f() -> int:\n    a = \"hi\"\n    b = \"hello\"\n    if a == b:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(diff_len, "f"), 0);
        // A runtime-built operand (concatenation) has a distinct offset, so a
        // content compare — not an offset compare — is required.
        let runtime =
            "def f() -> int:\n    a = \"foo\" + \"bar\"\n    b = \"foobar\"\n    if a == b:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(runtime, "f"), 1);
        let bytes_eq =
            "def f() -> int:\n    a = b\"abc\"\n    b = b\"abc\"\n    if a == b:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(bytes_eq, "f"), 1);
    }

    #[test]
    fn dict_string_value_read_back_has_length() {
        // Reading a str/bytes value out of a dict dropped its length (#91): the
        // matched value word is the blob offset, so rebuild (offset, length)
        // from the length prefix like list/tuple read-back does.
        let src =
            "def f() -> int:\n    d = {1: \"value\", 2: \"xy\"}\n    w = d[1]\n    return len(w)\n";
        assert_eq!(call_i32(src, "f"), 5);
        let other =
            "def f() -> int:\n    d = {1: \"value\", 2: \"xy\"}\n    w = d[2]\n    return len(w)\n";
        assert_eq!(call_i32(other, "f"), 2);
    }

    #[test]
    fn conditional_import_in_try_except_works() {
        // Conditional imports (#4): an import inside try/except parses, the
        // module resolves, and its members are usable afterwards.
        let src = "try:\n    import math\nexcept ImportError:\n    import math\n\ndef f() -> int:\n    pi = math.pi\n    if pi > 3.0:\n        return 1\n    return 0\n";
        assert_eq!(call_i32(src, "f"), 1);
    }

    #[test]
    fn string_slice_into_collection_has_length() {
        // A slice's offset points into the source blob, not past a fresh length
        // prefix, so collection read-back (load(offset - 4)) read the source's
        // length. Slicing now allocates a prefixed blob (#92).
        let into_list =
            "def f() -> int:\n    s = \"hello world\"\n    part = s[0:5]\n    xs = [part]\n    w = xs[0]\n    return len(w)\n";
        assert_eq!(call_i32(into_list, "f"), 5);
        let bytes_into_tuple =
            "def f() -> int:\n    b = b\"hello world\"\n    part = b[6:11]\n    t = (part,)\n    w = t[0]\n    return len(w)\n";
        assert_eq!(call_i32(bytes_into_tuple, "f"), 5);
        // The relocated blob still holds the right bytes (bytes indexing
        // returns the byte value; string indexing would return a char offset).
        let content =
            "def f() -> int:\n    b = b\"hello world\"\n    part = b[6:11]\n    return part[0]\n";
        assert_eq!(call_i32(content, "f"), 119); // 'w'
    }
}

#[cfg(test)]
mod user_module_tests {
    use super::*;

    use wasmi::{Engine, Linker, Module, Store};

    /// Compile several (filename, source) pairs into one module (unoptimized)
    /// and instantiate it, mirroring `collection_tests::instantiate`.
    fn instantiate_multi(sources: &[(&str, &str)]) -> (wasmi::Instance, Store<()>) {
        let options = CompilerOptions {
            optimize: false,
            ..CompilerOptions::default()
        };
        let wasm =
            compile_multiple_python_files_with_options(sources, &options).expect("compilation");
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

    fn call_multi_i32(sources: &[(&str, &str)], func: &str) -> i32 {
        let (instance, mut store) = instantiate_multi(sources);
        instance
            .get_typed_func::<(), i32>(&store, func)
            .expect("exported i32 fn")
            .call(&mut store, ())
            .expect("call")
    }

    const MATHMOD: &str =
        "FACTOR = 7\n\ndef add(a: int, b: int) -> int:\n    return a + b\n\ndef mul(a: int, b: int) -> int:\n    return a * b\n";

    #[test]
    fn from_import_calls_module_function() {
        // `from mod import f`: the merged function resolves by name (#41).
        let main = "from mathmod import add\n\ndef f() -> int:\n    return add(2, 3)\n";
        assert_eq!(
            call_multi_i32(&[("main.py", main), ("mathmod.py", MATHMOD)], "f"),
            5
        );
    }

    #[test]
    fn module_namespace_call() {
        // `import mod` + `mod.f(...)`: the namespace call resolves to the
        // statically linked function (#41).
        let main =
            "import mathmod\n\ndef f() -> int:\n    return mathmod.add(20, mathmod.mul(2, 11))\n";
        assert_eq!(
            call_multi_i32(&[("main.py", main), ("mathmod.py", MATHMOD)], "f"),
            42
        );
    }

    #[test]
    fn module_alias_namespace_call() {
        // `import mod as m` + `m.f(...)`.
        let main = "import mathmod as m\n\ndef f() -> int:\n    return m.add(2, 3)\n";
        assert_eq!(
            call_multi_i32(&[("main.py", main), ("mathmod.py", MATHMOD)], "f"),
            5
        );
    }

    #[test]
    fn from_import_alias_call() {
        // `from mod import f as g`: calls to `g` resolve to the merged `f`.
        let main = "from mathmod import add as plus\n\ndef f() -> int:\n    return plus(4, 5)\n";
        assert_eq!(
            call_multi_i32(&[("main.py", main), ("mathmod.py", MATHMOD)], "f"),
            9
        );
    }

    #[test]
    fn module_constant_read() {
        // `mod.CONST` inlines the merged module-level variable's value.
        let main = "import mathmod\n\ndef f() -> int:\n    return mathmod.FACTOR * 2\n";
        assert_eq!(
            call_multi_i32(&[("main.py", main), ("mathmod.py", MATHMOD)], "f"),
            14
        );
    }

    #[test]
    fn class_instantiation_through_module_namespace() {
        // `mod.ClassName(...)` instantiates the merged class; methods work.
        let shapes = "class Counter:\n    def __init__(self, start: int):\n        self.value = start\n    def bump(self) -> int:\n        self.value = self.value + 1\n        return self.value\n";
        let main = "import shapes\n\ndef f() -> int:\n    c = shapes.Counter(5)\n    c.bump()\n    return c.bump()\n";
        assert_eq!(
            call_multi_i32(&[("main.py", main), ("shapes.py", shapes)], "f"),
            7
        );
    }

    #[test]
    fn module_local_shadows_module_binding() {
        // A local named like an imported module shadows the namespace: the
        // method call goes to the object, not the module.
        let main = "import mathmod\n\ndef f() -> int:\n    mathmod = [1, 2, 3]\n    mathmod.append(4)\n    return len(mathmod)\n";
        assert_eq!(
            call_multi_i32(&[("main.py", main), ("mathmod.py", MATHMOD)], "f"),
            4
        );
    }

    /// Write a small on-disk project (entry + modules) into a fresh temp dir
    /// and return the entry path. Files are (relative path, contents).
    fn write_project(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("waspy_user_modules_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for (name, contents) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("mkdir");
            }
            std::fs::write(path, contents).expect("write");
        }
        dir.join(files[0].0)
    }

    fn call_file_i32(entry: &std::path::Path, func: &str) -> i32 {
        let options = CompilerOptions {
            optimize: false,
            ..CompilerOptions::default()
        };
        let wasm = compile_python_file_with_options(entry, &options).expect("compilation");
        let engine = Engine::default();
        let module = Module::new(&engine, &wasm[..]).expect("valid wasm module");
        let mut store = Store::new(&engine, ());
        let instance = Linker::<()>::new(&engine)
            .instantiate(&mut store, &module)
            .expect("instantiation")
            .start(&mut store)
            .expect("start");
        instance
            .get_typed_func::<(), i32>(&store, func)
            .expect("exported i32 fn")
            .call(&mut store, ())
            .expect("call")
    }

    #[test]
    fn entry_file_resolves_imports_from_disk() {
        // compile_python_file: the entry's imports load sibling .py files,
        // transitively, and a diamond dependency (`shared` imported by both
        // `util` and `helper`) links once (#41 module caching).
        let entry = write_project(
            "diamond",
            &[
                (
                    "app.py",
                    "from util import double\nimport helper\n\ndef f() -> int:\n    return double(10) + helper.triple(2)\n",
                ),
                (
                    "util.py",
                    "from shared import base\n\ndef double(x: int) -> int:\n    return x * 2 + base()\n",
                ),
                (
                    "helper.py",
                    "from shared import base\n\ndef triple(x: int) -> int:\n    return x * 3 + base()\n",
                ),
                ("shared.py", "def base() -> int:\n    return 0\n"),
            ],
        );
        assert_eq!(call_file_i32(&entry, "f"), 26);
    }

    #[test]
    fn dotted_import_resolves_package_path() {
        // `import pkg.mod` resolves to `pkg/mod.py`; its functions link into
        // the flat namespace (callable via `from pkg.mod import f`).
        let entry = write_project(
            "package",
            &[
                (
                    "app.py",
                    "from pkg.geometry import area\n\ndef f() -> int:\n    return area(6, 7)\n",
                ),
                (
                    "pkg/geometry.py",
                    "def area(w: int, h: int) -> int:\n    return w * h\n",
                ),
            ],
        );
        assert_eq!(call_file_i32(&entry, "f"), 42);
    }

    #[test]
    fn resolver_visits_each_module_once() {
        // The resolver's cache: a module reachable through several import
        // paths appears exactly once in the resolved set (#41).
        let entry = write_project(
            "cache",
            &[
                (
                    "app.py",
                    "import a\nimport b\n\ndef f() -> int:\n    return 0\n",
                ),
                ("a.py", "import c\n\ndef fa() -> int:\n    return 1\n"),
                ("b.py", "import c\n\ndef fb() -> int:\n    return 2\n"),
                ("c.py", "def fc() -> int:\n    return 3\n"),
            ],
        );
        let source = std::fs::read_to_string(&entry).unwrap();
        let modules = analysis::imports::resolve_user_modules(&entry, &source).unwrap();
        let mut names: Vec<&str> = modules.iter().map(|(n, _, _)| n.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["a", "b", "c"]);
    }
}

#[cfg(test)]
mod file_io_tests {
    use super::*;

    use std::collections::HashMap;
    use wasmi::{Caller, Engine, Extern, Linker, Module, Store};

    /// In-memory filesystem backing the `waspy_host` imports in tests. This is
    /// a reference implementation of the documented host interface (#25).
    #[derive(Default)]
    struct HostFs {
        files: HashMap<String, Vec<u8>>,
        /// fd -> (file name, read cursor, readable, writable). fds index this
        /// vec; a closed fd is None.
        handles: Vec<Option<(String, usize, bool, bool)>>,
    }

    const FLAG_READ: i32 = 1;
    const FLAG_WRITE: i32 = 2;
    const FLAG_APPEND: i32 = 4;
    const FLAG_UPDATE: i32 = 16;

    fn memory_of(caller: &mut Caller<'_, HostFs>) -> wasmi::Memory {
        caller
            .get_export("memory")
            .and_then(Extern::into_memory)
            .expect("exported memory")
    }

    fn linker_with_host_fs(engine: &Engine) -> Linker<HostFs> {
        let mut linker = Linker::<HostFs>::new(engine);
        linker
            .func_wrap(
                "waspy_host",
                "open",
                |mut caller: Caller<'_, HostFs>, path_ptr: i32, path_len: i32, flags: i32| -> i32 {
                    let memory = memory_of(&mut caller);
                    let (data, fs) = memory.data_and_store_mut(&mut caller);
                    let start = path_ptr as usize;
                    let name = String::from_utf8_lossy(&data[start..start + path_len as usize])
                        .to_string();

                    let readable = flags & (FLAG_READ | FLAG_UPDATE) != 0;
                    let writable = flags & (FLAG_WRITE | FLAG_APPEND | FLAG_UPDATE) != 0;
                    if flags & FLAG_WRITE != 0 {
                        fs.files.insert(name.clone(), Vec::new()); // truncate/create
                    } else if flags & FLAG_APPEND != 0 {
                        fs.files.entry(name.clone()).or_default();
                    } else if !fs.files.contains_key(&name) {
                        return -1; // read of a missing file
                    }
                    fs.handles.push(Some((name, 0, readable, writable)));
                    (fs.handles.len() - 1) as i32
                },
            )
            .unwrap();
        linker
            .func_wrap(
                "waspy_host",
                "read",
                |mut caller: Caller<'_, HostFs>, fd: i32, buf: i32, len: i32| -> i32 {
                    let memory = memory_of(&mut caller);
                    let (data, fs) = memory.data_and_store_mut(&mut caller);
                    let Some(Some((name, pos, readable, _))) = fs.handles.get_mut(fd as usize)
                    else {
                        return -1;
                    };
                    if !*readable {
                        return -1;
                    }
                    let contents = fs
                        .files
                        .get(name.as_str())
                        .map(|c| c.as_slice())
                        .unwrap_or(&[]);
                    let n = (contents.len().saturating_sub(*pos)).min(len as usize);
                    let start = buf as usize;
                    data[start..start + n].copy_from_slice(&contents[*pos..*pos + n]);
                    *pos += n;
                    n as i32
                },
            )
            .unwrap();
        linker
            .func_wrap(
                "waspy_host",
                "write",
                |mut caller: Caller<'_, HostFs>, fd: i32, buf: i32, len: i32| -> i32 {
                    let memory = memory_of(&mut caller);
                    let (data, fs) = memory.data_and_store_mut(&mut caller);
                    let Some(Some((name, _, _, writable))) = fs.handles.get(fd as usize) else {
                        return -1;
                    };
                    if !*writable {
                        return -1;
                    }
                    let start = buf as usize;
                    let bytes = &data[start..start + len as usize];
                    fs.files
                        .get_mut(name.as_str())
                        .unwrap()
                        .extend_from_slice(bytes);
                    len
                },
            )
            .unwrap();
        linker
            .func_wrap(
                "waspy_host",
                "close",
                |mut caller: Caller<'_, HostFs>, fd: i32| -> i32 {
                    if let Some(handle) = caller.data_mut().handles.get_mut(fd as usize) {
                        *handle = None;
                        0
                    } else {
                        -1
                    }
                },
            )
            .unwrap();
        linker
    }

    /// Compile (unoptimized), instantiate with the in-memory host filesystem,
    /// and return the instance + store for calls and post-run inspection.
    fn instantiate_with_fs(source: &str) -> (wasmi::Instance, Store<HostFs>) {
        let options = CompilerOptions {
            optimize: false,
            ..CompilerOptions::default()
        };
        let wasm = compile_python_to_wasm_with_options(source, &options).expect("compilation");
        let engine = Engine::default();
        let module = Module::new(&engine, &wasm[..]).expect("valid wasm module");
        let mut store = Store::new(&engine, HostFs::default());
        let instance = linker_with_host_fs(&engine)
            .instantiate(&mut store, &module)
            .expect("instantiation")
            .start(&mut store)
            .expect("start");
        (instance, store)
    }

    fn call_fs_i32(source: &str, func: &str) -> (i32, Store<HostFs>) {
        let (instance, mut store) = instantiate_with_fs(source);
        let result = instance
            .get_typed_func::<(), i32>(&store, func)
            .expect("exported i32 fn")
            .call(&mut store, ())
            .expect("call");
        (result, store)
    }

    #[test]
    fn write_then_read_round_trips() {
        // A full round trip through the host interface: write() reports the
        // byte count, read() returns a string with the file's contents, and
        // the host sees the exact bytes (#25).
        let src = "def f() -> int:\n    f = open(\"data.txt\", \"w\")\n    n = f.write(\"hello world\")\n    f.close()\n    g = open(\"data.txt\", \"r\")\n    s = g.read()\n    g.close()\n    return n + len(s)\n";
        let (result, store) = call_fs_i32(src, "f");
        assert_eq!(result, 22); // 11 written + 11 read back
        assert_eq!(
            store.data().files.get("data.txt").map(|c| c.as_slice()),
            Some("hello world".as_bytes())
        );
    }

    #[test]
    fn with_open_closes_the_file() {
        // `with open(...) as f:` desugars to open/body/close (#25); the write
        // lands and both handles are closed by the end.
        let src = "def f() -> int:\n    with open(\"out.txt\", \"w\") as f:\n        f.write(\"abc\")\n    with open(\"out.txt\") as g:\n        s = g.read()\n    return len(s)\n";
        let (result, store) = call_fs_i32(src, "f");
        assert_eq!(result, 3);
        assert!(store.data().handles.iter().all(|h| h.is_none()));
    }

    #[test]
    fn read_with_size_caps_the_result() {
        let src = "def f() -> int:\n    f = open(\"cap.txt\", \"w\")\n    f.write(\"abcdefgh\")\n    f.close()\n    g = open(\"cap.txt\")\n    s = g.read(3)\n    g.close()\n    return len(s)\n";
        let (result, _store) = call_fs_i32(src, "f");
        assert_eq!(result, 3);
    }

    #[test]
    fn open_missing_file_returns_invalid_fd() {
        // Reading a file that was never created: the host reports fd -1 and
        // read() on it yields an empty string rather than trapping.
        let src = "def f() -> int:\n    g = open(\"missing.txt\", \"r\")\n    s = g.read()\n    g.close()\n    return len(s)\n";
        let (result, _store) = call_fs_i32(src, "f");
        assert_eq!(result, 0);
    }

    #[test]
    fn module_without_file_io_has_no_imports() {
        // Programs that never call open() must keep instantiating with an
        // empty import object (no `waspy_host` requirement).
        let options = CompilerOptions {
            optimize: false,
            ..CompilerOptions::default()
        };
        let wasm = compile_python_to_wasm_with_options("def f() -> int:\n    return 1\n", &options)
            .expect("compilation");
        let engine = Engine::default();
        let module = Module::new(&engine, &wasm[..]).expect("valid wasm module");
        assert_eq!(module.imports().len(), 0);
    }
}
