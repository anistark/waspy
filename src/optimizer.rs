use anyhow::Result;
use binaryen::{CodegenConfig, Module};

/// Optimize WebAssembly binary using Binaryen
pub fn optimize_wasm(wasm_binary: &[u8]) -> Result<Vec<u8>> {
    if wasm_binary.len() < 8 {
        return Ok(wasm_binary.to_vec());
    }

    match Module::read(wasm_binary) {
        Ok(mut module) => {
            let config = CodegenConfig::default();

            // Optimize with the config
            module.optimize(&config);

            // Get the optimized binary
            let optimized_binary = module.write();

            if optimized_binary.len() < 8 {
                Ok(wasm_binary.to_vec())
            } else {
                Ok(optimized_binary)
            }
        }
        Err(_) => {
            // If optimization fails, return the original binary
            Ok(wasm_binary.to_vec())
        }
    }
}
