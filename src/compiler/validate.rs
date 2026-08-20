//! Validation of the generated WebAssembly module.
//!
//! Code generation can, in principle, emit a module that does not validate (an
//! unbalanced stack, a local index past the function's local vector, a type
//! mismatch). Handing such a module to Binaryen aborts the whole process
//! (`UNREACHABLE executed at ...`) with nothing to go on, and returning it
//! unoptimized hands the caller a binary every runtime will reject.
//!
//! So every compilation validates its output before anything else touches it,
//! and reports a normal compile error naming the offending function when it
//! fails. This is what keeps the project's correctness rule honest: a
//! compilation that reports success produces a module that validates.

use crate::core::errors::ChakraError;
use wasmparser::{ExternalKind, Parser, Payload, Validator};

/// Validate a generated WebAssembly binary.
///
/// Returns `Ok(())` when the module is valid. Otherwise the error carries the
/// validator's own message, the byte offset it stopped at, and (when the
/// offset falls inside a function body) the name or index of that function.
pub fn validate_wasm(wasm: &[u8]) -> Result<(), ChakraError> {
    let mut validator = Validator::new();
    match validator.validate_all(wasm) {
        Ok(_) => Ok(()),
        Err(err) => {
            let offset = err.offset();
            let where_ = match locate_function(wasm, offset) {
                Some(name) => format!(" in {name}"),
                None => String::new(),
            };
            Err(ChakraError::WasmCompilationError(format!(
                "generated module failed WebAssembly validation{where_}: {} (at byte offset {offset}). \
                 This is a code generation bug, not a problem with the Python source",
                err.message()
            )))
        }
    }
}

/// Describe the function whose body contains `offset`, as `function 'name'`
/// when it is exported and `function #N` otherwise. `None` when the offset
/// falls outside every function body, or when the module is too malformed to
/// re-scan.
fn locate_function(wasm: &[u8], offset: usize) -> Option<String> {
    let mut imported_functions = 0u32;
    let mut names: Vec<(u32, String)> = Vec::new();
    let mut body_index = 0u32;
    let mut hit: Option<u32> = None;

    for payload in Parser::new(0).parse_all(wasm) {
        // A malformed section stops the scan; whatever was collected before it
        // is still usable for attribution.
        let Ok(payload) = payload else { break };
        match payload {
            Payload::ImportSection(reader) => {
                for import in reader.into_iter().flatten() {
                    if matches!(import.ty, wasmparser::TypeRef::Func(_)) {
                        imported_functions += 1;
                    }
                }
            }
            Payload::ExportSection(reader) => {
                for export in reader.into_iter().flatten() {
                    if export.kind == ExternalKind::Func {
                        names.push((export.index, export.name.to_string()));
                    }
                }
            }
            Payload::CodeSectionEntry(body) => {
                let range = body.range();
                if range.contains(&offset) {
                    hit = Some(imported_functions + body_index);
                }
                body_index += 1;
            }
            _ => {}
        }
    }

    let index = hit?;
    match names.iter().find(|(i, _)| *i == index) {
        Some((_, name)) => Some(format!("function '{name}'")),
        None => Some(format!("function #{index}")),
    }
}
