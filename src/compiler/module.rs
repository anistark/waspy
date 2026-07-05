use crate::compiler::context::{ClassInfo, CompilationContext, COLLECTION_HEAP_BASE};
use crate::compiler::function::{compile_function, resolve_return_type};
use crate::ir::{IRBody, IRConstant, IRExpr, IRModule, IRStatement, IRType, STRING_LEN_PREFIX};
use std::collections::HashMap;
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ExportSection, Function, FunctionSection,
    GlobalSection, GlobalType, Instruction, MemorySection, MemoryType, Module, TypeSection,
    ValType,
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

/// Build the runtime bump allocator `__alloc(size: i32) -> i32`.
///
/// The next-free pointer lives in global 0 (initialized to the post-codegen
/// high-water mark). Each call rounds `size` up to 8 bytes, grows linear memory
/// if the request would run past the current end, advances the global, and
/// returns the old pointer. There is no `free`; this is a monotonic bump
/// allocator backing runtime-built strings/bytes (e.g. concatenation results).
fn build_alloc_function() -> Function {
    // locals 1..4 (local 0 is the `size` parameter): aligned size, old ptr,
    // new ptr, current memory size in bytes.
    let mut f = Function::new([(4u32, ValType::I32)]);

    // aligned = (size + 7) & ~7  -> local 1
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::I32Const(7));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I32Const(!7));
    f.instruction(&Instruction::I32And);
    f.instruction(&Instruction::LocalSet(1));

    // old = global 0 -> local 2
    f.instruction(&Instruction::GlobalGet(0));
    f.instruction(&Instruction::LocalSet(2));

    // new = old + aligned -> local 3
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::LocalSet(3));

    // cur_bytes = memory.size * 65536 -> local 4
    f.instruction(&Instruction::MemorySize(0));
    f.instruction(&Instruction::I32Const(16));
    f.instruction(&Instruction::I32Shl);
    f.instruction(&Instruction::LocalSet(4));

    // if new > cur_bytes: grow by ceil((new - cur_bytes) / 65536) pages
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32GtU);
    f.instruction(&Instruction::If(BlockType::Empty));
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::LocalGet(4));
    f.instruction(&Instruction::I32Sub);
    f.instruction(&Instruction::I32Const(65535));
    f.instruction(&Instruction::I32Add);
    f.instruction(&Instruction::I32Const(16));
    f.instruction(&Instruction::I32ShrU);
    f.instruction(&Instruction::MemoryGrow(0));
    f.instruction(&Instruction::Drop); // grow failure (-1) is left to trap on use
    f.instruction(&Instruction::End);

    // global 0 = new; return old
    f.instruction(&Instruction::LocalGet(3));
    f.instruction(&Instruction::GlobalSet(0));
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::End);
    f
}

/// Build `__alloc_obj(size: i32, class_id: i32) -> i32`: allocate an instance
/// via `__alloc` and stamp `class_id` into the tag word at offset 0 (the slot
/// every instance layout reserves), returning the instance pointer. Doing the
/// stamp in a helper keeps the `ClassName(...)` call sequence stack-only, so
/// nested instantiations compose without any scratch local.
fn build_alloc_obj_function(alloc_func_index: u32) -> Function {
    // local 2 (locals 0-1 are the parameters): the allocated pointer.
    let mut f = Function::new([(1u32, ValType::I32)]);

    // ptr = __alloc(size) -> local 2
    f.instruction(&Instruction::LocalGet(0));
    f.instruction(&Instruction::Call(alloc_func_index));
    f.instruction(&Instruction::LocalSet(2));

    // *(ptr + 0) = class_id
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::LocalGet(1));
    f.instruction(&Instruction::I32Store(wasm_encoder::MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }));

    // return ptr
    f.instruction(&Instruction::LocalGet(2));
    f.instruction(&Instruction::End);
    f
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

    // Resolve each function's (and method's) return type up front, inferring
    // unannotated ones from their bodies, so the type section, the registered
    // signatures, and the emitted code all use the same result type. Two passes
    // let a function's inferred return type depend on a callee resolved later in
    // the first pass.
    let mut resolved_returns: HashMap<String, IRType> = HashMap::new();
    for _ in 0..2 {
        for func in &ir_module.functions {
            let rt = resolve_return_type(func, &resolved_returns);
            resolved_returns.insert(func.name.clone(), rt);
        }
        for cls in &ir_module.classes {
            for method in &cls.methods {
                let rt = resolve_return_type(method, &resolved_returns);
                resolved_returns.insert(format!("{}::{}", cls.name, method.name), rt);
            }
        }
    }
    let module_return = |func: &crate::ir::IRFunction| -> IRType {
        resolved_returns
            .get(&func.name)
            .cloned()
            .unwrap_or_else(|| func.return_type.clone())
    };
    let method_return = |class: &str, method: &crate::ir::IRFunction| -> IRType {
        resolved_returns
            .get(&format!("{class}::{}", method.name))
            .cloned()
            .unwrap_or_else(|| method.return_type.clone())
    };

    // Build type section
    let mut types = TypeSection::new();

    // Calculate total number of functions (module functions + all class methods)
    let mut total_function_count = ir_module.functions.len();
    for cls in &ir_module.classes {
        total_function_count += cls.methods.len();
    }

    // The runtime allocator `__alloc` and the instance allocator `__alloc_obj`
    // are appended after all user functions and methods, so they take the next
    // two function (and type) indices. Record them so codegen can emit
    // `call $__alloc` / `call $__alloc_obj`.
    ctx.alloc_func_index = total_function_count as u32;
    ctx.alloc_obj_func_index = total_function_count as u32 + 1;

    // Create function types for module functions
    for func in &ir_module.functions {
        // Map IR parameter types to WebAssembly types
        let params: Vec<ValType> = func
            .params
            .iter()
            .map(|param| ir_type_to_wasm_type(&param.param_type))
            .collect();

        // Map IR return type to WebAssembly type
        let results = vec![ir_type_to_wasm_type(&module_return(func))];

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
            let results = vec![ir_type_to_wasm_type(&method_return(&cls.name, method))];
            types.ty().function(params, results);
        }
    }

    // Types for the runtime allocator `__alloc(size: i32) -> i32` and the
    // instance allocator `__alloc_obj(size: i32, class_id: i32) -> i32`.
    types.ty().function([ValType::I32], [ValType::I32]);
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I32]);

    module.section(&types);

    // Build function section (one entry per user function/method, referencing
    // the matching type, plus trailing entries for `__alloc` / `__alloc_obj`).
    let mut functions = FunctionSection::new();
    for _ in 0..total_function_count {
        functions.function(functions.len());
    }
    functions.function(total_function_count as u32); // __alloc
    functions.function(total_function_count as u32 + 1); // __alloc_obj
    module.section(&functions);

    // Export section - export both functions and memory
    let mut exports = ExportSection::new();

    // Register module functions
    let mut func_idx = 0u32;
    for func in &ir_module.functions {
        // Register function in context
        let param_types = func.params.iter().map(|p| p.param_type.clone()).collect();

        ctx.add_function(&func.name, func_idx, param_types, module_return(func));

        // Export the function
        exports.export(&func.name, wasm_encoder::ExportKind::Func, func_idx);
        func_idx += 1;
    }

    // Register and export class methods. Classes appear in source order and
    // Python requires a base to be defined before a subclass references it, so
    // a base's ClassInfo is always registered by the time its subclass is
    // processed.
    let mut next_class_id = 1i32;
    for cls in &ir_module.classes {
        // Single inheritance: resolve the (at most one, enforced during IR
        // conversion) base class. `object` is the implicit root, not a base.
        let base = cls
            .bases
            .iter()
            .find(|b| b.as_str() != "object" && ctx.get_class_info(b).is_some())
            .cloned();

        let mut class_info = ClassInfo {
            name: cls.name.clone(),
            base: base.clone(),
            class_id: next_class_id,
            methods: HashMap::new(),
            method_owner: HashMap::new(),
            field_offsets: HashMap::new(),
            field_types: HashMap::new(),
            class_var_values: HashMap::new(),
            instance_size: 0,
        };
        next_class_id += 1;

        // Each field is an 8-byte slot (holds an i32 or an f64); the first 4
        // bytes hold the class-id tag stamped by `__alloc_obj`. `add_field`
        // assigns the next slot the first time a field name is seen and
        // records its value type.
        //
        // A subclass inherits its base's layout as a prefix: base fields keep
        // their exact offsets and the subclass's own fields append after
        // `base.instance_size`, so a base method reading `self.x` works
        // unchanged on a subclass instance. Inherited methods are seeded with
        // the base's already-resolved function indices (the same compiled WASM
        // function — safe because of the prefix layout); the subclass's own
        // registration below overwrites any it redefines (override).
        let mut current_offset = 4u64;
        if let Some(base_info) = base.as_deref().and_then(|b| ctx.get_class_info(b)) {
            class_info.field_offsets = base_info.field_offsets.clone();
            class_info.field_types = base_info.field_types.clone();
            class_info.class_var_values = base_info.class_var_values.clone();
            class_info.methods = base_info.methods.clone();
            class_info.method_owner = base_info.method_owner.clone();
            current_offset = base_info.instance_size as u64;
        }
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
                method_return(&cls.name, method),
            );

            class_info.methods.insert(method.name.clone(), func_idx);
            class_info
                .method_owner
                .insert(method.name.clone(), cls.name.clone());

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

    // String data lives from offset 0. Each blob is `[len:i32][bytes][nul]`;
    // the recorded offset points at the bytes (past the length prefix), so
    // emitting the strings in offset order — each prefixed by its length and
    // followed by a NUL — reproduces those exact offsets. The prefix lets a
    // string's length be recovered as `load(offset - 4)` when only its offset
    // word survives (e.g. read back out of a collection slot).
    if !memory_layout.string_offsets.is_empty() {
        let mut offsets: Vec<(&String, u32)> = memory_layout
            .string_offsets
            .iter()
            .map(|(s, &offset)| (s, offset))
            .collect();
        offsets.sort_by_key(|(_s, offset)| *offset);

        let mut all_strings = Vec::new();
        for (s, _) in offsets {
            all_strings.extend_from_slice(&(s.len() as u32).to_le_bytes()); // length prefix
            all_strings.extend_from_slice(s.as_bytes());
            all_strings.push(0); // Null terminator
        }
        data.active(0, &ConstExpr::i32_const(0), all_strings);
    }

    // Bytes data lives from `next_bytes_offset`'s base (32768). Each blob is
    // `[len:i32][bytes]` (no terminator); the recorded offset points at the
    // bytes, so the data segment starts one prefix before the lowest offset.
    if !memory_layout.bytes_offsets.is_empty() {
        let mut offsets: Vec<(&Vec<u8>, u32)> = memory_layout
            .bytes_offsets
            .iter()
            .map(|(b, &offset)| (b, offset))
            .collect();
        offsets.sort_by_key(|(_b, offset)| *offset);

        let base = offsets[0].1 - STRING_LEN_PREFIX;
        let mut all_bytes = Vec::new();
        for (b, _) in offsets {
            all_bytes.extend_from_slice(&(b.len() as u32).to_le_bytes()); // length prefix
            all_bytes.extend_from_slice(b);
        }
        data.active(0, &ConstExpr::i32_const(base as i32), all_bytes);
    }

    // Code section
    let mut codes = CodeSection::new();

    for func_ir in &ir_module.functions {
        let return_type = module_return(func_ir);
        let compiled_func = compile_function(func_ir, &mut ctx, &memory_layout, &return_type, None);
        codes.function(&compiled_func);
    }

    // Compile class methods
    for cls in &ir_module.classes {
        for method in &cls.methods {
            let return_type = method_return(&cls.name, method);
            let compiled_method = compile_function(
                method,
                &mut ctx,
                &memory_layout,
                &return_type,
                Some(&cls.name),
            );
            codes.function(&compiled_method);
        }
    }

    // Runtime allocators, last so their indices match `ctx.alloc_func_index`
    // and `ctx.alloc_obj_func_index`.
    codes.function(&build_alloc_function());
    codes.function(&build_alloc_obj_function(ctx.alloc_func_index));

    // Memory must hold the static data (strings/bytes/object instances) plus
    // every collection region handed out during codegen. The collection heap
    // grows from COLLECTION_HEAP_BASE, so size memory to cover its high-water
    // mark (at least the base region). The runtime bump allocator starts just
    // past that mark and grows memory on demand.
    let heap_end = COLLECTION_HEAP_BASE + ctx.collection_alloc_offset.get();
    let runtime_heap_base = (heap_end + 7) & !7;
    let min_pages = (((runtime_heap_base as u64) + 65535) / 65536).max(2);
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: min_pages,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    // Global 0: the runtime allocator's next-free pointer.
    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(runtime_heap_base as i32),
    );

    // Section order follows the WASM spec: Memory (5), Global (6), Export (7),
    // Code (10), Data (11). Code must precede Data or strict validators (and
    // Binaryen's reader) reject the module, which previously disabled optimization.
    module.section(&memories);
    module.section(&globals);
    module.section(&exports);
    module.section(&codes);
    module.section(&data);

    module.finish()
}
