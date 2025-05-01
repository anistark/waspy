use anyhow::Result;
use rustpython_parser::ast::{Expr, Stmt, Suite};

/// Intermediate Representation (IR) for a module containing multiple functions
pub struct IRModule {
    pub functions: Vec<IRFunction>,
}

/// IR representation of a function
pub struct IRFunction {
    pub name: String,
    pub params: Vec<String>,
    pub body: IRBody,
}

/// IR representation of a function body, which can contain multiple statements
pub struct IRBody {
    pub statements: Vec<IRStatement>,
}

/// IR representation of statements
pub enum IRStatement {
    Return(Option<IRExpr>),
    Assign {
        target: String,
        value: IRExpr,
    },
    If {
        condition: IRExpr,
        then_body: Box<IRBody>,
        else_body: Option<Box<IRBody>>,
    },
    While {
        condition: IRExpr,
        body: Box<IRBody>,
    },
    Expression(IRExpr),
}

/// Expression types in the intermediate representation
#[derive(Debug, Clone)]
pub enum IRExpr {
    Const(IRConstant),
    BinaryOp {
        left: Box<IRExpr>,
        right: Box<IRExpr>,
        op: IROp,
    },
    UnaryOp {
        operand: Box<IRExpr>,
        op: IRUnaryOp,
    },
    CompareOp {
        left: Box<IRExpr>,
        right: Box<IRExpr>,
        op: IRCompareOp,
    },
    Param(String),
    Variable(String),
    FunctionCall {
        function_name: String,
        arguments: Vec<IRExpr>,
    },
    BoolOp {
        left: Box<IRExpr>,
        right: Box<IRExpr>,
        op: IRBoolOp,
    },
}

/// Constant value types supported in the IR
#[derive(Debug, Clone)]
pub enum IRConstant {
    Int(i32),
    Float(f64),
    Bool(bool),
    String(String),
}

/// Binary operators in the IR
#[derive(Debug, Clone)]
pub enum IROp {
    Add,     // +
    Sub,     // -
    Mul,     // *
    Div,     // /
    Mod,     // %
    FloorDiv, // //
    Pow,     // **
}

/// Unary operators in the IR
#[derive(Debug, Clone)]
pub enum IRUnaryOp {
    Neg, // -x
    Not, // not x
}

/// Comparison operators in the IR
#[derive(Debug, Clone)]
pub enum IRCompareOp {
    Eq,    // ==
    NotEq, // !=
    Lt,    // <
    LtE,   // <=
    Gt,    // >
    GtE,   // >=
}

/// Boolean operators in the IR
#[derive(Debug, Clone)]
pub enum IRBoolOp {
    And, // and
    Or,  // or
}

/// Lower a Python AST (Suite) into our IR.
pub fn lower_ast_to_ir(ast: &Suite) -> Result<IRModule> {
    let mut functions = Vec::new();

    for stmt in ast {
        if let Stmt::FunctionDef(fundef) = stmt {
            let name = fundef.name.to_string();

            let params = fundef
                .args
                .args
                .iter()
                .map(|arg_with_default| arg_with_default.def.arg.to_string())
                .collect();

            let body = lower_function_body(&fundef.body)?;

            functions.push(IRFunction { name, params, body });
        } else {
            // For now, we only handle function definitions at the module level
            return Err(anyhow::anyhow!("Only function definitions are supported at the module level"));
        }
    }

    if functions.is_empty() {
        return Err(anyhow::anyhow!("No functions found in the module"));
    }

    Ok(IRModule { functions })
}

/// Lower a function body (sequence of statements) to IR
fn lower_function_body(stmts: &[Stmt]) -> Result<IRBody> {
    let mut ir_statements = Vec::new();

    for stmt in stmts {
        match stmt {
            Stmt::Return(ret) => {
                let expr = if let Some(value) = &ret.value {
                    Some(lower_expr(value)?)
                } else {
                    None
                };
                ir_statements.push(IRStatement::Return(expr));
            }
            Stmt::Assign(assign) => {
                // For simplicity, we only support single target assignments for now
                if assign.targets.len() != 1 {
                    return Err(anyhow::anyhow!("Only single target assignments supported"));
                }

                let target = match &assign.targets[0] {
                    Expr::Name(name) => name.id.to_string(),
                    _ => return Err(anyhow::anyhow!("Only variable assignment supported")),
                };

                let value = lower_expr(&assign.value)?;
                ir_statements.push(IRStatement::Assign { target, value });
            }
            Stmt::If(if_stmt) => {
                let condition = lower_expr(&if_stmt.test)?;
                let then_body = Box::new(lower_function_body(&if_stmt.body)?);

                let else_body = if !if_stmt.orelse.is_empty() {
                    Some(Box::new(lower_function_body(&if_stmt.orelse)?))
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
                let condition = lower_expr(&while_stmt.test)?;
                let body = Box::new(lower_function_body(&while_stmt.body)?);

                ir_statements.push(IRStatement::While { condition, body });
            }
            Stmt::Expr(expr_stmt) => {
                let expr = lower_expr(&expr_stmt.value)?;
                ir_statements.push(IRStatement::Expression(expr));
            }
            _ => {
                return Err(anyhow::anyhow!("Unsupported statement type"));
            }
        }
    }

    Ok(IRBody {
        statements: ir_statements,
    })
}

/// Lower a Python expression into an IR expression
fn lower_expr(expr: &Expr) -> Result<IRExpr> {
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
                _ => return Err(anyhow::anyhow!("Unsupported binary operator")),
            };

            Ok(IRExpr::BinaryOp {
                left: Box::new(lower_expr(&binop.left)?),
                right: Box::new(lower_expr(&binop.right)?),
                op,
            })
        }
        Expr::UnaryOp(unaryop) => {
            let op = match &unaryop.op {
                rustpython_parser::ast::UnaryOp::USub => IRUnaryOp::Neg,
                rustpython_parser::ast::UnaryOp::Not => IRUnaryOp::Not,
                _ => return Err(anyhow::anyhow!("Unsupported unary operator")),
            };

            Ok(IRExpr::UnaryOp {
                operand: Box::new(lower_expr(&unaryop.operand)?),
                op,
            })
        }
        Expr::Compare(compare) => {
            if compare.ops.len() != 1 || compare.comparators.len() != 1 {
                return Err(anyhow::anyhow!("Only single comparisons supported"));
            }

            let op = match &compare.ops[0] {
                rustpython_parser::ast::CmpOp::Eq => IRCompareOp::Eq,
                rustpython_parser::ast::CmpOp::NotEq => IRCompareOp::NotEq,
                rustpython_parser::ast::CmpOp::Lt => IRCompareOp::Lt,
                rustpython_parser::ast::CmpOp::LtE => IRCompareOp::LtE,
                rustpython_parser::ast::CmpOp::Gt => IRCompareOp::Gt,
                rustpython_parser::ast::CmpOp::GtE => IRCompareOp::GtE,
                _ => return Err(anyhow::anyhow!("Unsupported comparison operator")),
            };

            Ok(IRExpr::CompareOp {
                left: Box::new(lower_expr(&compare.left)?),
                right: Box::new(lower_expr(&compare.comparators[0])?),
                op,
            })
        }
        Expr::BoolOp(boolop) => {
            if boolop.values.len() != 2 {
                return Err(anyhow::anyhow!("Only binary boolean operations supported"));
            }

            let op = match boolop.op {
                rustpython_parser::ast::BoolOp::And => IRBoolOp::And,
                rustpython_parser::ast::BoolOp::Or => IRBoolOp::Or,
            };

            Ok(IRExpr::BoolOp {
                left: Box::new(lower_expr(&boolop.values[0])?),
                right: Box::new(lower_expr(&boolop.values[1])?),
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
                        .map_err(|_| anyhow::anyhow!("Integer too large for i32"))?;
                    Ok(IRExpr::Const(IRConstant::Int(i32_value)))
                }
                rustpython_parser::ast::Constant::Float(f) => {
                    Ok(IRExpr::Const(IRConstant::Float(*f)))
                }
                rustpython_parser::ast::Constant::Bool(b) => {
                    Ok(IRExpr::Const(IRConstant::Bool(*b)))
                }
                rustpython_parser::ast::Constant::Str(s) => {
                    Ok(IRExpr::Const(IRConstant::String(s.clone())))
                }
                _ => Err(anyhow::anyhow!("Unsupported constant type")),
            }
        }
        Expr::Name(name) => Ok(IRExpr::Variable(name.id.to_string())),
        Expr::Call(call) => {
            let function_name = match call.func.as_ref() {
                Expr::Name(name) => name.id.to_string(),
                _ => return Err(anyhow::anyhow!("Only direct function calls supported")),
            };

            // Special handling for certain built-in functions
            if function_name == "int" {
                if call.args.len() != 1 {
                    return Err(anyhow::anyhow!("int() function expects exactly one argument"));
                }

                // For 'int(x)', we'll treat it as a special case
                // We'll directly evaluate the inner expression, then it will be
                // implicitly converted to an integer in the WebAssembly
                return lower_expr(&call.args[0]);
            }

            let mut arguments = Vec::new();
            for arg in &call.args {
                arguments.push(lower_expr(arg)?);
            }

            Ok(IRExpr::FunctionCall {
                function_name,
                arguments,
            })
        }
        _ => Err(anyhow::anyhow!("Unsupported expression type")),
    }
}
