//! Module for project metadata extraction and analysis.

use crate::core::parser;
use crate::ir;
use crate::utils::fs;
use anyhow::{Context, Result};
use std::path::Path;

/// Function's signature information
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    /// Name of the function
    pub name: String,
    /// Parameters with type annotations
    pub parameters: Vec<String>,
    /// Return type as a string
    pub return_type: String,
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
) -> Result<Vec<(String, Vec<FunctionSignature>)>> {
    let project_dir = project_dir.as_ref();
    let files = fs::collect_compilable_python_files(project_dir)?;

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

/// Convert IR type to string representation
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
        ir::IRType::Tuple(types) => {
            let inner = types
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(", ");
            format!("Tuple[{inner}]")
        }
        ir::IRType::Optional(inner) => format!("Optional[{}]", type_to_string(inner)),
        ir::IRType::Union(types) => {
            let inner = types
                .iter()
                .map(type_to_string)
                .collect::<Vec<_>>()
                .join(" | ");
            format!("Union[{inner}]")
        }
        ir::IRType::Class(name) => name.clone(),
        ir::IRType::Module(name) => format!("Module[{name}]"),
        ir::IRType::Bytes => "bytes".to_string(),
        ir::IRType::None => "None".to_string(),
        ir::IRType::Any => "Any".to_string(),
        ir::IRType::Unknown => "unknown".to_string(),
    }
}
