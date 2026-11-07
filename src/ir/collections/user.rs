//! `UserDict`, `UserList`, and `UserString`, written out in IR.
//!
//! These exist to be subclassed, so unlike the other `collections` classes
//! they are ordinary classes whose methods go through the special methods as
//! CPython's `collections/__init__.py` and `_collections_abc.py` do:
//! `update()` stores through `self[k] = v`, `get()` and `items()` read through
//! `self[k]`, and a subclass's override is reached by the usual virtual call.
//! Each reads as `data` through `<view>`, for what walks it.
//!
//! A method that makes a new instance calls `self.__class__(...)` in CPython.
//! No class can be called through a value here, so those methods are written
//! again for each subclass ([`wrappers`]), which also gives their results the
//! subclass's static type. One that cannot be (the subclass's `__init__` takes
//! something else) is named in the class's `<refused>` and refused where it
//! is called.

use super::classes::{class, iterable_sources, me};
use super::spec::{self, Kind};
use crate::ir::build::*;
use crate::ir::{
    IRBody, IRClass, IRCompareOp, IRComprehensionKind, IRExceptHandler, IRExpr, IRFunction,
    IRGenerator, IROp, IRStatement, IRType, IRVariable,
};

/// The hidden method that answers a mapping as `dict(self)` does, reading
/// each value through `self[k]`.
pub const MAPPING_METHOD: &str = "<mapping>";

/// The hidden method that makes an instance of the receiver's class from a
/// value of what it holds, as `self.__class__(value)` does.
pub const WRAP_METHOD: &str = "<wrap>";

/// The hidden method that answers what walking an instance yields, when that
/// is not what its view holds (a UserString yields UserStrings).
pub const WALK_METHOD: &str = "<walk>";

/// The hidden class variable naming what code generation refuses on an
/// instance: methods, or [`REFUSE_WALK`], [`REFUSE_COMPREHENSION`], `__eq__`,
/// and `__reversed__`.
pub const REFUSED_VAR: &str = "<refused>";

/// In `<refused>`: walking an instance, which CPython does through a
/// `__getitem__` a subclass overrides.
pub const REFUSE_WALK: &str = "<walk>";

/// In `<refused>`: a comprehension over an instance, which is built before
/// the comprehension runs while CPython reads each value as it goes.
pub const REFUSE_COMPREHENSION: &str = "<comprehension>";

/// The hidden method `UserList.__init__` copies its argument with: the
/// argument is optional there, and a list here.
const FILL_METHOD: &str = "<fill>";

/// The name of the class of a mapping view of `mapping`.
pub(crate) fn view_class(view: &str, mapping: &str) -> String {
    format!("{view}<{mapping}>")
}

fn dict_of(k: &IRType, val: &IRType) -> IRType {
    IRType::Dict(Box::new(k.clone()), Box::new(val.clone()))
}

fn list_of(t: &IRType) -> IRType {
    IRType::List(Box::new(t.clone()))
}

fn optional(t: &IRType) -> IRType {
    IRType::Optional(Box::new(t.clone()))
}

fn pairs_of(k: &IRType, val: &IRType) -> IRType {
    list_of(&IRType::Tuple(vec![k.clone(), val.clone()]))
}

fn named(base: &str, ty: &IRType) -> String {
    spec::variant(base, ty).expect("a spelled type")
}

fn is_not_none(e: IRExpr) -> IRExpr {
    cmp(e, IRCompareOp::IsNot, none())
}

fn or_op(l: IRExpr, r: IRExpr) -> IRExpr {
    bin(l, IROp::BitOr, r)
}

/// `try: <body> except KeyError: <handler>`.
fn on_key_error(body: Vec<IRStatement>, handler: Vec<IRStatement>) -> IRStatement {
    IRStatement::TryExcept {
        try_body: Box::new(IRBody { statements: body }),
        except_handlers: vec![IRExceptHandler {
            exception_types: vec!["KeyError".to_string()],
            name: None,
            body: IRBody {
                statements: handler,
            },
        }],
        finally_body: None,
    }
}

/// `self.__class__(value)`, for a class whose `__init__` takes it.
fn make(class: &str, value: IRExpr) -> IRExpr {
    call(class, vec![value])
}

// ---------------------------------------------------------------------------
// UserDict
// ---------------------------------------------------------------------------

/// The methods of a `UserDict` that make an instance of their class.
fn dict_wrappers(me_name: &str, root: &str, k: &IRType, val: &IRType) -> Vec<IRFunction> {
    let dict_ty = dict_of(k, val);
    let root_ty = IRType::Class(root.to_string());
    let me_ty = IRType::Class(me_name.to_string());
    let other_data = || attr(v("other"), "data");
    let two = |name: &str, other: &IRType, value: IRExpr| {
        method(
            name,
            vec![me(me_name), param("other", other)],
            me_ty.clone(),
            vec![ret_val(make(me_name, value))],
        )
    };
    let copied = if me_name == root {
        mcall(my("data"), "copy", Vec::new())
    } else {
        // `copy.copy(self)` with empty data, then `update(self)`, which reads
        // through `__getitem__`.
        mcall(v("self"), MAPPING_METHOD, Vec::new())
    };
    vec![
        two("__or__", &root_ty, or_op(my("data"), other_data())),
        two(
            &named("__or__", &dict_ty),
            &dict_ty,
            or_op(my("data"), v("other")),
        ),
        two("__ror__", &root_ty, or_op(other_data(), my("data"))),
        two(
            &named("__ror__", &dict_ty),
            &dict_ty,
            or_op(v("other"), my("data")),
        ),
        method(
            "copy",
            vec![me(me_name)],
            me_ty.clone(),
            vec![ret_val(make(me_name, copied))],
        ),
    ]
}

/// The class of a mapping view (`KeysView`, `ValuesView`, `ItemsView`): a
/// list of what it yields as its `<view>`, read from the mapping each time.
fn mapping_view(
    kind: &str,
    mapping: &str,
    elem: &IRType,
    yields: IRExpr,
    contains: Vec<IRStatement>,
    contained: &IRType,
) -> IRClass {
    let name = view_class(kind, mapping);
    let mapping_ty = IRType::Class(mapping.to_string());
    let list_ty = list_of(elem);
    let methods = vec![
        method(
            "__init__",
            vec![me(&name), param("mapping", &mapping_ty)],
            IRType::None,
            vec![set_my_typed("_mapping", v("mapping"), &mapping_ty)],
        ),
        method(
            crate::ir::VIEW_METHOD,
            vec![me(&name)],
            list_ty.clone(),
            vec![
                assign_typed("out", IRExpr::ListLiteral(Vec::new()), &list_ty),
                assign_typed("m", my("_mapping"), &mapping_ty),
                for_(
                    "key",
                    attr(v("m"), "data"),
                    vec![expr_stmt(mcall(v("out"), "append", vec![yields]))],
                ),
                ret_val(v("out")),
            ],
        ),
        method(
            "__len__",
            vec![me(&name)],
            IRType::Int,
            vec![ret_val(len(my("_mapping")))],
        ),
        method(
            "__contains__",
            vec![me(&name), param("item", contained)],
            IRType::Bool,
            contains,
        ),
        method(
            "__repr__",
            vec![me(&name)],
            IRType::String,
            vec![ret_val(bin(
                bin(
                    text(&format!("{kind}(")),
                    IROp::Add,
                    call("repr", vec![my("_mapping")]),
                ),
                IROp::Add,
                text(")"),
            ))],
        ),
    ];
    let mut c = class(&name, Vec::new(), methods, &[]);
    // A view compares as a set (or, for values, by identity), and has no
    // reversed() through a Mapping: what it reads as answers neither.
    refuse(&mut c, &["__eq__", "__reversed__"]);
    c
}

pub(crate) fn user_dict(name: &str, k: &IRType, val: &IRType) -> Vec<IRClass> {
    let dict_ty = dict_of(k, val);
    let pairs = pairs_of(k, val);
    let me_ty = IRType::Class(name.to_string());
    let pair = IRType::Tuple(vec![k.clone(), val.clone()]);
    let keyword_dict = dict_of(&IRType::String, val);
    let self_ = || v("self");

    let mut init_params = vec![
        me(name),
        param_default("src", &optional(&dict_ty), none()),
        param_default("pairs", &optional(&pairs), none()),
    ];
    let mut init_body = vec![
        set_my_typed("data", IRExpr::DictLiteral(Vec::new()), &dict_ty),
        if_(
            is_not_none(v("src")),
            vec![expr_stmt(mcall(
                self_(),
                &named("update", &dict_ty),
                vec![v("src")],
            ))],
        ),
        if_(
            is_not_none(v("pairs")),
            vec![expr_stmt(mcall(
                self_(),
                &named("update", &pairs),
                vec![v("pairs")],
            ))],
        ),
    ];
    // Keywords make string keys, which only a dict of them can hold.
    if *k == IRType::String {
        init_params.push(param_default("kw", &optional(&keyword_dict), none()));
        init_body.push(if_(
            is_not_none(v("kw")),
            vec![expr_stmt(mcall(
                self_(),
                &named("update", &dict_ty),
                vec![v("kw")],
            ))],
        ));
    }

    let mut m = vec![
        method("__init__", init_params, IRType::None, init_body),
        method(
            crate::ir::VIEW_METHOD,
            vec![me(name)],
            dict_ty.clone(),
            vec![ret_val(my("data"))],
        ),
        method(
            MAPPING_METHOD,
            vec![me(name)],
            dict_ty.clone(),
            vec![
                assign_typed("out", IRExpr::DictLiteral(Vec::new()), &dict_ty),
                for_(
                    "key",
                    my("data"),
                    vec![set_item(v("out"), v("key"), idx(self_(), v("key")))],
                ),
                ret_val(v("out")),
            ],
        ),
        method(
            "__len__",
            vec![me(name)],
            IRType::Int,
            vec![ret_val(len(my("data")))],
        ),
        method(
            "__missing__",
            vec![me(name), param("key", k)],
            val.clone(),
            vec![raise_expr("KeyError", v("key"))],
        ),
        method(
            "__getitem__",
            vec![me(name), param("key", k)],
            val.clone(),
            vec![
                if_(
                    cmp(v("key"), IRCompareOp::In, my("data")),
                    vec![ret_val(idx(my("data"), v("key")))],
                ),
                ret_val(mcall(self_(), "__missing__", vec![v("key")])),
            ],
        ),
        method(
            "__setitem__",
            vec![me(name), param("key", k), param("item", val)],
            IRType::None,
            vec![set_item(my("data"), v("key"), v("item"))],
        ),
        method(
            crate::ir::DEL_ITEM_METHOD,
            vec![me(name), param("key", k)],
            IRType::None,
            vec![del_item(my("data"), v("key"))],
        ),
        method(
            "__contains__",
            vec![me(name), param("key", k)],
            IRType::Bool,
            vec![ret_val(cmp(v("key"), IRCompareOp::In, my("data")))],
        ),
        method(
            "__repr__",
            vec![me(name)],
            IRType::String,
            vec![ret_val(call("repr", vec![my("data")]))],
        ),
        method(
            "__eq__",
            vec![me(name), param("other", &me_ty)],
            IRType::Bool,
            vec![ret_val(cmp(
                mcall(self_(), MAPPING_METHOD, Vec::new()),
                IRCompareOp::Eq,
                mcall(v("other"), MAPPING_METHOD, Vec::new()),
            ))],
        ),
        // `get(key)` answers None for a missing key, so its result is
        // optional; `get(key, default)` answers the default's type.
        method(
            "get",
            vec![me(name), param("key", k), param("default", val)],
            val.clone(),
            vec![
                if_(
                    cmp(v("key"), IRCompareOp::In, self_()),
                    vec![ret_val(idx(self_(), v("key")))],
                ),
                ret_val(v("default")),
            ],
        ),
        method(
            &named("get", k),
            vec![me(name), param("key", k)],
            optional(val),
            vec![
                if_(
                    cmp(v("key"), IRCompareOp::In, self_()),
                    vec![ret_val(idx(self_(), v("key")))],
                ),
                ret_val(none()),
            ],
        ),
        method(
            &named("pop", k),
            vec![me(name), param("key", k)],
            val.clone(),
            vec![
                assign_typed("value", idx(self_(), v("key")), val),
                del_item(self_(), v("key")),
                ret_val(v("value")),
            ],
        ),
        method(
            "pop",
            vec![me(name), param("key", k), param("default", val)],
            val.clone(),
            vec![
                on_key_error(
                    vec![assign_typed("value", idx(self_(), v("key")), val)],
                    vec![ret_val(v("default"))],
                ),
                del_item(self_(), v("key")),
                ret_val(v("value")),
            ],
        ),
        // The first key, as `next(iter(self))` is.
        method(
            "popitem",
            vec![me(name)],
            pair.clone(),
            vec![
                for_(
                    "key",
                    my("data"),
                    vec![
                        assign_typed("value", idx(self_(), v("key")), val),
                        del_item(self_(), v("key")),
                        ret_val(IRExpr::TupleLiteral(vec![v("key"), v("value")])),
                    ],
                ),
                IRStatement::Raise {
                    exception: Some(call("KeyError", Vec::new())),
                    cause: None,
                },
            ],
        ),
        method(
            "clear",
            vec![me(name)],
            IRType::None,
            vec![while_(
                cmp(len(my("data")), IRCompareOp::Gt, int(0)),
                vec![expr_stmt(mcall(self_(), "popitem", Vec::new()))],
            )],
        ),
        method(
            "setdefault",
            vec![me(name), param("key", k), param("default", val)],
            val.clone(),
            vec![
                on_key_error(
                    vec![ret_val(idx(self_(), v("key")))],
                    vec![set_item(self_(), v("key"), v("default"))],
                ),
                ret_val(v("default")),
            ],
        ),
        method(
            &named("update", &dict_ty),
            vec![me(name), param("other", &dict_ty)],
            IRType::None,
            vec![for_(
                "key",
                v("other"),
                vec![
                    assign_typed("value", idx(v("other"), v("key")), val),
                    set_item(self_(), v("key"), v("value")),
                ],
            )],
        ),
        method(
            &named("update", &pairs),
            vec![me(name), param("other", &pairs)],
            IRType::None,
            vec![for_(
                "kv",
                v("other"),
                vec![
                    assign_typed("key", idx(v("kv"), int(0)), k),
                    assign_typed("value", idx(v("kv"), int(1)), val),
                    set_item(self_(), v("key"), v("value")),
                ],
            )],
        ),
        method(
            "__ior__",
            vec![me(name), param("other", &me_ty)],
            me_ty.clone(),
            vec![
                expr_stmt(mcall(
                    my("data"),
                    "update",
                    vec![mcall(v("other"), MAPPING_METHOD, Vec::new())],
                )),
                ret_val(self_()),
            ],
        ),
        method(
            &named("__ior__", &dict_ty),
            vec![me(name), param("other", &dict_ty)],
            me_ty.clone(),
            vec![
                expr_stmt(mcall(my("data"), "update", vec![v("other")])),
                ret_val(self_()),
            ],
        ),
    ];
    let view_method = |method_name: &str, view: &str, ret: String| {
        method(
            method_name,
            vec![me(name)],
            IRType::Class(ret.clone()),
            vec![ret_val(call(&view_class(view, name), vec![self_()]))],
        )
    };
    m.push(view_method(
        "keys",
        "KeysView",
        view_class("KeysView", name),
    ));
    m.push(view_method(
        "values",
        "ValuesView",
        view_class("ValuesView", name),
    ));
    m.push(view_method(
        "items",
        "ItemsView",
        view_class("ItemsView", name),
    ));
    m.extend(dict_wrappers(name, name, k, val));

    let mut root = class(name, Vec::new(), m, &[]);
    // `Mapping.__reversed__` is None.
    refuse(&mut root, &["__reversed__"]);

    let mapping = || my("_mapping");
    let keys = mapping_view(
        "KeysView",
        name,
        k,
        v("key"),
        vec![ret_val(cmp(v("item"), IRCompareOp::In, mapping()))],
        k,
    );
    let values = mapping_view(
        "ValuesView",
        name,
        val,
        idx(v("m"), v("key")),
        vec![
            assign_typed("m", mapping(), &me_ty),
            for_(
                "key",
                attr(v("m"), "data"),
                vec![if_(
                    cmp(idx(v("m"), v("key")), IRCompareOp::Eq, v("item")),
                    vec![ret_val(boolean(true))],
                )],
            ),
            ret_val(boolean(false)),
        ],
        val,
    );
    let items = mapping_view(
        "ItemsView",
        name,
        &pair,
        IRExpr::TupleLiteral(vec![v("key"), idx(v("m"), v("key"))]),
        vec![
            assign_typed("m", mapping(), &me_ty),
            on_key_error(
                vec![ret_val(cmp(
                    idx(v("m"), idx(v("item"), int(0))),
                    IRCompareOp::Eq,
                    idx(v("item"), int(1)),
                ))],
                vec![ret_val(boolean(false))],
            ),
            ret_val(boolean(false)),
        ],
        &pair,
    );
    vec![keys, values, items, root]
}

// ---------------------------------------------------------------------------
// UserList
// ---------------------------------------------------------------------------

/// What an argument that is walked may be besides a list and an instance.
fn list_sources(t: &IRType) -> Vec<IRType> {
    iterable_sources(t, &[])
        .into_iter()
        .filter(|s| !matches!(s, IRType::Class(_)))
        .collect()
}

/// `other` as a list, for an argument of `source`.
fn as_list(source: &IRType) -> IRExpr {
    match source {
        IRType::List(_) => v("other"),
        _ => walked(v("other")),
    }
}

/// `list(e)`, which the converter has lowered in the program's own code.
fn walked(e: IRExpr) -> IRExpr {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let item = format!("__user_item_{n}");
    IRExpr::Comprehension {
        kind: IRComprehensionKind::List,
        element: Box::new(v(&item)),
        value: None,
        generators: vec![IRGenerator {
            targets: vec![item],
            iterable: e,
            conditions: Vec::new(),
        }],
    }
}

/// The methods of a `UserList` that make an instance of their class.
fn list_wrappers(me_name: &str, root: &str, t: &IRType) -> Vec<IRFunction> {
    let list_ty = list_of(t);
    let root_ty = IRType::Class(root.to_string());
    let me_ty = IRType::Class(me_name.to_string());
    let other_data = || attr(v("other"), "data");
    let answer = |name: &str, params: Vec<crate::ir::IRParam>, value: IRExpr| {
        method(
            name,
            params,
            me_ty.clone(),
            vec![ret_val(make(me_name, value))],
        )
    };
    let two = |other: &IRType| vec![me(me_name), param("other", other)];
    let add = |l: IRExpr, r: IRExpr| bin(l, IROp::Add, r);
    let n = || vec![me(me_name), param("n", &IRType::Int)];
    let mut m = vec![
        answer(
            WRAP_METHOD,
            vec![me(me_name), param("value", &list_ty)],
            v("value"),
        ),
        answer("__add__", two(&root_ty), add(my("data"), other_data())),
        answer("__radd__", two(&root_ty), add(other_data(), my("data"))),
        answer("__mul__", n(), bin(my("data"), IROp::Mul, v("n"))),
        answer("__rmul__", n(), bin(my("data"), IROp::Mul, v("n"))),
        answer("copy", vec![me(me_name)], my("data")),
    ];
    for source in list_sources(t) {
        m.push(answer(
            &named("__add__", &source),
            two(&source),
            add(my("data"), as_list(&source)),
        ));
        m.push(answer(
            &named("__radd__", &source),
            two(&source),
            add(as_list(&source), my("data")),
        ));
    }
    m
}

pub(crate) fn user_list(name: &str, t: &IRType) -> Vec<IRClass> {
    let list_ty = list_of(t);
    let me_ty = IRType::Class(name.to_string());
    let self_ = || v("self");
    let data_call = |method_name: &str, args: Vec<IRExpr>| mcall(my("data"), method_name, args);
    let compare = |method_name: &str, op: IRCompareOp| {
        method(
            method_name,
            vec![me(name), param("other", &me_ty)],
            IRType::Bool,
            vec![ret_val(cmp(my("data"), op, attr(v("other"), "data")))],
        )
    };
    let mut m = vec![
        method(
            "__init__",
            vec![
                me(name),
                param_default("initlist", &optional(&list_ty), none()),
            ],
            IRType::None,
            vec![
                set_my_typed("data", IRExpr::ListLiteral(Vec::new()), &list_ty),
                if_(
                    is_not_none(v("initlist")),
                    vec![expr_stmt(mcall(self_(), FILL_METHOD, vec![v("initlist")]))],
                ),
            ],
        ),
        method(
            FILL_METHOD,
            vec![me(name), param("initlist", &list_ty)],
            IRType::None,
            vec![expr_stmt(data_call("extend", vec![v("initlist")]))],
        ),
        method(
            crate::ir::VIEW_METHOD,
            vec![me(name)],
            list_ty.clone(),
            vec![ret_val(my("data"))],
        ),
        method(
            "__repr__",
            vec![me(name)],
            IRType::String,
            vec![ret_val(call("repr", vec![my("data")]))],
        ),
        compare("__eq__", IRCompareOp::Eq),
        compare("__lt__", IRCompareOp::Lt),
        compare("__le__", IRCompareOp::LtE),
        compare("__gt__", IRCompareOp::Gt),
        compare("__ge__", IRCompareOp::GtE),
        method(
            "__contains__",
            vec![me(name), param("item", t)],
            IRType::Bool,
            vec![ret_val(cmp(v("item"), IRCompareOp::In, my("data")))],
        ),
        method(
            "__len__",
            vec![me(name)],
            IRType::Int,
            vec![ret_val(len(my("data")))],
        ),
        method(
            "__getitem__",
            vec![me(name), param("i", &IRType::Int)],
            t.clone(),
            vec![ret_val(idx(my("data"), v("i")))],
        ),
        method(
            "__setitem__",
            vec![me(name), param("i", &IRType::Int), param("item", t)],
            IRType::None,
            vec![set_item(my("data"), v("i"), v("item"))],
        ),
        method(
            crate::ir::DEL_ITEM_METHOD,
            vec![me(name), param("i", &IRType::Int)],
            IRType::None,
            vec![del_item(my("data"), v("i"))],
        ),
        method(
            crate::ir::DEL_SLICE_METHOD,
            vec![
                me(name),
                param("lower", &IRType::Int),
                param("upper", &IRType::Int),
            ],
            IRType::None,
            vec![expr_stmt(data_call(
                crate::ir::DEL_SLICE_METHOD,
                vec![v("lower"), v("upper")],
            ))],
        ),
        method(
            "__iadd__",
            vec![me(name), param("other", &me_ty)],
            me_ty.clone(),
            vec![
                expr_stmt(data_call("extend", vec![attr(v("other"), "data")])),
                ret_val(self_()),
            ],
        ),
        method(
            "__imul__",
            vec![me(name), param("n", &IRType::Int)],
            me_ty.clone(),
            vec![
                assign_typed("grown", bin(my("data"), IROp::Mul, v("n")), &list_ty),
                expr_stmt(data_call("clear", Vec::new())),
                expr_stmt(data_call("extend", vec![v("grown")])),
                ret_val(self_()),
            ],
        ),
        method(
            "append",
            vec![me(name), param("item", t)],
            IRType::None,
            vec![expr_stmt(data_call("append", vec![v("item")]))],
        ),
        method(
            "insert",
            vec![me(name), param("i", &IRType::Int), param("item", t)],
            IRType::None,
            vec![expr_stmt(data_call("insert", vec![v("i"), v("item")]))],
        ),
        method(
            "pop",
            vec![me(name), param_default("i", &IRType::Int, int(-1))],
            t.clone(),
            vec![ret_val(data_call("pop", vec![v("i")]))],
        ),
        method(
            "remove",
            vec![me(name), param("item", t)],
            IRType::None,
            vec![expr_stmt(data_call("remove", vec![v("item")]))],
        ),
        method(
            "clear",
            vec![me(name)],
            IRType::None,
            vec![expr_stmt(data_call("clear", Vec::new()))],
        ),
        method(
            "count",
            vec![me(name), param("item", t)],
            IRType::Int,
            vec![ret_val(data_call("count", vec![v("item")]))],
        ),
        method(
            "reverse",
            vec![me(name)],
            IRType::None,
            vec![expr_stmt(data_call("reverse", Vec::new()))],
        ),
        method(
            "extend",
            vec![me(name), param("other", &me_ty)],
            IRType::None,
            vec![expr_stmt(data_call(
                "extend",
                vec![attr(v("other"), "data")],
            ))],
        ),
    ];
    for source in list_sources(t) {
        m.push(method(
            &named("extend", &source),
            vec![me(name), param("other", &source)],
            IRType::None,
            vec![expr_stmt(data_call("extend", vec![as_list(&source)]))],
        ));
        m.push(method(
            &named("__iadd__", &source),
            vec![me(name), param("other", &source)],
            me_ty.clone(),
            vec![
                expr_stmt(data_call("extend", vec![as_list(&source)])),
                ret_val(self_()),
            ],
        ));
    }
    m.extend(list_wrappers(name, name, t));
    vec![class(name, Vec::new(), m, &["index", "sort"])]
}

// ---------------------------------------------------------------------------
// UserString
// ---------------------------------------------------------------------------

/// The string methods that take nothing and answer a string.
const STRING_PLAIN: &[&str] = &["capitalize", "lower", "title", "upper"];

/// The string methods that take nothing or the characters to strip.
const STRING_STRIPS: &[&str] = &["strip", "lstrip", "rstrip"];

/// The string methods that take a width and a fill character.
const STRING_PADS: &[&str] = &["center", "ljust", "rjust"];

/// The string methods whose results are not strings, or are plain strings,
/// which answer as `data`'s do.
const STRING_DELEGATES: &[&str] = &[
    "count",
    "encode",
    "endswith",
    "find",
    "format",
    "index",
    "isalnum",
    "isalpha",
    "isascii",
    "isdecimal",
    "isdigit",
    "isidentifier",
    "islower",
    "isnumeric",
    "isprintable",
    "isspace",
    "istitle",
    "isupper",
    "join",
    "partition",
    "rfind",
    "rindex",
    "rpartition",
    "rsplit",
    "split",
    "splitlines",
    "startswith",
];

/// The methods of a `UserString` that make an instance of their class.
fn string_wrappers(me_name: &str, root: &str) -> Vec<IRFunction> {
    let s = IRType::String;
    let root_ty = IRType::Class(root.to_string());
    let me_ty = IRType::Class(me_name.to_string());
    let answer = |name: &str, params: Vec<crate::ir::IRParam>, value: IRExpr| {
        method(
            name,
            params,
            me_ty.clone(),
            vec![ret_val(make(me_name, value))],
        )
    };
    let add = |l: IRExpr, r: IRExpr| bin(l, IROp::Add, r);
    let data_call = |method_name: &str, args: Vec<IRExpr>| mcall(my("data"), method_name, args);
    let other_data = || attr(v("other"), "data");
    let two = |other: &IRType| vec![me(me_name), param("other", other)];
    let n = || vec![me(me_name), param("n", &IRType::Int)];
    let elems = list_of(&me_ty);
    let mut m = vec![
        answer(
            WRAP_METHOD,
            vec![me(me_name), param("value", &s)],
            v("value"),
        ),
        method(
            WALK_METHOD,
            vec![me(me_name)],
            elems.clone(),
            vec![
                assign_typed("out", IRExpr::ListLiteral(Vec::new()), &elems),
                for_(
                    "c",
                    my("data"),
                    vec![expr_stmt(mcall(
                        v("out"),
                        "append",
                        vec![make(me_name, v("c"))],
                    ))],
                ),
                ret_val(v("out")),
            ],
        ),
        answer(
            "__getitem__",
            vec![me(me_name), param("i", &IRType::Int)],
            idx(my("data"), v("i")),
        ),
        answer("__add__", two(&root_ty), add(my("data"), other_data())),
        answer(&named("__add__", &s), two(&s), add(my("data"), v("other"))),
        answer(&named("__radd__", &s), two(&s), add(v("other"), my("data"))),
        answer("__mul__", n(), bin(my("data"), IROp::Mul, v("n"))),
        answer("__rmul__", n(), bin(my("data"), IROp::Mul, v("n"))),
        answer(
            "replace",
            vec![me(me_name), param("old", &s), param("new", &s)],
            data_call("replace", vec![v("old"), v("new")]),
        ),
    ];
    for name in STRING_PLAIN {
        m.push(answer(name, vec![me(me_name)], data_call(name, Vec::new())));
    }
    for name in STRING_STRIPS {
        m.push(answer(name, vec![me(me_name)], data_call(name, Vec::new())));
        m.push(answer(
            &named(name, &s),
            vec![me(me_name), param("chars", &s)],
            data_call(name, vec![v("chars")]),
        ));
    }
    for name in STRING_PADS {
        m.push(answer(
            name,
            vec![
                me(me_name),
                param("width", &IRType::Int),
                param_default("fillchar", &s, text(" ")),
            ],
            data_call(name, vec![v("width"), v("fillchar")]),
        ));
    }
    m
}

pub(crate) fn user_string(name: &str) -> Vec<IRClass> {
    let s = IRType::String;
    let me_ty = IRType::Class(name.to_string());
    let compare = |method_name: &str, op: IRCompareOp| {
        method(
            method_name,
            vec![me(name), param("other", &me_ty)],
            IRType::Bool,
            vec![ret_val(cmp(my("data"), op, attr(v("other"), "data")))],
        )
    };
    let mut m = vec![
        method(
            "__init__",
            vec![me(name), param("seq", &s)],
            IRType::None,
            vec![set_my_typed("data", v("seq"), &s)],
        ),
        method(
            crate::ir::VIEW_METHOD,
            vec![me(name)],
            s.clone(),
            vec![ret_val(my("data"))],
        ),
        method(
            "__str__",
            vec![me(name)],
            s.clone(),
            vec![ret_val(my("data"))],
        ),
        method(
            "__repr__",
            vec![me(name)],
            s.clone(),
            vec![ret_val(call("repr", vec![my("data")]))],
        ),
        method(
            "__int__",
            vec![me(name)],
            IRType::Int,
            vec![ret_val(call("int", vec![my("data")]))],
        ),
        method(
            "__float__",
            vec![me(name)],
            IRType::Float,
            vec![ret_val(call("float", vec![my("data")]))],
        ),
        compare("__eq__", IRCompareOp::Eq),
        compare("__lt__", IRCompareOp::Lt),
        compare("__le__", IRCompareOp::LtE),
        compare("__gt__", IRCompareOp::Gt),
        compare("__ge__", IRCompareOp::GtE),
        method(
            "__contains__",
            vec![me(name), param("char", &s)],
            IRType::Bool,
            vec![ret_val(cmp(v("char"), IRCompareOp::In, my("data")))],
        ),
        method(
            "__len__",
            vec![me(name)],
            IRType::Int,
            vec![ret_val(len(my("data")))],
        ),
    ];
    m.extend(string_wrappers(name, name));
    vec![class(name, Vec::new(), m, STRING_DELEGATES)]
}

// ---------------------------------------------------------------------------
// Subclasses
// ---------------------------------------------------------------------------

/// Add `names` to what code generation refuses on `class`.
pub(crate) fn refuse(class: &mut IRClass, names: &[&str]) {
    let new = names.iter().map(|n| text(n));
    if let Some(var) = class.class_vars.iter_mut().find(|c| c.name == REFUSED_VAR) {
        if let IRExpr::TupleLiteral(items) = &mut var.value {
            items.extend(new);
        }
        return;
    }
    class.class_vars.push(IRVariable {
        name: REFUSED_VAR.to_string(),
        value: IRExpr::TupleLiteral(new.collect()),
        var_type: None,
    });
}

/// The root class's methods, for what an override's parameters take.
pub(crate) fn root_methods(kind: Kind, root: &str, args: &[IRType]) -> Vec<IRFunction> {
    let classes = match kind {
        Kind::UserDict => user_dict(root, &args[0], &args[1]),
        Kind::UserList => user_list(root, &args[0]),
        _ => user_string(root),
    };
    classes
        .into_iter()
        .find(|c| c.name == root)
        .map(|c| c.methods)
        .unwrap_or_default()
}

/// The methods `self.__class__(...)` makes an instance through, written for
/// `me_name`.
pub(crate) fn wrappers(kind: Kind, me_name: &str, root: &str, args: &[IRType]) -> Vec<IRFunction> {
    match kind {
        Kind::UserDict => dict_wrappers(me_name, root, &args[0], &args[1]),
        Kind::UserList => list_wrappers(me_name, root, &args[0]),
        _ => string_wrappers(me_name, root),
    }
}

/// What a value of the class holds, for an `__init__`'s first parameter.
pub(crate) fn held_type(kind: Kind, args: &[IRType]) -> IRType {
    match kind {
        Kind::UserDict => dict_of(&args[0], &args[1]),
        Kind::UserList => list_of(&args[0]),
        _ => IRType::String,
    }
}

/// A class that is, or derives from, a `UserDict`, `UserList`, or
/// `UserString` of `args`.
#[derive(Clone, Debug)]
pub(crate) struct UserClass {
    pub(crate) kind: Kind,
    /// The specialized class it derives from.
    pub(crate) root: String,
    pub(crate) args: Vec<IRType>,
    /// Whether a call of it reaches the root's `__init__`.
    pub(crate) inherits_init: bool,
}

/// The arguments of the root's `__init__` for a call written as CPython's
/// constructor takes them: one iterable or mapping, and for a `UserDict`
/// keywords, which become the hidden `pairs` and `kw` parameters' values.
pub(crate) fn arguments(
    kind: Kind,
    key: Option<&IRType>,
    written: &[IRExpr],
    of: &dyn Fn(&IRExpr) -> IRType,
) -> anyhow::Result<Vec<IRExpr>> {
    let positional: Vec<&IRExpr> = written
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect();
    let keywords: Vec<(&String, &IRExpr)> = written
        .iter()
        .filter_map(|a| match a {
            IRExpr::Keyword { name, value } => Some((name, value.as_ref())),
            _ => None,
        })
        .collect();
    let shown = kind.name();
    if positional.len() > 1 {
        return Err(refused(format!(
            "{shown}() takes at most one positional argument"
        )));
    }
    if kind != Kind::UserDict && !keywords.is_empty() {
        return Err(refused(format!("{shown}() takes no keyword arguments")));
    }
    let src = positional.first().map(|e| (*e).clone());
    Ok(match kind {
        Kind::UserString => {
            let Some(src) = src else {
                return Err(refused(
                    "UserString() is missing its required argument 'seq'".to_string(),
                ));
            };
            vec![match of(&src) {
                IRType::String | IRType::Unknown | IRType::Any => src,
                IRType::Class(c) if is_user_string(&c) => attr(src, "data"),
                _ => call("str", vec![src]),
            }]
        }
        Kind::UserList => match src {
            None => Vec::new(),
            Some(src) => vec![match of(&src) {
                IRType::List(_) | IRType::Unknown | IRType::Any | IRType::None => src,
                _ => walked(src),
            }],
        },
        _ => {
            let mut slots: [Option<IRExpr>; 3] = [None, None, None];
            if let Some(src) = src {
                match of(&src) {
                    IRType::List(_) | IRType::VarTuple(_) => slots[1] = Some(src),
                    IRType::Class(_) => slots[0] = Some(call("dict", vec![src])),
                    _ => slots[0] = Some(src),
                }
            }
            if !keywords.is_empty() {
                if key != Some(&IRType::String) {
                    return Err(refused(
                        "UserDict() with keywords makes string keys, which a dict of other keys \
                         cannot hold here"
                            .to_string(),
                    ));
                }
                slots[2] = Some(IRExpr::DictLiteral(
                    keywords
                        .iter()
                        .map(|(k, val)| (text(k), (*val).clone()))
                        .collect(),
                ));
            }
            let last = slots.iter().rposition(Option::is_some);
            slots
                .into_iter()
                .take(last.map_or(0, |i| i + 1))
                .map(|s| s.unwrap_or_else(none))
                .collect()
        }
    })
}

fn is_user_string(class: &str) -> bool {
    class == Kind::UserString.name()
}

/// Give an override the types its unannotated parameters and result have in
/// the method it overrides, and make a subclass's `update(other)` the variant
/// the root's `__init__` calls.
pub(crate) fn type_overrides(class: &mut IRClass, root: &[IRFunction], held: &IRType) {
    for m in &mut class.methods {
        if m.params
            .iter()
            .any(|p| p.star != crate::ir::ParamStar::None)
        {
            continue;
        }
        if m.name == "__init__" {
            if let (Some(p), Some(base)) = (
                m.params.get_mut(1),
                root.iter().find(|r| r.name == "__init__"),
            ) {
                if matches!(p.param_type, IRType::Unknown) {
                    p.param_type = base.params[1].param_type.clone();
                }
            }
            continue;
        }
        // What the root takes as what the class holds, it takes by variant.
        let held_variant =
            spec::variant(&m.name, held).filter(|n| root.iter().any(|r| r.name == *n));
        let base = held_variant
            .as_ref()
            .and_then(|n| root.iter().find(|r| r.name == *n))
            .or_else(|| root.iter().find(|r| r.name == m.name));
        let Some(base) = base else {
            continue;
        };
        for (p, b) in m.params.iter_mut().zip(&base.params).skip(1) {
            if matches!(p.param_type, IRType::Unknown) {
                let none_default = matches!(
                    p.default_value,
                    Some(IRExpr::Const(crate::ir::IRConstant::None))
                );
                p.param_type = if none_default {
                    optional(&b.param_type)
                } else {
                    b.param_type.clone()
                };
            }
        }
        if matches!(m.return_type, IRType::Unknown) {
            m.return_type = base.return_type.clone();
        }
        // The root's `__init__` calls the variant, which this then overrides.
        if m.name == "update" && m.params.len() == 2 && m.params[1].param_type == *held {
            m.name = named("update", held);
        }
    }
}

/// Whether `__init__` takes what the class holds as its one required
/// argument, as `self.__class__(value)` passes it.
pub(crate) fn takes_held(init: &IRFunction, kind: Kind, args: &[IRType]) -> bool {
    let params: Vec<&crate::ir::IRParam> = init
        .params
        .iter()
        .skip(1)
        .filter(|p| p.star == crate::ir::ParamStar::None)
        .collect();
    if params
        .iter()
        .any(|p| p.keyword_only && p.default_value.is_none())
    {
        return false;
    }
    let positional: Vec<&&crate::ir::IRParam> = params.iter().filter(|p| !p.keyword_only).collect();
    let required = positional
        .iter()
        .filter(|p| p.default_value.is_none())
        .count();
    let held = held_type(kind, args);
    required <= 1
        && positional
            .first()
            .is_some_and(|p| p.param_type == held || p.param_type == optional(&held))
}

/// The wrappers' names, which `<refused>` may hold.
const WRAPPER_NAMES: &[&str] = &[
    WRAP_METHOD,
    WALK_METHOD,
    "__add__",
    "__radd__",
    "__mul__",
    "__rmul__",
    "__or__",
    "__ror__",
    "__getitem__",
    "copy",
    "replace",
    "capitalize",
    "lower",
    "title",
    "upper",
    "strip",
    "lstrip",
    "rstrip",
    "center",
    "ljust",
    "rjust",
];

/// A wrapper's name, as `<refused>` holds it.
pub(crate) fn wrapper_name(name: &str) -> &'static str {
    WRAPPER_NAMES
        .iter()
        .find(|n| **n == name)
        .copied()
        .expect("every wrapper is listed")
}

/// The strings a class written here holds outside its methods' bodies.
pub(crate) fn strings() -> impl Iterator<Item = &'static str> {
    [
        " ",
        REFUSE_WALK,
        REFUSE_COMPREHENSION,
        "__eq__",
        "__reversed__",
    ]
    .into_iter()
    .chain(WRAPPER_NAMES.iter().copied())
}

/// Whether `class` is a mapping view this module makes.
pub(crate) fn is_view_class(class: &str) -> bool {
    ["KeysView<", "ValuesView<", "ItemsView<"]
        .iter()
        .any(|p| class.starts_with(p))
}
