pub mod parser;
pub mod ir;
pub mod compiler;

use anyhow::Result;

/// Compile Python source code into a WASM binary.
pub fn compile_python_to_wasm(source: &str) -> Result<Vec<u8>> {
    let ast = parser::parse_python(source)?;
    let ir = ir::lower_ast_to_ir(&ast)?;
    let wasm = compiler::compile_ir(&ir);
    Ok(wasm)
}