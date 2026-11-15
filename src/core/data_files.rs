//! The data files a program reads when it is compiled.
//!
//! Some of what a program reads at run time decides what code it is, the way
//! a pattern or a `dictConfig()` dict does: `logging.config.fileConfig()`
//! names handler classes and formats in a file. Such a file is read when the
//! program is compiled ([`crate::CompilerOptions::data_files`]) and the
//! program checks, when it runs, that the file still says what was compiled.
//!
//! The files of the compilation under way are kept here for the module
//! passes, which run without the options, as the parser keeps its line table.

use std::cell::RefCell;

thread_local! {
    /// The data files of the compilation on this thread, or `None` when the
    /// front end runs with nothing to compile for (function metadata).
    static DATA_FILES: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
}

/// What [`lookup`] finds for a path.
pub(crate) enum Lookup {
    /// The file's text.
    Found(String),
    /// A compilation is under way, and it was not given the file.
    Missing,
    /// No compilation is under way: the front end runs alone.
    NoCompilation,
}

/// Puts back the data files that were current, when a compilation ends.
pub(crate) struct Scope {
    previous: Option<Vec<(String, String)>>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        DATA_FILES.with(|files| *files.borrow_mut() = previous);
    }
}

/// Make `files` the data files of a compilation, until the scope is dropped.
pub(crate) fn scope(files: &[(String, String)]) -> Scope {
    let previous = DATA_FILES.with(|current| current.borrow_mut().replace(files.to_vec()));
    Scope { previous }
}

/// Whether a compilation is under way on this thread.
pub(crate) fn compiling() -> bool {
    DATA_FILES.with(|files| files.borrow().is_some())
}

/// The data file the program names `path`.
pub(crate) fn lookup(path: &str) -> Lookup {
    DATA_FILES.with(|files| match files.borrow().as_ref() {
        None => Lookup::NoCompilation,
        Some(files) => match files.iter().find(|(name, _)| name == path) {
            Some((_, text)) => Lookup::Found(text.clone()),
            None => Lookup::Missing,
        },
    })
}
