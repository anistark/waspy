use crate::ir::types::*;
use anyhow::{anyhow, Context, Result};
use rustpython_parser::ast::{ArgWithDefault, Arguments, Expr, Stmt, Suite};

/// Lower a Python AST (Suite) into our IR.
pub fn lower_ast_to_ir(ast: &Suite) -> Result<IRModule> {
    let mut functions = Vec::new();
    let mut memory_layout = MemoryLayout::new();

    for stmt in ast {
        match stmt {
            Stmt::FunctionDef(fundef) => {
                let name = fundef.name.to_string();
                let params = process_function_params(&fundef.args)?;
                let return_type = if let Some(returns) = &fundef.returns {
                    type_annotation_to_ir_type(returns)?
                } else {
                    IRType::Unknown
                };
                let body = lower_function_body(&fundef.body, &mut memory_layout)?;
                functions.push(IRFunction {
                    name,
                    params,
                    body,
                    return_type,
                });
            }
            Stmt::Expr(_) => {
                // Skip module-level expressions (like docstrings)
                continue;
            }
            _ => {
                return Err(anyhow!(
                    "Only function definitions and docstrings are supported at the module level"
                ));
            }
        }
    }

    if functions.is_empty() {
        return Err(anyhow!("No functions found in the module"));
    }

    Ok(IRModule { functions })
}

/// Convert type annotations to IR types
fn type_annotation_to_ir_type(expr: &Expr) -> Result<IRType> {
    match expr {
        Expr::Name(name) => {
            match name.id.as_str() {
                "int" => Ok(IRType::Int),
                "float" => Ok(IRType::Float),
                "bool" => Ok(IRType::Bool),
                "str" => Ok(IRType::String),
                "None" => Ok(IRType::None),
                _ => Ok(IRType::Any),
            }
        }
        Expr::Subscript(subscript) => {
            // Handle generic types like List[int]
            if let Expr::Name(container) = &*subscript.value {
                match container.id.as_str() {
                    "List" | "list" => {
                        let element_type = type_annotation_to_ir_type(&subscript.slice)?;
                        Ok(IRType::List(Box::new(element_type)))
                    }
                    "Dict" | "dict" => {
                        if let Expr::Tuple(tuple) = &*subscript.slice {
                            if tuple.elts.len() == 2 {
                                let key_type = type_annotation_to_ir_type(&tuple.elts[0])?;
                                let value_type = type_annotation_to_ir_type(&tuple.elts[1])?;
                                Ok(IRType::Dict(Box::new(key_type), Box::new(value_type)))
                            } else {
                                Err(anyhow!(
                                    "Dict type annotation should have exactly 2 elements"
                                ))
                            }
                        } else {
                            Err(anyhow!("Invalid Dict type annotation"))
                        }
                    }
                    _ => Ok(IRType::Any),
                }
            } else {
                Ok(IRType::Any)
            }
        }
        _ => Ok(IRType::Any), // Default to Any for complex annotations
    }
}

/// Process function parameters with possible type annotations
fn process_function_params(args: &Arguments) -> Result<Vec<IRParam>> {
    args.args
        .iter()
        .map(|arg_with_default: &ArgWithDefault| {
            let name = arg_with_default.def.arg.to_string();

            // Check for type annotation
            let param_type = if let Some(annotation) = &arg_with_default.def.annotation {
                type_annotation_to_ir_type(annotation)?
            } else {
                IRType::Unknown
            };

            Ok(IRParam { name, param_type })
        })
        .collect()
}

/// Lower a function body (sequence of statements) to IR
fn lower_function_body(stmts: &[Stmt], memory_layout: &mut MemoryLayout) -> Result<IRBody> {
    let mut ir_statements = Vec::new();

    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => {
                let expr = if let Some(value) = &ret.value {
                    Some(lower_expr(value, memory_layout)?)
                } else {
                    None
                };
                ir_statements.push(IRStatement::Return(expr));
            }
            Stmt::Assign(assign) => {
                // Handle simple assignment like "x = 5"
                if assign.targets.len() != 1 {
                    return Err(anyhow!("Only single target assignments supported"));
                }
                // TODO: Support multiple target assignments

                let target = match &assign.targets[0] {
                    Expr::Name(name) => name.id.to_string(),
                    _ => return Err(anyhow!("Only variable assignment supported")),
                };

                let value = lower_expr(&assign.value, memory_layout)?;
                ir_statements.push(IRStatement::Assign {
                    target,
                    value,
                    var_type: None,
                });
            }
            Stmt::AnnAssign(ann_assign) => {
                // Handle typed assignment like "x: int = 5"
                let target = match &*ann_assign.target {
                    Expr::Name(name) => name.id.to_string(),
                    _ => return Err(anyhow!("Only variable assignment supported")),
                };

                let var_type = type_annotation_to_ir_type(&ann_assign.annotation)?;

                let value = if let Some(value) = &ann_assign.value {
                    lower_expr(value, memory_layout)?
                } else {
                    // Handle declarations without assignment ("x: int")
                    match &var_type {
                        IRType::Int => IRExpr::Const(IRConstant::Int(0)),
                        IRType::Float => IRExpr::Const(IRConstant::Float(0.0)),
                        IRType::Bool => IRExpr::Const(IRConstant::Bool(false)),
                        IRType::String => IRExpr::Const(IRConstant::String(String::new())),
                        IRType::None => IRExpr::Const(IRConstant::None),
                        _ => IRExpr::Const(IRConstant::None),
                    }
                };

                ir_statements.push(IRStatement::Assign {
                    target,
                    value,
                    var_type: Some(var_type),
                });
            }
            Stmt::If(if_stmt) => {
                let condition = lower_expr(&if_stmt.test, memory_layout)?;
                let then_body = Box::new(lower_function_body(&if_stmt.body, memory_layout)?);

                let else_body = if !if_stmt.orelse.is_empty() {
                    Some(Box::new(lower_function_body(
                        &if_stmt.orelse,
                        memory_layout,
                    )?))
                } else {
                    None
                };

                ir_statements.push(IRStatement::If {
                    condition,
                    then_body,
                    else_body,
                });
            }
            Stmt::While(while_stmt) => {
                let condition = lower_expr(&while_stmt.test, memory_layout)?;
                let body = Box::new(lower_function_body(&while_stmt.body, memory_layout)?);

                ir_statements.push(IRStatement::While { condition, body });
            }
            Stmt::Expr(expr_stmt) => {
                let expr = lower_expr(&expr_stmt.value, memory_layout)?;
                ir_statements.push(IRStatement::Expression(expr));
            }
            _ => {
                return Err(anyhow!("Unsupported statement type: {:?}", stmt));
            }
        }
    }

    Ok(IRBody {
        statements: ir_statements,
    })
}

/// Lower a Python expression into an IR expression
pub fn lower_expr(expr: &Expr, memory_layout: &mut MemoryLayout) -> Result<IRExpr> {
    match expr {
        Expr::BinOp(binop) => {
            let op = match &binop.op {
                rustpython_parser::ast::Operator::Add => IROp::Add,
                rustpython_parser::ast::Operator::Sub => IROp::Sub,
                rustpython_parser::ast::Operator::Mult => IROp::Mul,
                rustpython_parser::ast::Operator::Div => IROp::Div,
                rustpython_parser::ast::Operator::Mod => IROp::Mod,
                rustpython_parser::ast::Operator::FloorDiv => IROp::FloorDiv,
                rustpython_parser::ast::Operator::Pow => IROp::Pow,
                _ => return Err(anyhow!("Unsupported binary operator")),
            };

            Ok(IRExpr::BinaryOp {
                left: Box::new(lower_expr(&binop.left, memory_layout)?),
                right: Box::new(lower_expr(&binop.right, memory_layout)?),
                op,
            })
        }
        Expr::UnaryOp(unaryop) => {
            let op = match &unaryop.op {
                rustpython_parser::ast::UnaryOp::USub => IRUnaryOp::Neg,
                rustpython_parser::ast::UnaryOp::Not => IRUnaryOp::Not,
                _ => return Err(anyhow!("Unsupported unary operator")),
            };

            Ok(IRExpr::UnaryOp {
                operand: Box::new(lower_expr(&unaryop.operand, memory_layout)?),
                op,
            })
        }
        Expr::Compare(compare) => {
            if compare.ops.len() != 1 || compare.comparators.len() != 1 {
                return Err(anyhow!("Only single comparisons supported"));
            }

            let op = match &compare.ops[0] {
                rustpython_parser::ast::CmpOp::Eq => IRCompareOp::Eq,
                rustpython_parser::ast::CmpOp::NotEq => IRCompareOp::NotEq,
                rustpython_parser::ast::CmpOp::Lt => IRCompareOp::Lt,
                rustpython_parser::ast::CmpOp::LtE => IRCompareOp::LtE,
                rustpython_parser::ast::CmpOp::Gt => IRCompareOp::Gt,
                rustpython_parser::ast::CmpOp::GtE => IRCompareOp::GtE,
                _ => return Err(anyhow!("Unsupported comparison operator")),
            };

            Ok(IRExpr::CompareOp {
                left: Box::new(lower_expr(&compare.left, memory_layout)?),
                right: Box::new(lower_expr(&compare.comparators[0], memory_layout)?),
                op,
            })
        }
        Expr::BoolOp(boolop) => {
            if boolop.values.len() != 2 {
                return Err(anyhow!("Only binary boolean operations supported"));
            }

            let op = match boolop.op {
                rustpython_parser::ast::BoolOp::And => IRBoolOp::And,
                rustpython_parser::ast::BoolOp::Or => IRBoolOp::Or,
            };

            Ok(IRExpr::BoolOp {
                left: Box::new(lower_expr(&boolop.values[0], memory_layout)?),
                right: Box::new(lower_expr(&boolop.values[1], memory_layout)?),
                op,
            })
        }
        Expr::Constant(c) => {
            match &c.value {
                rustpython_parser::ast::Constant::Int(i) => {
                    // Convert to i32 more safely
                    let i32_value = i
                        .to_string()
                        .parse::<i32>()
                        .context("Integer too large for i32")?;
                    Ok(IRExpr::Const(IRConstant::Int(i32_value)))
                }
                rustpython_parser::ast::Constant::Float(f) => {
                    Ok(IRExpr::Const(IRConstant::Float(*f)))
                }
                rustpython_parser::ast::Constant::Bool(b) => {
                    Ok(IRExpr::Const(IRConstant::Bool(*b)))
                }
                rustpython_parser::ast::Constant::Str(s) => {
                    // Register the string in memory layout
                    memory_layout.add_string(s);
                    Ok(IRExpr::Const(IRConstant::String(s.clone())))
                }
                rustpython_parser::ast::Constant::None => Ok(IRExpr::Const(IRConstant::None)),
                _ => Err(anyhow!("Unsupported constant type")),
            }
        }
        Expr::Name(name) => Ok(IRExpr::Variable(name.id.to_string())),
        Expr::Call(call) => {
            let function_name = match call.func.as_ref() {
                Expr::Name(name) => name.id.to_string(),
                Expr::Attribute(attr) => {
                    // Handle method calls like obj.method()
                    if let Expr::Name(name) = attr.value.as_ref() {
                        format!("{}.{}", name.id, attr.attr)
                    } else {
                        return Err(anyhow!("Complex method calls not supported"));
                    }
                }
                _ => return Err(anyhow!("Only direct function calls supported")),
            };

            // Type conversion functions like int, float, str
            let type_conversions = ["int", "float", "str", "bool"];
            if type_conversions.contains(&function_name.as_str()) {
                if call.args.len() != 1 {
                    return Err(anyhow!(
                        "Type conversion function expects exactly one argument"
                    ));
                }
                return lower_expr(&call.args[0], memory_layout);
            }

            let mut arguments = Vec::new();
            for arg in &call.args {
                arguments.push(lower_expr(arg, memory_layout)?);
            }

            Ok(IRExpr::FunctionCall {
                function_name,
                arguments,
            })
        }
        Expr::List(list) => {
            let mut elements = Vec::new();
            for item in &list.elts {
                elements.push(lower_expr(item, memory_layout)?);
            }
            Ok(IRExpr::ListLiteral(elements))
        }
        Expr::Dict(dict) => {
            let mut pairs = Vec::new();
            for (key, value) in dict.keys.iter().zip(dict.values.iter()) {
                if let Some(key) = key {
                    pairs.push((
                        lower_expr(key, memory_layout)?,
                        lower_expr(value, memory_layout)?,
                    ));
                }
            }
            Ok(IRExpr::DictLiteral(pairs))
        }
        Expr::Subscript(subscript) => Ok(IRExpr::Indexing {
            container: Box::new(lower_expr(&subscript.value, memory_layout)?),
            index: Box::new(lower_expr(&subscript.slice, memory_layout)?),
        }),
        Expr::Attribute(attr) => Ok(IRExpr::Attribute {
            object: Box::new(lower_expr(&attr.value, memory_layout)?),
            attribute: attr.attr.to_string(),
        }),
        _ => Err(anyhow!("Unsupported expression type: {:?}", expr)),
    }
}
