use crate::compiler::context::{ClassInfo, CompilationContext};
use crate::compiler::function::compile_function;
use crate::ir::{IRModule, IRType, MemoryLayout};
use std::collections::HashMap;
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, ExportSection, FunctionSection, MemorySection, MemoryType,
    Module, TypeSection, ValType,
};

/// Map IR type to WebAssembly ValType
fn ir_type_to_wasm_type(ir_type: &IRType) -> ValType {
    match ir_type {
        IRType::Float => ValType::F64,
        IRType::Int | IRType::Bool | IRType::String => ValType::I32,
        IRType::Class(_) => ValType::I32, // References to classes are pointers (i32)
        IRType::List(_) | IRType::Dict(_, _) | IRType::Tuple(_) => ValType::I32, // Collections are pointers
        IRType::Optional(_) | IRType::Union(_) => ValType::I32, // References to optionals/unions
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

    // Calculate total number of functions (module functions + all class methods)
    let mut total_function_count = ir_module.functions.len();
    for cls in &ir_module.classes {
        total_function_count += cls.methods.len();
    }

    // Create function types for module functions
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

    // Create function types for class methods
    for cls in &ir_module.classes {
        for method in &cls.methods {
            let params: Vec<ValType> = method
                .params
                .iter()
                .map(|param| ir_type_to_wasm_type(&param.param_type))
                .collect();
            let results = vec![ir_type_to_wasm_type(&method.return_type)];
            types.ty().function(params, results);
        }
    }

    module.section(&types);

    // Build function section
    let mut functions = FunctionSection::new();
    for _ in 0..total_function_count {
        functions.function(functions.len());
    }
    module.section(&functions);

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

    // Export section - export both functions and memory
    let mut exports = ExportSection::new();

    // Register module functions
    let mut func_idx = 0u32;
    for func in &ir_module.functions {
        // Register function in context
        let param_types = func.params.iter().map(|p| p.param_type.clone()).collect();

        ctx.add_function(&func.name, func_idx, param_types, func.return_type.clone());

        // Export the function
        exports.export(&func.name, wasm_encoder::ExportKind::Func, func_idx);
        func_idx += 1;
    }

    // Register and export class methods
    for cls in &ir_module.classes {
        let mut class_info = ClassInfo {
            name: cls.name.clone(),
            methods: HashMap::new(),
            field_offsets: HashMap::new(),
            instance_size: 0,
        };

        // Calculate field offsets and instance size
        let mut current_offset = 4u64; // 4 bytes for type tag at offset 0
        for var in &cls.class_vars {
            class_info
                .field_offsets
                .insert(var.name.clone(), current_offset);
            // Each field is 8 bytes (can hold i32 or f64)
            current_offset += 8;
        }
        class_info.instance_size = current_offset as u32;

        // Register methods
        for method in &cls.methods {
            let param_types = method.params.iter().map(|p| p.param_type.clone()).collect();
            let qualified_name = format!("{}::{}", cls.name, method.name);

            ctx.add_function(
                &qualified_name,
                func_idx,
                param_types,
                method.return_type.clone(),
            );

            class_info.methods.insert(method.name.clone(), func_idx);

            // Export method with qualified name
            exports.export(&qualified_name, wasm_encoder::ExportKind::Func, func_idx);
            func_idx += 1;
        }

        // Add class to context
        ctx.add_class(class_info);
    }

    // Export memory
    exports.export("memory", wasm_encoder::ExportKind::Memory, 0);

    module.section(&exports);

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

    // Compile class methods
    for cls in &ir_module.classes {
        for method in &cls.methods {
            let compiled_method = compile_function(method, &mut ctx, &memory_layout);
            codes.function(&compiled_method);
        }
    }

    module.section(&codes);

    module.finish()
}
