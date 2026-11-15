//! Compiler options and configuration.
//!
//! [`CompilerOptions`] and [`Verbosity`] are part of Waspy's stable public
//! API: every `*_with_options` entry point in the crate root takes a
//! `&CompilerOptions`. Both types are reviewed for the 1.0 freeze — options
//! that existed in 0.18.0 but were never honored by the pipeline
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
    /// correct; optimization only shrinks and speeds it up. Has no effect
    /// when waspy is built without its `optimize` feature.
    pub optimize: bool,

    /// How much diagnostic output the compiler emits while running
    /// (defaults to [`Verbosity::Normal`]).
    pub verbosity: Verbosity,

    /// Give every export a JavaScript-facing wrapper, so the loader
    /// ([`crate::js_loader`]) calls it with JavaScript values, even when the
    /// program does not import `js` or `pyodide` (defaults to `false`). A
    /// program that does gets the wrappers anyway. Set it for a module
    /// another loads by a name computed at run time
    /// (`importlib.import_module(name)`), or one a page calls with strings,
    /// lists, or dicts. The module then imports the bridge's functions
    /// (`waspy_js`), so it is instantiated through the loader.
    pub js_exports: bool,

    /// The data files the program reads when it is compiled: each a path as
    /// the program writes it, and the file's text (empty by default).
    ///
    /// `logging.config.fileConfig("logging.conf")` names handler classes and
    /// formats in a file, which decide what code the program is, so the file
    /// is read when the program is compiled and the compiled program raises
    /// `RuntimeError` when the file it finds at run time says anything else.
    /// [`crate::compile_python_file_with_options`] and
    /// [`crate::compile_python_project_with_options`] read each such file
    /// from disk, when its path is inside the entry file's directory or the
    /// project directory, unless it is given here; a file anywhere else, and
    /// every file of the entry points that take source text, is given here.
    ///
    /// ```
    /// use waspy::{compile_python_to_wasm_with_options, CompilerOptions};
    ///
    /// let source = "\
    /// import logging
    /// import logging.config
    ///
    /// def setup() -> None:
    ///     logging.config.fileConfig(\"logging.conf\")
    /// ";
    /// let conf = "\
    /// [loggers]
    /// keys=root
    ///
    /// [handlers]
    /// keys=console
    ///
    /// [formatters]
    /// keys=
    ///
    /// [logger_root]
    /// level=INFO
    /// handlers=console
    ///
    /// [handler_console]
    /// class=StreamHandler
    /// args=(sys.stderr,)
    /// ";
    /// let options = CompilerOptions {
    ///     optimize: false,
    ///     data_files: vec![("logging.conf".to_string(), conf.to_string())],
    ///     ..CompilerOptions::default()
    /// };
    /// let wasm = compile_python_to_wasm_with_options(source, &options)?;
    /// assert_eq!(&wasm[0..4], b"\0asm");
    /// # anyhow::Ok(())
    /// ```
    pub data_files: Vec<(String, String)>,
}

impl Default for CompilerOptions {
    fn default() -> Self {
        Self {
            optimize: true,
            verbosity: Verbosity::default(),
            js_exports: false,
            data_files: Vec::new(),
        }
    }
}
