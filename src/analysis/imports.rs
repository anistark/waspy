//! Module for analyzing Python imports.

use std::collections::VecDeque;
use std::path::Path;

/// Extract import statements from Python code
pub fn extract_imports(content: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();
    let mut in_try_block = false;
    let mut fallbacks = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Track try-except blocks for conditional imports
        if trimmed.starts_with("try:") {
            in_try_block = true;
            fallbacks.clear();
        } else if trimmed.starts_with("except ") {
            // We're in an except block, collect fallbacks
            in_try_block = false;
        } else if trimmed.starts_with("finally:") || trimmed == "else:" {
            // End of try-except block
            in_try_block = false;
            fallbacks.clear();
        }

        if trimmed.starts_with("import ") {
            // Handle simple import statements
            // Example: "import module" or "import module as alias"
            let module_part = trimmed.strip_prefix("import ").unwrap();

            // Split by commas to handle multiple imports
            // Example: "import os, sys, re"
            for module_item in module_part.split(',') {
                let parts: Vec<&str> = module_item.split_whitespace().collect();

                // Check for "import module as alias" pattern
                let module_name = parts[0].trim();
                let alias = if parts.len() >= 3 && parts[1] == "as" {
                    Some(parts[2].trim().to_string())
                } else {
                    None
                };

                if !module_name.is_empty() {
                    imports.push(ImportInfo {
                        module_name: module_name.to_string(),
                        import_type: ImportType::Direct,
                        alias,
                        name: None,
                        is_star: false,
                        is_conditional: in_try_block,
                        is_dynamic: false,
                        fallbacks: fallbacks.clone(),
                    });
                }
            }
        } else if trimmed.starts_with("from ") {
            // Handle from import statements
            // Example: "from module import function"
            let from_parts: Vec<&str> = trimmed.splitn(3, ' ').collect();

            if from_parts.len() >= 3 && from_parts[2].starts_with("import ") {
                let module_name = from_parts[1].trim();
                let import_part = from_parts[2].strip_prefix("import ").unwrap();

                // Handle star imports
                if import_part.trim() == "*" {
                    imports.push(ImportInfo {
                        module_name: module_name.to_string(),
                        import_type: if module_name.starts_with('.') {
                            if module_name == "." {
                                ImportType::RelativeSingle
                            } else {
                                ImportType::RelativeMultiple(
                                    module_name.chars().filter(|&c| c == '.').count(),
                                )
                            }
                        } else {
                            ImportType::From
                        },
                        alias: None,
                        name: Some("*".to_string()),
                        is_star: true,
                        is_conditional: in_try_block,
                        is_dynamic: false,
                        fallbacks: fallbacks.clone(),
                    });
                    continue;
                }

                // Handle named imports with possible aliases
                for import_item in import_part.split(',') {
                    let parts: Vec<&str> = import_item.split_whitespace().collect();

                    let name = parts[0].trim();
                    let alias = if parts.len() >= 3 && parts[1] == "as" {
                        Some(parts[2].trim().to_string())
                    } else {
                        None
                    };

                    imports.push(ImportInfo {
                        module_name: module_name.to_string(),
                        import_type: if module_name.starts_with('.') {
                            if module_name == "." {
                                ImportType::RelativeSingle
                            } else {
                                ImportType::RelativeMultiple(
                                    module_name.chars().filter(|&c| c == '.').count(),
                                )
                            }
                        } else {
                            ImportType::From
                        },
                        alias,
                        name: Some(name.to_string()),
                        is_star: false,
                        is_conditional: in_try_block,
                        is_dynamic: false,
                        fallbacks: fallbacks.clone(),
                    });
                }
            }
        } else if trimmed.contains("__import__") || trimmed.contains("importlib.import_module") {
            // Handle dynamic imports
            if let Some(module_name) = extract_dynamic_import(trimmed) {
                imports.push(ImportInfo {
                    module_name,
                    import_type: ImportType::Direct,
                    alias: None,
                    name: None,
                    is_star: false,
                    is_conditional: in_try_block,
                    is_dynamic: true,
                    fallbacks: fallbacks.clone(),
                });
            }
        }
    }

    imports
}

/// Extract dynamic import module name from a line of code
pub fn extract_dynamic_import(line: &str) -> Option<String> {
    // Extract from __import__('module')
    if let Some(start) = line.find("__import__(") {
        let rest = &line[start + 11..]; // Skip "__import__("
        if let Some(end) = rest.find(')') {
            let arg = &rest[..end];
            // Strip quotes
            let clean_arg = arg
                .trim()
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .trim_start_matches('"')
                .trim_end_matches('"');

            if !clean_arg.is_empty() {
                return Some(clean_arg.to_string());
            }
        }
    }

    // Extract from importlib.import_module('module')
    if let Some(start) = line.find("importlib.import_module(") {
        let rest = &line[start + 24..]; // Skip "importlib.import_module("
        if let Some(end) = rest.find(')') {
            let arg = &rest[..end];
            // Strip quotes
            let clean_arg = arg
                .trim()
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .trim_start_matches('"')
                .trim_end_matches('"');

            if !clean_arg.is_empty() {
                return Some(clean_arg.to_string());
            }
        }
    }

    None
}

/// Information about an import statement
#[derive(Debug, Clone)]
pub struct ImportInfo {
    pub module_name: String,
    pub import_type: ImportType,
    pub alias: Option<String>,
    pub name: Option<String>,
    pub is_star: bool,
    pub is_conditional: bool,
    pub is_dynamic: bool,
    pub fallbacks: Vec<String>,
}

/// Types of Python imports
#[derive(Debug, Clone)]
pub enum ImportType {
    Direct,
    From,
    RelativeSingle,
    RelativeMultiple(usize),
}

/// Convert file path to Python module path
pub fn path_to_module_path(path: &Path) -> String {
    let mut result = String::new();
    let mut path_components = VecDeque::new();

    // Get path components (except the extension)
    for component in path.components() {
        if let std::path::Component::Normal(comp) = component {
            let comp_str = comp.to_string_lossy();

            // Check if this is the file name component
            if let Some(file_name) = path.file_name() {
                if file_name == comp {
                    // For the file component, get the stem (without extension)
                    if let Some(stem) = path.file_stem() {
                        path_components.push_back(stem.to_string_lossy().to_string());
                    }
                    continue;
                }
            }

            // Skip __init__.py files
            if comp_str != "__init__.py" {
                path_components.push_back(comp_str.to_string());
            }
        }
    }

    // Build dot-separated module path
    if let Some(first) = path_components.pop_front() {
        result.push_str(&first);

        for component in path_components {
            result.push('.');
            result.push_str(&component);
        }
    }

    result
}
