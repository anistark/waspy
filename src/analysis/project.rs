//! Project structure analysis and dependencies.

use crate::analysis::imports::{extract_imports, path_to_module_path, ImportInfo, ImportType};
use crate::core::errors::ChakraError;
use crate::ir::IRImport;
use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, HashSet};
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
    /// Map to track circular dependencies
    pub circular_dependencies: HashMap<String, HashSet<String>>,
    /// Map of modules that use dynamic imports
    pub dynamic_imports: HashMap<String, Vec<String>>,
    /// Map of modules that use star imports
    pub star_imports: HashMap<String, Vec<String>>,
    /// Map to track import aliases
    pub import_aliases: HashMap<String, HashMap<String, String>>,
    /// Map of conditional imports (in try-except blocks)
    pub conditional_imports: HashMap<String, Vec<ConditionalImport>>,
}

/// Information about a conditional import (in try-except block)
#[derive(Debug, Clone)]
pub struct ConditionalImport {
    /// Primary module being imported
    pub primary_module: String,
    /// Fallback modules to use if primary fails
    pub fallback_modules: Vec<String>,
    /// Whether this is a star import
    pub is_star_import: bool,
    /// Name being imported (if from module import name)
    pub name: Option<String>,
    /// Alias for the import
    pub alias: Option<String>,
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
            circular_dependencies: HashMap::new(),
            dynamic_imports: HashMap::new(),
            star_imports: HashMap::new(),
            import_aliases: HashMap::new(),
            conditional_imports: HashMap::new(),
        };

        // Scan for Python files and build module map
        project.scan_directory()?;

        // Analyze dependencies
        project.analyze_dependencies()?;

        // Detect circular dependencies
        project.detect_circular_dependencies();

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
                let mut module_imports = HashMap::new();

                // Initialize dependency tracking for this module
                self.dependencies.entry(module_path.clone()).or_default();
                self.dynamic_imports.entry(module_path.clone()).or_default();
                self.star_imports.entry(module_path.clone()).or_default();
                self.import_aliases.entry(module_path.clone()).or_default();
                self.conditional_imports
                    .entry(module_path.clone())
                    .or_default();

                for import_info in imports {
                    // Handle different import types
                    match import_info.import_type {
                        ImportType::Direct => {
                            if let Some(module) =
                                self.resolve_direct_import(&import_info, module_path)
                            {
                                self.dependencies
                                    .entry(module_path.clone())
                                    .or_default()
                                    .insert(module.clone());

                                // Track import aliases if present
                                if let Some(alias) = &import_info.alias {
                                    self.import_aliases
                                        .entry(module_path.clone())
                                        .or_default()
                                        .insert(module.clone(), alias.clone());
                                }

                                // Track if this is a conditional import
                                if import_info.is_conditional {
                                    let conditional_import = ConditionalImport {
                                        primary_module: module.clone(),
                                        fallback_modules: import_info.fallbacks.clone(),
                                        is_star_import: false,
                                        name: None,
                                        alias: import_info.alias.clone(),
                                    };

                                    self.conditional_imports
                                        .entry(module_path.clone())
                                        .or_default()
                                        .push(conditional_import);
                                }

                                // Track if this is a dynamic import
                                if import_info.is_dynamic {
                                    self.dynamic_imports
                                        .entry(module_path.clone())
                                        .or_default()
                                        .push(module.clone());
                                }

                                // Store in module_imports for later circular dependency analysis
                                module_imports.insert(module.clone(), import_info.clone());
                            }
                        }
                        ImportType::From => {
                            if let Some(module) =
                                self.resolve_from_import(&import_info, module_path)
                            {
                                self.dependencies
                                    .entry(module_path.clone())
                                    .or_default()
                                    .insert(module.clone());

                                // Track star imports
                                if import_info.is_star {
                                    self.star_imports
                                        .entry(module_path.clone())
                                        .or_default()
                                        .push(module.clone());
                                }

                                // Track import aliases if present
                                if let Some(name) = &import_info.name {
                                    if let Some(alias) = &import_info.alias {
                                        let qualified_name = format!("{}.{}", module, name);
                                        self.import_aliases
                                            .entry(module_path.clone())
                                            .or_default()
                                            .insert(qualified_name, alias.clone());
                                    }
                                }

                                // Track if this is a conditional import
                                if import_info.is_conditional {
                                    let conditional_import = ConditionalImport {
                                        primary_module: module.clone(),
                                        fallback_modules: import_info.fallbacks.clone(),
                                        is_star_import: import_info.is_star,
                                        name: import_info.name.clone(),
                                        alias: import_info.alias.clone(),
                                    };

                                    self.conditional_imports
                                        .entry(module_path.clone())
                                        .or_default()
                                        .push(conditional_import);
                                }

                                // Track if this is a dynamic import
                                if import_info.is_dynamic {
                                    self.dynamic_imports
                                        .entry(module_path.clone())
                                        .or_default()
                                        .push(module.clone());
                                }

                                // Store in module_imports for later circular dependency analysis
                                module_imports.insert(module.clone(), import_info.clone());
                            }
                        }
                        ImportType::RelativeSingle => {
                            if let Some(module) = self.resolve_relative_import(module_path, 1) {
                                self.dependencies
                                    .entry(module_path.clone())
                                    .or_default()
                                    .insert(module.clone());

                                // Store in module_imports for later circular dependency analysis
                                module_imports.insert(module.clone(), import_info.clone());
                            }
                        }
                        ImportType::RelativeMultiple(level) => {
                            if let Some(module) = self.resolve_relative_import(module_path, level) {
                                self.dependencies
                                    .entry(module_path.clone())
                                    .or_default()
                                    .insert(module.clone());

                                // Store in module_imports for later circular dependency analysis
                                module_imports.insert(module.clone(), import_info.clone());
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Resolve a direct import statement to a module path
    fn resolve_direct_import(
        &self,
        import_info: &ImportInfo,
        _current_module: &str,
    ) -> Option<String> {
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

    /// Resolve a from-import statement to a module path
    fn resolve_from_import(
        &self,
        import_info: &ImportInfo,
        _current_module: &str,
    ) -> Option<String> {
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

    /// Resolve a relative import to a module path
    fn resolve_relative_import(&self, current_module: &str, level: usize) -> Option<String> {
        let mut parent = current_module.to_string();
        for _ in 0..level {
            parent = get_parent_module(&parent)?;
        }
        Some(parent)
    }

    /// Detect circular dependencies in the project
    fn detect_circular_dependencies(&mut self) {
        // Clear existing circular dependencies
        self.circular_dependencies.clear();

        // For each module, check if any of its dependencies import it back (directly or indirectly)
        for (module, deps) in &self.dependencies {
            for dep in deps {
                // Check if this dependency imports the module back
                if self.has_dependency(dep, module) {
                    // Add to circular dependencies map
                    self.circular_dependencies
                        .entry(module.clone())
                        .or_default()
                        .insert(dep.clone());
                }
            }
        }
    }

    /// Check if a module depends on another module (directly or indirectly)
    fn has_dependency(&self, module: &str, target: &str) -> bool {
        // Check direct dependency
        if let Some(deps) = self.dependencies.get(module) {
            if deps.contains(target) {
                return true;
            }

            // Check indirect dependencies recursively (with cycle detection)
            let mut visited = HashSet::new();
            let mut stack = Vec::new();
            stack.extend(deps.iter().cloned());

            while let Some(dep) = stack.pop() {
                if dep == target {
                    return true;
                }

                if visited.insert(dep.clone()) {
                    if let Some(sub_deps) = self.dependencies.get(&dep) {
                        stack.extend(sub_deps.iter().cloned());
                    }
                }
            }
        }

        false
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

    /// Topological sort of modules based on dependencies (handles circular dependencies)
    fn topological_sort(&self) -> Result<Vec<String>, String> {
        let mut result = Vec::new();
        let mut unmarked: HashSet<String> = self.module_map.keys().cloned().collect();
        let mut temp_mark = HashSet::new();

        // List to track modules in a circular dependency
        let mut circular_modules = HashSet::new();

        // Add all modules that are part of a circular dependency to the circular_modules set
        for (module, deps) in &self.circular_dependencies {
            circular_modules.insert(module.clone());
            circular_modules.extend(deps.iter().cloned());
        }

        // First, visit all modules that are not in circular dependencies
        let non_circular_modules: Vec<String> = unmarked
            .iter()
            .filter(|m| !circular_modules.contains(*m))
            .cloned()
            .collect();

        for module in non_circular_modules {
            self.visit(
                &module,
                &mut unmarked,
                &mut temp_mark,
                &mut result,
                &circular_modules,
            )?;
        }

        // Then visit the circular dependencies
        // For circular dependencies, the order doesn't matter as much
        // We just need to ensure all modules are included
        for module in circular_modules {
            if unmarked.contains(&module) {
                unmarked.remove(&module);
                result.push(module);
            }
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
        circular_modules: &HashSet<String>,
    ) -> Result<(), String> {
        // Skip nodes that are part of circular dependencies
        if circular_modules.contains(node) {
            unmarked.remove(node);
            return Ok(());
        }

        if temp_mark.contains(node) {
            return Err(format!("Cycle detected with module {}", node));
        }

        if unmarked.contains(node) {
            temp_mark.insert(node.to_string());

            // Visit all dependencies
            if let Some(deps) = self.dependencies.get(node) {
                for dep in deps {
                    if !circular_modules.contains(dep) {
                        self.visit(dep, unmarked, temp_mark, result, circular_modules)?;
                    }
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

    /// Convert all imports to IRImport objects
    pub fn get_ir_imports(&self, module_path: &str) -> Vec<IRImport> {
        let mut ir_imports = Vec::new();

        // Add normal dependencies
        if let Some(deps) = self.dependencies.get(module_path) {
            for dep in deps {
                let mut is_star_import = false;
                let mut is_conditional = false;
                let mut is_dynamic = false;
                let mut name = None;
                let mut alias = None;
                let mut conditional_fallbacks = Vec::new();

                // Check if this is a star import
                if let Some(star_imports) = self.star_imports.get(module_path) {
                    is_star_import = star_imports.contains(dep);
                    if is_star_import {
                        name = Some("*".to_string());
                    }
                }

                // Check if this is a dynamic import
                if let Some(dynamic_imports) = self.dynamic_imports.get(module_path) {
                    is_dynamic = dynamic_imports.contains(dep);
                }

                // Check for import alias
                if let Some(aliases) = self.import_aliases.get(module_path) {
                    if let Some(alias_name) = aliases.get(dep) {
                        alias = Some(alias_name.clone());
                    }
                }

                // Check if this is a conditional import
                if let Some(conditional_imports) = self.conditional_imports.get(module_path) {
                    for cond_import in conditional_imports {
                        if &cond_import.primary_module == dep {
                            is_conditional = true;
                            conditional_fallbacks = cond_import.fallback_modules.clone();

                            // Update other fields based on conditional import info
                            is_star_import = cond_import.is_star_import;
                            if cond_import.is_star_import {
                                name = Some("*".to_string());
                            } else {
                                name = cond_import.name.clone();
                            }
                            alias = cond_import.alias.clone();
                            break;
                        }
                    }
                }

                // Create the IRImport object
                ir_imports.push(IRImport {
                    module: dep.clone(),
                    name: name.clone(),
                    alias: alias.clone(),
                    is_from_import: name.is_some(),
                    is_star_import,
                    is_conditional,
                    is_dynamic,
                    conditional_fallbacks,
                });
            }
        }

        ir_imports
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

/// Get the parent module of a module path
fn get_parent_module(module_path: &str) -> Option<String> {
    let last_dot = module_path.rfind('.')?;
    Some(module_path[0..last_dot].to_string())
}
