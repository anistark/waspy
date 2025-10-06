use crate::ir::{IRBody, IRConstant, IRExpr, IRFunction, IRModule, IRStatement, IRType};
use anyhow::Result;
use std::path::Path;

/// Entry points
pub struct EntryPointInfo {
    pub main_function_name: String,
    pub detected_type: EntryPointType,
    pub contains_args_parsing: bool,
}

/// Types of entry points that can be detected
#[derive(Debug, PartialEq)]
pub enum EntryPointType {
    MainPyFile,    // __main__.py file
    MainNameCheck, // if __name__ == "__main__"
    CliScript,     // Script with argparse/click/etc.
    NoEntryPointDetected,
}

/// Detect entry points in a Python file
pub fn detect_entry_points(
    source: &str,
    file_path: Option<&Path>,
) -> Result<Option<EntryPointInfo>> {
    // Check if this is a __main__.py file
    if let Some(path) = file_path {
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy() == "__main__.py")
        {
            return Ok(Some(EntryPointInfo {
                main_function_name: "main".to_string(),
                detected_type: EntryPointType::MainPyFile,
                contains_args_parsing: source.contains("sys.argv") || source.contains("argparse"),
            }));
        }
    }

    // Check for if __name__ == "__main__" pattern
    if source.contains("if __name__ == \"__main__\"")
        || source.contains("if __name__ == '__main__'")
    {
        // Look for a "main" function in the source
        let main_function_name = if source.contains("def main(") {
            "main".to_string()
        } else {
            // If no main function, use a default name
            "main".to_string()
        };

        // Check for argument parsing code
        let contains_args_parsing = source.contains("argparse")
            || source.contains("sys.argv")
            || source.contains("import click")
            || source.contains("import typer");

        return Ok(Some(EntryPointInfo {
            main_function_name,
            detected_type: EntryPointType::MainNameCheck,
            contains_args_parsing,
        }));
    }

    // Check for command-line interface modules
    let is_cli_module = source.contains("argparse.ArgumentParser")
        || source.contains("import click")
        || source.contains("import typer")
        || (source.contains("import sys") && source.contains("sys.argv"));

    if is_cli_module {
        // Look for a "main" function in the source
        let main_function_name = if source.contains("def main(") {
            "main".to_string()
        } else {
            // If no main function, default to a generic name
            "main".to_string()
        };

        return Ok(Some(EntryPointInfo {
            main_function_name,
            detected_type: EntryPointType::CliScript,
            contains_args_parsing: true,
        }));
    }

    // If no entry point patterns are found
    Ok(None)
}

/// Generate a main function for WebAssembly from an entry point
pub fn create_main_function_from_entry_point(
    _source: &str,
    entry_point_info: &EntryPointInfo,
) -> Result<IRFunction> {
    // Create statements based on entry point type
    let body_statements = match entry_point_info.detected_type {
        EntryPointType::MainNameCheck | EntryPointType::CliScript => {
            // Call the user's main function and return its result
            vec![
                // Call the user's main function
                IRStatement::Return(Some(IRExpr::FunctionCall {
                    function_name: entry_point_info.main_function_name.clone(),
                    arguments: Vec::new(),
                })),
            ]
        }
        EntryPointType::MainPyFile => {
            // For __main__.py files, call the main function and return 0
            vec![
                // Call the user's main function
                IRStatement::Expression(IRExpr::FunctionCall {
                    function_name: entry_point_info.main_function_name.clone(),
                    arguments: Vec::new(),
                }),
                // Return 0 for success
                IRStatement::Return(Some(IRExpr::Const(IRConstant::Int(0)))),
            ]
        }
        _ => {
            // Default case: just return 0
            vec![IRStatement::Return(Some(IRExpr::Const(IRConstant::Int(0))))]
        }
    };

    // Create an IR function for the main entry point
    let main_function = IRFunction {
        name: "main".to_string(), // Always use "main" as the WebAssembly entry point
        params: Vec::new(),       // No parameters for main
        return_type: IRType::Int, // Return int for WebAssembly compatibility
        decorators: Vec::new(),
        body: IRBody {
            statements: body_statements,
        },
    };

    Ok(main_function)
}

/// Add entry point support to an existing IR module
pub fn add_entry_point_to_module(
    module: &mut IRModule,
    entry_point_info: &EntryPointInfo,
) -> Result<()> {
    // Create the main function
    let main_function = create_main_function_from_entry_point("", entry_point_info)?;

    // First check if a main function already exists
    let main_exists = module.functions.iter().any(|f| f.name == "main");

    // Add the main function to the module if it doesn't already exist
    if !main_exists {
        module.functions.push(main_function);
    }

    // Add entry point metadata
    module
        .metadata
        .insert("has_entry_point".to_string(), "true".to_string());
    module.metadata.insert(
        "entry_point_type".to_string(),
        format!("{:?}", entry_point_info.detected_type),
    );

    Ok(())
}
