use anyhow::Result;
use binaryen::{ffi, CodegenConfig, Module};
use std::os::raw::c_char;

/// Optimize WebAssembly binary using Binaryen
pub fn optimize_wasm(wasm_binary: &[u8]) -> Result<Vec<u8>> {
    if wasm_binary.len() < 8 {
        return Ok(wasm_binary.to_vec());
    }

    unsafe {
        let raw =
            ffi::BinaryenModuleSafeRead(wasm_binary.as_ptr() as *const c_char, wasm_binary.len());
        if raw.is_null() {
            // If the module can't be read, return the original binary.
            return Ok(wasm_binary.to_vec());
        }

        // The runtime string/bytes allocator emits `memory.copy`, which lives in
        // the bulk-memory feature. Binaryen's optimizer asserts if that feature
        // isn't enabled on the module, so turn it on before optimizing.
        let features = ffi::BinaryenModuleGetFeatures(raw) | ffi::BinaryenFeatureBulkMemory();
        ffi::BinaryenModuleSetFeatures(raw, features);

        let mut module = Module::from_raw(raw);
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
}
