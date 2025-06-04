use crate::compiler::context::CompilationContext;
use crate::compiler::expression::{emit_expr, emit_integer_power_operation};
use crate::ir::{IRBody, IRFunction, IROp, IRStatement, IRType, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction, ValType};

/// Compile an IR function into a WebAssembly function
pub fn compile_function(
    ir_func: &IRFunction,
    ctx: &mut CompilationContext,
    memory_layout: &MemoryLayout,
) -> Function {
    // Track function parameters as locals
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
            func.instruction(&Instruction::F64Const(0.0));
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
                    panic!("Variable {} not found in context", target);
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
                attribute: _, // Ignore the attribute field for now
                value,
            } => {
                // Emit code for the object (get reference)
                emit_expr(object, func, ctx, memory_layout, None);

                // Store the object reference temporarily
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit code for the value
                emit_expr(value, func, ctx, memory_layout, None);

                // Store the value temporarily
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

                // Load the object reference
                func.instruction(&Instruction::LocalGet(ctx.temp_local));

                // Load the value
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));

                // In a real implementation, you would need to:
                // 1. Get the offset of the attribute in the object
                // 2. Store the value at that offset
                // For now, we'll just emit a fake store operation

                // This is a placeholder for actual attribute assignment
                // TODO: Implement proper object attribute assignment
                func.instruction(&Instruction::Drop); // Drop the value
                func.instruction(&Instruction::Drop); // Drop the object reference
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
                    panic!("Variable {} not found in context", target);
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
                // This is a simplified implementation that doesn't fully implement Python's for loop
                // We'll treat it similar to a while loop but we need to initialize the iterator

                // Evaluate the iterable and get its "length" (simplified)
                emit_expr(iterable, func, ctx, memory_layout, None);

                // Save the iterable (simplified as just an integer count)
                let local_idx = ctx
                    .get_local_index(target)
                    .expect("Target variable not found");
                func.instruction(&Instruction::LocalSet(local_idx));

                // Start a loop block
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Check if we've reached the end (simplified)
                func.instruction(&Instruction::LocalGet(local_idx));
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::I32LeS); // Break if counter <= 0
                func.instruction(&Instruction::BrIf(1));

                // Execute the loop body
                compile_body(body, func, ctx, memory_layout);

                // Decrement the counter (simplified)
                func.instruction(&Instruction::LocalGet(local_idx));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Sub);
                func.instruction(&Instruction::LocalSet(local_idx));

                // Loop back
                func.instruction(&Instruction::Br(0));

                // End of loop
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);
            }

            IRStatement::TryExcept {
                try_body,
                except_handlers: _,
                finally_body,
            } => {
                // WebAssembly doesn't directly support exceptions yet
                // So this is a simplified implementation that just executes the try block
                // and ignores the exception handling

                // Execute the try block
                compile_body(try_body, func, ctx, memory_layout);

                // If there's a finally block, always execute it
                if let Some(finally_body) = finally_body {
                    compile_body(finally_body, func, ctx, memory_layout);
                }
            }

            IRStatement::With {
                context_expr,
                optional_vars: _,
                body,
            } => {
                // Similar to try-except, WebAssembly doesn't have direct support for context managers
                // We'll evaluate the context expression (which might have side effects)
                // and then just execute the body

                emit_expr(context_expr, func, ctx, memory_layout, None);
                func.instruction(&Instruction::Drop); // Discard the context manager result

                // Execute the body
                compile_body(body, func, ctx, memory_layout);
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
        }
    }
}
