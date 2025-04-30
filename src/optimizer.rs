use anyhow::Result;
use binaryen::{CodegenConfig, Module};

/// Optimize WebAssembly binary using Binaryen, with fallback to unoptimized binary
pub fn optimize_wasm(wasm_binary: &[u8]) -> Result<Vec<u8>> {
    // If we encounter any issues during optimization, just return the original binary
    // This is a fallback approach to ensure we always return something valid
    if wasm_binary.len() < 8 {
        return Ok(wasm_binary.to_vec());
    }

    // Try to load and optimize the module
    match Module::read(wasm_binary) {
        Ok(mut module) => {
            // Create a default configuration with minimal optimization
            let config = CodegenConfig::default();

            // Optimize with the config
            module.optimize(&config);

            // Get the optimized binary
            let optimized_binary = module.write();

            // If optimization somehow produced an empty or very small binary,
            // return the original instead
            if optimized_binary.len() < 8 {
                Ok(wasm_binary.to_vec())
            } else {
                Ok(optimized_binary)
            }
        }
        Err(_) => {
            // If we can't read the module, just return the original binary
            // This ensures we still get a working WASM file
            Ok(wasm_binary.to_vec())
        }
    }
}
