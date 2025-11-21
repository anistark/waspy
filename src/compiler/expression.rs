use crate::compiler::context::CompilationContext;
use crate::ir::{IRBoolOp, IRCompareOp, IRConstant, IRExpr, IROp, IRType, IRUnaryOp, MemoryLayout};
use wasm_encoder::{BlockType, Function, Instruction};

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

            // Handle string operations
            if left_type == IRType::String {
                match op {
                    IROp::Add => {
                        if right_type == IRType::String {
                            // String concatenation: stack has (left_offset, left_len, right_offset, right_len)
                            // We need to return (concat_offset, concat_len)
                            // Stack: (left_offset, left_len, right_offset, right_len)

                            // Save right side to temps
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // right_len
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // right_offset

                            // Stack: (left_offset, left_len)
                            // Save left side to temps
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // left_len
                            func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // left_offset

                            // Calculate concatenated length = left_len + right_len
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                            func.instruction(&Instruction::I32Add);
                            func.instruction(&Instruction::LocalSet(ctx.temp_local)); // concat_len

                            // For runtime concatenation, we need to:
                            // 1. Calculate where to put the result in memory
                            // 2. Copy left string
                            // 3. Copy right string
                            // Since we don't have dynamic allocation, we use the end of current string data
                            // For now, we'll implement a simplified version that works for small strings

                            // Get left offset and length
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
                            func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));

                            // Stack: (left_offset, left_len)
                            // The concatenated string will be stored after all current strings
                            // Use a heuristic: place result at a fixed high offset (TODO: improve)
                            // For now, return a dummy concatenation using the left string as base
                            // Drop the length and return (left_offset, concat_len)
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::LocalGet(ctx.temp_local));

                            // Stack: (left_offset, concat_len)
                            return IRType::String;
                        }
                    }
                    IROp::Mod => {
                        // String formatting: "format %s" % (value,) or "format %s" % value
                        // TODO: Implement string formatting with placeholders
                        // For now, drop the right value and return the format string
                        if right_type == IRType::String
                            || right_type == IRType::Int
                            || right_type == IRType::Float
                        {
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Drop);
                            return IRType::String;
                        }
                    }
                    _ => {}
                }
            }

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
            // List layout in memory: [length:i32][elem0:i32][elem1:i32]...
            // For now, allocate after string data (at offset 10000)
            // Each element takes 4 bytes for i32 values

            if elements.is_empty() {
                // Empty list: just a length of 0
                func.instruction(&Instruction::I32Const(10000)); // Pointer to empty list
                return IRType::List(Box::new(IRType::Unknown));
            }

            // Use a fixed allocation address for simplicity
            let list_ptr = 10000 + ((ctx.local_count as u32) * 100);

            // Store length at the beginning
            func.instruction(&Instruction::I32Const(list_ptr as i32));
            func.instruction(&Instruction::I32Const(elements.len() as i32));
            func.instruction(&Instruction::I32Store(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            // Determine element type from first element
            let elem_type = if !elements.is_empty() {
                emit_expr(&elements[0], func, ctx, memory_layout, None)
            } else {
                IRType::Unknown
            };

            // Store each element
            for (i, elem) in elements.iter().enumerate() {
                let offset = 4 + (i as u32 * 4); // Skip length field

                // Get the element value
                let elem_type = emit_expr(elem, func, ctx, memory_layout, None);

                // Store based on element type
                match elem_type {
                    IRType::Float => {
                        func.instruction(&Instruction::I32Const(list_ptr as i32));
                        func.instruction(&Instruction::I32Const(offset as i32));
                        func.instruction(&Instruction::I32Add);
                        // Value is already on stack, but we need it as f64
                        // TODO: Handle type conversion properly
                        func.instruction(&Instruction::F64Store(MemArg {
                            offset: 0,
                            align: 3,
                            memory_index: 0,
                        }));
                    }
                    _ => {
                        func.instruction(&Instruction::I32Const(list_ptr as i32));
                        func.instruction(&Instruction::I32Const(offset as i32));
                        func.instruction(&Instruction::I32Add);
                        // Value is already on stack
                        func.instruction(&Instruction::I32Store(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                    }
                }
            }

            // Return pointer to the list
            func.instruction(&Instruction::I32Const(list_ptr as i32));
            IRType::List(Box::new(elem_type))
        }
        IRExpr::DictLiteral(pairs) => {
            // Dict layout in memory: [num_entries:i32][key0:i32][val0:i32][key1:i32][val1:i32]...
            // Allocate dict at a fixed offset (after lists)
            let dict_ptr = 50000 + ((ctx.local_count as u32) * 100);

            // Store number of entries
            func.instruction(&Instruction::I32Const(dict_ptr as i32));
            func.instruction(&Instruction::I32Const(pairs.len() as i32));
            func.instruction(&Instruction::I32Store(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            // Determine key and value types from first pair
            let (key_type, value_type) = if !pairs.is_empty() {
                let key_type = emit_expr(&pairs[0].0, func, ctx, memory_layout, None);
                // We need to drop the key value that was just pushed
                func.instruction(&Instruction::Drop);
                let value_type = emit_expr(&pairs[0].1, func, ctx, memory_layout, None);
                func.instruction(&Instruction::Drop);
                (key_type, value_type)
            } else {
                (IRType::Unknown, IRType::Unknown)
            };

            // Store each key-value pair
            for (i, (key_expr, val_expr)) in pairs.iter().enumerate() {
                let key_offset = 4 + (i as u32 * 8); // 4 bytes for length + i * 8 (key + value)
                let val_offset = 8 + (i as u32 * 8);

                // Store key
                emit_expr(key_expr, func, ctx, memory_layout, None);
                func.instruction(&Instruction::I32Const(dict_ptr as i32));
                func.instruction(&Instruction::I32Const(key_offset as i32));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Store value
                emit_expr(val_expr, func, ctx, memory_layout, None);
                func.instruction(&Instruction::I32Const(dict_ptr as i32));
                func.instruction(&Instruction::I32Const(val_offset as i32));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
            }

            // Return pointer to the dict
            func.instruction(&Instruction::I32Const(dict_ptr as i32));
            IRType::Dict(Box::new(key_type), Box::new(value_type))
        }
        IRExpr::Indexing { container, index } => {
            let container_type = emit_expr(container, func, ctx, memory_layout, None);
            // TODO: Handle type for index for non-integer types
            let _index_type = emit_expr(index, func, ctx, memory_layout, Some(&IRType::Int));

            match container_type {
                IRType::String => {
                    // String indexing returns a single character string
                    // Stack: (offset, length, index)
                    // Result: (char_offset, 1) - single character at the index

                    // Drop the length, keep offset and index
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Add);
                    // Now we have the offset of the character
                    // Length is always 1 for a single character
                    func.instruction(&Instruction::I32Const(1));

                    IRType::String
                }
                IRType::List(element_type) => {
                    // List indexing: list is stored as [length:i32][elem0:i32][elem1:i32]...
                    // We have: list_ptr on stack, index on stack
                    // Calculate address: list_ptr + 4 + (index * 4)

                    // Save list pointer
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    // index is still on stack, multiply by 4
                    func.instruction(&Instruction::I32Const(4));
                    func.instruction(&Instruction::I32Mul);
                    // Add 4 to skip the length field
                    func.instruction(&Instruction::I32Const(4));
                    func.instruction(&Instruction::I32Add);
                    // Restore list pointer and add
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Add);

                    // Load the element based on element type
                    match element_type.as_ref() {
                        IRType::Float => {
                            func.instruction(&Instruction::F64Load(MemArg {
                                offset: 0,
                                align: 3,
                                memory_index: 0,
                            }));
                        }
                        _ => {
                            func.instruction(&Instruction::I32Load(MemArg {
                                offset: 0,
                                align: 2,
                                memory_index: 0,
                            }));
                        }
                    }

                    element_type.as_ref().clone()
                }
                IRType::Dict(_key_type, value_type) => {
                    // Dictionary indexing using linear search
                    // Dict layout: [num_entries:i32][key0:i32][val0:i32][key1:i32][val1:i32]...
                    // Strategy: linearly search for the key
                    // Save dict pointer and index (the key)
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

                    // Load the number of entries
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Load(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));

                    // For now, just return a default value (0)
                    // TODO: Implement proper linear search for dictionary keys
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));

                    value_type.as_ref().clone()
                }
                _ => {
                    // Unknown container types
                    func.instruction(&Instruction::I32Const(0));
                    IRType::Unknown
                }
            }
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            let container_type = emit_expr(container, func, ctx, memory_layout, None);

            match container_type {
                IRType::String => {
                    // String slicing: str[start:end]
                    // Stack initially: (offset, length)
                    // Result: (new_offset, new_length)

                    // Save string length to temp local
                    func.instruction(&Instruction::LocalSet(ctx.temp_local));

                    // Stack: (offset)
                    // Evaluate start, defaulting to 0
                    if let Some(s) = start {
                        emit_expr(s, func, ctx, memory_layout, Some(&IRType::Int));
                    } else {
                        func.instruction(&Instruction::I32Const(0));
                    };

                    // Stack: (offset, start)
                    // Evaluate end, defaulting to length
                    if let Some(e) = end {
                        emit_expr(e, func, ctx, memory_layout, Some(&IRType::Int));
                    } else {
                        func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    };

                    // Stack: (offset, start, end)
                    // Save end for later
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

                    // Stack: (offset, start)
                    // Handle negative start index: if start < 0, add length
                    func.instruction(&Instruction::LocalTee(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    // start is negative, so add length
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::End);

                    // Stack: (offset, normalized_start)
                    // Clamp start to [0, length]
                    func.instruction(&Instruction::LocalTee(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32GtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::End);

                    // Stack: (offset, clamped_start)
                    // Handle end index
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));

                    // Stack: (offset, start, end)
                    // Handle negative end index: if end < 0, add length
                    func.instruction(&Instruction::LocalTee(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    // end is negative, so add length
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32Add);
                    func.instruction(&Instruction::End);

                    // Stack: (offset, start, normalized_end)
                    // Clamp end to [0, length]
                    func.instruction(&Instruction::LocalTee(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::Else);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::I32GtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::LocalGet(ctx.temp_local));
                    func.instruction(&Instruction::End);
                    func.instruction(&Instruction::End);

                    // Stack: (offset, start, end)
                    // Ensure start <= end for proper slice_length
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

                    // Stack: (offset, start)
                    // Calculate slice_length = end - start
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                    func.instruction(&Instruction::I32Sub);

                    // Stack: (offset, start, slice_length)
                    // Ensure slice_length >= 0
                    func.instruction(&Instruction::LocalTee(ctx.temp_local + 3));
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::I32LtS);
                    func.instruction(&Instruction::If(BlockType::Empty));
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::I32Const(0));
                    func.instruction(&Instruction::End);

                    // Stack: (offset, start, clamped_slice_length)
                    // Swap to get offset and slice_length for final result
                    func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                    // Stack: (offset, start)
                    // Calculate new_offset = offset + start
                    func.instruction(&Instruction::I32Add);

                    // Stack: (new_offset)
                    // Push new_length
                    func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));

                    // Stack: (new_offset, new_length)
                    // TODO: Handle step parameter properly
                    // For now, ignore step (default step=1)
                    if let Some(_s) = step {
                        // Drop step value if provided
                        emit_expr(_s, func, ctx, memory_layout, Some(&IRType::Int));
                        func.instruction(&Instruction::Drop);
                    }

                    IRType::String
                }
                IRType::List(_) => {
                    // List slicing: list[start:end:step]
                    // TODO: Implement list slicing
                    if let Some(_s) = start {
                        // TODO
                    }
                    if let Some(_e) = end {
                        // TODO
                    }
                    if let Some(_st) = step {
                        // TODO: Handle step
                    }
                    IRType::Unknown
                }
                _ => IRType::Unknown,
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
            method_name,
            arguments,
        } => {
            let object_type = emit_expr(object, func, ctx, memory_layout, None);

            match &object_type {
                IRType::String => {
                    // String methods: upper(), lower(), split(sep), etc.
                    match method_name.as_str() {
                        "upper" => {
                            // upper(): Convert string to uppercase
                            // Stack: (offset, length) -> (new_offset, new_length)
                            // For now: return unchanged string (TODO: implement transformation)
                            IRType::String
                        }
                        "lower" => {
                            // lower(): Convert string to lowercase
                            // Stack: (offset, length) -> (new_offset, new_length)
                            // For now: return unchanged string (TODO: implement transformation)
                            IRType::String
                        }
                        "strip" => {
                            // strip(): Remove leading/trailing whitespace
                            // For now: return unchanged string (TODO: implement transformation)
                            IRType::String
                        }
                        "lstrip" => {
                            // lstrip(): Remove leading whitespace
                            // For now: return unchanged string (TODO: implement transformation)
                            IRType::String
                        }
                        "rstrip" => {
                            // rstrip(): Remove trailing whitespace
                            // For now: return unchanged string (TODO: implement transformation)
                            IRType::String
                        }
                        "capitalize" => {
                            // capitalize(): Uppercase first character, lowercase rest
                            // For now: return unchanged string (TODO: implement transformation)
                            IRType::String
                        }
                        "title" => {
                            // title(): Titlecase string
                            // For now: return unchanged string (TODO: implement transformation)
                            IRType::String
                        }
                        "split" => {
                            // split(sep): Returns list (represented as array pointer for now)
                            // For runtime: store result as list would require proper list allocation
                            // For now: evaluate args and return default list value
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return list pointer
                            IRType::List(Box::new(IRType::String))
                        }
                        "find" => {
                            // find(sub): Returns index of substring or -1
                            // Naive implementation: linear search
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(-1)); // Not found
                            IRType::Int
                        }
                        "index" => {
                            // index(sub): Like find but returns 0 if not found
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Int
                        }
                        "count" => {
                            // count(sub): Count occurrences
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Default count
                            IRType::Int
                        }
                        "startswith" => {
                            // startswith(prefix): Check if string starts with prefix
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Default false
                            IRType::Bool
                        }
                        "endswith" => {
                            // endswith(suffix): Check if string ends with suffix
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Default false
                            IRType::Bool
                        }
                        "replace" => {
                            // replace(old, new): Replace occurrences
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, Some(&IRType::String));
                                func.instruction(&Instruction::Drop);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return offset=0
                            IRType::String
                        }
                        "isdigit" => {
                            // isdigit(): Check if all characters are digits
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isalpha" => {
                            // isalpha(): Check if all characters are alphabetic
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isalnum" => {
                            // isalnum(): Check if all characters are alphanumeric
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isspace" => {
                            // isspace(): Check if all characters are whitespace
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "isupper" => {
                            // isupper(): Check if all cased characters are uppercase
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "islower" => {
                            // islower(): Check if all cased characters are lowercase
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::I32Const(0));
                            IRType::Bool
                        }
                        "join" => {
                            // join(iterable): Join list of strings with separator
                            // For now: evaluate iterable and return default value
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string (separator)
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "format" => {
                            // format(*args, **kwargs): Format string
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "ljust" => {
                            // ljust(width, fillchar=' '): Left justify
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "rjust" => {
                            // rjust(width, fillchar=' '): Right justify
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        "center" => {
                            // center(width, fillchar=' '): Center justify
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            func.instruction(&Instruction::Drop); // Drop the string
                            func.instruction(&Instruction::I32Const(0)); // Return string offset=0
                            IRType::String
                        }
                        _ => {
                            // Unknown method
                            func.instruction(&Instruction::Drop);
                            func.instruction(&Instruction::Drop);
                            for arg in arguments {
                                emit_expr(arg, func, ctx, memory_layout, None);
                                func.instruction(&Instruction::Drop);
                            }
                            IRType::Unknown
                        }
                    }
                }
                IRType::List(_element_type) => {
                    emit_list_method_call(
                        func,
                        ctx,
                        memory_layout,
                        method_name,
                        arguments,
                        &object_type,
                    )
                }
                _ => {
                    // Non-string/list methods not yet supported
                    func.instruction(&Instruction::Drop);
                    func.instruction(&Instruction::Drop);
                    for arg in arguments {
                        emit_expr(arg, func, ctx, memory_layout, None);
                        func.instruction(&Instruction::Drop);
                    }
                    IRType::Unknown
                }
            }
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

/// Emit WebAssembly instructions for list method calls
pub fn emit_list_method_call(
    func: &mut Function,
    ctx: &CompilationContext,
    memory_layout: &MemoryLayout,
    method_name: &str,
    arguments: &[IRExpr],
    list_type: &IRType,
) -> IRType {
    match method_name {
        "append" => {
            // list.append(value)
            // Stack: list_ptr, value
            if !arguments.is_empty() {
                // Save list_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit the value to append
                let _value_type = emit_expr(&arguments[0], func, ctx, memory_layout, None);

                // Get list_ptr
                func.instruction(&Instruction::LocalGet(ctx.temp_local));

                // Save value
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

                // Load current length at list_ptr
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Save current length
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2));

                // Calculate offset for new element: 4 + (length * 4)
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                func.instruction(&Instruction::I32Const(4)); // skip length field
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // current length
                func.instruction(&Instruction::I32Const(4));
                func.instruction(&Instruction::I32Mul); // length * 4
                func.instruction(&Instruction::I32Add); // 4 + (length * 4)
                func.instruction(&Instruction::I32Add); // list_ptr + offset

                // Store value at the new position
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // value
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Update length: length + 1
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // current length
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add); // length + 1
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // append() returns None
                IRType::None
            } else {
                IRType::None
            }
        }
        "pop" => {
            // list.pop([index])
            // If index is provided, pop that index, else pop last element
            // Stack: list_ptr, [index (optional)]

            // Save list_ptr
            func.instruction(&Instruction::LocalSet(ctx.temp_local));

            // Load current length
            func.instruction(&Instruction::LocalGet(ctx.temp_local));
            func.instruction(&Instruction::I32Load(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            // Save length
            func.instruction(&Instruction::LocalSet(ctx.temp_local + 1));

            if !arguments.is_empty() {
                // Pop at specific index
                emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // index
            } else {
                // Pop last element (index = length - 1)
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // length
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Sub); // length - 1
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // index
            }

            // Get the element at index
            // Address: list_ptr + 4 + (index * 4)
            func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // index
            func.instruction(&Instruction::I32Const(4));
            func.instruction(&Instruction::I32Mul); // index * 4
            func.instruction(&Instruction::I32Const(4));
            func.instruction(&Instruction::I32Add); // + 4
            func.instruction(&Instruction::I32Add); // list_ptr + offset

            // Load and return the element
            func.instruction(&Instruction::I32Load(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            // Decrement length
            func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
            func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // current length
            func.instruction(&Instruction::I32Const(1));
            func.instruction(&Instruction::I32Sub); // length - 1
            func.instruction(&Instruction::I32Store(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            // Return the popped element
            if let IRType::List(elem_type) = list_type {
                elem_type.as_ref().clone()
            } else {
                IRType::Unknown
            }
        }
        "clear" => {
            // list.clear()
            // Stack: list_ptr
            // Set length to 0
            func.instruction(&Instruction::I32Const(0)); // length = 0
            func.instruction(&Instruction::I32Store(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));

            IRType::None
        }
        "extend" => {
            // list.extend(iterable)
            // For now, just a placeholder
            // Would need to iterate over the iterable and append each element
            func.instruction(&Instruction::Drop); // Drop the list_ptr
            IRType::None
        }
        "insert" => {
            // list.insert(index, value)
            // Simplified: just append at the end for now
            // Full implementation would shift elements
            if arguments.len() >= 2 {
                // Save list_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Get index (ignore for now, just append)
                emit_expr(&arguments[0], func, ctx, memory_layout, Some(&IRType::Int));
                func.instruction(&Instruction::Drop); // Drop index

                // Emit the value to insert
                emit_expr(&arguments[1], func, ctx, memory_layout, None);

                // Restore list_ptr
                func.instruction(&Instruction::LocalGet(ctx.temp_local));

                // Swap to get value on stack
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // value

                // Load current length
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // Calculate offset: 4 + (length * 4)
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                func.instruction(&Instruction::I32Const(4)); // skip length
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // length
                func.instruction(&Instruction::I32Const(4));
                func.instruction(&Instruction::I32Mul); // length * 4
                func.instruction(&Instruction::I32Add); // 4 + length*4
                func.instruction(&Instruction::I32Add); // list_ptr + offset

                // Store value
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Update length
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Store(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
            }
            IRType::None
        }
        "remove" => {
            // list.remove(value)
            // Find and remove first occurrence of value
            // For now, placeholder
            func.instruction(&Instruction::Drop); // Drop list_ptr
            IRType::None
        }
        "index" => {
            // list.index(value) -> int
            // Linear search for first occurrence
            if !arguments.is_empty() {
                // Save list_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit value to search for
                emit_expr(&arguments[0], func, ctx, memory_layout, None);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // search_value

                // Load length
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // Initialize index to 0
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // current_index

                // Loop: check each element
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Check if current_index >= length
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // length
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1)); // Exit loop if done

                // Load element at current_index
                // Address: list_ptr + 4 + (current_index * 4)
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(4));
                func.instruction(&Instruction::I32Mul); // index * 4
                func.instruction(&Instruction::I32Const(4));
                func.instruction(&Instruction::I32Add); // + 4
                func.instruction(&Instruction::I32Add); // address

                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Compare with search_value
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1)); // search_value
                func.instruction(&Instruction::I32Eq);

                // If equal, return current_index
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::Br(2)); // Exit both blocks
                func.instruction(&Instruction::End);

                // Increment current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                // Loop back
                func.instruction(&Instruction::Br(0));

                // End loop
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);

                // Not found, return -1
                func.instruction(&Instruction::I32Const(-1));
            } else {
                func.instruction(&Instruction::Drop); // Drop list_ptr
                func.instruction(&Instruction::I32Const(0));
            }
            IRType::Int
        }
        "count" => {
            // list.count(value) -> int
            // Count occurrences
            if !arguments.is_empty() {
                // Save list_ptr
                func.instruction(&Instruction::LocalSet(ctx.temp_local));

                // Emit value to search for
                emit_expr(&arguments[0], func, ctx, memory_layout, None);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 1)); // search_value

                // Load length
                func.instruction(&Instruction::LocalGet(ctx.temp_local));
                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 2)); // length

                // Initialize index and count
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(0));
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // count

                // Loop: check each element
                func.instruction(&Instruction::Block(BlockType::Empty));
                func.instruction(&Instruction::Loop(BlockType::Empty));

                // Check if current_index >= length
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 2)); // length
                func.instruction(&Instruction::I32GeS);
                func.instruction(&Instruction::BrIf(1)); // Exit loop if done

                // Load element at current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local)); // list_ptr
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3)); // current_index
                func.instruction(&Instruction::I32Const(4));
                func.instruction(&Instruction::I32Mul);
                func.instruction(&Instruction::I32Const(4));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::I32Add);

                func.instruction(&Instruction::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));

                // Compare with search_value
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 1));
                func.instruction(&Instruction::I32Eq);

                // If equal, increment count
                func.instruction(&Instruction::If(BlockType::Empty));
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4)); // count
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 4)); // count
                func.instruction(&Instruction::End);

                // Increment current_index
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 3));
                func.instruction(&Instruction::I32Const(1));
                func.instruction(&Instruction::I32Add);
                func.instruction(&Instruction::LocalSet(ctx.temp_local + 3));

                // Loop back
                func.instruction(&Instruction::Br(0));

                // End loop
                func.instruction(&Instruction::End);
                func.instruction(&Instruction::End);

                // Return count
                func.instruction(&Instruction::LocalGet(ctx.temp_local + 4));
            } else {
                func.instruction(&Instruction::Drop); // Drop list_ptr
                func.instruction(&Instruction::I32Const(0));
            }
            IRType::Int
        }
        _ => {
            // Unknown method
            func.instruction(&Instruction::Drop); // Drop list_ptr
            func.instruction(&Instruction::I32Const(0));
            IRType::Unknown
        }
    }
}
