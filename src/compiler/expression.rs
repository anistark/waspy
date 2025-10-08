use crate::compiler::context::CompilationContext;
use crate::ir::{IRBoolOp, IRCompareOp, IRConstant, IRExpr, IROp, IRType, IRUnaryOp, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction, MemArg};

// Helper to convert f64 to Ieee64
#[inline]
fn f64_const(value: f64) -> wasm_encoder::Ieee64 {
    value.into()
}

/// Emit WebAssembly instructions for an IR expression
pub fn emit_expr(
    expr: &IRExpr,
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    expected_type: Option<&IRType>,
) -> IRType {
    match expr {
        IRExpr::Const(constant) => {
            match constant {
                IRConstant::Int(i) => {
                    func.instruction(&Instruction::I32Const(*i));
                    IRType::Int
                }
                IRConstant::Float(f) => {
                    func.instruction(&Instruction::F64Const(f64_const(*f)));

                    // Cast to i32 if an integer is expected
                    if let Some(IRType::Int) = expected_type {
                        func.instruction(&Instruction::I32TruncF64S);
                        IRType::Int
                    } else {
                        IRType::Float
                    }
                }
                IRConstant::Bool(b) => {
                    func.instruction(&Instruction::I32Const(if *b { 1 } else { 0 }));
                    IRType::Bool
                }
                IRConstant::String(s) => {
                    // Get the string's offset in memory
                    let offset = memory_layout.string_offsets.get(s).unwrap_or(&0); // Default to offset 0 if not found

                    // Push the string's memory offset and length onto the stack
                    func.instruction(&Instruction::I32Const(*offset as i32));
                    func.instruction(&Instruction::I32Const(s.len() as i32));

                    IRType::String
                }
                IRConstant::None => {
                    // None is represented as i32 constant 0
                    func.instruction(&Instruction::I32Const(0));
                    IRType::None
                }
                IRConstant::List(_) => {
                    // Temporary implementation - return a default list
                    func.instruction(&Instruction::I32Const(0));
                    IRType::List(Box::new(IRType::Unknown))
                }
                IRConstant::Dict(_) => {
                    // Temporary implementation - return a default dict
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Dict(Box::new(IRType::Unknown), Box::new(IRType::Unknown))
                }
                IRConstant::Tuple(_) => {
                    // Temporary implementation - return a default value
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Tuple(vec![IRType::Unknown])
                }
            }
        }
        IRExpr::Param(name) | IRExpr::Variable(name) => {
            if let Some(local_info) = ctx.get_local_info(name) {
                func.instruction(&Instruction::LocalGet(local_info.index));
                local_info.var_type.clone()
            } else {
                // Default to i32 if type info is missing
                if let Some(local_idx) = ctx.get_local_index(name) {
                    func.instruction(&Instruction::LocalGet(local_idx));
                } else {
                    // Indicate an error or unknown variable
                    func.instruction(&Instruction::I32Const(-999));
                }
                IRType::Unknown
            }
        }
        IRExpr::BinaryOp { left, right, op } => {
            let left_type = emit_expr(left, func, ctx, memory_layout, None);
            let right_type = emit_expr(right, func, ctx, memory_layout, Some(&left_type));

            if left_type == IRType::Float && right_type == IRType::Int {
                // Convert right operand from i32 to f64
                func.instruction(&Instruction::F64ConvertI32S);
            } else if left_type == IRType::Int && right_type == IRType::Float {
                // Move stack: f64 under i32
                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                func.instruction(&Instruction::F64ConvertI32S);
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
            }

            let result_type = if left_type == IRType::Float || right_type == IRType::Float {
                match op {
                    IROp::Add => {
                        func.instruction(&Instruction::F64Add);
                    }
                    IROp::Sub => {
                        func.instruction(&Instruction::F64Sub);
                    }
                    IROp::Mul => {
                        func.instruction(&Instruction::F64Mul);
                    }
                    IROp::Div => {
                        func.instruction(&Instruction::F64Div);
                    }
                    IROp::Mod => {
                        emit_float_modulo_operation(func);
                    }
                    IROp::FloorDiv => {
                        func.instruction(&Instruction::F64Div);
                        func.instruction(&Instruction::F64Floor);
                    }
                    IROp::Pow => {
                        emit_float_power_operation(func);
                    }
                    // New operations - placeholder implementations
                    IROp::MatMul => {
                        // Matrix multiplication not supported yet for floats
                        func.instruction(&Instruction::F64Const(f64_const(0.0)));
                    }
                    IROp::LShift | IROp::RShift | IROp::BitOr | IROp::BitXor | IROp::BitAnd => {
                        // Bitwise operations not supported for floats
                        func.instruction(&Instruction::F64Const(f64_const(0.0)));
                    }
                }
                IRType::Float
            } else {
                // Integer operations
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
                    // New operations
                    IROp::MatMul => {
                        // Not implemented yet for integers
                        func.instruction(&Instruction::I32Const(0));
                    }
                    IROp::LShift => {
                        func.instruction(&Instruction::I32Shl);
                    }
                    IROp::RShift => {
                        func.instruction(&Instruction::I32ShrS);
                    }
                    IROp::BitOr => {
                        func.instruction(&Instruction::I32Or);
                    }
                    IROp::BitXor => {
                        func.instruction(&Instruction::I32Xor);
                    }
                    IROp::BitAnd => {
                        func.instruction(&Instruction::I32And);
                    }
                }
                IRType::Int
            };

            // Cast the result to expected type if needed
            if let Some(expected) = expected_type {
                if *expected == IRType::Int && result_type == IRType::Float {
                    func.instruction(&Instruction::I32TruncF64S);
                    return IRType::Int;
                } else if *expected == IRType::Float && result_type == IRType::Int {
                    func.instruction(&Instruction::F64ConvertI32S);
                    return IRType::Float;
                }
            }

            result_type
        }
        IRExpr::UnaryOp { operand, op } => {
            let operand_type = emit_expr(operand, func, ctx, memory_layout, None);

            match operand_type {
                IRType::Float => {
                    match op {
                        IRUnaryOp::Neg => {
                            // Negate float: -x
                            func.instruction(&Instruction::F64Const(f64_const(-1.0)));
                            func.instruction(&Instruction::F64Mul);
                        }
                        IRUnaryOp::Not => {
                            // Logical not for float: convert to bool first
                            func.instruction(&Instruction::F64Const(f64_const(0.0)));
                            func.instruction(&Instruction::F64Eq);
                            // Invert (1->0, 0->1)
                            func.instruction(&Instruction::I32Const(1));
                            func.instruction(&Instruction::I32Xor);
                        }
                        IRUnaryOp::Invert => {
                            // Not meaningful for floats
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::F64Const(f64_const(0.0)));
                        }
                        IRUnaryOp::UAdd => {
                            // No-op for floats
                        }
                    }
                    if matches!(op, IRUnaryOp::Not) {
                        IRType::Bool
                    } else {
                        IRType::Float
                    }
                }
                _ => {
                    // Integer/Boolean operations
                    match op {
                        IRUnaryOp::Neg => {
                            // Negate: -x = 0 - x
                            func.instruction(&Instruction::I32Const(0));
                            func.instruction(&Instruction::I32Sub);
                            IRType::Int
                        }
                        IRUnaryOp::Not => {
                            // Logical not: ensure it's 0 or 1 first
                            func.instruction(&Instruction::I32Const(0));
                            func.instruction(&Instruction::I32Ne);
                            // Then invert (1->0, 0->1)
                            func.instruction(&Instruction::I32Const(1));
                            func.instruction(&Instruction::I32Xor);
                            IRType::Bool
                        }
                        IRUnaryOp::Invert => {
                            // Bitwise NOT: ~x
                            func.instruction(&Instruction::I32Const(-1));
                            func.instruction(&Instruction::I32Xor);
                            IRType::Int
                        }
                        IRUnaryOp::UAdd => {
                            // No operation needed for unary +
                            IRType::Int
                        }
                    }
                }
            }
        }
        IRExpr::CompareOp { left, right, op } => {
            let left_type = emit_expr(left, func, ctx, memory_layout, None);
            let right_type = emit_expr(right, func, ctx, memory_layout, Some(&left_type));

            // Handle type coercion for comparison
            if left_type == IRType::Float && right_type == IRType::Int {
                func.instruction(&Instruction::F64ConvertI32S);
            } else if left_type == IRType::Int && right_type == IRType::Float {
                // Move stack: f64 under i32
                func.instruction(&Instruction::LocalSet(ctx.temp_local));
                func.instruction(&Instruction::F64ConvertI32S);
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
            }

            if left_type == IRType::Float || right_type == IRType::Float {
                // Float comparisons
                match op {
                    IRCompareOp::Eq => {
                        func.instruction(&Instruction::F64Eq);
                    }
                    IRCompareOp::NotEq => {
                        func.instruction(&Instruction::F64Eq);
                        func.instruction(&Instruction::I32Const(1));
                        func.instruction(&Instruction::I32Xor); // Invert
                    }
                    IRCompareOp::Lt => {
                        func.instruction(&Instruction::F64Lt);
                    }
                    IRCompareOp::LtE => {
                        func.instruction(&Instruction::F64Le);
                    }
                    IRCompareOp::Gt => {
                        func.instruction(&Instruction::F64Gt);
                    }
                    IRCompareOp::GtE => {
                        func.instruction(&Instruction::F64Ge);
                    }
                    // New operations
                    IRCompareOp::In | IRCompareOp::NotIn | IRCompareOp::Is | IRCompareOp::IsNot => {
                        // These comparisons aren't directly supported for floats in WebAssembly
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
                    }
                }
            } else {
                // Integer comparisons
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
                    // New operations
                    IRCompareOp::In | IRCompareOp::NotIn | IRCompareOp::Is | IRCompareOp::IsNot => {
                        // These operations aren't directly supported in WebAssembly
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::Drop);
                        func.instruction(&Instruction::I32Const(0));
                    }
                }
            }

            IRType::Bool
        }
        IRExpr::BoolOp { left, right, op } => {
            match op {
                IRBoolOp::And => {
                    // Short-circuit AND operation
                    emit_expr(left, func, ctx, memory_layout, Some(&IRType::Bool));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));

                    // If-else pattern for short-circuit evaluation
                    func.instruction(&Instruction::If(BlockType::Empty));
                    emit_expr(right, func, ctx, memory_layout, Some(&IRType::Bool));
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::I32Const(0)); // False
                    func.instruction(&Instruction::End);
                }
                IRBoolOp::Or => {
                    // Short-circuit OR operation
                    emit_expr(left, func, ctx, memory_layout, Some(&IRType::Bool));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));

                    // If-else pattern for short-circuit evaluation
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::I32Const(1)); // True
                    func.instruction(&Instruction::Else);
                    emit_expr(right, func, ctx, memory_layout, Some(&IRType::Bool));
                    func.instruction(&Instruction::End);
                }
            }

            IRType::Bool
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            // Push arguments onto the stack in order
            let mut arg_types = Vec::new();
            for arg in arguments {
                let arg_type = emit_expr(arg, func, ctx, memory_layout, None);
                arg_types.push(arg_type);
            }

            // Look up the function index if it exists in our context
            if let Some(func_info) = ctx.get_function_info(function_name.as_str()) {
                func.instruction(&Instruction::Call(func_info.index));
                func_info.return_type.clone()
            } else {
                // Built-in functions
                match function_name.as_str() {
                    "len" => {
                        // For strings: return length
                        // TODO: For lists/dicts revisit this
                        IRType::Int
                    }
                    "print" => {
                        // Pop the arguments off the stack
                        for _ in 0..arguments.len() {
                            func.instruction(&Instruction::Drop);
                        }
                        IRType::None
                    }
                    _ => {
                        // Unknown function, return default value
                        func.instruction(&Instruction::I32Const(0));
                        IRType::Unknown
                    }
                }
            }
        }
        IRExpr::ListLiteral(elements) => {
            // Creating a list:
            // 1. Allocate memory for the list (length + elements)
            // 2. Store the length at the start
            // 3. Store each element after the length

            // Store the length
            func.instruction(&Instruction::I32Const(elements.len() as i32));

            // Return a pointer to the list structure
            // TODO: Revisit memory management
            IRType::List(Box::new(IRType::Unknown))
        }
        IRExpr::DictLiteral(pairs) => {
            // Store the number of pairs of key-value
            func.instruction(&Instruction::I32Const(pairs.len() as i32));

            IRType::Dict(Box::new(IRType::Unknown), Box::new(IRType::Unknown))
        }
        IRExpr::Indexing { container, index } => {
            let container_type = emit_expr(container, func, ctx, memory_layout, None);
            // TODO: Handle type for index for non-integer types
            let _index_type = emit_expr(index, func, ctx, memory_layout, Some(&IRType::Int));

            match container_type {
                IRType::String => {
                    // Access the byte at string_ptr + index
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::I32Load8U(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));

                    IRType::Int
                }
                IRType::List(_) => {
                    // List indexing

                    IRType::Unknown
                }
                _ => {
                    // Unknown container types
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
            }
        }
        IRExpr::Attribute {
            object,
            attribute: _,
        } => {
            // TODO: Object support
            // For now, return 0
            emit_expr(object, func, ctx, memory_layout, None);
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::I32Const(0));

            IRType::Unknown
        }
        IRExpr::ListComp {
            expr,
            var_name: _,
            iterable: _,
        } => {
            // Temporary implementation for list comprehension
            // For now, just evaluate the expression once and wrap it in a list
            emit_expr(expr, func, ctx, memory_layout, None);
            func.instruction(&Instruction::I32Const(1)); // Length 1 list
            IRType::List(Box::new(IRType::Unknown))
        }
        IRExpr::MethodCall {
            object,
            method_name: _,
            arguments: _,
        } => {
            // Temporary implementation - just evaluate the object and return null
            emit_expr(object, func, ctx, memory_layout, None);
            func.instruction(&Instruction::Drop);
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
        IRExpr::DynamicImportExpr { module_name } => {
            // Emit code to evaluate the module name
            emit_expr(module_name, func, ctx, memory_layout, None);

            // TODO: dynamic imports requires more extensive runtime support
            func.instruction(&Instruction::Drop); // Drop the module name
            func.instruction(&Instruction::I32Const(0)); // Return a dummy value

            IRType::Unknown
        }
    }
}

/// Emit WebAssembly instructions for the integer power operation (a ** b)
pub fn emit_integer_power_operation(func: &mut Function) {
    // Power operation: a ** b

    // Save the base value to a local
    func.instruction(&Instruction::LocalSet(0));
    func.instruction(&Instruction::LocalSet(1));

    // Initialize result to 1
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::LocalSet(2));

    // Check if exponent is 0, if so return 1
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // Handle negative exponent as special case (return 0 for now)
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::I32LtS);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::I32Const(0));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // Start loop to calculate power
    func.instruction(&Instruction::Block(BlockType::Empty));
    func.instruction(&Instruction::Loop(BlockType::Empty));

    // Check if exponent is 0, if so break out of loop
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Eqz);
    func.instruction(&Instruction::BrIf(1));

    // result *= base
    func.instruction(&Instruction::LocalGet(2));
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::I32Mul);
    func.instruction(&Instruction::LocalSet(2));

    // exponent--
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::I32Const(1));
    func.instruction(&Instruction::I32Sub);
    func.instruction(&Instruction::LocalSet(0));

    // Loop back
    func.instruction(&Instruction::Br(0));

    // End loop
    func.instruction(&Instruction::End);
    func.instruction(&Instruction::End);

    // Push result to stack
    func.instruction(&Instruction::LocalGet(2));
}

/// Emit WebAssembly instructions for the float power operation (a ** b)
pub fn emit_float_power_operation(func: &mut Function) {
    // Float power operation
    // TODO: Improve using approximation or call to external function

    func.instruction(&Instruction::LocalSet(0));

    // Handle special case: base ** 0 = 1
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Const(f64_const(0.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Drop);
    func.instruction(&Instruction::F64Const(f64_const(1.0)));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // Handle special case: base ** 1 = base
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Const(f64_const(1.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // Handle special case: base ** 2 = base * base
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Const(f64_const(2.0)));
    func.instruction(&Instruction::F64Eq);
    func.instruction(&Instruction::If(BlockType::Empty));
    func.instruction(&Instruction::LocalTee(1));
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::F64Mul);
    func.instruction(&Instruction::Br(1));
    func.instruction(&Instruction::End);

    // For all other exponents, return 0 for now
    // TODO: Implement a proper power function
    func.instruction(&Instruction::Drop);
    func.instruction(&Instruction::F64Const(f64_const(0.0)));
}

/// Emit WebAssembly instructions for float modulo operation (a % b)
pub fn emit_float_modulo_operation(func: &mut Function) {
    // Float modulo: a % b = a - b * floor(a / b)

    // Stack starts with: a b

    // Save b to local 0
    func.instruction(&Instruction::LocalSet(0));

    // Save a to local 1
    func.instruction(&Instruction::LocalSet(1));

    // Compute floor(a / b)
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Div);
    func.instruction(&Instruction::F64Floor);

    // Compute b * floor(a / b)
    func.instruction(&Instruction::LocalGet(0));
    func.instruction(&Instruction::F64Mul);

    // Compute a - b * floor(a / b)
    func.instruction(&Instruction::LocalGet(1));
    func.instruction(&Instruction::F64Sub);
}
