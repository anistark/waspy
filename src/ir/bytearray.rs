//! `bytearray`, the mutable sequence of bytes, written in IR as a class
//! named `bytearray`, made only when the program uses it.
//!
//! An instance holds its bytes as a `list[int]` (`data`), so a change is a
//! change to that list, and walking it (`<walk>`) reads the list as it is at
//! each step, as CPython's iterator does. Read as a whole it is the `bytes`
//! of the same values (`<view>`): that is how it compares (`==` and ordering,
//! against bytes or another bytearray) and what `bytes(b)` copies; a slice is
//! a new bytearray (`<wrap>`). Every value stored is checked to be a byte,
//! with CPython's `ValueError`, and every index with its `IndexError`.
//!
//! `bytearray(x)` is `bytearray().<from>(x)`, whose variant code generation
//! picks by `x`'s type: an int (that many zeros), bytes, another bytearray,
//! or a list of ints. In a module using the JavaScript bridge, a typed
//! array's `assign_to(b)` copies into one (`<assign_js>`).

use super::build::*;
use crate::ir::collections::spec;
use crate::ir::{
    IRBody, IRClass, IRCompareOp, IRConstant, IRExpr, IRFunction, IRModule, IROp, IRStatement,
    IRType, IRVariable,
};
use anyhow::Result;

/// The class's name, which is the builtin's.
pub const BYTEARRAY: &str = "bytearray";

/// The hidden method a typed array's `assign_to(b)` is: the JavaScript
/// buffer's bytes copied into the bytearray, with Pyodide's errors.
pub const ASSIGN_JS_METHOD: &str = "<assign_js>";

/// The intrinsic that answers whether a JavaScript value is a buffer of
/// bytes (1: a `Uint8Array`, `Uint8ClampedArray`, or `ArrayBuffer`), a typed
/// array of another format (0), or not a buffer (-1).
pub const BYTE_FORMAT_FN: &str = "__js_byte_format";

const RANGE_ERROR: &str = "byte must be in range(0, 256)";

pub fn lower_bytearray(module: &mut IRModule) -> Result<()> {
    // A program that defines its own `bytearray` keeps it.
    if module.classes.iter().any(|c| c.name == BYTEARRAY)
        || module.functions.iter().any(|f| f.name == BYTEARRAY)
    {
        return Ok(());
    }
    let mut used = module
        .functions
        .iter()
        .any(|f| f.params.iter().any(|p| mentions(&p.param_type)) || mentions(&f.return_type))
        || module.classes.iter().any(|c| {
            c.methods.iter().any(|m| {
                m.params.iter().any(|p| mentions(&p.param_type)) || mentions(&m.return_type)
            })
        })
        || module
            .variables
            .iter()
            .any(|v| v.var_type.as_ref().is_some_and(mentions));
    let mut refusal = None;
    visit_module(module, &mut |e| match e {
        IRExpr::Variable(name) if name == BYTEARRAY => used = true,
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if function_name == BYTEARRAY => {
            used = true;
            match arguments.len() {
                0 => {}
                1 if !matches!(arguments[0], IRExpr::Keyword { .. }) => {
                    let source = arguments.pop().expect("one argument");
                    *e = mcall(
                        call(BYTEARRAY, Vec::new()),
                        crate::ir::FROM_METHOD,
                        vec![source],
                    );
                }
                _ => {
                    refusal.get_or_insert(
                        "bytearray() of a str and its encoding is not supported, since \
                             str.encode() is not. Hint: build it from bytes, as \
                             bytearray(b\"...\")",
                    );
                }
            }
        }
        _ => {}
    });
    if let Some(message) = refusal {
        return Err(refused(message.to_string()));
    }
    if !used {
        return Ok(());
    }
    let bridge = module
        .imports
        .iter()
        .any(|i| matches!(i.module.as_str(), "js" | "pyodide" | "pyodide.ffi"));
    let class = bytearray_class(bridge);
    for m in &class.methods {
        let mut body = m.body.clone();
        let _ = crate::ir::visit_body(&mut body, &mut |e: &mut IRExpr| {
            if let IRExpr::Const(IRConstant::String(s)) = e {
                module.memory_layout.add_string(s);
            }
            Ok(())
        });
    }
    module.classes.insert(0, class);
    Ok(())
}

/// Whether `ty` is, or holds, a bytearray.
fn mentions(ty: &IRType) -> bool {
    match ty {
        IRType::Class(c) => c == BYTEARRAY,
        IRType::List(t) | IRType::Set(t) | IRType::Optional(t) | IRType::VarTuple(t) => mentions(t),
        IRType::Dict(k, v) => mentions(k) || mentions(v),
        IRType::Tuple(ts) => ts.iter().any(mentions),
        _ => false,
    }
}

/// Apply `f` to every expression of the program, after its children.
fn visit_module(module: &mut IRModule, f: &mut impl FnMut(&mut IRExpr)) {
    let mut each = |body: &mut IRBody| {
        let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
            f(e);
            Ok(())
        });
    };
    for func in &mut module.functions {
        each(&mut func.body);
    }
    for class in &mut module.classes {
        for m in &mut class.methods {
            each(&mut m.body);
        }
    }
    let mut value = |slot: &mut IRExpr| {
        let mut body = IRBody {
            statements: vec![IRStatement::Return(Some(slot.clone()))],
        };
        each(&mut body);
        if let Some(IRStatement::Return(Some(e))) = body.statements.pop() {
            *slot = e;
        }
    };
    for var in &mut module.variables {
        value(&mut var.value);
    }
    for class in &mut module.classes {
        for var in &mut class.class_vars {
            value(&mut var.value);
        }
    }
}

fn me_ty() -> IRType {
    IRType::Class(BYTEARRAY.to_string())
}

fn me() -> crate::ir::IRParam {
    param("self", &me_ty())
}

fn ints() -> IRType {
    IRType::List(Box::new(IRType::Int))
}

/// `raise ValueError(...)` unless `x` is a byte.
fn check_byte(x: IRExpr) -> IRStatement {
    if_(
        or(
            cmp(x.clone(), IRCompareOp::Lt, int(0)),
            cmp(x, IRCompareOp::Gt, int(255)),
        ),
        vec![raise_expr("ValueError", text(RANGE_ERROR))],
    )
}

/// `j = i`, counted from the end when negative, and `raise IndexError(message)`
/// unless it is an index of `data`.
fn checked_index(i: &str, message: &str) -> Vec<IRStatement> {
    vec![
        assign_typed("j", v(i), &IRType::Int),
        if_(
            cmp(v("j"), IRCompareOp::Lt, int(0)),
            vec![assign("j", bin(v("j"), IROp::Add, len(my("data"))))],
        ),
        if_(
            or(
                cmp(v("j"), IRCompareOp::Lt, int(0)),
                cmp(v("j"), IRCompareOp::GtE, len(my("data"))),
            ),
            vec![raise_expr("IndexError", text(message))],
        ),
    ]
}

/// A new bytearray holding `data`, answered.
fn answer_new(data: IRExpr) -> Vec<IRStatement> {
    vec![
        assign_typed("new", call(BYTEARRAY, Vec::new()), &me_ty()),
        set_attr(v("new"), "data", data),
        ret_val(v("new")),
    ]
}

/// The list of ints `b`'s bytes are.
fn ints_of(b: IRExpr) -> IRExpr {
    let x = "__ba_x";
    IRExpr::Comprehension {
        kind: crate::ir::IRComprehensionKind::List,
        element: Box::new(v(x)),
        value: None,
        generators: vec![crate::ir::IRGenerator {
            targets: vec![x.to_string()],
            iterable: b,
            conditions: Vec::new(),
        }],
    }
}

fn variant(base: &str, ty: &IRType) -> String {
    spec::variant(base, ty).expect("a builtin type has a variant name")
}

fn bytearray_class(bridge: bool) -> IRClass {
    let bytes = IRType::Bytes;
    let int_ty = IRType::Int;
    let data = || my("data");
    let mut m: Vec<IRFunction> = vec![
        method(
            "__init__",
            vec![me()],
            IRType::None,
            vec![set_my_typed(
                "data",
                IRExpr::ListLiteral(Vec::new()),
                &ints(),
            )],
        ),
        // What walking it yields: its list, read as it is at each step.
        method(
            crate::ir::WALK_METHOD,
            vec![me()],
            ints(),
            vec![ret_val(data())],
        ),
        // What it reads as whole: the bytes of its values.
        method(
            crate::ir::VIEW_METHOD,
            vec![me()],
            bytes.clone(),
            vec![ret_val(call("bytes", vec![data()]))],
        ),
        // A slice of it, which is a new bytearray.
        method(
            crate::ir::WRAP_METHOD,
            vec![me(), param("value", &bytes)],
            me_ty(),
            answer_new(ints_of(v("value"))),
        ),
        method(
            &variant(crate::ir::FROM_METHOD, &int_ty),
            vec![me(), param("n", &int_ty)],
            me_ty(),
            vec![
                if_(
                    cmp(v("n"), IRCompareOp::Lt, int(0)),
                    vec![raise_expr("ValueError", text("negative count"))],
                ),
                set_attr(
                    v("self"),
                    "data",
                    bin(IRExpr::ListLiteral(vec![int(0)]), IROp::Mul, v("n")),
                ),
                ret_val(v("self")),
            ],
        ),
        method(
            &variant(crate::ir::FROM_METHOD, &bytes),
            vec![me(), param("src", &bytes)],
            me_ty(),
            vec![
                set_attr(v("self"), "data", ints_of(v("src"))),
                ret_val(v("self")),
            ],
        ),
        method(
            &variant(crate::ir::FROM_METHOD, &me_ty()),
            vec![me(), param("src", &me_ty())],
            me_ty(),
            vec![
                set_attr(v("self"), "data", slice(attr(v("src"), "data"), None, None)),
                ret_val(v("self")),
            ],
        ),
        method(
            &variant(crate::ir::FROM_METHOD, &ints()),
            vec![me(), param("src", &ints())],
            me_ty(),
            vec![
                for_("x", v("src"), vec![check_byte(v("x"))]),
                set_attr(v("self"), "data", slice(v("src"), None, None)),
                ret_val(v("self")),
            ],
        ),
        method(
            "__len__",
            vec![me()],
            int_ty.clone(),
            vec![ret_val(len(data()))],
        ),
        method(
            "__repr__",
            vec![me()],
            IRType::String,
            vec![ret_val(bin(
                bin(
                    text("bytearray("),
                    IROp::Add,
                    call("repr", vec![call("bytes", vec![data()])]),
                ),
                IROp::Add,
                text(")"),
            ))],
        ),
        method(
            "__getitem__",
            vec![me(), param("i", &int_ty)],
            int_ty.clone(),
            {
                let mut body = checked_index("i", "bytearray index out of range");
                body.push(ret_val(idx(data(), v("j"))));
                body
            },
        ),
        method(
            "__setitem__",
            vec![me(), param("i", &int_ty), param("x", &int_ty)],
            IRType::None,
            {
                let mut body = vec![check_byte(v("x"))];
                body.extend(checked_index("i", "bytearray index out of range"));
                body.push(set_item(data(), v("j"), v("x")));
                body
            },
        ),
        method(
            crate::ir::DEL_ITEM_METHOD,
            vec![me(), param("i", &int_ty)],
            IRType::None,
            {
                let mut body = checked_index("i", "bytearray index out of range");
                body.push(del_item(data(), v("j")));
                body
            },
        ),
        method(
            crate::ir::DEL_SLICE_METHOD,
            vec![me(), param("lower", &int_ty), param("upper", &int_ty)],
            IRType::None,
            vec![expr_stmt(mcall(
                data(),
                crate::ir::DEL_SLICE_METHOD,
                vec![v("lower"), v("upper")],
            ))],
        ),
        method(
            "__contains__",
            vec![me(), param("x", &int_ty)],
            IRType::Bool,
            vec![
                check_byte(v("x")),
                ret_val(cmp(v("x"), IRCompareOp::In, data())),
            ],
        ),
        method(
            "append",
            vec![me(), param("x", &int_ty)],
            IRType::None,
            vec![
                check_byte(v("x")),
                expr_stmt(mcall(data(), "append", vec![v("x")])),
            ],
        ),
        method(
            "insert",
            vec![me(), param("i", &int_ty), param("x", &int_ty)],
            IRType::None,
            vec![
                check_byte(v("x")),
                expr_stmt(mcall(data(), "insert", vec![v("i"), v("x")])),
            ],
        ),
        method(
            &variant("extend", &bytes),
            vec![me(), param("src", &bytes)],
            IRType::None,
            vec![expr_stmt(mcall(data(), "extend", vec![ints_of(v("src"))]))],
        ),
        method(
            &variant("extend", &me_ty()),
            vec![me(), param("src", &me_ty())],
            IRType::None,
            vec![expr_stmt(mcall(
                data(),
                "extend",
                vec![slice(attr(v("src"), "data"), None, None)],
            ))],
        ),
        method(
            &variant("extend", &ints()),
            vec![me(), param("src", &ints())],
            IRType::None,
            vec![
                for_("x", v("src"), vec![check_byte(v("x"))]),
                expr_stmt(mcall(data(), "extend", vec![v("src")])),
            ],
        ),
        method(
            "pop",
            vec![me(), param_default("i", &int_ty, int(-1))],
            int_ty.clone(),
            {
                let mut body = vec![if_(
                    cmp(len(data()), IRCompareOp::Eq, int(0)),
                    vec![raise_expr("IndexError", text("pop from empty bytearray"))],
                )];
                body.extend(checked_index("i", "pop index out of range"));
                body.push(ret_val(mcall(data(), "pop", vec![v("j")])));
                body
            },
        ),
        method(
            "remove",
            vec![me(), param("x", &int_ty)],
            IRType::None,
            vec![
                check_byte(v("x")),
                if_(
                    cmp(v("x"), IRCompareOp::NotIn, data()),
                    vec![raise_expr(
                        "ValueError",
                        text("value not found in bytearray"),
                    )],
                ),
                expr_stmt(mcall(data(), "remove", vec![v("x")])),
            ],
        ),
        method(
            "clear",
            vec![me()],
            IRType::None,
            vec![expr_stmt(mcall(data(), "clear", Vec::new()))],
        ),
        method(
            "reverse",
            vec![me()],
            IRType::None,
            vec![expr_stmt(mcall(data(), "reverse", Vec::new()))],
        ),
        method(
            "copy",
            vec![me()],
            me_ty(),
            answer_new(slice(data(), None, None)),
        ),
        method(
            "count",
            vec![me(), param("x", &int_ty)],
            int_ty.clone(),
            vec![
                check_byte(v("x")),
                ret_val(mcall(data(), "count", vec![v("x")])),
            ],
        ),
        method(
            "find",
            vec![me(), param("x", &int_ty)],
            int_ty.clone(),
            vec![
                check_byte(v("x")),
                if_(
                    cmp(v("x"), IRCompareOp::NotIn, data()),
                    vec![ret_val(int(-1))],
                ),
                ret_val(mcall(data(), "index", vec![v("x")])),
            ],
        ),
        method(
            "index",
            vec![me(), param("x", &int_ty)],
            int_ty.clone(),
            vec![
                check_byte(v("x")),
                if_(
                    cmp(v("x"), IRCompareOp::NotIn, data()),
                    vec![raise_expr("ValueError", text("subsection not found"))],
                ),
                ret_val(mcall(data(), "index", vec![v("x")])),
            ],
        ),
        method(
            "startswith",
            vec![me(), param("prefix", &bytes)],
            IRType::Bool,
            vec![ret_val(cmp(
                slice(call("bytes", vec![data()]), None, Some(len(v("prefix")))),
                IRCompareOp::Eq,
                v("prefix"),
            ))],
        ),
        method(
            "endswith",
            vec![me(), param("suffix", &bytes)],
            IRType::Bool,
            vec![ret_val(and(
                cmp(len(data()), IRCompareOp::GtE, len(v("suffix"))),
                cmp(
                    slice(
                        call("bytes", vec![data()]),
                        Some(bin(len(data()), IROp::Sub, len(v("suffix")))),
                        None,
                    ),
                    IRCompareOp::Eq,
                    v("suffix"),
                ),
            ))],
        ),
        method(
            &variant("__add__", &bytes),
            vec![me(), param("other", &bytes)],
            me_ty(),
            answer_new(bin(data(), IROp::Add, ints_of(v("other")))),
        ),
        method(
            &variant("__add__", &me_ty()),
            vec![me(), param("other", &me_ty())],
            me_ty(),
            answer_new(bin(data(), IROp::Add, attr(v("other"), "data"))),
        ),
        // bytes + bytearray is bytes.
        method(
            &variant("__radd__", &bytes),
            vec![me(), param("other", &bytes)],
            bytes.clone(),
            vec![ret_val(bin(
                v("other"),
                IROp::Add,
                call("bytes", vec![data()]),
            ))],
        ),
        method(
            "__mul__",
            vec![me(), param("n", &int_ty)],
            me_ty(),
            answer_new(bin(data(), IROp::Mul, v("n"))),
        ),
        method(
            "__rmul__",
            vec![me(), param("n", &int_ty)],
            me_ty(),
            answer_new(bin(data(), IROp::Mul, v("n"))),
        ),
        method(
            &variant("__iadd__", &bytes),
            vec![me(), param("other", &bytes)],
            me_ty(),
            vec![
                expr_stmt(mcall(data(), "extend", vec![ints_of(v("other"))])),
                ret_val(v("self")),
            ],
        ),
        method(
            &variant("__iadd__", &me_ty()),
            vec![me(), param("other", &me_ty())],
            me_ty(),
            vec![
                expr_stmt(mcall(
                    data(),
                    "extend",
                    vec![slice(attr(v("other"), "data"), None, None)],
                )),
                ret_val(v("self")),
            ],
        ),
        method(
            "__imul__",
            vec![me(), param("n", &int_ty)],
            me_ty(),
            vec![
                assign_typed("grown", bin(data(), IROp::Mul, v("n")), &ints()),
                expr_stmt(mcall(data(), "clear", Vec::new())),
                expr_stmt(mcall(data(), "extend", vec![v("grown")])),
                ret_val(v("self")),
            ],
        ),
    ];
    // Two bytearrays order as their bytes do (one against bytes is compared
    // through the view).
    for (name, op) in [
        ("__lt__", IRCompareOp::Lt),
        ("__le__", IRCompareOp::LtE),
        ("__gt__", IRCompareOp::Gt),
        ("__ge__", IRCompareOp::GtE),
    ] {
        m.push(method(
            name,
            vec![me(), param("other", &me_ty())],
            IRType::Bool,
            vec![ret_val(cmp(
                call("bytes", vec![data()]),
                op,
                call("bytes", vec![attr(v("other"), "data")]),
            ))],
        ));
    }
    if bridge {
        m.push(assign_js());
    }
    IRClass {
        name: BYTEARRAY.to_string(),
        bases: Vec::new(),
        methods: m,
        class_vars: vec![IRVariable {
            name: crate::ir::DELEGATES_VAR.to_string(),
            value: IRExpr::TupleLiteral(Vec::new()),
            var_type: None,
        }],
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// `buffer.assign_to(self)`: the typed array's bytes copied in, after
/// Pyodide's checks that it holds bytes and has this length.
fn assign_js() -> IRFunction {
    let n = || len(my("data"));
    let message = bin(
        bin(
            bin(
                text("cannot assign from TypedArray of length "),
                IROp::Add,
                call("str", vec![len(v("src"))]),
            ),
            IROp::Add,
            text(" to buffer of length "),
        ),
        IROp::Add,
        call("str", vec![n()]),
    );
    method(
        ASSIGN_JS_METHOD,
        vec![me(), param("buffer", &IRType::JsProxy)],
        IRType::None,
        vec![
            if_(
                cmp(
                    call(BYTE_FORMAT_FN, vec![v("buffer")]),
                    IRCompareOp::Eq,
                    int(0),
                ),
                vec![raise_expr(
                    "ValueError",
                    text("TypedArray and memorybuffer have incompatible formats"),
                )],
            ),
            assign_typed(
                "src",
                mcall(v("buffer"), "to_bytes", Vec::new()),
                &IRType::Bytes,
            ),
            if_(
                cmp(len(v("src")), IRCompareOp::NotEq, n()),
                vec![raise_expr("ValueError", message)],
            ),
            for_(
                "k",
                range(n()),
                vec![set_item(my("data"), v("k"), idx(v("src"), v("k")))],
            ),
        ],
    )
}
