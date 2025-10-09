//! Logging utilities for waspy compiler.
//!
//! This module provides logging macros that respect the verbosity level
//! set in CompilerOptions. If a project using waspy provides --debug or
//! --verbose flags, they can control the logging output.

use crate::core::options::Verbosity;
use std::sync::RwLock;

/// Global verbosity level
static VERBOSITY: RwLock<Verbosity> = RwLock::new(Verbosity::Normal);

/// Initialize the logging system with a verbosity level
pub fn init(verbosity: Verbosity) {
    if let Ok(mut v) = VERBOSITY.write() {
        *v = verbosity;
    }
}

/// Get the current verbosity level
pub fn get_verbosity() -> Verbosity {
    VERBOSITY.read().map(|v| *v).unwrap_or(Verbosity::Normal)
}

/// Log a debug message (only shown with --debug)
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        if $crate::utils::logging::get_verbosity().is_debug() {
            println!("[DEBUG] {}", format!($($arg)*));
        }
    };
}

/// Log a verbose message (shown with --verbose or --debug)
#[macro_export]
macro_rules! log_verbose {
    ($($arg:tt)*) => {
        if $crate::utils::logging::get_verbosity().is_verbose() {
            println!("[VERBOSE] {}", format!($($arg)*));
        }
    };
}

/// Log an info message (shown at normal verbosity and above)
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        if $crate::utils::logging::get_verbosity().is_normal() {
            println!("{}", format!($($arg)*));
        }
    };
}

/// Log a warning message (always shown unless quiet)
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        if $crate::utils::logging::get_verbosity() >= $crate::core::options::Verbosity::Normal {
            eprintln!("Warning: {}", format!($($arg)*));
        }
    };
}

/// Log an error message (always shown)
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        eprintln!("Error: {}", format!($($arg)*));
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verbosity_levels() {
        init(Verbosity::Debug);
        assert!(get_verbosity().is_debug());
        assert!(get_verbosity().is_verbose());
        assert!(get_verbosity().is_normal());

        init(Verbosity::Verbose);
        assert!(!get_verbosity().is_debug());
        assert!(get_verbosity().is_verbose());
        assert!(get_verbosity().is_normal());

        init(Verbosity::Normal);
        assert!(!get_verbosity().is_debug());
        assert!(!get_verbosity().is_verbose());
        assert!(get_verbosity().is_normal());
    }

    #[test]
    fn test_verbosity_from_flags() {
        assert_eq!(Verbosity::from_flags(false, false), Verbosity::Normal);
        assert_eq!(Verbosity::from_flags(true, false), Verbosity::Verbose);
        assert_eq!(Verbosity::from_flags(false, true), Verbosity::Debug);
        assert_eq!(Verbosity::from_flags(true, true), Verbosity::Debug);
    }
}
