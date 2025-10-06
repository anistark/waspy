//! File system utilities for working with Python files.

use crate::core::config;
use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Collect Python files that can be compiled to WebAssembly
pub fn collect_compilable_python_files(dir: &Path) -> Result<HashMap<String, String>> {
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
            println!("Skipping {path} (no functions)");
            continue;
        }

        // Skip files with import errors or other issues
        if has_complex_imports(&content) {
            println!("Skipping {path} (complex imports)");
            continue;
        }

        // Check for module-level code
        let has_module_level = has_module_level_code(&content);

        // With new IR support, we can handle some module-level code
        // but let's still skip complex cases
        if has_module_level && has_complex_module_level_code(&content) {
            println!("Skipping {path} (complex module-level code)");
            continue;
        }

        // Add compilable file
        compilable_files.insert(path, content);
    }

    Ok(compilable_files)
}

/// Recursively collect Python files from a directory
pub fn collect_python_files_recursive(
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
        } else if path.is_file() && path.extension().is_some_and(|ext| ext == "py") {
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
pub fn is_special_python_file(filename: &str) -> bool {
    let filename = Path::new(filename)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    filename.starts_with("__")
        || filename == "setup.py"
        || filename.contains("test")
        || filename.contains("config")
        || config::is_config_file(&filename)
}

/// Check if a Python file contains function definitions
pub fn contains_function_definitions(content: &str) -> bool {
    for line in content.lines() {
        if line.trim().starts_with("def ") {
            return true;
        }
    }
    false
}

/// Check if a Python file has complex imports that might not be supported
pub fn has_complex_imports(content: &str) -> bool {
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
pub fn has_module_level_code(content: &str) -> bool {
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
pub fn has_complex_module_level_code(content: &str) -> bool {
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

/// Check if a directory should be skipped during scanning
pub fn should_skip_directory(dir_name: &str) -> bool {
    // Skip these directories:
    dir_name.starts_with("__pycache__") || // Python cache
    dir_name.starts_with('.') ||           // Hidden directories
    dir_name == "venv" ||                  // Common virtual environment name
    dir_name.starts_with("env") ||         // Another common virtual environment name  
    dir_name == "node_modules" ||          // JavaScript dependencies
    dir_name.contains("site-packages") ||  // Installed packages
    dir_name == "dist" ||                  // Distribution directory
    dir_name == "build" // Build directory
}
