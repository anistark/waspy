use crate::compiler::context::CompilationContext;
use crate::compiler::expression::emit_expr;
use crate::ir::{IRBody, IRFunction, IRStatement, IRType, MemoryLayout};
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
            _ => {}
        }
    }
}

/// Compile a function body into WebAssembly instructions
pub fn compile_body(
    body: &IRBody,
    func: &mut Function,
    ctx: &CompilationContext,
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
        }
    }
}
