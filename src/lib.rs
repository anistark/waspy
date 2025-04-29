pub mod compiler;
pub mod ir;
pub mod optimizer;
pub mod parser;

use anyhow::Result;

/// Compile Python source code into a WASM binary.
/// By default, this applies optimization to the generated WebAssembly.
pub fn compile_python_to_wasm(source: &str) -> Result<Vec<u8>> {
    compile_python_to_wasm_with_options(source, true)
}

/// Compile Python source code into a WASM binary with options.
pub fn compile_python_to_wasm_with_options(source: &str, optimize: bool) -> Result<Vec<u8>> {
    // Parse Python to AST
    let ast = parser::parse_python(source)?;

    // Lower AST to IR
    let ir = ir::lower_ast_to_ir(&ast)?;

    // Generate WASM binary
    let raw_wasm = compiler::compile_ir(&ir);

    // Optimize the WASM binary
    if optimize {
        optimizer::optimize_wasm(&raw_wasm)
    } else {
        Ok(raw_wasm)
    }
}
