//! Compiler options and configuration.

/// Verbosity level for compiler output
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    /// Minimal output (errors only)
    Quiet = 0,
    /// Normal output
    Normal = 1,
    /// Verbose output (--verbose)
    Verbose = 2,
    /// Debug output (--debug)
    Debug = 3,
}

impl Default for Verbosity {
    fn default() -> Self {
        Self::Normal
    }
}

impl Verbosity {
    /// Create from common command-line flag patterns
    pub fn from_flags(verbose: bool, debug: bool) -> Self {
        if debug {
            Self::Debug
        } else if verbose {
            Self::Verbose
        } else {
            Self::Normal
        }
    }

    /// Check if debug logging is enabled
    pub fn is_debug(&self) -> bool {
        *self >= Self::Debug
    }

    /// Check if verbose logging is enabled
    pub fn is_verbose(&self) -> bool {
        *self >= Self::Verbose
    }

    /// Check if normal logging is enabled
    pub fn is_normal(&self) -> bool {
        *self >= Self::Normal
    }
}

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

    /// Verbosity level for compiler output
    pub verbosity: Verbosity,
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
            verbosity: Verbosity::default(),
        }
    }
}
