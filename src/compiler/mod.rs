mod context;
mod expression;
mod function;
mod module;
mod validate;

pub use module::{compile_ir_module, COMMENTS_SECTION_NAME};
pub use validate::validate_wasm;
