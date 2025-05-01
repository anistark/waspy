use crate::compiler::context::CompilationContext;
use crate::ir::{IRBoolOp, IRCompareOp, IRConstant, IRExpr, IROp, IRUnaryOp};
use wasm_encoder::{BlockType, Function, Instruction};

/// Emit WebAssembly instructions for an IR expression
pub fn emit_expr(expr: &IRExpr, func: &mut Function, ctx: &CompilationContext) {
    match expr {
        IRExpr::Const(constant) => {
            match constant {
                IRConstant::Int(i) => {
                    func.instruction(&Instruction::I32Const(*i));
                }
                IRConstant::Float(f) => {
                    // For simplicity, we'll convert float to int
                    let truncated = *f as i32;
                    func.instruction(&Instruction::I32Const(truncated));
                }
                IRConstant::Bool(b) => {
                    func.instruction(&Instruction::I32Const(if *b { 1 } else { 0 }));
                }
                IRConstant::String(_) => {
                    // String handling will be added later
                    func.instruction(&Instruction::I32Const(0));
                }
            }
        }
        IRExpr::Param(name) => {
            if let Some(local_idx) = ctx.get_local(name) {
                func.instruction(&Instruction::LocalGet(local_idx));
            } else {
                panic!("Parameter {} not found in context", name);
            }
        }
        IRExpr::Variable(name) => {
            if let Some(local_idx) = ctx.get_local(name) {
                func.instruction(&Instruction::LocalGet(local_idx));
            } else {
                panic!("Variable {} not found in context", name);
            }
        }
        IRExpr::BinaryOp { left, right, op } => {
            emit_expr(left, func, ctx);
            emit_expr(right, func, ctx);

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
                    // Modulo operation: a % b
                    func.instruction(&Instruction::I32RemS);
                }
                IROp::FloorDiv => {
                    // Floor division: a // b
                    // In WebAssembly, I32DivS already does floor division for signed integers
                    func.instruction(&Instruction::I32DivS);
                }
                IROp::Pow => emit_power_operation(func),
            }
        }
        IRExpr::UnaryOp { operand, op } => {
            emit_expr(operand, func, ctx);

            match op {
                IRUnaryOp::Neg => {
                    // Negate: -x
                    // WebAssembly doesn't have a direct negate instruction,
                    // but we can use 0 - x
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32Sub);
                }
                IRUnaryOp::Not => {
                    // Logical not: not x
                    // In WebAssembly, we can use the eqz instruction (which is true if x == 0)
                    // We first convert our value to either 0 or 1 (boolean), then flip it

                    // First ensure it's 0 or 1
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32Ne);

                    // Then invert it (1 becomes 0, 0 becomes 1)
                    func.instruction(&Instruction::I32Const(1));
                    func.instruction(&Instruction::I32Xor);
                }
            }
        }
        IRExpr::CompareOp { left, right, op } => {
            emit_expr(left, func, ctx);
            emit_expr(right, func, ctx);

            match op {
                IRCompareOp::Eq => {
                    func.instruction(&Instruction::I32Eq);
                }
                IRCompareOp::NotEq => {
                    func.instruction(&Instruction::I32Ne);
                }
                IRCompareOp::Lt => {
                    func.instruction(&Instruction::I32LtS);
                }
                IRCompareOp::LtE => {
                    func.instruction(&Instruction::I32LeS);
                }
                IRCompareOp::Gt => {
                    func.instruction(&Instruction::I32GtS);
                }
                IRCompareOp::GtE => {
                    func.instruction(&Instruction::I32GeS);
                }
            }
        }
        IRExpr::BoolOp { left, right, op } => {
            match op {
                IRBoolOp::And => {
                    // Simpler implementation of AND operation
                    emit_expr(left, func, ctx);
                    // Store in temporary local
                    func.instruction(&Instruction::LocalSet(0));
                    // Get it back
                    func.instruction(&Instruction::LocalGet(0));

                    // If-else pattern for short-circuit evaluation
                    func.instruction(&Instruction::If(BlockType::Empty));
                    emit_expr(right, func, ctx);
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::I32Const(0)); // False
                    func.instruction(&Instruction::End);
                }
                IRBoolOp::Or => {
                    // Simpler implementation of OR operation
                    emit_expr(left, func, ctx);
                    // Store in temporary local
                    func.instruction(&Instruction::LocalSet(0));
                    // Get it back
                    func.instruction(&Instruction::LocalGet(0));

                    // If-else pattern for short-circuit evaluation
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(1)); // True
                    func.instruction(&Instruction::Else);
                    emit_expr(right, func, ctx);
                    func.instruction(&Instruction::End);
                }
            }
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            // Push arguments onto the stack in order
            for arg in arguments {
                emit_expr(arg, func, ctx);
            }

            // Look up the function index if it exists in our context
            if let Some(func_idx) = ctx.function_types.get(function_name.as_str()) {
                func.instruction(&Instruction::Call(*func_idx));
            } else {
                // We don't need special handling for "int" since we handle it in the IR conversion

                // For unknown functions, we'll just return 0 for now
                func.instruction(&Instruction::I32Const(0));
            }
        }
    }
}

/// Emit WebAssembly instructions for the power operation (a ** b)
fn emit_power_operation(func: &mut Function) {
    // Power operation: a ** b
    // WebAssembly doesn't have a direct power instruction, so we implement it
    // via a loop for integer powers

    // Save the base value to a local (we'll use local 0 for simplicity)
    func.instruction(&Instruction::LocalSet(0)); // Save exponent
    func.instruction(&Instruction::LocalSet(1)); // Save base

    // Initialize result to 1
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(2)); // result = 1

    // Check if exponent is 0, if so return 1
    func.instruction(&Instruction::LocalGet(0)); // Get exponent
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Handle negative exponent as special case (return 0 for now)
    func.instruction(&Instruction::LocalGet(0)); // Get exponent
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Return);
    func.instruction(&Instruction::End);

    // Start loop to calculate power
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));

    // Check if exponent is 0, if so break out of loop
    func.instruction(&Instruction::LocalGet(0)); // Get exponent
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1)); // Break out of loop

    // result *= base
    func.instruction(&Instruction::LocalGet(2)); // Get result
    func.instruction(&Instruction::LocalGet(1)); // Get base
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(2)); // Update result

    // exponent--
    func.instruction(&Instruction::LocalGet(0)); // Get exponent
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(0)); // Update exponent

    // Loop back
    func.instruction(&Instruction::Br(0));

    // End loop
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Push result to stack
    func.instruction(&Instruction::LocalGet(2));
}
