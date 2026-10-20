mod closures;
mod context;
mod equality;
mod exceptions;
mod expression;
mod float_parse;
mod format;
mod function;
mod host;
mod module;
mod operators;
mod runtime;
mod sets;
mod unicode;
mod validate;

pub use module::{compile_ir_module, COMMENTS_SECTION_NAME};
pub use validate::validate_wasm;
