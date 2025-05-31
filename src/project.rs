use crate::errors::ChakraError;
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

/// Structure representing a Python project
pub struct PythonProject {
    /// Root directory of the project
    pub root_dir: PathBuf,
    /// Map of module paths to file paths
    pub module_map: HashMap<String, PathBuf>,
    /// Map of file paths to their content
    pub file_contents: HashMap<PathBuf, String>,
    /// Dependency graph between modules
    pub dependencies: HashMap<String, HashSet<String>>,
}

impl PythonProject {
    /// Create a new Python project from a directory
    pub fn from_directory(dir: impl AsRef<Path>) -> Result<Self> {
        let root_dir = dir.as_ref().to_path_buf();

        if !root_dir.exists() || !root_dir.is_dir() {
            return Err(anyhow!(
                "Project directory does not exist or is not a directory"
            ));
        }

        let mut project = PythonProject {
            root_dir,
            module_map: HashMap::new(),
            file_contents: HashMap::new(),
            dependencies: HashMap::new(),
        };

        // Scan for Python files and build module map
        project.scan_directory()?;

        // Analyze dependencies
        project.analyze_dependencies()?;

        Ok(project)
    }

    /// Scan directory recursively for Python files
    fn scan_directory(&mut self) -> Result<()> {
        let python_files = self.collect_python_files(&self.root_dir)?;

        for path in python_files {
            // Read file content
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read Python file: {:?}", path))?;

            // Determine module path
            let rel_path = path.strip_prefix(&self.root_dir).unwrap_or(&path);
            let module_path = path_to_module_path(rel_path);

            // Add to maps
            self.module_map.insert(module_path.clone(), path.clone());
            self.file_contents.insert(path, content);
            self.dependencies.insert(module_path, HashSet::new());
        }

        Ok(())
    }

    /// Analyze dependencies between modules
    fn analyze_dependencies(&mut self) -> Result<()> {
        // For each file, extract imports and map them to modules
        for (module_path, file_path) in &self.module_map.clone() {
            if let Some(content) = self.file_contents.get(file_path) {
                let imports = extract_imports(content);

                for import_info in imports {
                    if let Some(dep_module) = self.resolve_import(&import_info, module_path) {
                        self.dependencies
                            .entry(module_path.clone())
                            .or_default()
                            .insert(dep_module);
                    }
                }
            }
        }

        Ok(())
    }

    /// Resolve an import statement to a module path
    fn resolve_import(&self, import_info: &ImportInfo, current_module: &str) -> Option<String> {
        match &import_info.import_type {
            ImportType::Direct => {
                // Direct import: "import module" or "import package.module"
                if self.module_map.contains_key(&import_info.module_name) {
                    Some(import_info.module_name.clone())
                } else {
                    // Check if it's a package import
                    let package_name = import_info.module_name.split('.').next()?;
                    if self.module_map.contains_key(package_name) {
                        Some(package_name.to_string())
                    } else {
                        None
                    }
                }
            }
            ImportType::From => {
                // From import: "from module import name"
                let module_parts: Vec<&str> = import_info.module_name.split('.').collect();
                if module_parts.is_empty() {
                    return None;
                }

                let base_module = module_parts[0].to_string();
                if self.module_map.contains_key(&base_module) {
                    Some(base_module)
                } else {
                    None
                }
            }
            ImportType::RelativeSingle => {
                // Relative import: "from . import module"
                let parent_module = get_parent_module(current_module)?;
                Some(parent_module)
            }
            ImportType::RelativeMultiple(levels) => {
                // Relative import with multiple levels: "from .. import module"
                let mut parent = current_module.to_string();
                for _ in 0..*levels {
                    parent = get_parent_module(&parent)?;
                }
                Some(parent)
            }
        }
    }

    /// Get all files in topological order (dependencies first)
    pub fn get_ordered_files(&self) -> Result<Vec<(PathBuf, String)>> {
        // Perform topological sort on module dependencies
        let ordered_modules = self
            .topological_sort()
            .map_err(|e| ChakraError::Other(format!("Dependency cycle detected: {}", e)))?;

        // Map modules back to file paths and contents
        let mut ordered_files = Vec::new();
        for module in ordered_modules {
            if let Some(path) = self.module_map.get(&module) {
                if let Some(content) = self.file_contents.get(path) {
                    ordered_files.push((path.clone(), content.clone()));
                }
            }
        }

        Ok(ordered_files)
    }

    /// Topological sort of modules based on dependencies
    fn topological_sort(&self) -> Result<Vec<String>, String> {
        let mut result = Vec::new();
        let mut unmarked: HashSet<String> = self.module_map.keys().cloned().collect();
        let mut temp_mark = HashSet::new();

        while let Some(node) = unmarked.iter().next().cloned() {
            self.visit(&node, &mut unmarked, &mut temp_mark, &mut result)?;
        }

        // Reverse so dependencies come first
        result.reverse();
        Ok(result)
    }

    /// Visit a node in topological sort
    fn visit(
        &self,
        node: &str,
        unmarked: &mut HashSet<String>,
        temp_mark: &mut HashSet<String>,
        result: &mut Vec<String>,
    ) -> Result<(), String> {
        if temp_mark.contains(node) {
            return Err(format!("Cycle detected with module {}", node));
        }

        if unmarked.contains(node) {
            temp_mark.insert(node.to_string());

            // Visit all dependencies
            if let Some(deps) = self.dependencies.get(node) {
                for dep in deps {
                    self.visit(dep, unmarked, temp_mark, result)?;
                }
            }

            // Add to result and remove marks
            temp_mark.remove(node);
            unmarked.remove(node);
            result.push(node.to_string());
        }

        Ok(())
    }

    /// Collect all Python files in a directory recursively
    fn collect_python_files(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut python_files = Vec::new();

        for entry in
            fs::read_dir(dir).with_context(|| format!("Failed to read directory: {:?}", dir))?
        {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Skip __pycache__, hidden directories, and virtual environments
                let dir_name = path.file_name().unwrap_or_default().to_string_lossy();

                if should_skip_directory(&dir_name) {
                    continue;
                }

                let mut subdir_files = self.collect_python_files(&path)?;
                python_files.append(&mut subdir_files);
            } else if path.is_file() && path.extension().map_or(false, |ext| ext == "py") {
                python_files.push(path);
            }
        }

        Ok(python_files)
    }

    /// Get a map of module paths to (file_path, content) pairs
    pub fn get_module_contents(&self) -> Vec<(String, (PathBuf, String))> {
        let mut result = Vec::new();

        for (module, path) in &self.module_map {
            if let Some(content) = self.file_contents.get(path) {
                result.push((module.clone(), (path.clone(), content.clone())));
            }
        }

        result
    }
}

/// Check if a directory should be skipped during scanning
fn should_skip_directory(dir_name: &str) -> bool {
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

/// Extract import statements from Python code
pub fn extract_imports(content: &str) -> Vec<ImportInfo> {
    let mut imports = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("import ") {
            // Handle simple import statements
            // Example: "import module" or "import module as alias"
            let module_part = trimmed.strip_prefix("import ").unwrap();

            // Split by commas to handle multiple imports
            // Example: "import os, sys, re"
            for module_item in module_part.split(',') {
                let module_name = module_item.trim().split(' ').next().unwrap_or("").trim();

                if !module_name.is_empty() {
                    imports.push(ImportInfo {
                        module_name: module_name.to_string(),
                        import_type: ImportType::Direct,
                    });
                }
            }
        } else if trimmed.starts_with("from ") {
            // Handle from import statements
            // Example: "from module import function"
            let from_parts: Vec<&str> = trimmed.splitn(3, ' ').collect();

            if from_parts.len() >= 3 && from_parts[2].starts_with("import ") {
                let module_name = from_parts[1].trim();

                if module_name == "." {
                    // Relative import from current package
                    imports.push(ImportInfo {
                        module_name: "".to_string(),
                        import_type: ImportType::RelativeSingle,
                    });
                } else if module_name.starts_with("..") {
                    // Relative import from parent package(s)
                    let level = module_name.chars().take_while(|&c| c == '.').count();
                    imports.push(ImportInfo {
                        module_name: module_name[level..].to_string(),
                        import_type: ImportType::RelativeMultiple(level),
                    });
                } else {
                    // Regular from import
                    imports.push(ImportInfo {
                        module_name: module_name.to_string(),
                        import_type: ImportType::From,
                    });
                }
            }
        }
    }

    imports
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

/// Get the parent module of a module path
fn get_parent_module(module_path: &str) -> Option<String> {
    let last_dot = module_path.rfind('.')?;
    Some(module_path[0..last_dot].to_string())
}

/// Types of Python imports
#[derive(Debug, Clone)]
pub enum ImportType {
    /// Regular import (import module)
    Direct,
    /// From import (from module import name)
    From,
    /// Relative import from current package (from . import name)
    RelativeSingle,
    /// Relative import from parent package (from .. import name)
    RelativeMultiple(usize),
}

/// Information about an import statement
#[derive(Debug, Clone)]
pub struct ImportInfo {
    /// Name of the imported module
    pub module_name: String,
    /// Type of import
    pub import_type: ImportType,
}
