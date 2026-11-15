//! What the files of one program tell the module passes about each other.
//!
//! A module pass writes a standard-library module out for the file it is
//! lowering, shaped by what that file uses: `logging` compiles the formats
//! the file spells out and gives records the fields its `extra=` name,
//! `asyncio` reports a task's unretrieved exception only where tasks are
//! made. A program of several files has one `logging` and one event loop, so
//! what each file's pass writes has to be what the whole program uses, or the
//! copies differ and cannot be merged (`ir::merge`).
//!
//! The files are therefore lowered twice. The first time each pass records
//! here what its file uses; the second time it reads back what every file
//! recorded, in the order it was recorded, so each file's copy of the
//! module's shared code is the same and its numbering (a format's id) means
//! the same thing everywhere. A single source has nothing to share: outside
//! a [`scope`] every function here answers its own argument.

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Default)]
struct Facts {
    flags: BTreeMap<&'static str, bool>,
    entries: BTreeMap<&'static str, Box<dyn Any>>,
}

thread_local! {
    /// The facts of the program being compiled on this thread, when it has
    /// several files.
    static FACTS: RefCell<Option<Facts>> = const { RefCell::new(None) };
}

/// Ends the sharing when the compilation does.
pub(crate) struct Scope {
    previous: Option<Facts>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let previous = self.previous.take();
        FACTS.with(|facts| *facts.borrow_mut() = previous);
    }
}

/// Start sharing facts between the files lowered until the scope is dropped.
pub(crate) fn scope() -> Scope {
    let previous = FACTS.with(|facts| facts.borrow_mut().replace(Facts::default()));
    Scope { previous }
}

/// Whether any pass recorded a fact since the scope began: only then is a
/// second lowering needed.
pub(crate) fn recorded() -> bool {
    FACTS.with(|facts| {
        facts
            .borrow()
            .as_ref()
            .is_some_and(|f| !f.flags.is_empty() || !f.entries.is_empty())
    })
}

/// A switch a pass reads from the file it lowers (`here`), on for the whole
/// program once any of its files turns it on.
pub(crate) fn flag(key: &'static str, here: bool) -> bool {
    FACTS.with(|facts| match facts.borrow_mut().as_mut() {
        None => here,
        Some(facts) => {
            let on = facts.flags.entry(key).or_insert(false);
            *on |= here;
            *on
        }
    })
}

/// The entries a pass collects from the file it lowers (`here`), after the
/// ones the program's files recorded before, each once: the program's list,
/// in an order every file sees alike.
pub(crate) fn entries<T: Clone + PartialEq + 'static>(key: &'static str, here: Vec<T>) -> Vec<T> {
    FACTS.with(|facts| match facts.borrow_mut().as_mut() {
        None => here,
        Some(facts) => {
            let all = facts
                .entries
                .entry(key)
                .or_insert_with(|| Box::new(Vec::<T>::new()))
                .downcast_mut::<Vec<T>>()
                .expect("one type per key");
            for entry in here {
                if !all.contains(&entry) {
                    all.push(entry);
                }
            }
            all.clone()
        }
    })
}
