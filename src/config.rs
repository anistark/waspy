use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration data from a Python project
pub struct ProjectConfig {
    /// Project name
    pub name: String,
    /// Project version
    pub version: String,
    /// Project description
    pub description: Option<String>,
    /// Project author
    pub author: Option<String>,
    /// Dependencies
    pub dependencies: Vec<String>,
    /// Python version requirement
    pub python_requires: Option<String>,
    /// Module initialization code from __init__.py files
    pub init_code: HashMap<String, String>,
    /// Configuration settings
    pub settings: HashMap<String, String>,
    /// Build options
    pub build_options: HashMap<String, String>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        ProjectConfig {
            name: String::new(),
            version: "0.1.0".to_string(),
            description: None,
            author: None,
            dependencies: Vec::new(),
            python_requires: None,
            init_code: HashMap::new(),
            settings: HashMap::new(),
            build_options: HashMap::new(),
        }
    }
}

impl ProjectConfig {
    /// Create a new empty project configuration
    pub fn new() -> Self {
        ProjectConfig::default()
    }

    /// Extract a value from a Python string assignment
    fn extract_value_from_string(line: &str) -> Option<String> {
        let parts: Vec<&str> = line.split('=').collect();
        if parts.len() < 2 {
            return None;
        }

        let value_part = parts[1].trim();
        if (value_part.starts_with('"') && value_part.ends_with('"'))
            || (value_part.starts_with('\'') && value_part.ends_with('\''))
        {
            // Remove quotes and trailing comma if present
            Some(
                value_part[1..value_part.len() - 1]
                    .trim_end_matches(',')
                    .to_string(),
            )
        } else {
            // For non-string values, just take the value as is
            Some(value_part.trim_end_matches(',').to_string())
        }
    }

    /// Parse a setup.py file
    pub fn parse_setup_py(&mut self, content: &str) -> Result<()> {
        // This is a simplified parser for setup.py files
        for line in content.lines() {
            let line = line.trim();

            if line.starts_with("name=") || line.contains("'name'") || line.contains("\"name\"") {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.name = value;
                }
            } else if line.starts_with("version=")
                || line.contains("'version'")
                || line.contains("\"version\"")
            {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.version = value;
                }
            } else if line.starts_with("description=")
                || line.contains("'description'")
                || line.contains("\"description\"")
            {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.description = Some(value);
                }
            } else if line.starts_with("author=")
                || line.contains("'author'")
                || line.contains("\"author\"")
            {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.author = Some(value);
                }
            } else if line.starts_with("python_requires=")
                || line.contains("'python_requires'")
                || line.contains("\"python_requires\"")
            {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.python_requires = Some(value);
                }
            } else if line.starts_with("install_requires=")
                || line.contains("'install_requires'")
                || line.contains("\"install_requires\"")
            {
                // This is a simplified approach - properly parsing list contents would be more complex
                if line.contains("[") && line.contains("]") {
                    let start = line.find('[').unwrap();
                    let end = line.rfind(']').unwrap();
                    let deps_str = &line[start + 1..end];

                    for dep in deps_str.split(',') {
                        let dep = dep.trim();
                        if !dep.is_empty() {
                            // Remove quotes
                            let clean_dep = dep
                                .trim_start_matches('\'')
                                .trim_end_matches('\'')
                                .trim_start_matches('"')
                                .trim_end_matches('"');
                            if !clean_dep.is_empty() {
                                self.dependencies.push(clean_dep.to_string());
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse a __version__.py or __about__.py file
    pub fn parse_version_file(&mut self, content: &str) -> Result<()> {
        for line in content.lines() {
            let line = line.trim();

            if line.starts_with("__version__") {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.version = value;
                }
            } else if line.starts_with("__author__") {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.author = Some(value);
                }
            } else if line.starts_with("__description__") {
                if let Some(value) = Self::extract_value_from_string(line) {
                    self.description = Some(value);
                }
            }
        }

        Ok(())
    }

    /// Parse a pyproject.toml file
    pub fn parse_pyproject_toml(&mut self, content: &str) -> Result<()> {
        let mut in_build_system = false;
        let mut in_project = false;
        let mut in_dependencies = false;

        for line in content.lines() {
            let line = line.trim();

            if line.starts_with("[build-system]") {
                in_build_system = true;
                in_project = false;
                in_dependencies = false;
            } else if line.starts_with("[project]") {
                in_build_system = false;
                in_project = true;
                in_dependencies = false;
            } else if line.starts_with("[project.dependencies]")
                || line.starts_with("[tool.poetry.dependencies]")
            {
                in_build_system = false;
                in_project = false;
                in_dependencies = true;
            } else if line.starts_with("[") {
                // Any other section
                in_build_system = false;
                in_project = false;
                in_dependencies = false;
            } else if !line.is_empty() && !line.starts_with('#') {
                if in_build_system {
                    let parts: Vec<&str> = line.splitn(2, '=').collect();
                    if parts.len() == 2 {
                        let key = parts[0].trim();
                        let value = parts[1].trim();
                        // Remove quotes if present
                        let clean_value = value
                            .trim_start_matches('"')
                            .trim_end_matches('"')
                            .trim_start_matches('\'')
                            .trim_end_matches('\'');
                        self.build_options
                            .insert(key.to_string(), clean_value.to_string());
                    }
                } else if in_project {
                    let parts: Vec<&str> = line.splitn(2, '=').collect();
                    if parts.len() == 2 {
                        let key = parts[0].trim();
                        let value = parts[1].trim();
                        // Remove quotes if present
                        let clean_value = value
                            .trim_start_matches('"')
                            .trim_end_matches('"')
                            .trim_start_matches('\'')
                            .trim_end_matches('\'');

                        match key {
                            "name" => self.name = clean_value.to_string(),
                            "version" => self.version = clean_value.to_string(),
                            "description" => self.description = Some(clean_value.to_string()),
                            "authors" => {
                                // Simple parsing of author list
                                if clean_value.starts_with('[') && clean_value.ends_with(']') {
                                    let authors = clean_value[1..clean_value.len() - 1].trim();
                                    if !authors.is_empty() {
                                        self.author = Some(authors.to_string());
                                    }
                                } else {
                                    self.author = Some(clean_value.to_string());
                                }
                            }
                            "requires-python" => {
                                self.python_requires = Some(clean_value.to_string())
                            }
                            _ => {
                                self.settings
                                    .insert(key.to_string(), clean_value.to_string());
                            }
                        }
                    }
                } else if in_dependencies {
                    let parts: Vec<&str> = line.splitn(2, '=').collect();
                    if parts.len() == 2 {
                        let package = parts[0].trim();
                        let version = parts[1].trim();
                        self.dependencies.push(format!("{} {}", package, version));
                    }
                }
            }
        }

        Ok(())
    }

    /// Parse an __init__.py file
    pub fn parse_init_py(&mut self, module_path: &str, content: &str) -> Result<()> {
        // Store the content for later use in compilation
        self.init_code
            .insert(module_path.to_string(), content.to_string());
        Ok(())
    }

    /// Parse a conftest.py file
    pub fn parse_conftest_py(&mut self, _content: &str) -> Result<()> {
        // For now, we just store a marker that we have pytest configuration
        self.settings
            .insert("has_pytest".to_string(), "true".to_string());
        Ok(())
    }
}

/// Load and parse configuration files from a Python project
pub fn load_project_config<P: AsRef<Path>>(project_dir: P) -> Result<ProjectConfig> {
    let project_dir = project_dir.as_ref();
    let mut config = ProjectConfig::new();

    // Try to get project name from directory
    if let Some(dir_name) = project_dir.file_name() {
        config.name = dir_name.to_string_lossy().to_string();
    }

    // Check for setup.py
    let setup_py_path = project_dir.join("setup.py");
    if setup_py_path.exists() && setup_py_path.is_file() {
        match fs::read_to_string(&setup_py_path) {
            Ok(content) => {
                config
                    .parse_setup_py(&content)
                    .context("Failed to parse setup.py")?;
            }
            Err(e) => {
                println!("Warning: Could not read setup.py: {}", e);
            }
        }
    }

    // Check for __version__.py or __about__.py in project root
    for version_file in &["__version__.py", "__about__.py"] {
        let version_path = project_dir.join(version_file);
        if version_path.exists() && version_path.is_file() {
            match fs::read_to_string(&version_path) {
                Ok(content) => {
                    config
                        .parse_version_file(&content)
                        .context(format!("Failed to parse {}", version_file))?;
                }
                Err(e) => {
                    println!("Warning: Could not read {}: {}", version_file, e);
                }
            }
        }
    }

    // Check for pyproject.toml
    let pyproject_path = project_dir.join("pyproject.toml");
    if pyproject_path.exists() && pyproject_path.is_file() {
        match fs::read_to_string(&pyproject_path) {
            Ok(content) => {
                config
                    .parse_pyproject_toml(&content)
                    .context("Failed to parse pyproject.toml")?;
            }
            Err(e) => {
                println!("Warning: Could not read pyproject.toml: {}", e);
            }
        }
    }

    // Look for __init__.py files in the project
    find_and_parse_init_files(&mut config, project_dir, "")?;

    // Check for conftest.py (pytest configuration)
    find_conftest_files(&mut config, project_dir)?;

    Ok(config)
}

/// Find and parse all __init__.py files in the project
fn find_and_parse_init_files(
    config: &mut ProjectConfig,
    dir: &Path,
    parent_module: &str,
) -> Result<()> {
    // Skip directories that shouldn't be included
    if should_skip_directory(dir) {
        return Ok(());
    }

    let init_path = dir.join("__init__.py");
    if init_path.exists() && init_path.is_file() {
        match fs::read_to_string(&init_path) {
            Ok(content) => {
                // Calculate module path
                let module_path = if parent_module.is_empty() {
                    dir.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                } else {
                    format!(
                        "{}.{}",
                        parent_module,
                        dir.file_name().unwrap_or_default().to_string_lossy()
                    )
                };

                config
                    .parse_init_py(&module_path, &content)
                    .context(format!("Failed to parse {}", init_path.display()))?;
            }
            Err(e) => {
                println!("Warning: Could not read {}: {}", init_path.display(), e);
            }
        }
    }

    // Recursively check subdirectories
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Calculate the new parent module
            let new_parent = if parent_module.is_empty() {
                dir.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            } else {
                format!(
                    "{}.{}",
                    parent_module,
                    dir.file_name().unwrap_or_default().to_string_lossy()
                )
            };

            find_and_parse_init_files(config, &path, &new_parent)?;
        }
    }

    Ok(())
}

/// Find and parse all conftest.py files in the project
fn find_conftest_files(config: &mut ProjectConfig, dir: &Path) -> Result<()> {
    // Skip directories that shouldn't be included
    if should_skip_directory(dir) {
        return Ok(());
    }

    let conftest_path = dir.join("conftest.py");
    if conftest_path.exists() && conftest_path.is_file() {
        match fs::read_to_string(&conftest_path) {
            Ok(content) => {
                config
                    .parse_conftest_py(&content)
                    .context(format!("Failed to parse {}", conftest_path.display()))?;
            }
            Err(e) => {
                println!("Warning: Could not read {}: {}", conftest_path.display(), e);
            }
        }
    }

    // Recursively check subdirectories
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() && !should_skip_directory(&path) {
            find_conftest_files(config, &path)?;
        }
    }

    Ok(())
}

/// Check if a directory should be skipped during scanning
fn should_skip_directory(dir: &Path) -> bool {
    if let Some(dir_name) = dir.file_name() {
        let dir_name = dir_name.to_string_lossy();
        return dir_name.starts_with("__pycache__") || // Python cache
            dir_name.starts_with('.') ||           // Hidden directories
            dir_name == "venv" ||                  // Common virtual environment name
            dir_name.starts_with("env") ||         // Another common virtual environment name  
            dir_name == "node_modules" ||          // JavaScript dependencies
            dir_name.contains("site-packages") ||  // Installed packages
            dir_name == "dist" ||                  // Distribution directory
            dir_name == "build"; // Build directory
    }
    false
}

/// Check if a file is a configuration file
pub fn is_config_file(filename: &str) -> bool {
    let filename_lower = filename.to_lowercase();
    filename_lower == "setup.py"
        || filename_lower == "__init__.py"
        || filename_lower == "__about__.py"
        || filename_lower == "__version__.py"
        || filename_lower == "pyproject.toml"
        || filename_lower == "conftest.py"
}

/// Extract module initialization code for a specific module
pub fn get_module_init_code<'a>(config: &'a ProjectConfig, module_name: &str) -> Option<&'a str> {
    config.init_code.get(module_name).map(|s| s.as_str())
}

/// Extract all Python files from project with config awareness
pub fn collect_python_files_with_config<P: AsRef<Path>>(
    project_dir: P,
    config: &ProjectConfig,
) -> Result<HashMap<String, (PathBuf, String)>> {
    let project_dir = project_dir.as_ref();
    let mut files = HashMap::new();

    collect_python_files_recursive(project_dir, project_dir, &mut files, config)?;

    Ok(files)
}

/// Recursively collect Python files from a directory with config awareness
fn collect_python_files_recursive(
    root_dir: &Path,
    current_dir: &Path,
    files: &mut HashMap<String, (PathBuf, String)>,
    config: &ProjectConfig,
) -> Result<()> {
    for entry in fs::read_dir(current_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Skip directories that shouldn't be included
            if should_skip_directory(&path) {
                continue;
            }

            // Recursively scan subdirectory
            collect_python_files_recursive(root_dir, &path, files, config)?;
        } else if path.is_file() && path.extension().map_or(false, |ext| ext == "py") {
            // Skip configuration files
            if let Some(file_name) = path.file_name() {
                let file_name = file_name.to_string_lossy();
                if is_config_file(&file_name) {
                    // Skip known config files
                    continue;
                }
            }

            // Read file content
            match fs::read_to_string(&path) {
                Ok(content) => {
                    // Calculate module path
                    let rel_path = path
                        .strip_prefix(root_dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();

                    // Add the file to our collection
                    files.insert(rel_path.clone(), (path.clone(), content));
                }
                Err(e) => {
                    println!("Warning: Failed to read {}: {}", path.display(), e);
                }
            }
        }
    }

    Ok(())
}
