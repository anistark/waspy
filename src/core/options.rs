//! Compiler options and configuration.

/// Options for Python to WebAssembly compilation
#[derive(Debug, Clone)]
pub struct CompilerOptions {
    /// Whether to optimize the WebAssembly output
    pub optimize: bool,

    /// Whether to include debug information
    pub debug_info: bool,

    /// Maximum memory size in pages (64KB each)
    pub max_memory: u32,

    /// The entry point function name
    pub entry_point: Option<String>,

    /// Whether to generate HTML test harness
    pub generate_html: bool,

    /// Whether to include metadata in the WebAssembly module
    pub include_metadata: bool,
}

impl Default for CompilerOptions {
    fn default() -> Self {
        Self {
            optimize: true,
            debug_info: false,
            max_memory: 2, // 2 pages = 128KB
            entry_point: None,
            generate_html: false,
            include_metadata: false,
        }
    }
}
