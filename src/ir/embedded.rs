//! Standard-library code written in the Python subset.
//!
//! Some of what CPython writes in Python (or in C over strings) is longest in
//! IR builders and shortest in Python: a loop over characters, a parser. Such
//! a helper is kept here as source, lowered through the same front end as a
//! program's (`core::parser`, then `lower_ast_to_ir`, module passes included),
//! and its functions added to the module that calls them. They are then
//! compiled like the program's own code, so nothing is interpreted (critical
//! rule 6), and they hold to the subset's own rules: what the compiler would
//! refuse in a program, it refuses here, at the helper's first use.
//!
//! A helper's functions carry a prefix `ir::is_internal_function` knows, so
//! they stay out of exports and metadata, and use no lambda or nested `def`,
//! whose lifted names would collide with the program's.

use crate::ir::IRModule;
use anyhow::{Context, Result};

/// Lower `source`, Python in the supported subset, to IR.
pub(crate) fn lower(source: &str) -> Result<IRModule> {
    let ast = crate::core::parser::parse_embedded(source)?;
    crate::ir::lower_ast_to_ir_named(&ast, "<embedded>")
}

/// Add the functions and classes of `source` to `module`, leaving out any it
/// already has by name (a merge of several files adds each once).
pub(crate) fn graft(module: &mut IRModule, source: &str) -> Result<()> {
    let lowered = lower(source)
        .context("a standard-library helper written in Python does not compile here")?;
    module.memory_layout.merge_from(&lowered.memory_layout);
    for function in lowered.functions {
        if !module.functions.iter().any(|f| f.name == function.name) {
            module.functions.push(function);
        }
    }
    for class in lowered.classes {
        if !module.classes.iter().any(|c| c.name == class.name) {
            module.classes.push(class);
        }
    }
    Ok(())
}
