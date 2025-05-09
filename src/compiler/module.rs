use crate::compiler::context::CompilationContext;
use crate::compiler::function::compile_function;
use crate::ir::{IRModule, IRType, MemoryLayout};
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, ExportSection, FunctionSection, MemorySection, MemoryType,
    Module, TypeSection, ValType,
};

/// Map IR type to WebAssembly ValType
fn ir_type_to_wasm_type(ir_type: &IRType) -> ValType {
    match ir_type {
        IRType::Float => ValType::F64,
        IRType::Int | IRType::Bool | IRType::String => ValType::I32,
        _ => ValType::I32,
    }
}

/// Compile an IR module into WebAssembly binary format
pub fn compile_ir_module(ir_module: &IRModule) -> Vec<u8> {
    let mut module = Module::new();
    let mut ctx = CompilationContext::new();
    let memory_layout = MemoryLayout::new();

    // Pre-scan for string constants to allocate in memory
    for _func in &ir_module.functions {
        // TODO: Scan for string literals
    }

    // Build type section
    let mut types = TypeSection::new();

    // Create function types for each function
    for func in &ir_module.functions {
        // Map IR parameter types to WebAssembly types
        let params: Vec<ValType> = func
            .params
            .iter()
            .map(|param| ir_type_to_wasm_type(&param.param_type))
            .collect();

        // Map IR return type to WebAssembly type
        let results = vec![ir_type_to_wasm_type(&func.return_type)];

        // Add the type to the type section
        types.ty().function(params, results);
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
        // Register function in context
        let param_types = func.params.iter().map(|p| p.param_type.clone()).collect();

        ctx.add_function(
            &func.name,
            idx as u32,
            param_types,
            func.return_type.clone(),
        );

        // Export the function
        exports.export(&func.name, wasm_encoder::ExportKind::Func, idx as u32);
    }
    module.section(&exports);

    // Memory section
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: Some(2),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    module.section(&memories);

    // Memory export
    let mut memory_exports = ExportSection::new();
    memory_exports.export("memory", wasm_encoder::ExportKind::Memory, 0);
    module.section(&memory_exports);

    // Data section for string constants
    let mut data = DataSection::new();

    // Add string data to memory
    if !memory_layout.string_offsets.is_empty() {
        let mut all_strings = Vec::new();

        // Sort strings by offset to maintain order
        let mut offsets: Vec<(String, u32)> = memory_layout
            .string_offsets
            .iter()
            .map(|(s, &offset)| (s.clone(), offset))
            .collect();

        offsets.sort_by_key(|(_s, offset)| *offset);

        // Concatenate all strings with null terminators
        for (s, _) in offsets {
            all_strings.extend_from_slice(s.as_bytes());
            all_strings.push(0); // Null terminator
        }

        // Create an active data segment at offset 0
        data.active(0, &ConstExpr::i32_const(0), all_strings);
    }

    module.section(&data);

    // Code section
    let mut codes = CodeSection::new();

    for func_ir in &ir_module.functions {
        let compiled_func = compile_function(func_ir, &mut ctx, &memory_layout);
        codes.function(&compiled_func);
    }

    module.section(&codes);

    module.finish()
}
