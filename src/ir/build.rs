//! Builders for IR synthesized by the module passes (`itertools`,
//! `collections`): short constructors for the expressions and statements a
//! pass writes out by hand.

use crate::ir::{
    IRBody, IRBoolOp, IRCompareOp, IRComprehensionKind, IRConstant, IRExpr, IRGenerator, IROp,
    IRParam, IRStatement, IRType, IRUnaryOp, ParamStar,
};

pub(crate) fn v(name: &str) -> IRExpr {
    IRExpr::Variable(name.to_string())
}
pub(crate) fn int(i: i32) -> IRExpr {
    IRExpr::Const(IRConstant::Int(i))
}
pub(crate) fn boolean(b: bool) -> IRExpr {
    IRExpr::Const(IRConstant::Bool(b))
}
pub(crate) fn text(s: &str) -> IRExpr {
    IRExpr::Const(IRConstant::String(s.to_string()))
}
pub(crate) fn call(f: &str, args: Vec<IRExpr>) -> IRExpr {
    IRExpr::FunctionCall {
        function_name: f.to_string(),
        arguments: args,
    }
}
pub(crate) fn mcall(object: IRExpr, method: &str, args: Vec<IRExpr>) -> IRExpr {
    IRExpr::MethodCall {
        object: Box::new(object),
        method_name: method.to_string(),
        arguments: args,
    }
}
pub(crate) fn bin(l: IRExpr, op: IROp, r: IRExpr) -> IRExpr {
    IRExpr::BinaryOp {
        left: Box::new(l),
        right: Box::new(r),
        op,
    }
}
pub(crate) fn cmp(l: IRExpr, op: IRCompareOp, r: IRExpr) -> IRExpr {
    IRExpr::CompareOp {
        left: Box::new(l),
        right: Box::new(r),
        op,
    }
}
pub(crate) fn not(e: IRExpr) -> IRExpr {
    IRExpr::UnaryOp {
        operand: Box::new(e),
        op: IRUnaryOp::Not,
    }
}
pub(crate) fn and(l: IRExpr, r: IRExpr) -> IRExpr {
    IRExpr::BoolOp {
        left: Box::new(l),
        right: Box::new(r),
        op: IRBoolOp::And,
    }
}
pub(crate) fn idx(c: IRExpr, i: IRExpr) -> IRExpr {
    IRExpr::Indexing {
        container: Box::new(c),
        index: Box::new(i),
    }
}
pub(crate) fn len(e: IRExpr) -> IRExpr {
    call("len", vec![e])
}
pub(crate) fn range(stop: IRExpr) -> IRExpr {
    IRExpr::RangeCall {
        start: None,
        stop: Box::new(stop),
        step: None,
    }
}
pub(crate) fn assign(target: &str, value: IRExpr) -> IRStatement {
    IRStatement::Assign {
        target: target.to_string(),
        value,
        var_type: None,
    }
}
/// An assignment whose type is fixed, unless it is not known.
pub(crate) fn assign_typed(target: &str, value: IRExpr, ty: &IRType) -> IRStatement {
    IRStatement::Assign {
        target: target.to_string(),
        value,
        var_type: Some(ty.clone()).filter(known),
    }
}
pub(crate) fn add_to(target: &str, value: IRExpr) -> IRStatement {
    assign(target, bin(v(target), IROp::Add, value))
}
pub(crate) fn set_item(c: IRExpr, i: IRExpr, value: IRExpr) -> IRStatement {
    IRStatement::IndexAssign {
        container: c,
        index: i,
        value,
    }
}
pub(crate) fn yield_(e: IRExpr) -> IRStatement {
    IRStatement::Yield { value: Some(e) }
}
pub(crate) fn body(statements: Vec<IRStatement>) -> Box<IRBody> {
    Box::new(IRBody { statements })
}
pub(crate) fn for_(target: &str, iterable: IRExpr, statements: Vec<IRStatement>) -> IRStatement {
    IRStatement::For {
        target: target.to_string(),
        iterable,
        body: body(statements),
        else_body: None,
    }
}
pub(crate) fn while_(condition: IRExpr, statements: Vec<IRStatement>) -> IRStatement {
    IRStatement::While {
        condition,
        body: body(statements),
    }
}
pub(crate) fn if_(condition: IRExpr, then: Vec<IRStatement>) -> IRStatement {
    IRStatement::If {
        condition,
        then_body: body(then),
        else_body: None,
    }
}
pub(crate) fn if_else(
    condition: IRExpr,
    then: Vec<IRStatement>,
    otherwise: Vec<IRStatement>,
) -> IRStatement {
    IRStatement::If {
        condition,
        then_body: body(then),
        else_body: Some(body(otherwise)),
    }
}
pub(crate) fn ret() -> IRStatement {
    IRStatement::Return(None)
}
pub(crate) fn raise(class: &str, message: &str) -> IRStatement {
    IRStatement::Raise {
        exception: Some(call(class, vec![text(message)])),
        cause: None,
    }
}
/// `tuple(items)`, a list read as a tuple.
pub(crate) fn as_tuple(list: IRExpr) -> IRExpr {
    call(crate::ir::AS_TUPLE_FN, vec![list])
}
/// `tuple(pool[indices[k]] for k in range(r))`.
pub(crate) fn picked(pool: &str, indices: &str, r: IRExpr, n: usize) -> IRExpr {
    let k = format!("__k{n}");
    as_tuple(IRExpr::Comprehension {
        kind: IRComprehensionKind::List,
        element: Box::new(idx(v(pool), idx(v(indices), v(&k)))),
        value: None,
        generators: vec![IRGenerator {
            targets: vec![k],
            iterable: range(r),
            conditions: Vec::new(),
        }],
    })
}
/// `[x for x in it]`: a list of what iterating `it` yields.
pub(crate) fn listed(it: IRExpr, n: usize, k: usize) -> IRExpr {
    let x = format!("__x{n}_{k}");
    IRExpr::Comprehension {
        kind: IRComprehensionKind::List,
        element: Box::new(v(&x)),
        value: None,
        generators: vec![IRGenerator {
            targets: vec![x],
            iterable: it,
            conditions: Vec::new(),
        }],
    }
}
pub(crate) fn param(name: &str, ty: &IRType) -> IRParam {
    IRParam {
        name: name.to_string(),
        param_type: if known(ty) {
            ty.clone()
        } else {
            IRType::Unknown
        },
        default_value: None,
        keyword_only: false,
        star: ParamStar::None,
    }
}

pub(crate) fn known(ty: &IRType) -> bool {
    !matches!(ty, IRType::Unknown | IRType::Any)
}

pub(crate) fn refused(message: String) -> anyhow::Error {
    crate::core::errors::unsupported_feature(message, None).into()
}

pub(crate) fn none() -> IRExpr {
    IRExpr::Const(IRConstant::None)
}
pub(crate) fn float(f: f64) -> IRExpr {
    IRExpr::Const(IRConstant::Float(f))
}
pub(crate) fn attr(object: IRExpr, field: &str) -> IRExpr {
    IRExpr::Attribute {
        object: Box::new(object),
        attribute: field.to_string(),
    }
}
/// `self.<field>`.
pub(crate) fn my(field: &str) -> IRExpr {
    attr(v("self"), field)
}
pub(crate) fn set_attr(object: IRExpr, field: &str, value: IRExpr) -> IRStatement {
    IRStatement::AttributeAssign {
        object,
        attribute: field.to_string(),
        value,
        annotation: None,
    }
}
/// `self.<field>: ty = value`, which fixes the field's type.
pub(crate) fn set_my_typed(field: &str, value: IRExpr, ty: &IRType) -> IRStatement {
    IRStatement::AttributeAssign {
        object: v("self"),
        attribute: field.to_string(),
        value,
        annotation: Some(ty.clone()),
    }
}
pub(crate) fn expr_stmt(e: IRExpr) -> IRStatement {
    IRStatement::Expression(e)
}
pub(crate) fn ret_val(e: IRExpr) -> IRStatement {
    IRStatement::Return(Some(e))
}
pub(crate) fn or(l: IRExpr, r: IRExpr) -> IRExpr {
    IRExpr::BoolOp {
        left: Box::new(l),
        right: Box::new(r),
        op: IRBoolOp::Or,
    }
}
/// `del c[k]`.
pub(crate) fn del_item(c: IRExpr, k: IRExpr) -> IRStatement {
    expr_stmt(mcall(c, crate::ir::DEL_ITEM_METHOD, vec![k]))
}
pub(crate) fn slice(c: IRExpr, start: Option<IRExpr>, end: Option<IRExpr>) -> IRExpr {
    IRExpr::Slicing {
        container: Box::new(c),
        start: start.map(Box::new),
        end: end.map(Box::new),
        step: None,
    }
}
/// `raise E(arg)`.
pub(crate) fn raise_expr(class: &str, arg: IRExpr) -> IRStatement {
    IRStatement::Raise {
        exception: Some(call(class, vec![arg])),
        cause: None,
    }
}
pub(crate) fn lambda(params: Vec<IRParam>, body: IRExpr) -> IRExpr {
    IRExpr::Lambda {
        params,
        body: Box::new(body),
        captured_vars: Vec::new(),
    }
}
/// A parameter with a default value.
pub(crate) fn param_default(name: &str, ty: &IRType, default: IRExpr) -> IRParam {
    IRParam {
        default_value: Some(default),
        ..param(name, ty)
    }
}
pub(crate) fn method(
    name: &str,
    params: Vec<IRParam>,
    ret: IRType,
    statements: Vec<IRStatement>,
) -> crate::ir::IRFunction {
    crate::ir::IRFunction {
        name: name.to_string(),
        params,
        body: IRBody { statements },
        return_type: ret,
        decorators: Vec::new(),
    }
}
