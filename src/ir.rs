use anyhow::Result;
use rustpython_parser::ast::{Expr, Stmt, Suite};

/// Minimal Intermediate Representation (IR)
pub struct IRFunction {
    pub name: String,
    pub params: Vec<String>,
    pub body: IRExpr,
}

#[derive(Debug)]
pub enum IRExpr {
    Const(i32),
    BinaryOp {
        left: Box<IRExpr>,
        right: Box<IRExpr>,
        op: IROp,
    },
    Param(String),
}

#[derive(Debug)]
pub enum IROp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Lower a Python AST (Suite) into our IR.
pub fn lower_ast_to_ir(ast: &Suite) -> Result<IRFunction> {
    // Only one function
    if ast.len() != 1 {
        anyhow::bail!("Only single function supported");
    }

    let Stmt::FunctionDef(fundef) = &ast[0] else {
        anyhow::bail!("Expected a function definition");
    };

    let name = fundef.name.to_string();

    let params = fundef
        .args
        .args
        .iter()
        .map(|arg_with_default| arg_with_default.def.arg.to_string())
        .collect();

    if fundef.body.len() != 1 {
        anyhow::bail!("Only single return statement supported");
    }

    let Stmt::Return(ret) = &fundef.body[0] else {
        anyhow::bail!("Expected a return statement");
    };

    let expr = ret
        .value
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Return with no value"))?;

    let body_ir = lower_expr(expr)?;

    Ok(IRFunction {
        name,
        params,
        body: body_ir,
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
                _ => anyhow::bail!("Unsupported binary operator"),
            };

            Ok(IRExpr::BinaryOp {
                left: Box::new(lower_expr(&binop.left)?),
                right: Box::new(lower_expr(&binop.right)?),
                op,
            })
        }
        Expr::Constant(c) => {
            if let rustpython_parser::ast::Constant::Int(i) = &c.value {
                Ok(IRExpr::Const((i.clone()).try_into()?))
            } else {
                anyhow::bail!("Only integer constants supported")
            }
        }
        Expr::Name(name) => Ok(IRExpr::Param(name.id.to_string())),
        _ => anyhow::bail!("Unsupported expression type"),
    }
}