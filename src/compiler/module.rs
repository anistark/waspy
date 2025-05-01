use crate::compiler::context::CompilationContext;
use crate::compiler::function::compile_function;
use crate::ir::IRModule;
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, FunctionSection, MemorySection, MemoryType, Module,
    TypeSection, ValType,
};

/// Compile an IR module into WebAssembly binary format
pub fn compile_ir_module(ir_module: &IRModule) -> Vec<u8> {
    let mut module = Module::new();
    let mut ctx = CompilationContext::new();

    // Build type section
    let mut types = TypeSection::new();

    // Create function types for each function
    for (idx, func) in ir_module.functions.iter().enumerate() {
        // For now, all functions take i32 params and return i32
        let param_count = func.params.len();
        let params = vec![ValType::I32; param_count];
        let results = vec![ValType::I32]; // Assuming all functions return i32 for now

        types.ty().function(params, results);
        ctx.function_types.insert(func.name.clone(), idx as u32);
    }

    module.section(&types);

    // Build function section
    let mut functions = FunctionSection::new();
    for idx in 0..ir_module.functions.len() {
        functions.function(idx as u32);
    }
    module.section(&functions);

    // Export section
    let mut exports = ExportSection::new();
    for (idx, func) in ir_module.functions.iter().enumerate() {
        exports.export(func.name.as_str(), ExportKind::Func, idx as u32);
    }
    module.section(&exports);

    // Memory section for string storage
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,       // Start with 1 page (64KB)
        maximum: Some(1), // Also set maximum to 1 page for now
        memory64: false,
        shared: false,
        page_size_log2: Some(16), // Standard WebAssembly page size (64KB = 2^16 bytes)
    });
    module.section(&memories);

    // Memory export
    let mut memory_exports = ExportSection::new();
    memory_exports.export("memory", ExportKind::Memory, 0);
    module.section(&memory_exports);

    // Code section
    let mut codes = CodeSection::new();

    for func_ir in &ir_module.functions {
        codes.function(&compile_function(func_ir, &mut ctx));
    }

    module.section(&codes);

    module.finish()
}
