//! Compiler options and configuration.
//!
//! [`CompilerOptions`] and [`Verbosity`] are part of Waspy's stable public
//! API: every `*_with_options` entry point in the crate root takes a
//! `&CompilerOptions`. Both types are reviewed for the 1.0 freeze — options
//! that existed before 0.20.0 but were never honored by the pipeline
//! (`debug_info`, `max_memory`, `entry_point`, `generate_html`,
//! `include_metadata`) have been removed; driver-side concerns like HTML
//! harness generation belong to the embedding tool, not the compiler.

/// Verbosity level for compiler diagnostics.
///
/// Passed via [`CompilerOptions::verbosity`]; the compiler's logging macros
/// gate their output on the level. Levels are ordered, so
/// `Verbose >= Normal` holds and each level includes everything below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Verbosity {
    /// Errors only.
    Quiet = 0,
    /// Progress and warnings (the default).
    #[default]
    Normal = 1,
    /// Per-stage progress detail (`--verbose` in the example drivers).
    Verbose = 2,
    /// Full diagnostic output, including IR dumps (`--debug`).
    Debug = 3,
}

impl Verbosity {
    /// Map the common `--verbose` / `--debug` command-line flag pair to a
    /// level. `debug` wins over `verbose`; neither yields [`Verbosity::Normal`].
    pub fn from_flags(verbose: bool, debug: bool) -> Self {
        if debug {
            Self::Debug
        } else if verbose {
            Self::Verbose
        } else {
            Self::Normal
        }
    }

    /// Whether debug logging is enabled.
    pub fn is_debug(&self) -> bool {
        *self >= Self::Debug
    }

    /// Whether verbose logging is enabled.
    pub fn is_verbose(&self) -> bool {
        *self >= Self::Verbose
    }

    /// Whether normal logging is enabled.
    pub fn is_normal(&self) -> bool {
        *self >= Self::Normal
    }
}

/// Options for Python to WebAssembly compilation.
///
/// Construct with struct-update syntax over [`Default`] so adding future
/// options stays non-breaking:
///
/// ```
/// use waspy::CompilerOptions;
///
/// let options = CompilerOptions {
///     optimize: false,
///     ..CompilerOptions::default()
/// };
/// ```
///
/// Linear memory is sized automatically from the compiled module's data
/// (string/bytes regions plus the collection heap's high-water mark) and
/// grows on demand at runtime, so there is no memory-size option.
#[derive(Debug, Clone)]
pub struct CompilerOptions {
    /// Run the Binaryen optimization pass over the generated WebAssembly
    /// (defaults to `true`). The unoptimized binary is already valid and
    /// correct; optimization only shrinks and speeds it up.
    pub optimize: bool,

    /// How much diagnostic output the compiler emits while running
    /// (defaults to [`Verbosity::Normal`]).
    pub verbosity: Verbosity,
}

impl Default for CompilerOptions {
    fn default() -> Self {
        Self {
            optimize: true,
            verbosity: Verbosity::default(),
        }
    }
}
