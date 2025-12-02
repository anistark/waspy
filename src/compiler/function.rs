use crate::compiler::context::CompilationContext;
use crate::compiler::expression::{emit_expr, emit_integer_power_operation};
use crate::ir::{IRBody, IRFunction, IROp, IRStatement, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

/// Compile an IR function into a WebAssembly function
pub fn compile_function(
    ir_func: &IRFunction,
    ctx: &mut CompilationContext,
    memory_layout: &MemoryLayout,
) -> Function {
    ctx.locals_map.clear();
    ctx.local_count = 0;

    for param in &ir_func.params {
        ctx.add_local(&param.name, param.param_type.clone());
    }

    // Scan for variable declarations to allocate locals
    scan_and_allocate_locals(&ir_func.body, ctx);

    // Determine local types for WebAssembly
    let mut locals = Vec::new();
    let num_params = ir_func.params.len() as u32;

    // Group locals by type
    let mut i32_count = 0;
    let mut f64_count = 0;

    // Count locals by type (excluding parameters)
    for i in num_params..ctx.local_count {
        match get_local_type_by_index(ctx, i) {
            IRType::Float => f64_count += 1,
            _ => i32_count += 1,
        }
    }

    // Add locals to function signature
    if i32_count > 0 {
        locals.push((i32_count, ValType::I32));
    }
    if f64_count > 0 {
        locals.push((f64_count, ValType::F64));
    }

    let mut func = Function::new(locals);

    // Compile the function body
    compile_body(&ir_func.body, &mut func, ctx, memory_layout);

    // Add default return value if no explicit return
    match ir_func.return_type {
        IRType::Float => {
            func.instruction(&Instruction::F64Const(0.0_f64.into()));
        }
        IRType::None => {
            func.instruction(&Instruction::I32Const(0));
        }
        _ => {
            func.instruction(&Instruction::I32Const(0));
        }
    }

    func.instruction(&Instruction::End);

    func
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

/// Scan the function body for variable declarations and allocate local variables
pub fn scan_and_allocate_locals(body: &IRBody, ctx: &mut CompilationContext) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Assign {
                target, var_type, ..
            } => {
                if ctx.get_local_index(target).is_none() {
                    let var_type = var_type.clone().unwrap_or(IRType::Unknown);
                    ctx.add_local(target, var_type);
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
                scan_and_allocate_locals(body, ctx);
                if let Some(else_body) = else_body {
                    scan_and_allocate_locals(else_body, ctx);
                }
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
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
                emit_expr(value, func, ctx, memory_layout, expected_type.as_ref());

                if let Some(local_idx) = ctx.get_local_index(target) {
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

                // Condition check
                emit_expr(condition, func, ctx, memory_layout, Some(&IRType::Bool));
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
                emit_expr(expr, func, ctx, memory_layout, None);
                func.instruction(&Instruction::Drop);
            }
            IRStatement::AttributeAssign {
                object,
                attribute,
                value,
            } => {
                // Emit code for the object (get reference)
                let obj_type = emit_expr(object, func, ctx, memory_layout, None);

                // Store the object reference temporarily
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit code for the value
                emit_expr(value, func, ctx, memory_layout, None);

                // Store the value temporarily
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

                // Load the object reference
                func.instruction(&Instruction::LocalGet(ctx.temp_local));

                // Check if object is a custom class
                if let crate::ir::IRType::Class(class_name) = &obj_type {
                    if let Some(class_info) = ctx.get_class_info(class_name) {
                        if let Some(&field_offset) = class_info.field_offsets.get(attribute) {
                            // Stack: object_ptr
                            // Load the value to store
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            // Store value at object_ptr + field_offset
                            func.instruction(&Instruction::I32Store(MemArg {
                                offset: field_offset,
                                align: 2,
                                memory_index: 0,
                            }));
                            return;
                        }
                    }
                }

                // Fallback: drop everything
                func.instruction(&Instruction::Drop); // Drop object pointer
                func.instruction(&Instruction::Drop); // Drop value
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
                attribute: _, // Ignore the attribute field for now
                value,
                op: _, // Ignore the operation for now
            } => {
                // This is a simplified implementation that doesn't actually perform the operation
                // It's just a placeholder to get the code to compile

                // Emit code for the object (get reference)
                emit_expr(object, func, ctx, memory_layout, None);

                // Emit code for the value
                emit_expr(value, func, ctx, memory_layout, None);

                // For now, just drop the values - we don't have a proper object system
                func.instruction(&Instruction::Drop);
                func.instruction(&Instruction::Drop);
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

                let iterator_ptr_idx = ctx.add_local("__iter_ptr", IRType::Unknown);
                let loop_counter_idx = ctx.add_local("__iter_idx", IRType::Int);
                let list_length_idx = ctx.add_local("__iter_len", IRType::Int);
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

                        // Check if current >= stop
                        func.instruction(&Instruction::LocalGet(target_idx));
                        func.instruction(&Instruction::LocalGet(list_length_idx));
                        func.instruction(&Instruction::I32GeS);
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
                let exception_flag_idx = ctx.add_local("__exception_flag", IRType::Int);
                let exception_type_idx = ctx.add_local("__exception_type", IRType::Int);

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

                // End of exception handling
                func.instruction(&Instruction::End);
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
                        // Dictionary assignment (linear search and update)
                        // For now, just store at a fixed offset after the entries
                        // TODO: Implement proper hash table or linear probe storage
                        func.instruction(&Instruction::LocalGet(ctx.temp_local)); // dict_ptr
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // key
                        func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // value

                        // Just drop the values for now - proper implementation would search/update
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
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
