use crate::compiler::context::{strlen_local_name, CompilationContext, SCRATCH_LOCALS};
use crate::compiler::expression::{emit_expr, emit_integer_power_operation};
use crate::ir::{IRBody, IRConstant, IRExpr, IRFunction, IROp, IRStatement, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

/// Compile an IR function into a WebAssembly function
pub fn compile_function(
    ir_func: &IRFunction,
    ctx: &mut CompilationContext,
    memory_layout: &MemoryLayout,
    return_type: &IRType,
) -> Function {
    ctx.locals_map.clear();
    ctx.local_count = 0;

    for param in &ir_func.params {
        ctx.add_local(&param.name, param.param_type.clone());
    }

    // Scan for variable declarations to allocate locals. The for-loop counter
    // is advanced during the scan and replayed during codegen, so reset it here.
    ctx.for_loop_seq = 0;
    scan_and_allocate_locals(&ir_func.body, ctx);

    // Reserve scratch locals after all params and named locals so temporary
    // calculations never clobber real variables. The i32 scratch run is absent
    // from locals_map (defaults to i32); the f64 scratch is registered so it is
    // declared as f64 and used for operand juggling during int/float coercion.
    ctx.temp_local = ctx.local_count;
    ctx.local_count += SCRATCH_LOCALS;
    ctx.temp_local_f64 = ctx.add_local("__f64_scratch", IRType::Float);

    // Declare locals in index order, coalescing adjacent same-type runs. The
    // local index assigned by `add_local` must match the WASM declaration
    // order, so grouping all i32s then all f64s (which reorders indices) is
    // wrong once a function mixes int and float locals.
    let num_params = ir_func.params.len() as u32;
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    for i in num_params..ctx.local_count {
        let val_type = match get_local_type_by_index(ctx, i) {
            IRType::Float => ValType::F64,
            _ => ValType::I32,
        };
        match locals.last_mut() {
            Some((count, last)) if *last == val_type => *count += 1,
            _ => locals.push((1, val_type)),
        }
    }

    let mut func = Function::new(locals);

    // Replay the same for-loop numbering used by the scan above so codegen
    // resolves the matching iterator helper locals.
    ctx.for_loop_seq = 0;

    // Compile the function body
    compile_body(&ir_func.body, &mut func, ctx, memory_layout);

    // Add default return value if no explicit return. Use the resolved return
    // type (which may have been inferred from the body) so the fall-through
    // value matches the function's declared WASM result.
    match return_type {
        IRType::Float => {
            func.instruction(&Instruction::F64Const(0.0_f64.into()));
        }
        _ => {
            func.instruction(&Instruction::I32Const(0));
        }
    }

    func.instruction(&Instruction::End);

    func
}

/// Resolve a class field to its `(byte offset, value type)`, if known.
pub(crate) fn lookup_field(
    ctx: &CompilationContext,
    class_name: &str,
    field: &str,
) -> Option<(u64, IRType)> {
    let class_info = ctx.get_class_info(class_name)?;
    let offset = *class_info.field_offsets.get(field)?;
    let ty = class_info
        .field_types
        .get(field)
        .cloned()
        .unwrap_or(IRType::Unknown);
    Some((offset, ty))
}

/// Store instruction for a field of the given type (f64 for floats, i32 else).
fn store_field_instr(ty: &IRType, offset: u64) -> Instruction<'static> {
    let mem = MemArg {
        offset,
        align: if matches!(ty, IRType::Float) { 3 } else { 2 },
        memory_index: 0,
    };
    if matches!(ty, IRType::Float) {
        Instruction::F64Store(mem)
    } else {
        Instruction::I32Store(mem)
    }
}

/// Emit a binary arithmetic op for an augmented field assignment, choosing the
/// f64 or i32 instruction by operand type.
fn emit_arith_op(func: &mut Function, op: &IROp, is_float: bool) {
    let instr = match (op, is_float) {
        (IROp::Add, false) => Instruction::I32Add,
        (IROp::Sub, false) => Instruction::I32Sub,
        (IROp::Mul, false) => Instruction::I32Mul,
        (IROp::Div, false) | (IROp::FloorDiv, false) => Instruction::I32DivS,
        (IROp::Mod, false) => Instruction::I32RemS,
        (IROp::Add, true) => Instruction::F64Add,
        (IROp::Sub, true) => Instruction::F64Sub,
        (IROp::Mul, true) => Instruction::F64Mul,
        (IROp::Div, true) | (IROp::FloorDiv, true) => Instruction::F64Div,
        // Anything else (e.g. Pow, bitwise) is uncommon for fields; fall back to
        // a numeric add so the stack stays balanced.
        (_, true) => Instruction::F64Add,
        (_, false) => Instruction::I32Add,
    };
    func.instruction(&instr);
}

/// Load instruction for a field of the given type (f64 for floats, i32 else).
pub(crate) fn load_field_instr(ty: &IRType, offset: u64) -> Instruction<'static> {
    let mem = MemArg {
        offset,
        align: if matches!(ty, IRType::Float) { 3 } else { 2 },
        memory_index: 0,
    };
    if matches!(ty, IRType::Float) {
        Instruction::F64Load(mem)
    } else {
        Instruction::I32Load(mem)
    }
}

/// Get the type of a local variable by its index
fn get_local_type_by_index(ctx: &CompilationContext, index: u32) -> IRType {
    for local_info in ctx.locals_map.values() {
        if local_info.index == index {
            return local_info.var_type.clone();
        }
    }
    IRType::Int // Default to i32
}

/// Reserve a named local if it has not been allocated yet. Used by the scan to
/// pre-declare the compiler's internal helper locals (exception state, context
/// managers, ...) so codegen never adds locals after the function's local
/// vector is fixed.
fn ensure_local(ctx: &mut CompilationContext, name: &str, var_type: IRType) {
    if ctx.get_local_index(name).is_none() {
        ctx.add_local(name, var_type);
    }
}

/// Best-effort type inference for an unannotated assignment value. Used to
/// decide a local's WASM value type (f64 vs i32) and to recognise string/bytes
/// locals so a companion length local can be reserved for them. It only needs
/// to recognise float- and string/bytes-producing expressions confidently;
/// anything else is left `Unknown` (an i32 slot, which collections and pointers
/// also use).
fn infer_value_type(value: &IRExpr, ctx: &CompilationContext) -> IRType {
    match value {
        IRExpr::Const(IRConstant::Float(_)) => IRType::Float,
        IRExpr::Const(IRConstant::String(_)) => IRType::String,
        IRExpr::Const(IRConstant::Bytes(_)) => IRType::Bytes,
        IRExpr::BinaryOp { left, right, op } => {
            let lt = infer_value_type(left, ctx);
            let rt = infer_value_type(right, ctx);
            if lt == IRType::Float || rt == IRType::Float {
                IRType::Float
            } else if matches!(op, IROp::Add) && matches!(lt, IRType::String | IRType::Bytes) {
                // String/bytes concatenation yields the same kind.
                lt
            } else {
                IRType::Unknown
            }
        }
        IRExpr::UnaryOp { operand, .. } => infer_value_type(operand, ctx),
        // Float-valued stdlib constants (e.g. `math.pi`, `math.e`) must make
        // their local an f64; otherwise the f64 store lands in an i32 slot.
        IRExpr::Attribute { object, attribute } => match object.as_ref() {
            IRExpr::Variable(module)
                if matches!(
                    crate::stdlib::get_stdlib_attributes(module, attribute),
                    Some(crate::stdlib::StdlibValue::Float(_))
                ) =>
            {
                IRType::Float
            }
            _ => IRType::Unknown,
        },
        // Slicing a string/bytes yields the same kind; indexing a string yields
        // a one-character string (bytes/list indexing yields a scalar).
        IRExpr::Slicing { container, .. } => match infer_value_type(container, ctx) {
            t @ (IRType::String | IRType::Bytes) => t,
            _ => IRType::Unknown,
        },
        IRExpr::Indexing { container, .. }
            if infer_value_type(container, ctx) == IRType::String =>
        {
            IRType::String
        }
        IRExpr::Variable(name) => ctx
            .get_local_info(name)
            .map(|info| info.var_type.clone())
            .unwrap_or(IRType::Unknown),
        IRExpr::FunctionCall { function_name, .. } if function_name == "float" => IRType::Float,
        IRExpr::FunctionCall { function_name, .. } => ctx
            .get_function_info(function_name)
            .map(|f| f.return_type.clone())
            .filter(|t| *t == IRType::Float)
            .unwrap_or(IRType::Unknown),
        _ => IRType::Unknown,
    }
}

/// Resolve a function's WASM result type. An explicit annotation wins; otherwise
/// the type is inferred from the body's `return` statements so that, e.g., a
/// function returning `math.pi` gets an f64 result instead of a default i32 (an
/// f64 return value into an i32 result fails validation and aborts Binaryen).
///
/// `known_returns` carries the already-resolved return types of other functions
/// so a `return some_call()` resolves; callees defined earlier are resolved
/// first, and a second resolution pass handles forward references.
pub(crate) fn resolve_return_type(
    ir_func: &IRFunction,
    known_returns: &std::collections::HashMap<String, IRType>,
) -> IRType {
    if !matches!(ir_func.return_type, IRType::Unknown) {
        return ir_func.return_type.clone();
    }

    // Build a scratch context with the params and known function return types,
    // then run the local scan so local types (including float stdlib constants)
    // are available to the return-expression inference.
    let mut ctx = CompilationContext::new();
    for (name, ret) in known_returns {
        ctx.add_function(name, 0, Vec::new(), ret.clone());
    }
    for param in &ir_func.params {
        ctx.add_local(&param.name, param.param_type.clone());
    }
    ctx.for_loop_seq = 0;
    scan_and_allocate_locals(&ir_func.body, &mut ctx);

    let mut inferred = IRType::Unknown;
    collect_return_type(&ir_func.body, &ctx, &mut inferred);
    inferred
}

/// Fold the inferred types of a body's `return` expressions into `out`. A float
/// return forces an f64 result; otherwise the first concrete type seen wins.
fn collect_return_type(body: &IRBody, ctx: &CompilationContext, out: &mut IRType) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Return(Some(expr)) => {
                let t = infer_value_type(expr, ctx);
                if t == IRType::Float {
                    *out = IRType::Float;
                } else if matches!(out, IRType::Unknown) && !matches!(t, IRType::Unknown) {
                    *out = t;
                }
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                collect_return_type(then_body, ctx, out);
                if let Some(else_body) = else_body {
                    collect_return_type(else_body, ctx, out);
                }
            }
            IRStatement::While { body, .. } => collect_return_type(body, ctx, out),
            IRStatement::For { body, .. } => collect_return_type(body, ctx, out),
            _ => {}
        }
    }
}

/// Scan the function body for variable declarations and allocate local variables
pub fn scan_and_allocate_locals(body: &IRBody, ctx: &mut CompilationContext) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Assign {
                target,
                var_type,
                value,
            } => {
                if ctx.get_local_index(target).is_none() {
                    // Use the annotation if present; otherwise infer the type
                    // from the value so unannotated float locals become f64.
                    let var_type = var_type
                        .clone()
                        .unwrap_or_else(|| infer_value_type(value, ctx));
                    // String/bytes locals carry an (offset, length) pair, so they
                    // need a companion local for the length. Reserve one for
                    // `Unknown` locals too: a stdlib call like `os.path.join`
                    // infers as `Unknown` here but is upgraded to `String` during
                    // codegen, and the companion can't be added after the local
                    // vector is fixed.
                    let needs_companion =
                        matches!(var_type, IRType::String | IRType::Bytes | IRType::Unknown);
                    ctx.add_local(target, var_type);
                    if needs_companion {
                        ctx.add_local(&strlen_local_name(target), IRType::Int);
                    }
                }
            }
            IRStatement::TupleUnpack { targets, .. } => {
                for target in targets {
                    if ctx.get_local_index(target).is_none() {
                        ctx.add_local(target, IRType::Unknown);
                    }
                }
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                scan_and_allocate_locals(then_body, ctx);
                if let Some(else_body) = else_body {
                    scan_and_allocate_locals(else_body, ctx);
                }
            }
            IRStatement::While { body, .. } => {
                scan_and_allocate_locals(body, ctx);
            }
            IRStatement::For {
                target,
                body,
                else_body,
                ..
            } => {
                // Allocate the loop variable
                if ctx.get_local_index(target).is_none() {
                    ctx.add_local(target, IRType::Unknown);
                }
                // Reserve this loop's iterator helper locals up front (codegen
                // can't add locals after the function's local vector is fixed).
                // Keyed by sequence number so nested loops get distinct locals.
                let seq = ctx.for_loop_seq;
                ctx.for_loop_seq += 1;
                ctx.add_local(&format!("__iter_ptr_{seq}"), IRType::Unknown);
                ctx.add_local(&format!("__iter_idx_{seq}"), IRType::Int);
                ctx.add_local(&format!("__iter_len_{seq}"), IRType::Int);
                scan_and_allocate_locals(body, ctx);
                if let Some(else_body) = else_body {
                    scan_and_allocate_locals(else_body, ctx);
                }
            }
            IRStatement::Raise { .. } => {
                // Raise uses the shared exception-state locals; reserve them so
                // codegen never has to add locals after the local set is fixed.
                ensure_local(ctx, "__exception_flag", IRType::Int);
                ensure_local(ctx, "__exception_type", IRType::Int);
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                ensure_local(ctx, "__exception_flag", IRType::Int);
                ensure_local(ctx, "__exception_type", IRType::Int);
                scan_and_allocate_locals(try_body, ctx);

                for handler in except_handlers {
                    // Allocate exception variable if it exists
                    if let Some(name) = &handler.name {
                        if ctx.get_local_index(name).is_none() {
                            ctx.add_local(name, IRType::Unknown);
                        }
                    }
                    scan_and_allocate_locals(&handler.body, ctx);
                }

                if let Some(finally_body) = finally_body {
                    scan_and_allocate_locals(finally_body, ctx);
                }
            }
            IRStatement::With {
                optional_vars,
                body,
                ..
            } => {
                // Allocate context variable if it exists
                if let Some(name) = optional_vars {
                    if ctx.get_local_index(name).is_none() {
                        ctx.add_local(name, IRType::Unknown);
                    }
                }
                scan_and_allocate_locals(body, ctx);
            }
            _ => {}
        }
    }
}

/// Compile a function body into WebAssembly instructions
pub fn compile_body(
    body: &IRBody,
    func: &mut Function,
    ctx: &mut CompilationContext,
    memory_layout: &MemoryLayout,
) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Return(expr_opt) => {
                if let Some(expr) = expr_opt {
                    emit_expr(expr, func, ctx, memory_layout, None);
                } else {
                    func.instruction(&Instruction::I32Const(0));
                }
                func.instruction(&Instruction::Return);
            }
            IRStatement::Assign {
                target,
                value,
                var_type,
            } => {
                // Get the expected type for the assignment
                let expected_type = var_type
                    .as_ref()
                    .cloned()
                    .or_else(|| ctx.get_local_info(target).map(|info| info.var_type.clone()));

                // Emit code for the value
                let value_type = emit_expr(value, func, ctx, memory_layout, expected_type.as_ref());

                // An unannotated local is allocated as Unknown (an i32 slot).
                // Recover the element/entry types of collections so later
                // indexing knows how to load each slot. Only pointer-shaped
                // types are adopted, since they share that same i32 slot and
                // won't disturb the already-fixed local layout.
                if let Some(info) = ctx.locals_map.get_mut(target) {
                    if matches!(info.var_type, IRType::Unknown)
                        && matches!(
                            value_type,
                            IRType::List(_)
                                | IRType::Tuple(_)
                                | IRType::Dict(_, _)
                                | IRType::Set(_)
                                | IRType::String
                                | IRType::Bytes
                                | IRType::Class(_)
                        )
                    {
                        info.var_type = value_type.clone();
                    }
                }

                if let Some(local_idx) = ctx.get_local_index(target) {
                    // A string/bytes value is an (offset, length) pair with the
                    // length on top of the stack. Store the length into the
                    // companion local first, then the offset into the named one.
                    if matches!(value_type, IRType::String | IRType::Bytes) {
                        match ctx.get_local_index(&strlen_local_name(target)) {
                            Some(len_idx) => func.instruction(&Instruction::LocalSet(len_idx)),
                            // No companion was reserved (inference missed this
                            // string local); drop the length to keep the stack
                            // balanced rather than leaving it stranded.
                            None => func.instruction(&Instruction::Drop),
                        };
                    }
                    func.instruction(&Instruction::LocalSet(local_idx));
                } else {
                    // Handle the case where the variable is not found in the context
                    panic!("Variable {target} not found in context");
                }
            }
            IRStatement::TupleUnpack { targets, value } => {
                // Emit code for the value (should be a tuple)
                let _tuple_type = emit_expr(value, func, ctx, memory_layout, None);

                // Load tuple length
                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Verify that number of targets matches tuple length
                func.instruction(&Instruction::I32Const(targets.len() as i32));
                func.instruction(&Instruction::I32Ne);
                func.instruction(&Instruction::If(BlockType::Empty));
                // Error case: tuple size mismatch - for now just continue
                func.instruction(&Instruction::End);

                // Extract each element from the tuple and assign to target variables
                for (i, target) in targets.iter().enumerate() {
                    // Load tuple pointer
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));

                    // Add offset to get element (4 + i*4)
                    func.instruction(&Instruction::I32Const(4 + (i as i32) * 4));
                    func.instruction(&Instruction::I32Add);

                    // Load element value
                    func.instruction(&Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));

                    // Store in target variable
                    if let Some(local_idx) = ctx.get_local_index(target) {
                        func.instruction(&Instruction::LocalSet(local_idx));
                    } else {
                        panic!("Variable {target} not found in context");
                    }
                }
            }
            IRStatement::If {
                condition,
                then_body,
                else_body,
            } => {
                // Emit condition code, ensuring it returns a boolean
                emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));

                // If-else block with no result value
                func.instruction(&Instruction::If(BlockType::Empty));

                // branch
                compile_body(then_body, func, ctx, memory_layout);

                if let Some(else_body) = else_body {
                    func.instruction(&Instruction::Else);
                    // branch
                    compile_body(else_body, func, ctx, memory_layout);
                }

                func.instruction(&Instruction::End);
            }

            IRStatement::Raise { exception } => {
                // Mark exception as raised by setting exception flag
                // Try to get existing exception flag variable if in a try block
                let exception_flag_idx = ctx
                    .get_local_index("__exception_flag")
                    .unwrap_or_else(|| ctx.add_local("__exception_flag", IRType::Int));
                let exception_type_idx = ctx
                    .get_local_index("__exception_type")
                    .unwrap_or_else(|| ctx.add_local("__exception_type", IRType::Int));

                if let Some(exc_expr) = exception {
                    // Evaluate exception expression to get exception code/type
                    emit_expr(exc_expr, func, ctx, memory_layout, None);
                    // Store as exception type code
                    func.instruction(&Instruction::LocalSet(exception_type_idx));
                } else {
                    // Generic exception code
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::LocalSet(exception_type_idx));
                }

                // Set exception flag to 1
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::LocalSet(exception_flag_idx));
            }

            IRStatement::While { condition, body } => {
                // Loop block
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Condition check: exit the loop when the condition is false.
                emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));
                func.instruction(&Instruction::I32Eqz);
                func.instruction(&Instruction::BrIf(1));

                // Loop body
                compile_body(body, func, ctx, memory_layout);

                // Jump back to the start of the loop
                func.instruction(&Instruction::Br(0));

                // End of loop and block
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);
            }
            IRStatement::Expression(expr) => {
                // Discard the result only when the expression actually leaves a
                // value. Calls like print() return None and push nothing, so an
                // unconditional drop would underflow the stack. String/bytes
                // values (e.g. a docstring statement) are an (offset, length)
                // pair and need two drops.
                let result_type = emit_expr(expr, func, ctx, memory_layout, None);
                match result_type {
                    IRType::None => {}
                    IRType::String | IRType::Bytes => {
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                    }
                    _ => {
                        func.instruction(&Instruction::Drop);
                    }
                }
            }
            IRStatement::AttributeAssign {
                object,
                attribute,
                value,
            } => {
                // Emit the object reference (the store address) first; a WASM
                // store pops the value, then the address.
                let obj_type = emit_expr(object, func, ctx, memory_layout, None);

                let field = match &obj_type {
                    IRType::Class(class_name) => lookup_field(ctx, class_name, attribute),
                    _ => None,
                };

                if let Some((field_offset, field_ty)) = field {
                    // Stack: object_ptr. Emit the value coerced to the field's
                    // type, then store with the matching width (f64 for float
                    // fields, i32 otherwise).
                    emit_expr(value, func, ctx, memory_layout, Some(&field_ty));
                    func.instruction(&store_field_instr(&field_ty, field_offset));
                } else {
                    // Unknown field: drop the address and the value.
                    emit_expr(value, func, ctx, memory_layout, None);
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::Drop);
                }
            }

            IRStatement::AugAssign { target, value, op } => {
                // Get the local index
                if let Some(local_idx) = ctx.get_local_index(target) {
                    // Load the current value
                    func.instruction(&Instruction::LocalGet(local_idx));

                    // Emit code for the value to add/multiply/etc.
                    emit_expr(value, func, ctx, memory_layout, None);

                    // Apply the operation (add, multiply, etc.)
                    match op {
                        IROp::Add => {
                            func.instruction(&Instruction::I32Add);
                        }
                        IROp::Sub => {
                            func.instruction(&Instruction::I32Sub);
                        }
                        IROp::Mul => {
                            func.instruction(&Instruction::I32Mul);
                        }
                        IROp::Div => {
                            func.instruction(&Instruction::I32DivS);
                        }
                        IROp::Mod => {
                            func.instruction(&Instruction::I32RemS);
                        }
                        IROp::FloorDiv => {
                            func.instruction(&Instruction::I32DivS);
                        }
                        IROp::Pow => {
                            emit_integer_power_operation(func);
                        }
                        // Handle other operations with placeholder implementations
                        _ => {
                            // Default for unimplemented operations
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                        }
                    }

                    // Store the result back
                    func.instruction(&Instruction::LocalSet(local_idx));
                } else {
                    // Variable not found
                    panic!("Variable {target} not found in context");
                }
            }

            IRStatement::AttributeAugAssign {
                object,
                attribute,
                value,
                op,
            } => {
                // `obj.field OP= value` -> obj.field = (obj.field OP value).
                let obj_type = emit_expr(object, func, ctx, memory_layout, None);
                func.instruction(&Instruction::LocalSet(ctx.temp_local)); // temp = obj_ptr

                let field = match &obj_type {
                    IRType::Class(class_name) => lookup_field(ctx, class_name, attribute),
                    _ => None,
                };

                if let Some((offset, field_ty)) = field {
                    let is_float = matches!(field_ty, IRType::Float);
                    // Store address.
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    // Current field value.
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&load_field_instr(&field_ty, offset));
                    // Operand, coerced to the field's type.
                    emit_expr(value, func, ctx, memory_layout, Some(&field_ty));
                    emit_arith_op(func, op, is_float);
                    func.instruction(&store_field_instr(&field_ty, offset));
                } else {
                    emit_expr(value, func, ctx, memory_layout, None);
                    func.instruction(&Instruction::Drop);
                }
            }

            IRStatement::For {
                target,
                iterable,
                body,
                else_body: _,
            } => {
                // Proper for loop implementation that iterates over lists
                // Allocate locals for loop variables:
                // - iterator_ptr: pointer to the list/iterable
                // - loop_counter: current index in the list
                // - list_length: length of the list

                // Reuse the iterator helper locals reserved for this loop during
                // the scan, replaying the same sequence numbering.
                let seq = ctx.for_loop_seq;
                ctx.for_loop_seq += 1;
                let iterator_ptr_idx = ctx
                    .get_local_index(&format!("__iter_ptr_{seq}"))
                    .expect("iterator ptr local not reserved");
                let loop_counter_idx = ctx
                    .get_local_index(&format!("__iter_idx_{seq}"))
                    .expect("iterator idx local not reserved");
                let list_length_idx = ctx
                    .get_local_index(&format!("__iter_len_{seq}"))
                    .expect("iterator len local not reserved");
                let target_idx = ctx
                    .get_local_index(target)
                    .expect("Target variable not found");

                // Evaluate the iterable (should return a pointer to list or value)
                let iterable_type = emit_expr(iterable, func, ctx, memory_layout, None);

                match iterable_type {
                    IRType::List(_) | IRType::String => {
                        // Store the pointer to the list/string
                        func.instruction(&Instruction::LocalSet(iterator_ptr_idx));

                        // Get list length: load from memory at ptr+0
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(list_length_idx));

                        // Initialize loop counter to 0
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));

                        // Loop structure
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));

                        // Check if counter >= length
                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1)); // Break if true

                        // Load element from list[counter]
                        // Memory: [length:i32][elem0:i32][elem1:i32]...
                        // Element at index i is at offset 4 + (i * 4)
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::I32Const(4));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));

                        // Store element in target variable
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // Execute the loop body
                        compile_body(body, func, ctx, memory_layout);

                        // Increment counter
                        func.instruction(&Instruction::LocalGet(loop_counter_idx));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));

                        // Loop back
                        func.instruction(&Instruction::Br(0));

                        // End of loop
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::End);
                    }
                    IRType::Range => {
                        // Range object layout: [start:i32][stop:i32][step:i32][current:i32]
                        func.instruction(&Instruction::LocalSet(iterator_ptr_idx));

                        // Load start value into target
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // Initialize loop counter to 0
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(loop_counter_idx));

                        // Loop structure
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));

                        // Load stop and step for comparison
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 4,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(list_length_idx));

                        // Break condition depends on the sign of step, which may
                        // be dynamic, so branch on it at runtime:
                        //   step > 0  -> stop iterating once current >= stop
                        //   step <= 0 -> stop iterating once current <= stop
                        // (A single ascending `current >= stop` test would make a
                        // descending range, e.g. range(10, 0, -1), exit immediately.)
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 8,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32GtS); // step > 0
                        func.instruction(&Instruction::If(BlockType::Result(ValType::I32)));
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32GeS); // current >= stop
                        func.instruction(&Instruction::Else);
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32LeS); // current <= stop
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::BrIf(1)); // Break if true

                        // Execute the loop body
                        compile_body(body, func, ctx, memory_layout);

                        // Increment by step
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::LocalGet(iterator_ptr_idx));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 8,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // Loop back
                        func.instruction(&Instruction::Br(0));

                        // End of loop
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::End);
                    }
                    _ => {
                        // For non-list iterables, fall back to simple counting
                        // Treat the value as a count (integer)
                        func.instruction(&Instruction::LocalSet(target_idx));

                        // Simple loop: counter from 1 to value
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));

                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::I32LeS);
                        func.instruction(&Instruction::BrIf(1));

                        // Execute body
                        compile_body(body, func, ctx, memory_layout);

                        // Decrement
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Sub);
                        func.instruction(&Instruction::LocalSet(target_idx));

                        func.instruction(&Instruction::Br(0));
                        func.instruction(&Instruction::End);
                        func.instruction(&Instruction::End);
                    }
                }
            }

            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                // Implement exception handling with a global exception state
                // We use a special local variable to track if an exception was raised
                // Reuse the exception-state locals reserved during the scan.
                let exception_flag_idx = ctx
                    .get_local_index("__exception_flag")
                    .unwrap_or_else(|| ctx.add_local("__exception_flag", IRType::Int));
                let exception_type_idx = ctx
                    .get_local_index("__exception_type")
                    .unwrap_or_else(|| ctx.add_local("__exception_type", IRType::Int));

                // Initialize exception flag to 0 (no exception)
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(exception_flag_idx));

                // Execute the try block
                compile_body(try_body, func, ctx, memory_layout);

                // Check if an exception was raised
                func.instruction(&Instruction::LocalGet(exception_flag_idx));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32Eq);

                // If no exception (flag == 0), skip all except handlers and go to finally
                func.instruction(&Instruction::If(BlockType::Empty));

                // If an exception occurred, check handlers
                func.instruction(&Instruction::Else);

                // Try to match exception handlers
                for (idx, handler) in except_handlers.iter().enumerate() {
                    let is_last = idx == except_handlers.len() - 1;

                    // Check if this handler matches the exception type
                    // For now, match any exception if no type is specified, or match by type
                    if handler.exception_type.is_none() {
                        // Bare except: catches all exceptions
                        if let Some(var_name) = &handler.name {
                            let handler_var_idx = ctx
                                .get_local_index(var_name)
                                .unwrap_or_else(|| ctx.add_local(var_name, IRType::Unknown));
                            // Store exception type in the handler variable
                            func.instruction(&Instruction::LocalGet(exception_type_idx));
                            func.instruction(&Instruction::LocalSet(handler_var_idx));
                        }

                        // Execute handler body
                        compile_body(&handler.body, func, ctx, memory_layout);

                        // Clear exception flag
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(exception_flag_idx));
                    } else if let Some(exc_type) = &handler.exception_type {
                        // Typed exception handler
                        // Map exception type names to codes
                        let exc_code = match exc_type.as_str() {
                            "ZeroDivisionError" => 1,
                            "ValueError" => 2,
                            "TypeError" => 3,
                            "KeyError" => 4,
                            "IndexError" => 5,
                            "AttributeError" => 6,
                            "RuntimeError" => 7,
                            _ => 99, // Unknown exception type
                        };

                        func.instruction(&Instruction::Block(BlockType::Empty));

                        // Check if exception type matches
                        func.instruction(&Instruction::LocalGet(exception_type_idx));
                        func.instruction(&Instruction::I32Const(exc_code));
                        func.instruction(&Instruction::I32Eq);
                        func.instruction(&Instruction::I32Eqz);
                        func.instruction(&Instruction::BrIf(0)); // Branch to next handler if no match

                        if let Some(var_name) = &handler.name {
                            let handler_var_idx = ctx
                                .get_local_index(var_name)
                                .unwrap_or_else(|| ctx.add_local(var_name, IRType::Unknown));
                            func.instruction(&Instruction::LocalGet(exception_type_idx));
                            func.instruction(&Instruction::LocalSet(handler_var_idx));
                        }

                        // Execute handler body
                        compile_body(&handler.body, func, ctx, memory_layout);

                        // Clear exception flag and skip remaining handlers
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(exception_flag_idx));

                        func.instruction(&Instruction::End);
                    }

                    if is_last && handler.exception_type.is_some() {
                        // Add final block for unmatched exceptions
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        // If we reach here and exception_flag is still set, no handler matched
                        func.instruction(&Instruction::End);
                    }
                }

                // Close the exception-dispatch if/else. Each typed handler opens
                // and closes its own block, so only this `If` remains open here;
                // a second `End` would close the function frame early.
                func.instruction(&Instruction::End);

                // If there's a finally block, always execute it
                if let Some(finally_body) = finally_body {
                    compile_body(finally_body, func, ctx, memory_layout);
                }
            }

            IRStatement::With {
                context_expr,
                optional_vars,
                body,
            } => {
                // Context manager implementation
                // with expr as var: body
                // This requires calling __enter__ on the context manager and __exit__ after

                let context_var_idx = ctx.add_local("__context_mgr", IRType::Unknown);
                let exception_flag_idx = ctx
                    .get_local_index("__exception_flag")
                    .unwrap_or_else(|| ctx.add_local("__exception_flag", IRType::Int));

                // Evaluate context expression
                let ctx_type = emit_expr(context_expr, func, ctx, memory_layout, None);

                // Store context manager
                func.instruction(&Instruction::LocalSet(context_var_idx));

                // If optional_vars is provided, assign it the context manager value
                if let Some(var_name) = optional_vars {
                    let var_idx = ctx
                        .get_local_index(var_name)
                        .unwrap_or_else(|| ctx.add_local(var_name, ctx_type));
                    func.instruction(&Instruction::LocalGet(context_var_idx));
                    func.instruction(&Instruction::LocalSet(var_idx));
                }

                // Initialize exception flag for the with block
                let pre_exception_flag_idx = ctx.add_local("__pre_exception_flag", IRType::Int);
                func.instruction(&Instruction::LocalGet(exception_flag_idx));
                func.instruction(&Instruction::LocalSet(pre_exception_flag_idx));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(exception_flag_idx));

                // Execute the body (may raise exceptions)
                compile_body(body, func, ctx, memory_layout);

                // Check if exception was raised
                func.instruction(&Instruction::LocalGet(exception_flag_idx));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32Eq);
                func.instruction(&Instruction::If(BlockType::Empty));

                // No exception: normal exit
                // Restore pre-with exception state
                func.instruction(&Instruction::LocalGet(pre_exception_flag_idx));
                func.instruction(&Instruction::LocalSet(exception_flag_idx));

                func.instruction(&Instruction::Else);

                // Exception occurred: still need to run __exit__ with exception info
                // Restore pre-with exception state and re-raise if needed
                func.instruction(&Instruction::LocalGet(pre_exception_flag_idx));
                func.instruction(&Instruction::LocalSet(exception_flag_idx));

                func.instruction(&Instruction::End);
            }

            IRStatement::DynamicImport {
                target,
                module_name,
            } => {
                // Emit code to evaluate the module name expression
                emit_expr(module_name, func, ctx, memory_layout, None);

                // Get the target local index or create one if it doesn't exist
                let local_idx = ctx
                    .get_local_index(target)
                    .unwrap_or_else(|| ctx.add_local(target, IRType::Unknown));

                // Store the result (currently just a placeholder) in the target variable
                func.instruction(&Instruction::LocalSet(local_idx));
            }

            IRStatement::IndexAssign {
                container,
                index,
                value,
            } => {
                // Get container type to determine storage strategy
                let container_type = emit_expr(container, func, ctx, memory_layout, None);

                // Save container pointer
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit index expression
                emit_expr(index, func, ctx, memory_layout, Some(&IRType::Int));

                // Save index
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

                // Emit value expression
                let value_type = emit_expr(value, func, ctx, memory_layout, None);

                // Save value
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2));

                match container_type {
                    IRType::List(_) => {
                        // Calculate address: container_ptr + 4 + (index * 4)
                        func.instruction(&Instruction::LocalGet(ctx.temp_local)); // container_ptr
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // index
                        func.instruction(&Instruction::I32Const(4));
                        func.instruction(&Instruction::I32Mul); // index * 4
                        func.instruction(&Instruction::I32Const(4)); // skip length field
                        func.instruction(&Instruction::I32Add); // + 4
                        func.instruction(&Instruction::I32Add); // container_ptr + 4 + (index * 4)

                        // Restore value
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));

                        // Store based on value type
                        match value_type {
                            IRType::Float => {
                                func.instruction(&Instruction::F64Store(MemArg {
                                    offset: 0,
                                    align: 3,
                                    memory_index: 0,
                                }));
                            }
                            _ => {
                                func.instruction(&Instruction::I32Store(MemArg {
                                    offset: 0,
                                    align: 2,
                                    memory_index: 0,
                                }));
                            }
                        }
                    }
                    IRType::Dict(_key_type, _value_type) => {
                        // Dictionary assignment via linear search.
                        // Layout: [num_entries:i32][key0][val0][key1][val1]...
                        //   temp_local     = dict_ptr
                        //   temp_local + 1 = key
                        //   temp_local + 2 = value
                        //   temp_local + 3 = num_entries
                        //   temp_local + 4 = counter
                        //   temp_local + 5 = found flag (0/1)

                        // num_entries = load(dict_ptr)
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                        // counter = 0; found = 0
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(0));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 5));

                        // Search for an existing entry with a matching key.
                        func.instruction(&Instruction::Block(BlockType::Empty));
                        func.instruction(&Instruction::Loop(BlockType::Empty));

                        // if counter >= num_entries: break
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32GeS);
                        func.instruction(&Instruction::BrIf(1));

                        // key_at = load(dict_ptr + counter*8 + 4)
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(8));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Const(4));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));

                        // if key_at == key: update value and break
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                        func.instruction(&Instruction::I32Eq);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        // address = dict_ptr + counter*8 + 8
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(8));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Const(8));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // value
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 5)); // found = 1
                        func.instruction(&Instruction::Br(2)); // exit the loop
                        func.instruction(&Instruction::End);

                        // counter += 1; continue
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalSet(ctx.temp_local + 4));
                        func.instruction(&Instruction::Br(0));
                        func.instruction(&Instruction::End); // loop
                        func.instruction(&Instruction::End); // block

                        // If the key was not present, append a new entry at slot
                        // num_entries and bump the entry count.
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 5));
                        func.instruction(&Instruction::I32Eqz);
                        func.instruction(&Instruction::If(BlockType::Empty));
                        // store key at dict_ptr + num_entries*8 + 4
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32Const(8));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Const(4));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // key
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        // store value at dict_ptr + num_entries*8 + 8
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32Const(8));
                        func.instruction(&Instruction::I32Mul);
                        func.instruction(&Instruction::I32Const(8));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // value
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        // num_entries += 1; store back to dict_ptr
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Add);
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        func.instruction(&Instruction::End);
                    }
                    IRType::String => {
                        // String indexing is read-only in Python, assignment not directly supported
                        func.instruction(&Instruction::Drop);
                    }
                    _ => {
                        // Unknown container type
                        func.instruction(&Instruction::Drop);
                    }
                }
            }

            IRStatement::Yield { value } => {
                // Emit the yielded value expression
                if let Some(val) = value {
                    emit_expr(val, func, ctx, memory_layout, None);
                } else {
                    // yield without a value yields None
                    func.instruction(&Instruction::I32Const(0));
                }

                // For generator support, the yielded value would be stored
                // in a generator state and execution would be paused.
                // For now, this is a placeholder that just drops the value.
                func.instruction(&Instruction::Drop);
            }

            IRStatement::ImportModule { module_name, alias } => {
                // Create a variable to hold the imported module
                let var_name = alias.as_ref().unwrap_or(module_name);
                let _local_idx = ctx.add_local(var_name, IRType::Module(module_name.clone()));

                // For now, store a dummy module reference
                // Full implementation would load and execute the module
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(_local_idx));
            }
        }
    }
}
