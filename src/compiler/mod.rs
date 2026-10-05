mod context;
mod equality;
mod expression;
mod function;
mod module;
mod operators;
mod validate;

pub use module::{compile_ir_module, COMMENTS_SECTION_NAME};
pub use validate::validate_wasm;
