use crate::compiler::context::{ClassInfo, CompilationContext, COLLECTION_HEAP_BASE};
use crate::compiler::function::compile_function;
use crate::ir::{IRBody, IRConstant, IRExpr, IRModule, IRStatement, IRType};
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

/// Is this expression a reference to the implicit `self` parameter?
fn is_self_ref(expr: &IRExpr) -> bool {
    matches!(expr, IRExpr::Variable(n) | IRExpr::Param(n) if n == "self")
}

/// Best-effort type inference for a class field initializer. Only needs to
/// distinguish floats (an f64 slot) from the i32-shaped default; `params` maps
/// the enclosing method's parameter names to their annotated types so that
/// `self.width = width` adopts `width`'s declared type.
fn infer_field_value_type(value: &IRExpr, params: &HashMap<String, IRType>) -> IRType {
    match value {
        IRExpr::Const(IRConstant::Float(_)) => IRType::Float,
        IRExpr::Const(IRConstant::Int(_)) => IRType::Int,
        IRExpr::Const(IRConstant::Bool(_)) => IRType::Bool,
        IRExpr::Variable(name) | IRExpr::Param(name) => {
            params.get(name).cloned().unwrap_or(IRType::Unknown)
        }
        IRExpr::BinaryOp { left, right, .. } => {
            let lt = infer_field_value_type(left, params);
            let rt = infer_field_value_type(right, params);
            if lt == IRType::Float || rt == IRType::Float {
                IRType::Float
            } else {
                lt
            }
        }
        IRExpr::UnaryOp { operand, .. } => infer_field_value_type(operand, params),
        _ => IRType::Unknown,
    }
}

/// Collect `self.<field> = value` assignments (including augmented ones) from a
/// method body, recursing into nested blocks, with each field's inferred type.
fn collect_self_fields(
    body: &IRBody,
    params: &HashMap<String, IRType>,
    out: &mut Vec<(String, IRType)>,
) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::AttributeAssign {
                object,
                attribute,
                value,
            } if is_self_ref(object) => {
                out.push((attribute.clone(), infer_field_value_type(value, params)));
            }
            IRStatement::AttributeAugAssign {
                object,
                attribute,
                value,
                ..
            } if is_self_ref(object) => {
                out.push((attribute.clone(), infer_field_value_type(value, params)));
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_self_fields(then_body, params, out);
                if let Some(else_body) = else_body {
                    collect_self_fields(else_body, params, out);
                }
            }
            IRStatement::While { body, .. } => collect_self_fields(body, params, out),
            IRStatement::For { body, .. } => collect_self_fields(body, params, out),
            _ => {}
        }
    }
}

/// Compile an IR module into WebAssembly binary format
pub fn compile_ir_module(ir_module: &IRModule) -> Vec<u8> {
    let mut module = Module::new();
    let mut ctx = CompilationContext::new();
    // String/bytes offsets are resolved during lowering and carried on the IR
    // module; the compiler reuses that layout to emit loads and the data section.
    let memory_layout = ir_module.memory_layout.clone();

    // Register module-level variables so functions can inline their values.
    for var in &ir_module.variables {
        ctx.add_module_var(&var.name, var.var_type.clone(), var.value.clone());
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
            field_types: HashMap::new(),
            class_var_values: HashMap::new(),
            instance_size: 0,
        };

        // Each field is an 8-byte slot (holds an i32 or an f64); the first 4
        // bytes are reserved for a type tag. `add_field` assigns the next slot
        // the first time a field name is seen and records its value type.
        let mut current_offset = 4u64;
        let add_field = |info: &mut ClassInfo, name: &str, ty: IRType, current_offset: &mut u64| {
            if !info.field_offsets.contains_key(name) {
                info.field_offsets.insert(name.to_string(), *current_offset);
                *current_offset += 8;
            }
            // Prefer a concrete type over an earlier `Unknown` inference.
            let entry = info
                .field_types
                .entry(name.to_string())
                .or_insert(IRType::Unknown);
            if matches!(entry, IRType::Unknown) {
                *entry = ty;
            }
        };

        // Class-level variables occupy instance slots and are also accessible as
        // `ClassName.var`; keep their initializers for that read path.
        for var in &cls.class_vars {
            let ty = var
                .var_type
                .clone()
                .unwrap_or_else(|| infer_field_value_type(&var.value, &HashMap::new()));
            add_field(&mut class_info, &var.name, ty, &mut current_offset);
            class_info
                .class_var_values
                .insert(var.name.clone(), var.value.clone());
        }

        // Instance fields are discovered from `self.<field> = ...` assignments in
        // method bodies (their types inferred from the method's parameters).
        for method in &cls.methods {
            let params: HashMap<String, IRType> = method
                .params
                .iter()
                .map(|p| (p.name.clone(), p.param_type.clone()))
                .collect();
            let mut fields = Vec::new();
            collect_self_fields(&method.body, &params, &mut fields);
            for (name, ty) in fields {
                add_field(&mut class_info, &name, ty, &mut current_offset);
            }
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

    // Data section for string and bytes constants.
    let mut data = DataSection::new();

    // String data lives from offset 0. Offsets are assigned sequentially as
    // `len + 1` (a null terminator after each), so emitting the strings in
    // offset order, each followed by a NUL, reproduces those exact offsets.
    if !memory_layout.string_offsets.is_empty() {
        let mut offsets: Vec<(&String, u32)> = memory_layout
            .string_offsets
            .iter()
            .map(|(s, &offset)| (s, offset))
            .collect();
        offsets.sort_by_key(|(_s, offset)| *offset);

        let mut all_strings = Vec::new();
        for (s, _) in offsets {
            all_strings.extend_from_slice(s.as_bytes());
            all_strings.push(0); // Null terminator
        }
        data.active(0, &ConstExpr::i32_const(0), all_strings);
    }

    // Bytes data lives from `next_bytes_offset`'s base (32768). Offsets advance
    // by exactly the byte length (no terminator), so the values are contiguous
    // from the lowest assigned offset; emit them in offset order from there.
    if !memory_layout.bytes_offsets.is_empty() {
        let mut offsets: Vec<(&Vec<u8>, u32)> = memory_layout
            .bytes_offsets
            .iter()
            .map(|(b, &offset)| (b, offset))
            .collect();
        offsets.sort_by_key(|(_b, offset)| *offset);

        let base = offsets[0].1;
        let mut all_bytes = Vec::new();
        for (b, _) in offsets {
            all_bytes.extend_from_slice(b);
        }
        data.active(0, &ConstExpr::i32_const(base as i32), all_bytes);
    }

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

    // Memory must hold the static data (strings/bytes/object instances) plus
    // every collection region handed out during codegen. The collection heap
    // grows from COLLECTION_HEAP_BASE, so size memory to cover its high-water
    // mark (at least the base region).
    let heap_end = COLLECTION_HEAP_BASE + ctx.collection_alloc_offset.get();
    let min_pages = (((heap_end as u64) + 65535) / 65536).max(2);
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: min_pages,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    // Section order follows the WASM spec: Memory (5), Export (7), Code (10),
    // Data (11). Code must precede Data or strict validators (and Binaryen's
    // reader) reject the module, which previously disabled optimization.
    module.section(&memories);
    module.section(&exports);
    module.section(&codes);
    module.section(&data);

    module.finish()
}
