//! Core functionality for the Waspy compiler.

pub mod comments;
pub mod config;
pub mod errors;
pub mod options;
pub mod parser;
pub mod unicode;
pub mod unicode_tables;

pub use comments::*;
pub use config::*;
pub use errors::*;
pub use options::*;
