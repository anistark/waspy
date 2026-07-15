use std::fmt;
use thiserror::Error;

/// Render an optional line/column pair as a display suffix (" at line L,
/// column C"), or nothing when the location is unknown.
fn line_col_suffix(line: &Option<usize>, column: &Option<usize>) -> String {
    match (line, column) {
        (Some(line), Some(column)) => format!(" at line {line}, column {column}"),
        (Some(line), None) => format!(" at line {line}"),
        _ => String::new(),
    }
}

/// Render an optional [`ErrorLocation`] as a display suffix (" in file f at
/// line L, ..."), or nothing when the location is unknown.
fn location_suffix(location: &Option<ErrorLocation>) -> String {
    match location {
        Some(location) => format!(" ({location})"),
        None => String::new(),
    }
}

/// Custom error types for Waspy.
///
/// Every located variant renders its position information in the display
/// output, so downstream users see "Unsupported feature: ... (at line 3)"
/// without having to destructure the error. (The `Chakra` name is legacy —
/// the project's original name — kept because the type is public API.)
#[derive(Error, Debug)]
pub enum ChakraError {
    #[error("Python parsing error: {message}{}", line_col_suffix(.line, .column))]
    ParseError {
        message: String,
        line: Option<usize>,
        column: Option<usize>,
    },

    #[error("Type error: {message}{}", location_suffix(.location))]
    TypeError {
        message: String,
        location: Option<ErrorLocation>,
    },

    #[error("Unsupported feature: {message}{}", location_suffix(.location))]
    UnsupportedFeature {
        message: String,
        location: Option<ErrorLocation>,
    },

    #[error("Name error: {message}{}", location_suffix(.location))]
    NameError {
        message: String,
        location: Option<ErrorLocation>,
    },

    #[error("WebAssembly compilation error: {0}")]
    WasmCompilationError(String),

    #[error("WebAssembly optimization error: {0}")]
    WasmOptimizationError(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Other error: {0}")]
    Other(String),
}

/// Location information for errors
#[derive(Debug, Clone)]
pub struct ErrorLocation {
    pub file: Option<String>,
    pub line: usize,
    pub column: Option<usize>,
    pub function: Option<String>,
}

impl fmt::Display for ErrorLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(file) = &self.file {
            write!(f, "in file {file} ")?;
        }

        write!(f, "at line {}", self.line)?;

        if let Some(column) = self.column {
            write!(f, ", column {column}")?;
        }

        if let Some(function) = &self.function {
            write!(f, " (in function '{function}')")?;
        }

        Ok(())
    }
}

/// Compiler warning
#[derive(Debug, Clone)]
pub struct Warning {
    pub message: String,
    pub location: Option<ErrorLocation>,
    pub warning_type: WarningType,
}

/// Types of warnings
#[derive(Debug, Clone)]
pub enum WarningType {
    Performance,
    Compatibility,
    TypeInference,
    UnusedVariable,
    Other,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} warning: {}", self.warning_type, self.message)?;

        if let Some(location) = &self.location {
            write!(f, " ({location})")?;
        }

        Ok(())
    }
}

impl fmt::Display for WarningType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WarningType::Performance => write!(f, "Performance"),
            WarningType::Compatibility => write!(f, "Compatibility"),
            WarningType::TypeInference => write!(f, "Type inference"),
            WarningType::UnusedVariable => write!(f, "Unused variable"),
            WarningType::Other => write!(f, "Warning"),
        }
    }
}

/// Compilation results with warnings
pub struct CompilationResult {
    pub wasm: Vec<u8>,
    pub warnings: Vec<Warning>,
}

/// Parse error
pub fn parse_error(
    message: impl Into<String>,
    line: Option<usize>,
    column: Option<usize>,
) -> ChakraError {
    ChakraError::ParseError {
        message: message.into(),
        line,
        column,
    }
}

/// Type error
pub fn type_error(message: impl Into<String>, location: Option<ErrorLocation>) -> ChakraError {
    ChakraError::TypeError {
        message: message.into(),
        location,
    }
}

/// Unsupported feature error
pub fn unsupported_feature(
    message: impl Into<String>,
    location: Option<ErrorLocation>,
) -> ChakraError {
    ChakraError::UnsupportedFeature {
        message: message.into(),
        location,
    }
}

/// Name error (undefined variable/function)
pub fn name_error(message: impl Into<String>, location: Option<ErrorLocation>) -> ChakraError {
    ChakraError::NameError {
        message: message.into(),
        location,
    }
}

/// WebAssembly compilation error
pub fn wasm_compilation_error(message: impl Into<String>) -> ChakraError {
    ChakraError::WasmCompilationError(message.into())
}

/// WebAssembly optimization error
pub fn wasm_optimization_error(message: impl Into<String>) -> ChakraError {
    ChakraError::WasmOptimizationError(message.into())
}

/// Generic error
pub fn other_error(message: impl Into<String>) -> ChakraError {
    ChakraError::Other(message.into())
}

/// New warning
pub fn warning(
    message: impl Into<String>,
    location: Option<ErrorLocation>,
    warning_type: WarningType,
) -> Warning {
    Warning {
        message: message.into(),
        location,
        warning_type,
    }
}
