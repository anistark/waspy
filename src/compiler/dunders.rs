//! Operators and builtins on instances, through the class's special methods.
//!
//! `a + b`, `-a`, `x in c`, `c[k]`, `c[k] = v`, `a += b`, `len(c)`, `abs(a)`,
//! `int(a)`, `float(a)`, `hash(a)`, calling an instance, and testing one for
//! truth are each a call of a special method when the operand's static type is
//! a class that defines (or inherits) it. Most are rewritten here into the
//! method call they mean, so the call's argument conversion, virtual dispatch,
//! and exception checks are the ordinary method call's. The ones CPython checks
//! the answer of (`len()` and truth) are emitted here.
//!
//! There is no `NotImplemented` (the name is not defined), so the method the
//! rules pick answers: the left operand's own method, or the right operand's
//! reflected one when the left has none, or when the right is an instance of
//! a subclass of the left's class that overrides the reflected method.

use crate::compiler::context::CompilationContext;
use crate::compiler::exceptions::ExcArg;
use crate::compiler::expression::emit_user_call;
use crate::compiler::function::{
    emit_call_depth_step, emit_post_call_check, emit_raise, infer_value_type,
};
use crate::ir::{IRCompareOp, IRExpr, IROp, IRStatement, IRType, IRUnaryOp, MethodKind};
use wasm_encoder::{BlockType, Function, Instruction as I, MemArg};

/// Every special method an operator, a truth test, or a builtin calls.
pub(crate) const PROTOCOL_METHODS: &[&str] = &[
    "__add__",
    "__radd__",
    "__iadd__",
    "__sub__",
    "__rsub__",
    "__isub__",
    "__mul__",
    "__rmul__",
    "__imul__",
    "__truediv__",
    "__rtruediv__",
    "__itruediv__",
    "__mod__",
    "__rmod__",
    "__imod__",
    "__floordiv__",
    "__rfloordiv__",
    "__ifloordiv__",
    "__pow__",
    "__rpow__",
    "__ipow__",
    "__matmul__",
    "__rmatmul__",
    "__imatmul__",
    "__lshift__",
    "__rlshift__",
    "__ilshift__",
    "__rshift__",
    "__rrshift__",
    "__irshift__",
    "__or__",
    "__ror__",
    "__ior__",
    "__xor__",
    "__rxor__",
    "__ixor__",
    "__and__",
    "__rand__",
    "__iand__",
    "__neg__",
    "__pos__",
    "__invert__",
    "__contains__",
    "__getitem__",
    "__setitem__",
    "__len__",
    "__bool__",
    "__abs__",
    "__int__",
    "__float__",
    "__hash__",
    "__round__",
];

/// The method `op` calls on its left operand, the reflected one it calls on
/// its right, and the in-place one `+=` calls.
pub(crate) fn binary_methods(op: &IROp) -> (&'static str, &'static str, &'static str) {
    match op {
        IROp::Add => ("__add__", "__radd__", "__iadd__"),
        IROp::Sub => ("__sub__", "__rsub__", "__isub__"),
        IROp::Mul => ("__mul__", "__rmul__", "__imul__"),
        IROp::Div => ("__truediv__", "__rtruediv__", "__itruediv__"),
        IROp::Mod => ("__mod__", "__rmod__", "__imod__"),
        IROp::FloorDiv => ("__floordiv__", "__rfloordiv__", "__ifloordiv__"),
        IROp::Pow => ("__pow__", "__rpow__", "__ipow__"),
        IROp::MatMul => ("__matmul__", "__rmatmul__", "__imatmul__"),
        IROp::LShift => ("__lshift__", "__rlshift__", "__ilshift__"),
        IROp::RShift => ("__rshift__", "__rrshift__", "__irshift__"),
        IROp::BitOr => ("__or__", "__ror__", "__ior__"),
        IROp::BitXor => ("__xor__", "__rxor__", "__ixor__"),
        IROp::BitAnd => ("__and__", "__rand__", "__iand__"),
    }
}

/// Whether any class of the module defines `method`, which most expressions
/// answer without inferring a type.
fn defined_anywhere(ctx: &CompilationContext, method: &str) -> bool {
    ctx.class_map
        .values()
        .any(|ci| ci.methods.contains_key(method))
}

/// The class `expr` is an instance of, when its static type is one.
fn class_of(expr: &IRExpr, ctx: &CompilationContext) -> Option<String> {
    match infer_value_type(expr, ctx) {
        IRType::Class(c) => Some(c),
        _ => None,
    }
}

fn has(ctx: &CompilationContext, class: &str, method: &str) -> bool {
    ctx.get_class_info(class)
        .is_some_and(|ci| ci.methods.contains_key(method))
}

/// A constant or a local: evaluating something else first cannot change it,
/// so the two operands may be evaluated in either order.
fn order_free(expr: &IRExpr) -> bool {
    match expr {
        IRExpr::Const(_) | IRExpr::Variable(_) | IRExpr::Param(_) => true,
        // A negative number is written as a negated constant.
        IRExpr::UnaryOp {
            operand,
            op: IRUnaryOp::Neg | IRUnaryOp::UAdd,
        } => matches!(operand.as_ref(), IRExpr::Const(_)),
        _ => false,
    }
}

fn call(object: &IRExpr, method: &str, arguments: Vec<IRExpr>) -> IRExpr {
    IRExpr::MethodCall {
        object: Box::new(object.clone()),
        method_name: method.to_string(),
        arguments,
    }
}

/// How an instance of a class reads as a builtin value, when it does: a named
/// tuple as the tuple of its fields, a `collections` class as the dict or
/// list it holds.
#[derive(Clone, Copy, PartialEq)]
enum View {
    /// A value made from the instance (a named tuple's tuple, a ChainMap's
    /// merged dict): reading it is reading the instance, changing it is not.
    Copy,
    /// The container the instance holds, so changing it changes the instance.
    Shared,
}

fn view_kind(ctx: &CompilationContext, class: &str) -> Option<View> {
    if has(ctx, class, crate::ir::DATA_METHOD) {
        Some(View::Shared)
    } else if has(ctx, class, crate::ir::VIEW_METHOD) {
        Some(View::Copy)
    } else {
        None
    }
}

/// What `class`'s view is: its view method's declared result.
fn view_type(ctx: &CompilationContext, class: &str) -> Option<IRType> {
    let method = match view_kind(ctx, class)? {
        View::Shared => crate::ir::DATA_METHOD,
        View::Copy => crate::ir::VIEW_METHOD,
    };
    let owner = ctx
        .get_class_info(class)?
        .method_owner
        .get(method)
        .cloned()
        .unwrap_or_else(|| class.to_string());
    ctx.get_function_info(&format!("{owner}::{method}"))
        .map(|f| f.return_type.clone())
}

/// Whether instances of `class` cannot be changed once made (its
/// `<frozen>` class variable says so): the `datetime` classes.
pub(crate) fn is_frozen(ctx: &CompilationContext, class: &str) -> bool {
    ctx.get_class_info(class)
        .is_some_and(|ci| ci.class_var_values.contains_key(crate::ir::FROZEN_VAR))
}

/// Whether instances of `class` are named tuples, which read as the tuple of
/// their fields.
pub(crate) fn is_named_tuple(ctx: &CompilationContext, class: &str) -> bool {
    view_kind(ctx, class) == Some(View::Copy)
        && matches!(view_type(ctx, class), Some(IRType::Tuple(_)))
}

/// `expr` read as the builtin value it is, when its class has a view.
fn view_of(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let class = class_of(expr, ctx)?;
    let method = match view_kind(ctx, &class)? {
        View::Shared => crate::ir::DATA_METHOD,
        View::Copy => crate::ir::VIEW_METHOD,
    };
    Some(call(expr, method, Vec::new()))
}

/// `expr`'s shared view: the container it holds, which a change goes to.
fn shared_view_of(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let class = class_of(expr, ctx)?;
    (view_kind(ctx, &class)? == View::Shared)
        .then(|| call(expr, crate::ir::DATA_METHOD, Vec::new()))
}

/// `expr`'s view, when `expr` is an instance whose class reads as a builtin
/// value of `slot`'s kind (a dict-like class where a dict is expected, a
/// deque where a list is, a named tuple where a tuple is). A class that is
/// not a subclass of the builtin (ChainMap, whose view is a merged copy)
/// does not.
pub(crate) fn shared_view_for(
    expr: &IRExpr,
    slot: &IRType,
    ctx: &CompilationContext,
) -> Option<IRExpr> {
    if !matches!(
        slot,
        IRType::Dict(..) | IRType::List(_) | IRType::Tuple(_) | IRType::VarTuple(_)
    ) {
        return None;
    }
    let class = class_of(expr, ctx)?;
    let view = view_type(ctx, &class)?;
    let fits = match (view_kind(ctx, &class)?, &view, slot) {
        (View::Shared, IRType::Dict(..), IRType::Dict(..)) => true,
        // A deque is not a list in CPython.
        (View::Shared, _, _) => false,
        (View::Copy, IRType::Tuple(_), IRType::Tuple(_) | IRType::VarTuple(_)) => {
            is_named_tuple(ctx, &class)
        }
        _ => false,
    };
    fits.then(|| view_of(expr, ctx)).flatten()
}

/// What `json.dumps` serializes an instance as: a named tuple's tuple, and
/// the dict a dict-like `collections` class holds (which CPython serializes
/// as the dict subclass it is). `None` for any other value.
pub(crate) fn json_view(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let class = class_of(expr, ctx)?;
    match (view_kind(ctx, &class)?, view_type(ctx, &class)?) {
        (View::Shared, IRType::Dict(..)) => view_of(expr, ctx),
        (View::Copy, IRType::Tuple(_)) if is_named_tuple(ctx, &class) => view_of(expr, ctx),
        _ => None,
    }
}

/// `expr` as a named tuple's tuple if it is one, else itself.
fn viewed_tuple(expr: &IRExpr, ctx: &CompilationContext) -> IRExpr {
    match class_of(expr, ctx) {
        Some(c) if is_named_tuple(ctx, &c) => view_of(expr, ctx).expect("a named tuple has a view"),
        _ => expr.clone(),
    }
}

fn any_view(ctx: &CompilationContext) -> bool {
    defined_anywhere(ctx, crate::ir::VIEW_METHOD) || defined_anywhere(ctx, crate::ir::DATA_METHOD)
}

/// Whether a value of `ty` is a tuple, or a named tuple.
fn tuple_like(ty: &IRType, ctx: &CompilationContext) -> bool {
    match ty {
        IRType::Tuple(_) | IRType::VarTuple(_) => true,
        IRType::Class(c) => is_named_tuple(ctx, c),
        _ => false,
    }
}

/// The methods of its view a class answers for its instances (its
/// `<delegates>`): the ones whose meaning is the builtin value's.
fn delegates(ctx: &CompilationContext, class: &str, method: &str) -> bool {
    let Some(IRExpr::TupleLiteral(items)) = ctx
        .get_class_info(class)
        .and_then(|ci| ci.class_var_values.get(crate::ir::DELEGATES_VAR))
    else {
        return false;
    };
    items
        .iter()
        .any(|i| matches!(i, IRExpr::Const(crate::ir::IRConstant::String(s)) if s == method))
}

/// Whether `class` has a variant of `method` for some argument type.
fn has_variants(ctx: &CompilationContext, class: &str, method: &str) -> bool {
    let prefix = format!("{method}<");
    ctx.get_class_info(class)
        .is_some_and(|ci| ci.methods.keys().any(|m| m.starts_with(&prefix)))
}

/// `obj.method(arg)` as the call of the variant of `method` for `arg`'s
/// type, or for the builtin value `arg` reads as. A method with variants and
/// none for the argument is refused.
fn rewrite_variant(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::MethodCall {
        object,
        method_name,
        arguments,
    } = expr
    else {
        return None;
    };
    let class = class_of(object, ctx)?;
    if !has_variants(ctx, &class, method_name) {
        return None;
    }
    if let [arg] = arguments.as_slice() {
        if !matches!(arg, IRExpr::Keyword { .. }) {
            let ty = argument_type(arg, ctx);
            let named = |t: &IRType| {
                crate::ir::collections::spec::variant(method_name, t)
                    .filter(|n| has(ctx, &class, n))
            };
            if let Some(n) = named(&ty).or_else(|| fitting_variant(ctx, &class, method_name, &ty)) {
                return Some(call(object, &n, vec![arg.clone()]));
            }
            if let IRType::Class(c) = &ty {
                if let (Some(vt), Some(view)) = (view_type(ctx, c), view_of(arg, ctx)) {
                    if let Some(n) = named(&vt) {
                        return Some(call(object, &n, vec![view]));
                    }
                }
            }
            if has(ctx, &class, method_name) {
                return None;
            }
            let shown = match method_name.as_str() {
                m if m == crate::ir::FROM_METHOD => format!("{class}()"),
                m => format!("{class}.{m}()"),
            };
            ctx.report(format!(
                "{shown} of a '{}' is not supported",
                crate::type_to_string(&ty)
            ));
            return Some((**object).clone());
        }
    }
    None
}

/// What an argument is, the literals of ints and bools included, which
/// [`infer_value_type`] leaves to their emission.
fn argument_type(expr: &IRExpr, ctx: &CompilationContext) -> IRType {
    let one = |items: &[IRExpr]| {
        let types: Vec<IRType> = items.iter().map(|i| argument_type(i, ctx)).collect();
        match types.first() {
            Some(first) if types.iter().all(|t| t == first) => first.clone(),
            _ => IRType::Unknown,
        }
    };
    match expr {
        IRExpr::Const(crate::ir::IRConstant::Int(_)) => IRType::Int,
        IRExpr::Const(crate::ir::IRConstant::Bool(_)) => IRType::Bool,
        IRExpr::ListLiteral(items) if !items.is_empty() => IRType::List(Box::new(one(items))),
        IRExpr::SetLiteral(items) if !items.is_empty() => IRType::Set(Box::new(one(items))),
        IRExpr::TupleLiteral(items) => {
            IRType::Tuple(items.iter().map(|i| argument_type(i, ctx)).collect())
        }
        IRExpr::DictLiteral(pairs) if !pairs.is_empty() => {
            let keys: Vec<IRExpr> = pairs.iter().map(|(k, _)| k.clone()).collect();
            let values: Vec<IRExpr> = pairs.iter().map(|(_, v)| v.clone()).collect();
            IRType::Dict(Box::new(one(&keys)), Box::new(one(&values)))
        }
        IRExpr::UnaryOp {
            operand,
            op: IRUnaryOp::Neg | IRUnaryOp::UAdd,
        } if matches!(**operand, IRExpr::Const(_)) => argument_type(operand, ctx),
        other => infer_value_type(other, ctx),
    }
}

/// The one variant of `method` whose argument type `actual` fits where the
/// value's elements are not typed until they are emitted (`[1, 2]`, whose
/// ints are words until then): an untyped word fits an int or a bool.
fn fitting_variant(
    ctx: &CompilationContext,
    class: &str,
    method: &str,
    actual: &IRType,
) -> Option<String> {
    fn fits(a: &IRType, w: &IRType) -> bool {
        match (a, w) {
            _ if a == w => true,
            (IRType::Unknown | IRType::Any, IRType::Int | IRType::Bool) => true,
            (IRType::List(x), IRType::List(y))
            | (IRType::Set(x), IRType::Set(y))
            | (IRType::VarTuple(x), IRType::VarTuple(y)) => fits(x, y),
            (IRType::Dict(a, b), IRType::Dict(c, d)) => fits(a, c) && fits(b, d),
            (IRType::Tuple(xs), IRType::Tuple(ys)) => {
                xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| fits(x, y))
            }
            _ => false,
        }
    }
    let prefix = format!("{method}<");
    let variants: Vec<(&String, IRType)> = ctx
        .get_class_info(class)?
        .methods
        .keys()
        .filter_map(|m| {
            let w = m
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix('>'))
                .and_then(crate::ir::collections::spec::decode)?;
            Some((m, w))
        })
        .collect();
    let only = |found: Vec<&String>| match found.as_slice() {
        [one] => Some((*one).clone()),
        _ => None,
    };
    let exact: Vec<&String> = variants
        .iter()
        .filter(|(_, w)| fits(actual, w))
        .map(|(m, _)| *m)
        .collect();
    if !exact.is_empty() {
        return only(exact);
    }
    // An int is taken where a float is, as an argument is.
    if matches!(actual, IRType::Int | IRType::Bool) {
        return only(
            variants
                .iter()
                .filter(|(_, w)| *w == IRType::Float)
                .map(|(m, _)| *m)
                .collect(),
        );
    }
    None
}

/// `od.popitem(last=False)` and `od.move_to_end(k, last=False)`: the
/// keyword in its place.
fn place_last(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::MethodCall {
        object,
        method_name,
        arguments,
    } = expr
    else {
        return None;
    };
    let at = match method_name.as_str() {
        "popitem" => 0,
        "move_to_end" => 1,
        _ => return None,
    };
    let (Some(IRExpr::Keyword { name, value }), Some(class)) =
        (arguments.last(), class_of(object, ctx))
    else {
        return None;
    };
    if name != "last" || !has(ctx, &class, method_name) || arguments.len() != at + 1 {
        return None;
    }
    let mut placed = arguments[..at].to_vec();
    placed.push((**value).clone());
    Some(call(object, method_name, placed))
}

/// The builtin functions that walk an argument, which take a viewed instance
/// as the value it reads as.
const WALKING_BUILTINS: &[&str] = &[
    "sum",
    "min",
    "max",
    "sorted",
    "enumerate",
    "zip",
    "set",
    "any",
    "all",
    "reversed",
    "__sorted_desc",
    "dict",
    "list",
    "tuple",
    "frozenset",
];

/// Arguments with each viewed instance replaced by its view, and each
/// `reversed()` by the list it walks, when any is.
fn view_arguments(arguments: &[IRExpr], ctx: &CompilationContext) -> Option<Vec<IRExpr>> {
    let views: Vec<Option<IRExpr>> = arguments
        .iter()
        .map(|a| match a {
            IRExpr::Keyword { .. } => None,
            other => reversed_walk(other, ctx).or_else(|| view_of(other, ctx)),
        })
        .collect();
    if views.iter().all(Option::is_none) {
        return None;
    }
    Some(
        views
            .into_iter()
            .zip(arguments)
            .map(|(view, a)| view.unwrap_or_else(|| a.clone()))
            .collect(),
    )
}

/// An instance read as its view wherever CPython reads it as the builtin
/// value it is (or, for a dict or list subclass, is a subclass of).
fn rewrite_view(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    match expr {
        IRExpr::Indexing { container, index } => {
            let class = class_of(container, ctx)?;
            if has(ctx, &class, "__getitem__") {
                return None;
            }
            Some(IRExpr::Indexing {
                container: Box::new(view_of(container, ctx)?),
                index: index.clone(),
            })
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            let class = class_of(container, ctx)?;
            if has(ctx, &class, "__getitem__") {
                return None;
            }
            Some(IRExpr::Slicing {
                container: Box::new(view_of(container, ctx)?),
                start: start.clone(),
                end: end.clone(),
                step: step.clone(),
            })
        }
        IRExpr::CompareOp { left, right, op } => {
            let (lt, rt) = (infer_value_type(left, ctx), infer_value_type(right, ctx));
            let named = |t: &IRType| matches!(t, IRType::Class(c) if is_named_tuple(ctx, c));
            match op {
                IRCompareOp::In | IRCompareOp::NotIn => {
                    let class = class_of(right, ctx)?;
                    if has(ctx, &class, "__contains__") {
                        return None;
                    }
                    Some(IRExpr::CompareOp {
                        left: left.clone(),
                        right: Box::new(view_of(right, ctx)?),
                        op: op.clone(),
                    })
                }
                IRCompareOp::Is | IRCompareOp::IsNot => None,
                // A named tuple against a tuple or another named tuple, as
                // tuples compare.
                _ if (named(&lt) || named(&rt)) && tuple_like(&lt, ctx) && tuple_like(&rt, ctx) => {
                    Some(IRExpr::CompareOp {
                        left: Box::new(viewed_tuple(left, ctx)),
                        right: Box::new(viewed_tuple(right, ctx)),
                        op: op.clone(),
                    })
                }
                // An instance compared for equality with a builtin value, or
                // with an instance whose class has no `__eq__` of its own:
                // as the value it reads as.
                IRCompareOp::Eq | IRCompareOp::NotEq => {
                    let side = |e: &IRExpr, t: &IRType, other: &IRType| match t {
                        IRType::Class(c)
                            if view_kind(ctx, c).is_some()
                                && (!matches!(other, IRType::Class(_))
                                    || !has(ctx, c, "__eq__")) =>
                        {
                            view_of(e, ctx)
                        }
                        _ => None,
                    };
                    let (l, r) = (side(left, &lt, &rt), side(right, &rt, &lt));
                    if l.is_none() && r.is_none() {
                        return None;
                    }
                    Some(IRExpr::CompareOp {
                        left: Box::new(l.unwrap_or_else(|| (**left).clone())),
                        right: Box::new(r.unwrap_or_else(|| (**right).clone())),
                        op: op.clone(),
                    })
                }
                _ => None,
            }
        }
        // A named tuple concatenated or repeated is its tuple's.
        IRExpr::BinaryOp { left, right, op } if matches!(op, IROp::Add | IROp::Mul) => {
            let tuple_of = |e: &IRExpr| match class_of(e, ctx) {
                Some(c) if is_named_tuple(ctx, &c) => view_of(e, ctx),
                _ => None,
            };
            let (l, r) = (tuple_of(left), tuple_of(right));
            if l.is_none() && r.is_none() {
                return None;
            }
            Some(IRExpr::BinaryOp {
                left: Box::new(l.unwrap_or_else(|| (**left).clone())),
                right: Box::new(r.unwrap_or_else(|| (**right).clone())),
                op: op.clone(),
            })
        }
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => {
            if let Some(class) = class_of(object, ctx) {
                let kind = view_kind(ctx, &class)?;
                if has(ctx, &class, method_name) {
                    return None;
                }
                if method_name == "_replace" && is_named_tuple(ctx, &class) {
                    return replace(object, &class, arguments, ctx);
                }
                // A method the class does not define is its view's, for the
                // ones the class names as its view's.
                let _ = kind;
                if !delegates(ctx, &class, method_name) {
                    return None;
                }
                return Some(call(&view_of(object, ctx)?, method_name, arguments.clone()));
            }
            // A builtin method that walks its argument.
            if !matches!(method_name.as_str(), "join" | "extend" | "update") {
                return None;
            }
            Some(IRExpr::MethodCall {
                object: object.clone(),
                method_name: method_name.clone(),
                arguments: view_arguments(arguments, ctx)?,
            })
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            if ctx.get_function_info(function_name).is_some() {
                return None;
            }
            if function_name == "isinstance" {
                // `isinstance(c, Counter)`: whether `c`'s class is that
                // `collections` class, whatever it holds.
                if let [value, IRExpr::Variable(t)] = arguments.as_slice() {
                    if let Some((module, name)) = ctx.stdlib_imports.get(t) {
                        if module == "collections" && order_free(value) {
                            let wanted = crate::ir::collections::spec::Kind::named(name)?;
                            let is = class_of(value, ctx)
                                .and_then(|c| crate::ir::collections::spec::kind_of_class(&c))
                                == Some(wanted);
                            return Some(IRExpr::Const(crate::ir::IRConstant::Bool(is)));
                        }
                    }
                }
                // A named tuple is a tuple, and a dict-like `collections`
                // class a dict.
                if let [value, IRExpr::Variable(t)] = arguments.as_slice() {
                    let ty = view_type(ctx, &class_of(value, ctx)?)?;
                    let is = match (t.as_str(), &ty) {
                        ("tuple", IRType::Tuple(_)) => true,
                        ("dict", IRType::Dict(..)) => {
                            view_kind(ctx, &class_of(value, ctx)?) == Some(View::Shared)
                        }
                        _ => return None,
                    };
                    if order_free(value) {
                        return Some(IRExpr::Const(crate::ir::IRConstant::Bool(is)));
                    }
                }
                return None;
            }
            if function_name == "len" {
                let class = class_of(arguments.first()?, ctx)?;
                if has(ctx, &class, "__len__") {
                    return None;
                }
                return Some(IRExpr::FunctionCall {
                    function_name: function_name.clone(),
                    arguments: vec![view_of(&arguments[0], ctx)?],
                });
            }
            if !WALKING_BUILTINS.contains(&function_name.as_str()) {
                return None;
            }
            Some(IRExpr::FunctionCall {
                function_name: function_name.clone(),
                arguments: view_arguments(arguments, ctx)?,
            })
        }
        IRExpr::Comprehension {
            kind,
            element,
            value,
            generators,
        } => {
            // An instance iterated, or a named tuple unpacked from each
            // element.
            let views: Vec<Option<IRExpr>> = generators
                .iter()
                .map(|g| {
                    reversed_walk(&g.iterable, ctx)
                        .or_else(|| view_of(&g.iterable, ctx))
                        .or_else(|| {
                            if g.targets.len() < 2 {
                                return None;
                            }
                            let class = match infer_value_type(&g.iterable, ctx) {
                                IRType::List(e) | IRType::VarTuple(e) | IRType::Set(e) => {
                                    match *e {
                                        IRType::Class(c) if is_named_tuple(ctx, &c) => c,
                                        _ => return None,
                                    }
                                }
                                _ => return None,
                            };
                            let each = format!("__nt_{class}_{}", g.targets.join("_"));
                            Some(IRExpr::Comprehension {
                                kind: crate::ir::IRComprehensionKind::List,
                                element: Box::new(call(
                                    &IRExpr::Variable(each.clone()),
                                    crate::ir::VIEW_METHOD,
                                    Vec::new(),
                                )),
                                value: None,
                                generators: vec![crate::ir::IRGenerator {
                                    targets: vec![each],
                                    iterable: g.iterable.clone(),
                                    conditions: Vec::new(),
                                }],
                            })
                        })
                })
                .collect();
            if views.iter().all(Option::is_none) {
                return None;
            }
            Some(IRExpr::Comprehension {
                kind: *kind,
                element: element.clone(),
                value: value.clone(),
                generators: generators
                    .iter()
                    .zip(views)
                    .map(|(g, view)| crate::ir::IRGenerator {
                        targets: g.targets.clone(),
                        iterable: view.unwrap_or_else(|| g.iterable.clone()),
                        conditions: g.conditions.clone(),
                    })
                    .collect(),
            })
        }
        _ => None,
    }
}

/// `p._replace(x=1)`: a new instance with the named fields replaced and the
/// others read from `p`, which must be a plain path since it is read once per
/// field.
fn replace(
    object: &IRExpr,
    class: &str,
    arguments: &[IRExpr],
    ctx: &CompilationContext,
) -> Option<IRExpr> {
    let fields: Vec<String> = ctx
        .get_function_info(&format!("{class}::__init__"))
        .map(|_| ())
        .and_then(|_| named_tuple_fields(ctx, class))?;
    let mut given: Vec<(String, IRExpr)> = Vec::new();
    for a in arguments {
        match a {
            IRExpr::Keyword { name, value } => given.push((name.clone(), (**value).clone())),
            _ => {
                ctx.report(format!(
                    "{class}._replace() takes only keyword arguments, as CPython's does"
                ));
                return Some(IRExpr::Const(crate::ir::IRConstant::None));
            }
        }
    }
    if let Some((bad, _)) = given.iter().find(|(n, _)| !fields.contains(n)) {
        ctx.report(format!("Got unexpected field names: ['{bad}']"));
        return Some(IRExpr::Const(crate::ir::IRConstant::None));
    }
    if !matches!(object, IRExpr::Variable(_) | IRExpr::Param(_)) && given.len() < fields.len() {
        ctx.report(format!(
            "{class}._replace() on an expression is not supported. Hint: assign the instance to              a variable first"
        ));
        return Some(IRExpr::Const(crate::ir::IRConstant::None));
    }
    let arguments = fields
        .iter()
        .map(|f| match given.iter().find(|(n, _)| n == f) {
            Some((_, value)) => value.clone(),
            None => IRExpr::Attribute {
                object: Box::new(object.clone()),
                attribute: f.clone(),
            },
        })
        .collect();
    Some(IRExpr::FunctionCall {
        function_name: class.to_string(),
        arguments,
    })
}

/// A named tuple's fields, in order: its `_fields`.
pub(crate) fn named_tuple_fields(ctx: &CompilationContext, class: &str) -> Option<Vec<String>> {
    match ctx.get_class_info(class)?.class_var_values.get("_fields")? {
        IRExpr::TupleLiteral(items) => items
            .iter()
            .map(|i| match i {
                IRExpr::Const(crate::ir::IRConstant::String(s)) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// `right.reflected(left)`, which evaluates the right operand first, when that
/// order cannot be told apart from CPython's.
fn reflected(left: &IRExpr, right: &IRExpr, method: &str) -> Option<IRExpr> {
    (order_free(left) || order_free(right)).then(|| call(right, method, vec![left.clone()]))
}

/// The binary operator `left op right` as the method call it is, when either
/// operand is an instance whose class defines the method.
fn rewrite_binary(
    left: &IRExpr,
    right: &IRExpr,
    op: &IROp,
    ctx: &CompilationContext,
) -> Option<IRExpr> {
    let (own, refl, _) = binary_methods(op);
    // A method with variants by its argument's type is one too.
    let has = |ctx: &CompilationContext, class: &str, method: &str| {
        has(ctx, class, method) || has_variants(ctx, class, method)
    };
    let somewhere = |m: &str| {
        let prefix = format!("{m}<");
        ctx.class_map
            .values()
            .any(|ci| ci.methods.keys().any(|k| k == m || k.starts_with(&prefix)))
    };
    if !somewhere(own) && !somewhere(refl) {
        return None;
    }
    let lc = class_of(left, ctx);
    let rc = class_of(right, ctx);
    // A subclass's reflected method goes first when it overrides the left's.
    if let (Some(l), Some(r)) = (&lc, &rc) {
        if l != r && ctx.is_class_or_subclass(r, l) && has(ctx, r, refl) {
            let mine = ctx.get_class_info(r).and_then(|ci| ci.methods.get(refl));
            let theirs = ctx.get_class_info(l).and_then(|ci| ci.methods.get(refl));
            if mine != theirs {
                return reflected(left, right, refl);
            }
        }
    }
    if let Some(l) = &lc {
        if has(ctx, l, own) {
            return Some(call(left, own, vec![right.clone()]));
        }
    }
    if let Some(r) = &rc {
        if has(ctx, r, refl) {
            return reflected(left, right, refl);
        }
    }
    None
}

/// `expr` as the special-method call it means, when it is one.
pub(crate) fn rewrite(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    if let Some(logged) = rewrite_log_expr(expr, ctx) {
        return Some(logged);
    }
    if let Some(compared) = rewrite_optional_text_compare(expr, ctx) {
        return Some(compared);
    }
    if let Some(built) = rewrite_dict_call(expr, ctx).or_else(|| rewrite_iter_seq(expr, ctx)) {
        return Some(built);
    }
    if let Some(unwrapped) = rewrite_optional_receiver(expr, ctx) {
        return Some(unwrapped);
    }
    if let Some(placed) = place_method_keywords(expr, ctx) {
        return Some(placed);
    }
    if let Some(filled) = fill_method_defaults(expr, ctx) {
        return Some(filled);
    }
    if let Some(walked) = rewrite_reversed_uses(expr, ctx) {
        return Some(walked);
    }
    // A method with variants by its argument's type, whichever class has it.
    if let IRExpr::MethodCall { method_name, .. } = expr {
        let prefix = format!("{method_name}<");
        let has_any = ctx
            .class_map
            .values()
            .any(|ci| ci.methods.keys().any(|k| k.starts_with(&prefix)));
        if has_any {
            if let Some(chosen) = rewrite_variant(expr, ctx) {
                return Some(chosen);
            }
        }
    }
    if any_view(ctx) {
        if let Some(chosen) = rewrite_variant(expr, ctx).or_else(|| place_last(expr, ctx)) {
            return Some(chosen);
        }
        if let Some(view) = rewrite_view(expr, ctx) {
            return Some(view);
        }
    }
    match expr {
        IRExpr::BinaryOp { left, right, op } => rewrite_binary(left, right, op, ctx),
        IRExpr::UnaryOp { operand, op } => {
            let method = match op {
                IRUnaryOp::Neg => "__neg__",
                IRUnaryOp::UAdd => "__pos__",
                IRUnaryOp::Invert => "__invert__",
                IRUnaryOp::Not => return None,
            };
            if !defined_anywhere(ctx, method) {
                return None;
            }
            let class = class_of(operand, ctx)?;
            has(ctx, &class, method).then(|| call(operand, method, Vec::new()))
        }
        IRExpr::CompareOp {
            left,
            right,
            op: op @ (IRCompareOp::In | IRCompareOp::NotIn),
        } => {
            if !defined_anywhere(ctx, "__contains__") {
                return None;
            }
            let class = class_of(right, ctx)?;
            if !has(ctx, &class, "__contains__") {
                return None;
            }
            let test = reflected(left, right, "__contains__")?;
            Some(match op {
                IRCompareOp::NotIn => IRExpr::UnaryOp {
                    operand: Box::new(test),
                    op: IRUnaryOp::Not,
                },
                _ => test,
            })
        }
        IRExpr::Indexing { container, index } => {
            let class = class_of(container, ctx)?;
            // A `__getitem__` of its own, or variants of one by key type.
            (has(ctx, &class, "__getitem__") || has_variants(ctx, &class, "__getitem__"))
                .then(|| call(container, "__getitem__", vec![(**index).clone()]))
        }
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            // Calling an instance held in a variable is its `__call__`.
            if ctx.get_function_info(function_name).is_none() && defined_anywhere(ctx, "__call__") {
                let var = IRExpr::Variable(function_name.clone());
                if let Some(class) = class_of(&var, ctx) {
                    if has(ctx, &class, "__call__") {
                        return Some(call(&var, "__call__", arguments.clone()));
                    }
                }
            }
            // `format(x, spec)`, an f-string's `{x:spec}` too, is the
            // instance's `__format__(spec)`.
            if function_name == "format"
                && arguments.len() == 2
                && defined_anywhere(ctx, "__format__")
            {
                let class = class_of(&arguments[0], ctx)?;
                return has(ctx, &class, "__format__")
                    .then(|| call(&arguments[0], "__format__", vec![arguments[1].clone()]));
            }
            let method = match function_name.as_str() {
                "abs" => "__abs__",
                "int" => "__int__",
                "float" => "__float__",
                "hash" => "__hash__",
                "round" if arguments.len() == 1 => "__round__",
                _ => return None,
            };
            if arguments.len() != 1 || !defined_anywhere(ctx, method) {
                return None;
            }
            let class = class_of(&arguments[0], ctx)?;
            has(ctx, &class, method).then(|| call(&arguments[0], method, Vec::new()))
        }
        _ => None,
    }
}

/// A method called on, or a field read of, an optional instance: of the
/// instance, after CPython's AttributeError when it is None.
fn rewrite_optional_receiver(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    // `x[k]` of an optional instance: of the instance, after CPython's
    // TypeError when it is None.
    if let IRExpr::Indexing { container, index } = expr {
        let IRType::Optional(inner) = infer_value_type(container, ctx) else {
            return None;
        };
        if !matches!(*inner, IRType::Class(_)) {
            return None;
        }
        return Some(IRExpr::Indexing {
            container: Box::new(IRExpr::FunctionCall {
                function_name: crate::ir::SOME_FN.to_string(),
                arguments: vec![
                    (**container).clone(),
                    IRExpr::Const(crate::ir::IRConstant::String(
                        "'NoneType' object is not subscriptable".to_string(),
                    )),
                    IRExpr::Const(crate::ir::IRConstant::String("TypeError".to_string())),
                ],
            }),
            index: index.clone(),
        });
    }
    let (object, name) = match expr {
        IRExpr::MethodCall {
            object,
            method_name,
            ..
        } => (object, method_name),
        IRExpr::Attribute { object, attribute } => (object, attribute),
        _ => return None,
    };
    let IRType::Optional(inner) = infer_value_type(object, ctx) else {
        return None;
    };
    // An instance's attributes and methods, and a string's methods.
    let text_method = matches!(*inner, IRType::String | IRType::Bytes)
        && matches!(expr, IRExpr::MethodCall { .. });
    if !matches!(*inner, IRType::Class(_)) && !text_method {
        return None;
    }
    let some = IRExpr::FunctionCall {
        function_name: crate::ir::SOME_FN.to_string(),
        arguments: vec![
            (**object).clone(),
            IRExpr::Const(crate::ir::IRConstant::String(name.clone())),
        ],
    };
    Some(match expr {
        IRExpr::MethodCall {
            method_name,
            arguments,
            ..
        } => IRExpr::MethodCall {
            object: Box::new(some),
            method_name: method_name.clone(),
            arguments: arguments.clone(),
        },
        _ => IRExpr::Attribute {
            object: Box::new(some),
            attribute: name.clone(),
        },
    })
}

/// A method call with keyword arguments, each placed at its parameter (and
/// the parameters left out given their defaults), as CPython binds them. Only
/// the receiver's class says which method it is, so the IR cannot. A method
/// of a builtin value takes none here.
fn place_method_keywords(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::MethodCall {
        object,
        method_name,
        arguments,
    } = expr
    else {
        return None;
    };
    let keywords: Vec<(&String, &IRExpr)> = arguments
        .iter()
        .filter_map(|a| match a {
            IRExpr::Keyword { name, value } => Some((name, value.as_ref())),
            _ => None,
        })
        .collect();
    // `super().m(...)` is placed against the method it reaches, where its
    // call is emitted.
    let on_super = matches!(object.as_ref(),
        IRExpr::FunctionCall { function_name, arguments } if function_name == "super" && arguments.is_empty());
    if keywords.is_empty()
        || on_super
        || crate::compiler::expression::stdlib_call_name(ctx, object).is_some()
        || method_name == "format"
    {
        return None;
    }
    let (class, through_class) = match infer_value_type(object, ctx) {
        IRType::Class(c) => (c, false),
        IRType::ClassRef(c) => (c, true),
        _ => {
            // A builtin value's method: its keywords were refused by name.
            if matches!(method_name.as_str(), "_replace" | "popitem" | "move_to_end") {
                return None;
            }
            ctx.report(format!(
                "keyword argument '{}' is not supported on '{method_name}()'. Hint: pass the \
                 arguments positionally",
                keywords[0].0
            ));
            return Some(call(
                object,
                method_name,
                arguments
                    .iter()
                    .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
                    .cloned()
                    .collect(),
            ));
        }
    };
    let info = ctx.get_class_info(&class)?;
    let owner = info.method_owner.get(method_name)?;
    let qualified = format!("{owner}::{method_name}");
    let names = ctx.method_params.get(&qualified)?;
    let defaults = ctx.method_defaults.get(&qualified)?;
    let implicit = match info.method_kinds.get(method_name) {
        Some(MethodKind::Static) => 0,
        Some(MethodKind::Class) => 1,
        _ if through_class => 0,
        _ => 1,
    };
    let positional: Vec<&IRExpr> = arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect();
    let params = names.get(implicit..)?;
    let defaults = defaults.get(implicit..)?;
    let shown = format!(
        "{}.{method_name}()",
        class.rsplit('.').next().unwrap_or(&class)
    );
    if let Some((bad, _)) = keywords.iter().find(|(k, _)| !params.contains(k)) {
        ctx.report(format!(
            "{shown} got an unexpected keyword argument '{bad}'"
        ));
        return None;
    }
    if let Some((dup, _)) = keywords
        .iter()
        .find(|(k, _)| params[..positional.len().min(params.len())].contains(k))
    {
        ctx.report(format!("{shown} got multiple values for argument '{dup}'"));
        return None;
    }
    let mut placed: Vec<IRExpr> = positional.iter().map(|e| (*e).clone()).collect();
    let mut order: Vec<usize> = Vec::new();
    for (i, name) in params.iter().enumerate().skip(positional.len()) {
        match keywords.iter().position(|(k, _)| *k == name) {
            Some(at) => {
                placed.push(keywords[at].1.clone());
                order.push(at);
            }
            None => match &defaults[i] {
                Some(d) if constant_default(d) => placed.push(d.clone()),
                Some(_) => {
                    ctx.report(format!(
                        "the default of '{name}' in {shown} is not a constant, and CPython \
                         evaluates it once, when the method is defined. Hint: pass it"
                    ));
                    return None;
                }
                None => {
                    ctx.report(format!("{shown} missing required argument: '{name}'"));
                    return None;
                }
            },
        }
    }
    // Keywords are evaluated in the order written; placed in another order,
    // they must be values that cannot tell.
    let reordered = order.windows(2).any(|w| w[0] > w[1]);
    if reordered && keywords.iter().any(|(_, v)| !order_free(v)) {
        ctx.report(format!(
            "the keyword arguments of {shown} are computed and written out of the parameters' \
             order, which would change the order they run in. Hint: write them in order, or \
             assign them to variables first"
        ));
        return None;
    }
    Some(call(object, method_name, placed))
}

/// A method call that leaves out parameters with defaults, given them. Only
/// the receiver's class says which method it is, so the IR's own call-site
/// defaults cannot. A default is evaluated once in CPython, when the `def`
/// runs, so only a constant one can be given again at each call.
fn fill_method_defaults(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::MethodCall {
        object,
        method_name,
        arguments,
    } = expr
    else {
        return None;
    };
    if arguments
        .iter()
        .any(|a| matches!(a, IRExpr::Keyword { .. }))
    {
        return None;
    }
    let (class, through_class) = match infer_value_type(object, ctx) {
        IRType::Class(c) => (c, false),
        IRType::ClassRef(c) => (c, true),
        _ => return None,
    };
    let info = ctx.get_class_info(&class)?;
    let owner = info.method_owner.get(method_name)?;
    let defaults = ctx
        .method_defaults
        .get(&format!("{owner}::{method_name}"))?;
    let implicit = match info.method_kinds.get(method_name) {
        Some(MethodKind::Static) => 0,
        Some(MethodKind::Class) => 1,
        // Through the class, an instance method takes `self` explicitly.
        _ if through_class => 0,
        _ => 1,
    };
    let rest = defaults.get(implicit..)?;
    if arguments.len() >= rest.len() {
        return None;
    }
    let mut filled = arguments.clone();
    for default in &rest[arguments.len()..] {
        match default {
            Some(d) if constant_default(d) => filled.push(d.clone()),
            Some(_) => {
                ctx.report(format!(
                    "the default of a parameter of '{class}.{method_name}' is not a constant, \
                     and CPython evaluates it once, when the method is defined. Hint: pass \
                     the argument"
                ));
                return None;
            }
            None => return None,
        }
    }
    Some(call(object, method_name, filled))
}

fn constant_default(expr: &IRExpr) -> bool {
    match expr {
        IRExpr::Const(_) => true,
        IRExpr::UnaryOp { operand, .. } => constant_default(operand),
        IRExpr::TupleLiteral(items) => items.iter().all(constant_default),
        _ => false,
    }
}

/// `reversed(x)` where it is walked (a loop, a comprehension, a builtin that
/// iterates its argument): the elements from the last, as a list built by
/// indexing from the end. Anywhere else it is CPython's iterator object,
/// which is refused as a call of an unknown builtin.
fn reversed_walk(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::FunctionCall {
        function_name,
        arguments,
    } = expr
    else {
        return None;
    };
    if function_name != "reversed" || ctx.get_function_info(function_name).is_some() {
        return None;
    }
    let [arg] = arguments.as_slice() else {
        return None;
    };
    let seq = match infer_value_type(arg, ctx) {
        IRType::List(_)
        | IRType::VarTuple(_)
        | IRType::Tuple(_)
        | IRType::String
        | IRType::Range => arg.clone(),
        IRType::Dict(..) => call(arg, "keys", Vec::new()),
        IRType::Class(_) => {
            let view = view_of(arg, ctx)?;
            return reversed_walk(
                &IRExpr::FunctionCall {
                    function_name: function_name.clone(),
                    arguments: vec![view],
                },
                ctx,
            );
        }
        _ => return None,
    };
    // Read twice (its length, then each element), so it has to be a path.
    fn plain(e: &IRExpr) -> bool {
        match e {
            IRExpr::Variable(_) | IRExpr::Param(_) | IRExpr::Const(_) => true,
            IRExpr::RangeCall { start, stop, step } => {
                plain(stop) && [start, step].into_iter().flatten().all(|p| plain(p))
            }
            IRExpr::UnaryOp { operand, .. } => plain(operand),
            IRExpr::Attribute { object, .. } => plain(object),
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } => {
                arguments.is_empty()
                    && matches!(
                        method_name.as_str(),
                        crate::ir::DATA_METHOD | crate::ir::VIEW_METHOD | "keys"
                    )
                    && plain(object)
            }
            _ => false,
        }
    }
    if !plain(&seq) {
        ctx.report(
            "reversed() of an expression is not supported here. Hint: assign the sequence to a \
             variable first",
        );
        return None;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{seq:?}").hash(&mut h);
    let i = format!("__rev_{:x}", h.finish());
    let int = |n: i32| IRExpr::Const(crate::ir::IRConstant::Int(n));
    Some(IRExpr::Comprehension {
        kind: crate::ir::IRComprehensionKind::List,
        element: Box::new(IRExpr::Indexing {
            container: Box::new(seq.clone()),
            index: Box::new(IRExpr::Variable(i.clone())),
        }),
        value: None,
        generators: vec![crate::ir::IRGenerator {
            targets: vec![i],
            iterable: IRExpr::RangeCall {
                start: Some(Box::new(IRExpr::BinaryOp {
                    left: Box::new(IRExpr::FunctionCall {
                        function_name: "len".to_string(),
                        arguments: vec![seq],
                    }),
                    right: Box::new(int(1)),
                    op: IROp::Sub,
                })),
                stop: Box::new(int(-1)),
                step: Some(Box::new(int(-1))),
            },
            conditions: Vec::new(),
        }],
    })
}

/// The places that walk a `reversed()`: a comprehension's iterables, and the
/// arguments of the builtins and methods that iterate theirs.
fn rewrite_reversed_uses(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let replaced = |items: &[IRExpr]| -> Option<Vec<IRExpr>> {
        let walks: Vec<Option<IRExpr>> = items.iter().map(|a| reversed_walk(a, ctx)).collect();
        if walks.iter().all(Option::is_none) {
            return None;
        }
        Some(
            walks
                .into_iter()
                .zip(items)
                .map(|(w, a)| w.unwrap_or_else(|| a.clone()))
                .collect(),
        )
    };
    match expr {
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if WALKING_BUILTINS.contains(&function_name.as_str())
            && ctx.get_function_info(function_name).is_none() =>
        {
            Some(IRExpr::FunctionCall {
                function_name: function_name.clone(),
                arguments: replaced(arguments)?,
            })
        }
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } if matches!(method_name.as_str(), "join" | "extend") => Some(IRExpr::MethodCall {
            object: object.clone(),
            method_name: method_name.clone(),
            arguments: replaced(arguments)?,
        }),
        IRExpr::Comprehension {
            kind,
            element,
            value,
            generators,
        } => {
            let iterables: Vec<IRExpr> = generators.iter().map(|g| g.iterable.clone()).collect();
            let walked = replaced(&iterables)?;
            Some(IRExpr::Comprehension {
                kind: *kind,
                element: element.clone(),
                value: value.clone(),
                generators: generators
                    .iter()
                    .zip(walked)
                    .map(|(g, iterable)| crate::ir::IRGenerator {
                        targets: g.targets.clone(),
                        iterable,
                        conditions: g.conditions.clone(),
                    })
                    .collect(),
            })
        }
        _ => None,
    }
}

/// `__iter_seq(x)`: `x`'s elements as a sequence a loop indexes.
fn rewrite_iter_seq(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::FunctionCall {
        function_name,
        arguments,
    } = expr
    else {
        return None;
    };
    if function_name != crate::ir::ITER_SEQ_FN {
        return None;
    }
    let [x] = arguments.as_slice() else {
        return None;
    };
    Some(match infer_value_type(x, ctx) {
        IRType::Dict(..) => call(x, "keys", Vec::new()),
        IRType::Set(e) => {
            // Named for the element type, so the scan that reserves it and
            // the code that uses it agree.
            let each = format!(
                "__iter_seq_{}",
                crate::ir::collections::spec::encode(&e).unwrap_or_default()
            );
            IRExpr::Comprehension {
                kind: crate::ir::IRComprehensionKind::List,
                element: Box::new(IRExpr::Variable(each.clone())),
                value: None,
                generators: vec![crate::ir::IRGenerator {
                    targets: vec![each],
                    iterable: x.clone(),
                    conditions: Vec::new(),
                }],
            }
        }
        IRType::Class(_) => match view_of(x, ctx) {
            Some(view) => IRExpr::FunctionCall {
                function_name: function_name.clone(),
                arguments: vec![view],
            },
            None => x.clone(),
        },
        _ => x.clone(),
    })
}

/// `dict(x)`: a copy of a dict (or of the dict a `collections` class reads
/// as), a dict of a list of pairs, or of keyword arguments.
fn rewrite_dict_call(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::FunctionCall {
        function_name,
        arguments,
    } = expr
    else {
        return None;
    };
    if function_name != "dict"
        || ctx.get_function_info(function_name).is_some()
        || ctx.get_local_info(function_name).is_some()
    {
        return None;
    }
    if !arguments.is_empty()
        && arguments
            .iter()
            .all(|a| matches!(a, IRExpr::Keyword { .. }))
    {
        return Some(IRExpr::DictLiteral(
            arguments
                .iter()
                .filter_map(|a| match a {
                    IRExpr::Keyword { name, value } => Some((
                        IRExpr::Const(crate::ir::IRConstant::String(name.clone())),
                        (**value).clone(),
                    )),
                    _ => None,
                })
                .collect(),
        ));
    }
    let [arg] = arguments.as_slice() else {
        return None;
    };
    match infer_value_type(arg, ctx) {
        IRType::Dict(..) => Some(call(arg, "copy", Vec::new())),
        IRType::Class(c) if matches!(view_type(ctx, &c), Some(IRType::Dict(..))) => {
            Some(call(&view_of(arg, ctx)?, "copy", Vec::new()))
        }
        IRType::List(e) | IRType::VarTuple(e) if matches!(&*e, IRType::Tuple(ms) if ms.len() == 2) =>
        {
            let pair = format!(
                "__dict_pair_{}",
                crate::ir::collections::spec::encode(&e).unwrap_or_default()
            );
            let at = |i: i32| IRExpr::Indexing {
                container: Box::new(IRExpr::Variable(pair.clone())),
                index: Box::new(IRExpr::Const(crate::ir::IRConstant::Int(i))),
            };
            Some(IRExpr::Comprehension {
                kind: crate::ir::IRComprehensionKind::Dict,
                element: Box::new(at(0)),
                value: Some(Box::new(at(1))),
                generators: vec![crate::ir::IRGenerator {
                    targets: vec![pair.clone()],
                    iterable: arg.clone(),
                    conditions: Vec::new(),
                }],
            })
        }
        _ => None,
    }
}

/// `stmt` with its special-method operations as calls, when it has one:
/// `c[k] = v` is `c.__setitem__(k, v)`, and `a += b` on an instance is
/// `a = a.__iadd__(b)`, or `a = a + b` without one.
pub(crate) fn rewrite_statement(
    stmt: &IRStatement,
    ctx: &CompilationContext,
) -> Option<IRStatement> {
    if let IRStatement::Expression(call) = stmt {
        if let Some(logged) = log_parts(call, ctx, true) {
            return Some(logged.statement());
        }
    }
    if let IRStatement::For {
        target,
        iterable,
        body,
        else_body,
    } = stmt
    {
        if let Some(walk) = reversed_walk(iterable, ctx) {
            return Some(IRStatement::For {
                target: target.clone(),
                iterable: walk,
                body: body.clone(),
                else_body: else_body.clone(),
            });
        }
    }
    if any_view(ctx) {
        match stmt {
            IRStatement::For {
                target,
                iterable,
                body,
                else_body,
            } => {
                if let Some(view) = view_of(iterable, ctx) {
                    return Some(IRStatement::For {
                        target: target.clone(),
                        iterable: view,
                        body: body.clone(),
                        else_body: else_body.clone(),
                    });
                }
            }
            IRStatement::TupleUnpack {
                targets,
                value,
                starred,
            } => {
                if let Some(view) = view_of(value, ctx) {
                    return Some(IRStatement::TupleUnpack {
                        targets: targets.clone(),
                        value: view,
                        starred: *starred,
                    });
                }
            }
            // A store into an instance without `__setitem__` goes to the
            // container it holds.
            IRStatement::IndexAssign {
                container,
                index,
                value,
            } => {
                if let Some(class) = class_of(container, ctx) {
                    if !has(ctx, &class, "__setitem__") {
                        if let Some(view) = shared_view_of(container, ctx) {
                            return Some(IRStatement::IndexAssign {
                                container: view,
                                index: index.clone(),
                                value: value.clone(),
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    match stmt {
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            if !defined_anywhere(ctx, "__setitem__") {
                return None;
            }
            let class = class_of(container, ctx)?;
            if !has(ctx, &class, "__setitem__") {
                return None;
            }
            // CPython evaluates the value first, then the container and key.
            if !(order_free(value) || (order_free(container) && order_free(index))) {
                return None;
            }
            Some(IRStatement::Expression(call(
                container,
                "__setitem__",
                vec![index.clone(), value.clone()],
            )))
        }
        IRStatement::AugAssign { target, value, op } => {
            let var = IRExpr::Variable(target.clone());
            let class = class_of(&var, ctx)?;
            let (_, _, inplace) = binary_methods(op);
            let new = if has(ctx, &class, inplace) || has_variants(ctx, &class, inplace) {
                call(&var, inplace, vec![value.clone()])
            } else {
                rewrite_binary(&var, value, op, ctx)?
            };
            Some(IRStatement::Assign {
                target: target.clone(),
                value: new,
                var_type: None,
            })
        }
        IRStatement::AttributeAugAssign {
            object,
            attribute,
            value,
            op,
        } => {
            // The object is read twice, so it has to be a plain path.
            if !matches!(object, IRExpr::Variable(_) | IRExpr::Param(_)) {
                return None;
            }
            let field = IRExpr::Attribute {
                object: Box::new(object.clone()),
                attribute: attribute.clone(),
            };
            let class = class_of(&field, ctx)?;
            let (_, _, inplace) = binary_methods(op);
            let new = if has(ctx, &class, inplace) || has_variants(ctx, &class, inplace) {
                call(&field, inplace, vec![value.clone()])
            } else {
                rewrite_binary(&field, value, op, ctx)?
            };
            Some(IRStatement::AttributeAssign {
                object: object.clone(),
                attribute: attribute.clone(),
                value: new,
                annotation: None,
            })
        }
        _ => None,
    }
}

/// Call `method` of `class` on the instance on the stack, with no other
/// arguments, through the vtable when a subclass overrides it. Answers the
/// method's declared return type, or `None` when the class has no such
/// method.
pub(crate) fn emit_receiver_call(
    func: &mut Function,
    ctx: &CompilationContext,
    class: &str,
    method: &str,
) -> Option<IRType> {
    let info = ctx.get_class_info(class)?;
    let &index = info.methods.get(method)?;
    let owner = info
        .method_owner
        .get(method)
        .cloned()
        .unwrap_or_else(|| class.to_string());
    let ret = ctx
        .get_function_info(&format!("{owner}::{method}"))
        .map(|f| f.return_type.clone())
        .unwrap_or(IRType::Unknown);
    match ctx.virtual_call(class, method) {
        None => emit_user_call(func, ctx, index),
        Some((column, type_index)) => {
            let receiver = crate::compiler::equality::hold(ctx);
            func.instruction(&I::LocalTee(receiver));
            func.instruction(&I::LocalGet(receiver));
            func.instruction(&I::I32Load(MemArg {
                offset: 0,
                align: 2,
                memory_index: 0,
            }));
            func.instruction(&I::I32Const(1));
            func.instruction(&I::I32Sub);
            func.instruction(&I::I32Const(ctx.vtable_stride as i32));
            func.instruction(&I::I32Mul);
            func.instruction(&I::I32Const((ctx.vtable_base + column) as i32));
            func.instruction(&I::I32Add);
            let raises = ctx
                .virtual_implementations(column)
                .any(|f| ctx.can_raise.contains(&f));
            if raises {
                emit_call_depth_step(func, 1);
            }
            func.instruction(&I::CallIndirect {
                type_index,
                table_index: 0,
            });
            if raises {
                emit_call_depth_step(func, -1);
                emit_post_call_check(func, ctx);
            }
            crate::compiler::equality::release(ctx, 1);
        }
    }
    Some(ret)
}

/// `len()` of the instance on the stack: its `__len__`, which CPython
/// requires to be a non-negative int. `None` when the class has none.
pub(crate) fn emit_len(func: &mut Function, ctx: &CompilationContext, class: &str) -> Option<()> {
    let ret = emit_receiver_call(func, ctx, class, "__len__")?;
    check_len_result(func, ctx, &ret);
    Some(())
}

fn check_len_result(func: &mut Function, ctx: &CompilationContext, ret: &IRType) {
    if !matches!(ret, IRType::Int | IRType::Bool) {
        ctx.report(format!(
            "__len__ is declared to return '{}'; CPython requires an int. Hint: annotate it \
             '-> int'",
            crate::type_to_string(ret)
        ));
        if matches!(ret, IRType::String | IRType::Bytes) {
            func.instruction(&I::Drop);
        }
        if matches!(ret, IRType::Float) {
            func.instruction(&I::I32TruncF64S);
        }
        if matches!(ret, IRType::None) {
            func.instruction(&I::I32Const(0));
        }
        return;
    }
    let n = crate::compiler::equality::hold(ctx);
    func.instruction(&I::LocalTee(n));
    func.instruction(&I::I32Const(0));
    func.instruction(&I::I32LtS);
    func.instruction(&I::If(BlockType::Empty));
    emit_raise(
        func,
        ctx,
        "ValueError",
        ExcArg::Text("__len__() should return >= 0"),
        1,
    );
    func.instruction(&I::End);
    func.instruction(&I::LocalGet(n));
    crate::compiler::equality::release(ctx, 1);
}

/// Truth of the instance on the stack: its `__bool__`, else its `__len__`
/// (non-zero), else true. Leaves an i32.
pub(crate) fn emit_truth(func: &mut Function, ctx: &CompilationContext, class: &str) {
    if let Some(ret) = emit_receiver_call(func, ctx, class, "__bool__") {
        if !matches!(ret, IRType::Bool) {
            ctx.report(format!(
                "__bool__ is declared to return '{}'; CPython requires a bool. Hint: annotate \
                 it '-> bool'",
                crate::type_to_string(&ret)
            ));
            if matches!(ret, IRType::String | IRType::Bytes) {
                func.instruction(&I::Drop);
            }
            if matches!(ret, IRType::Float) {
                func.instruction(&I::I32TruncF64S);
            }
            if matches!(ret, IRType::None) {
                func.instruction(&I::I32Const(0));
            }
        }
        return;
    }
    if let Some(ret) = emit_receiver_call(func, ctx, class, "__len__") {
        check_len_result(func, ctx, &ret);
        func.instruction(&I::I32Const(0));
        func.instruction(&I::I32Ne);
        return;
    }
    func.instruction(&I::I32Const(0));
    func.instruction(&I::I32Ne);
}

/// Whether `class` is `logging.Logger` or below it.
fn is_logger(ctx: &CompilationContext, class: &str) -> bool {
    ctx.get_class_info("logging.Logger").is_some()
        && ctx.is_class_or_subclass(class, "logging.Logger")
}

fn is_log_keyword(e: &IRExpr) -> bool {
    matches!(e, IRExpr::Keyword { name, .. }
        if [
            crate::ir::LOG_LINE_ARG,
            crate::ir::LOG_FUNC_ARG,
            crate::ir::LOG_TEXT_ARG,
            crate::ir::LOG_TEXT_ERROR_ARG,
        ]
        .contains(&name.as_str()))
}

/// `text` with each argument placeholder the logging pass left in it
/// replaced by the argument.
fn substitute_arguments(text: &IRExpr, args: &[IRExpr]) -> IRExpr {
    let mut body = crate::ir::IRBody {
        statements: vec![IRStatement::Expression(text.clone())],
    };
    let _ = crate::ir::visit_body(&mut body, &mut |e: &mut IRExpr| {
        if let IRExpr::Variable(name) = e {
            if let Some(k) = name
                .strip_prefix(crate::ir::LOG_ARG_PREFIX)
                .and_then(|k| k.parse::<usize>().ok())
            {
                if let Some(arg) = args.get(k) {
                    *e = arg.clone();
                }
            }
        }
        Ok(())
    });
    match body.statements.pop() {
        Some(IRStatement::Expression(e)) => e,
        _ => text.clone(),
    }
}

/// A logging call, made: what is evaluated first, and the logger, level,
/// and `_log` arguments.
struct Logged {
    setup: Vec<IRStatement>,
    receiver: IRExpr,
    level: IRExpr,
    arguments: Vec<IRExpr>,
}

impl Logged {
    /// The statement: the arguments evaluated, then the record made and
    /// handled only when the logger is enabled for the level, so the
    /// message is formatted only then, as CPython formats it.
    fn statement(self) -> IRStatement {
        let mut statements = self.setup;
        let mut arguments = vec![self.level.clone()];
        arguments.extend(self.arguments);
        statements.push(IRStatement::If {
            condition: call(&self.receiver, "isEnabledFor", vec![self.level]),
            then_body: Box::new(crate::ir::IRBody {
                statements: vec![IRStatement::Expression(call(
                    &self.receiver,
                    crate::ir::LOG_METHOD,
                    arguments,
                ))],
            }),
            else_body: None,
        });
        IRStatement::If {
            condition: IRExpr::Const(crate::ir::IRConstant::Bool(true)),
            then_body: Box::new(crate::ir::IRBody { statements }),
            else_body: None,
        }
    }
}

/// `logger.info(msg, *args)` and its kin on a `logging.Logger`, as the
/// record it logs: `statement` says whether the call is a statement, where
/// its arguments can be evaluated into variables first.
fn log_parts(expr: &IRExpr, ctx: &CompilationContext, statement: bool) -> Option<Logged> {
    use crate::ir::IRConstant;
    let IRExpr::MethodCall {
        object,
        method_name,
        arguments,
    } = expr
    else {
        return None;
    };
    if !crate::ir::LOG_METHODS.contains(&method_name.as_str()) {
        return None;
    }
    let class = class_of(object, ctx)?;
    if !is_logger(ctx, &class) {
        return None;
    }
    let shown = format!("Logger.{method_name}()");
    // A refused call still compiles to something, so the walk goes on.
    let refused = |why: String| {
        ctx.report(why);
        Some(Logged {
            setup: Vec::new(),
            receiver: (**object).clone(),
            level: IRExpr::Const(IRConstant::Int(0)),
            arguments: vec![
                IRExpr::Const(IRConstant::String(String::new())),
                IRExpr::Const(IRConstant::String(String::new())),
                IRExpr::Const(IRConstant::Int(0)),
                IRExpr::Const(IRConstant::String(String::new())),
            ],
        })
    };
    match method_name.as_str() {
        "warn" => {
            return refused(format!(
                "{shown} is deprecated, and CPython prints a DeprecationWarning for it, which is \
                 not supported. Hint: call warning()"
            ))
        }
        "exception" => {
            return refused(format!(
                "{shown} logs the traceback of the exception being handled, which is not \
                 supported. Hint: log the exception with error(\"%s\", e)"
            ))
        }
        _ => {}
    }
    let (mut line, mut func, mut formatted, mut unformattable) = (None, None, None, None);
    for a in arguments {
        let IRExpr::Keyword { name, value } = a else {
            continue;
        };
        match (name.as_str(), value.as_ref()) {
            (n, IRExpr::Const(IRConstant::Int(l))) if n == crate::ir::LOG_LINE_ARG => {
                line = Some(*l)
            }
            (n, IRExpr::Const(IRConstant::String(f))) if n == crate::ir::LOG_FUNC_ARG => {
                func = Some(f.clone())
            }
            (n, text) if n == crate::ir::LOG_TEXT_ARG => formatted = Some(text.clone()),
            (n, IRExpr::Const(IRConstant::String(why))) if n == crate::ir::LOG_TEXT_ERROR_ARG => {
                unformattable = Some(why.clone())
            }
            (
                "exc_info" | "stack_info",
                IRExpr::Const(IRConstant::Bool(false) | IRConstant::None),
            ) => {}
            ("stacklevel", IRExpr::Const(IRConstant::Int(1))) => {}
            ("exc_info", _) => {
                return refused(format!(
                    "{shown} with exc_info logs a traceback, which is not supported"
                ))
            }
            ("stack_info", _) => {
                return refused(format!(
                    "{shown} with stack_info logs the stack, which is not supported"
                ))
            }
            ("stacklevel", _) => {
                return refused(format!(
                    "{shown} with a stacklevel other than 1 is not supported"
                ))
            }
            ("extra", _) => {
                return refused(format!(
                    "{shown}'s extra= adds attributes to the record, which is not supported"
                ))
            }
            (other, _) => {
                return refused(format!(
                    "{shown} got an unexpected keyword argument '{other}'"
                ))
            }
        }
    }
    let (Some(line), Some(func)) = (line, func) else {
        return refused(format!(
            "the line of this {shown} call is not known, so its record cannot be made"
        ));
    };
    let mut positional: Vec<&IRExpr> = arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect();
    let level = match method_name.as_str() {
        "debug" => IRExpr::Const(IRConstant::Int(10)),
        "info" => IRExpr::Const(IRConstant::Int(20)),
        "warning" => IRExpr::Const(IRConstant::Int(30)),
        "error" => IRExpr::Const(IRConstant::Int(40)),
        "critical" | "fatal" => IRExpr::Const(IRConstant::Int(50)),
        _ => {
            if positional.is_empty() {
                return refused(format!("{shown} missing required argument: 'level'"));
            }
            let level = positional.remove(0).clone();
            if !matches!(
                infer_value_type(&level, ctx),
                IRType::Int | IRType::Bool | IRType::Unknown
            ) {
                return refused(format!(
                    "{shown} raises TypeError: level must be an integer"
                ));
            }
            level
        }
    };
    if positional.is_empty() {
        return refused(format!("{shown} missing required argument: 'msg'"));
    }
    let msg = positional.remove(0).clone();
    let args: Vec<IRExpr> = positional.into_iter().cloned().collect();
    let module_root = matches!(object.as_ref(),
        IRExpr::FunctionCall { function_name, .. } if function_name == crate::ir::MODULE_ROOT_FN);

    // Evaluated once each, in the order written: the logger, then the
    // arguments (the module's own functions find the root logger after).
    let tag = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{expr:?}").hash(&mut h);
        h.finish() as u32
    };
    let mut setup = Vec::new();
    let held = |e: IRExpr, k: usize, setup: &mut Vec<IRStatement>| -> IRExpr {
        if !statement || order_free(&e) {
            return e;
        }
        let name = format!("__lg{tag:x}_{k}");
        setup.push(IRStatement::Assign {
            target: name.clone(),
            value: e,
            var_type: None,
        });
        IRExpr::Variable(name)
    };
    let mut receiver = (**object).clone();
    if !module_root {
        receiver = held(receiver, 0, &mut setup);
    }
    let level = held(level, 1, &mut setup);
    let constant = match &msg {
        IRExpr::Const(IRConstant::String(s)) => Some(s.clone()),
        _ => None,
    };
    if !statement && constant.is_none() && !order_free(&msg) {
        return refused(format!(
            "{shown} used as a value with a message computed in the call. Hint: call it as \
             a statement"
        ));
    }
    let msg = held(msg, 2, &mut setup);
    let args: Vec<IRExpr> = args
        .into_iter()
        .enumerate()
        .map(|(k, a)| held(a, k + 3, &mut setup))
        .collect();
    if module_root {
        receiver = held(receiver, 0, &mut setup);
    }
    let record_msg = match &constant {
        Some(_) => msg.clone(),
        None => IRExpr::FunctionCall {
            function_name: "str".to_string(),
            arguments: vec![msg.clone()],
        },
    };
    let text = if args.is_empty() {
        record_msg.clone()
    } else {
        if let Some(why) = unformattable {
            return refused(format!("{shown}'s message cannot be formatted: {why}"));
        }
        match (&constant, &formatted) {
            (Some(_), Some(text)) => substitute_arguments(text, &args),
            _ => {
                return refused(format!(
                    "{shown}'s message is formatted with % and its arguments, which needs \
                     the message written as a literal here"
                ))
            }
        }
    };
    Some(Logged {
        setup,
        receiver,
        level,
        arguments: vec![
            record_msg,
            text,
            IRExpr::Const(IRConstant::Int(line)),
            IRExpr::Const(IRConstant::String(func)),
        ],
    })
}

/// A logging call used as a value (its message made whether or not it is
/// logged), or a call to another receiver's method of a logging name, which
/// sheds the line and function the converter gave it.
fn rewrite_log_expr(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let IRExpr::MethodCall {
        object,
        method_name,
        arguments,
    } = expr
    else {
        return None;
    };
    if !arguments.iter().any(is_log_keyword) {
        return None;
    }
    if let Some(logged) = log_parts(expr, ctx, false) {
        let mut arguments = vec![logged.level];
        arguments.extend(logged.arguments);
        return Some(call(&logged.receiver, crate::ir::LOG_NOW_METHOD, arguments));
    }
    Some(IRExpr::MethodCall {
        object: object.clone(),
        method_name: method_name.clone(),
        arguments: arguments
            .iter()
            .filter(|a| !is_log_keyword(a))
            .cloned()
            .collect(),
    })
}

/// A `super().method(...)` call's arguments, placed against the method it
/// reaches (`owner::method`): keywords by name, then the defaults of the
/// parameters left out, as a call through an instance gets them.
pub(crate) fn place_super_arguments(
    ctx: &CompilationContext,
    owner: &str,
    method: &str,
    arguments: &[IRExpr],
) -> Vec<IRExpr> {
    let qualified = format!("{owner}::{method}");
    let (Some(names), Some(defaults)) = (
        ctx.method_params.get(&qualified),
        ctx.method_defaults.get(&qualified),
    ) else {
        return arguments.to_vec();
    };
    let positional: Vec<&IRExpr> = arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect();
    let keywords: Vec<(&String, &IRExpr)> = arguments
        .iter()
        .filter_map(|a| match a {
            IRExpr::Keyword { name, value } => Some((name, value.as_ref())),
            _ => None,
        })
        .collect();
    let shown = format!("super().{method}()");
    let (names, defaults) = (&names[1..], &defaults[1..]);
    if positional.len() > names.len() {
        ctx.report(format!(
            "{shown} takes {} positional argument(s) but {} were given",
            names.len(),
            positional.len()
        ));
        return arguments.to_vec();
    }
    if let Some((bad, _)) = keywords.iter().find(|(k, _)| !names.contains(k)) {
        ctx.report(format!(
            "{shown} got an unexpected keyword argument '{bad}'"
        ));
        return arguments.to_vec();
    }
    let mut placed: Vec<IRExpr> = positional.iter().map(|e| (*e).clone()).collect();
    let mut order = Vec::new();
    for (i, name) in names.iter().enumerate().skip(positional.len()) {
        match keywords.iter().position(|(k, _)| *k == name) {
            Some(at) => {
                placed.push(keywords[at].1.clone());
                order.push(at);
            }
            None => match &defaults[i] {
                Some(d) if constant_default(d) => placed.push(d.clone()),
                Some(_) => {
                    ctx.report(format!(
                        "the default of '{name}' in {shown} is not a constant. Hint: pass it"
                    ));
                    return arguments.to_vec();
                }
                None => {
                    ctx.report(format!("{shown} missing required argument: '{name}'"));
                    return arguments.to_vec();
                }
            },
        }
    }
    if order.windows(2).any(|w| w[0] > w[1]) && keywords.iter().any(|(_, v)| !order_free(v)) {
        ctx.report(format!(
            "the keyword arguments of {shown} are computed and written out of the parameters' \
             order, which would change the order they run in. Hint: write them in order"
        ));
    }
    placed
}

/// `x == s` and `x != s` for an optional string `x` and a string `s`: None
/// is unequal to every string, and a string compares by its text. Comparing
/// the optional's word with the string answered False for equal text.
fn rewrite_optional_text_compare(expr: &IRExpr, ctx: &CompilationContext) -> Option<IRExpr> {
    let optional_text = |e: &IRExpr| {
        matches!(infer_value_type(e, ctx), IRType::Optional(inner)
            if matches!(*inner, IRType::String | IRType::Bytes))
    };
    let text = |e: &IRExpr| matches!(infer_value_type(e, ctx), IRType::String | IRType::Bytes);
    let some = |e: &IRExpr, message: &str, class: &str| IRExpr::FunctionCall {
        function_name: crate::ir::SOME_FN.to_string(),
        arguments: vec![
            e.clone(),
            IRExpr::Const(crate::ir::IRConstant::String(message.to_string())),
            IRExpr::Const(crate::ir::IRConstant::String(class.to_string())),
        ],
    };
    match expr {
        IRExpr::CompareOp { left, right, op }
            if matches!(op, IRCompareOp::Eq | IRCompareOp::NotEq) =>
        {
            let (optional, other, optional_left) = if optional_text(left) && text(right) {
                (left, right, true)
            } else if optional_text(right) && text(left) {
                (right, left, false)
            } else {
                return None;
            };
            let eq = IRExpr::FunctionCall {
                function_name: OPTIONAL_TEXT_EQ_FN.to_string(),
                arguments: vec![
                    (**optional).clone(),
                    (**other).clone(),
                    IRExpr::Const(crate::ir::IRConstant::Bool(optional_left)),
                ],
            };
            Some(match op {
                IRCompareOp::Eq => eq,
                _ => IRExpr::UnaryOp {
                    operand: Box::new(eq),
                    op: IRUnaryOp::Not,
                },
            })
        }
        // `x + s` and `s + x`: CPython's TypeError when x is None.
        IRExpr::BinaryOp {
            left,
            right,
            op: IROp::Add,
        } => {
            if optional_text(left) && text(right) {
                Some(IRExpr::BinaryOp {
                    left: Box::new(some(
                        left,
                        "unsupported operand type(s) for +: 'NoneType' and 'str'",
                        "TypeError",
                    )),
                    right: right.clone(),
                    op: IROp::Add,
                })
            } else if text(left) && optional_text(right) {
                Some(IRExpr::BinaryOp {
                    left: left.clone(),
                    right: Box::new(some(
                        right,
                        "can only concatenate str (not \"NoneType\") to str",
                        "TypeError",
                    )),
                    op: IROp::Add,
                })
            } else {
                None
            }
        }
        // `len(x)`, `int(x)`, `float(x)`: of the string, or CPython's
        // TypeError for None.
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if arguments.len() == 1
            && ctx.get_function_info(function_name).is_none()
            && optional_text(&arguments[0]) =>
        {
            let message = match function_name.as_str() {
                "len" => "object of type 'NoneType' has no len()",
                "int" => "int() argument must be a string, a bytes-like object or a real number, not 'NoneType'",
                "float" => "float() argument must be a string or a real number, not 'NoneType'",
                _ => return None,
            };
            Some(IRExpr::FunctionCall {
                function_name: function_name.clone(),
                arguments: vec![some(&arguments[0], message, "TypeError")],
            })
        }
        _ => None,
    }
}

/// `__optional_text_eq(x, s, x_first)`: an optional string equal to a
/// string, evaluated in the order written.
pub(crate) const OPTIONAL_TEXT_EQ_FN: &str = "__optional_text_eq";
