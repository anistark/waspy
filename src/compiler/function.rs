use crate::compiler::context::CompilationContext;
use crate::compiler::expression::emit_expr;
use crate::ir::{IRBody, IRFunction, IRStatement};
use wasm_encoder::{BlockType, Function, Instruction, ValType};

/// Compile an IR function into a WebAssembly function
pub fn compile_function(ir_func: &IRFunction, _ctx: &mut CompilationContext) -> Function {
    let mut local_ctx = CompilationContext::new();

    // Track function parameters as locals
    for param in &ir_func.params {
        local_ctx.add_local(param);
    }

    // Scan for variable declarations to allocate locals
    scan_and_allocate_locals(&ir_func.body, &mut local_ctx);

    // Locals will be the function parameters and any local variables
    // For now, we'll use i32 for all locals
    let locals = vec![(
        local_ctx.local_count - ir_func.params.len() as u32,
        ValType::I32,
    )];

    let mut func = Function::new(locals);

    // Compile the function body
    compile_body(&ir_func.body, &mut func, &local_ctx);

    // If no explicit return, add a default return
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::End);

    func
}

/// Scan the function body for variable declarations and allocate local variables
pub fn scan_and_allocate_locals(body: &IRBody, ctx: &mut CompilationContext) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Assign { target, .. } => {
                if ctx.get_local(target).is_none() {
                    ctx.add_local(target);
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
pub fn compile_body(body: &IRBody, func: &mut Function, ctx: &CompilationContext) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Return(expr_opt) => {
                if let Some(expr) = expr_opt {
                    emit_expr(expr, func, ctx);
                } else {
                    func.instruction(&Instruction::I32Const(0));
                }
                func.instruction(&Instruction::Return);
            }
            IRStatement::Assign { target, value } => {
                emit_expr(value, func, ctx);
                if let Some(local_idx) = ctx.get_local(target) {
                    func.instruction(&Instruction::LocalSet(local_idx));
                } else {
                    // This should not happen if we properly allocated locals
                    panic!("Variable {} not found in context", target);
                }
            }
            IRStatement::If {
                condition,
                then_body,
                else_body,
            } => {
                emit_expr(condition, func, ctx);

                // If-else block with no result value
                func.instruction(&Instruction::If(BlockType::Empty));

                // Then branch
                compile_body(then_body, func, ctx);

                if let Some(else_body) = else_body {
                    // Else branch
                    func.instruction(&Instruction::Else);
                    compile_body(else_body, func, ctx);
                }

                func.instruction(&Instruction::End);
            }
            IRStatement::While { condition, body } => {
                // Loop block
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Condition check
                emit_expr(condition, func, ctx);
                func.instruction(&Instruction::I32Eqz);
                func.instruction(&Instruction::BrIf(1)); // Break out of the loop if condition is false

                // Loop body
                compile_body(body, func, ctx);

                // Jump back to the start of the loop
                func.instruction(&Instruction::Br(0));

                // End of loop and block
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);
            }
            IRStatement::Expression(expr) => {
                emit_expr(expr, func, ctx);
                // We don't need the result value, so we just let it fall off the stack
            }
        }
    }
}
