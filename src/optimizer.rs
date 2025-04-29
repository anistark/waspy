use anyhow::Result;
use binaryen::{CodegenConfig, Module};

/// Optimize WebAssembly binary using Binaryen
pub fn optimize_wasm(wasm_binary: &[u8]) -> Result<Vec<u8>> {
    // Create a new Binaryen module from the WASM binary
    let mut module = Module::read(wasm_binary)
        .map_err(|_| anyhow::anyhow!("Failed to read WASM binary into Binaryen module"))?;

    // Create a default CodegenConfig
    let config = CodegenConfig::default();

    // Optimize with the config parameter
    module.optimize(&config);

    // Get the optimized binary
    let optimized_binary = module.write();

    Ok(optimized_binary)
}
