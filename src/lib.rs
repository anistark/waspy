pub mod compiler;
pub mod errors;
pub mod ir;
pub mod optimizer;
pub mod parser;

use anyhow::{anyhow, Context, Result};

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
        // Parse Python to AST
        let ast = parser::parse_python(source)
            .with_context(|| format!("Failed to parse Python file: {}", filename))?;

        // Lower AST to IR
        let ir_module = ir::lower_ast_to_ir(&ast).with_context(|| {
            format!("Failed to convert Python AST to IR for file: {}", filename)
        })?;

        // Check for duplicate function names
        for func in &ir_module.functions {
            if !function_names.insert(func.name.clone()) {
                return Err(anyhow!(
                    "Duplicate function name '{}' found in file: {}",
                    func.name,
                    filename
                ));
            }
        }

        // Collect functions from the IR module
        all_functions.extend(ir_module.functions);
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
