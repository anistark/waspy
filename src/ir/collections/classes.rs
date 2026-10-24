//! The specialized `collections` classes, written out in IR.
//!
//! Each holds a builtin container (`data`, or ChainMap's `maps`) and reads as
//! it through `<data>` (or ChainMap's merged `<view>`), so iterating one,
//! `in`, `len()`, and the dict or list methods it shares go to that
//! container in code generation. The methods here are the ones whose
//! behaviour is the class's own, following CPython's `collections/__init__.py`
//! and `_collectionsmodule.c`: Counter's missing count of 0 and its multiset
//! arithmetic, defaultdict's `__missing__`, OrderedDict's ordering, deque's
//! two ends and `maxlen`, and each one's repr.
//!
//! A method that takes an iterable is written once per type an argument may
//! have (`update<list[str]>`, `update<str>`, ...), since a parameter has one
//! type; code generation calls the one the argument's type names
//! ([`super::spec::variant`]).

use super::spec::{self, Kind};
use crate::ir::build::*;
use crate::ir::{
    IRClass, IRCompareOp, IRComprehensionKind, IRExpr, IRFunction, IRGenerator, IROp, IRParam,
    IRStatement, IRType, IRVariable,
};

/// The types an argument walked for its elements of `elem` may have: the
/// builtin sequences and sets of it, and the module's generators that yield
/// it (their state classes, `__Gen_<f>`).
pub(crate) fn iterable_sources(elem: &IRType, generators: &[(String, IRType)]) -> Vec<IRType> {
    let mut out = vec![
        IRType::List(Box::new(elem.clone())),
        IRType::VarTuple(Box::new(elem.clone())),
        IRType::Set(Box::new(elem.clone())),
    ];
    match elem {
        IRType::String => out.push(IRType::String),
        IRType::Int => out.push(IRType::Range),
        _ => {}
    }
    for (class, yields) in generators {
        if yields == elem {
            out.push(IRType::Class(class.clone()));
        }
    }
    out
}

fn me(class: &str) -> IRParam {
    param("self", &IRType::Class(class.to_string()))
}

fn variant(base: &str, ty: &IRType) -> Option<String> {
    spec::variant(base, ty)
}

/// `for <x> in <src>: <body>`.
fn each(x: &str, src: IRExpr, body: Vec<IRStatement>) -> IRStatement {
    for_(x, src, body)
}

fn class(name: &str, bases: Vec<String>, methods: Vec<IRFunction>, delegates: &[&str]) -> IRClass {
    IRClass {
        name: name.to_string(),
        bases,
        methods,
        class_vars: vec![IRVariable {
            name: crate::ir::DELEGATES_VAR.to_string(),
            value: IRExpr::TupleLiteral(delegates.iter().map(|d| text(d)).collect()),
            var_type: None,
        }],
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// The methods every dict-like class has: an empty `data`, `<data>`,
/// `__len__`, and `copy`.
fn dict_common(name: &str, k: &IRType, val: &IRType, methods: &mut Vec<IRFunction>) {
    let dict_ty = IRType::Dict(Box::new(k.clone()), Box::new(val.clone()));
    let me_ty = IRType::Class(name.to_string());
    methods.push(method(
        "__init__",
        vec![me(name)],
        IRType::None,
        vec![set_my_typed(
            "data",
            IRExpr::DictLiteral(Vec::new()),
            &dict_ty,
        )],
    ));
    methods.push(method(
        crate::ir::DATA_METHOD,
        vec![me(name)],
        dict_ty.clone(),
        vec![ret_val(my("data"))],
    ));
    methods.push(method(
        "__len__",
        vec![me(name)],
        IRType::Int,
        vec![ret_val(len(my("data")))],
    ));
    methods.push(method(
        "copy",
        vec![me(name)],
        me_ty,
        vec![
            assign("new", call(name, Vec::new())),
            set_attr(v("new"), "data", mcall(my("data"), "copy", Vec::new())),
            ret_val(v("new")),
        ],
    ));
}

/// `<from T>` for each source type: fill from `src` through `method<T>`, and
/// answer the instance, so `Kind(src)` is `Kind().<from>(src)`.
fn from_variants(name: &str, base: &str, sources: &[IRType], methods: &mut Vec<IRFunction>) {
    for s in sources {
        let (Some(from), Some(via)) = (variant(crate::ir::FROM_METHOD, s), variant(base, s)) else {
            continue;
        };
        methods.push(method(
            &from,
            vec![me(name), param("src", s)],
            IRType::Class(name.to_string()),
            vec![
                expr_stmt(mcall(v("self"), &via, vec![v("src")])),
                ret_val(v("self")),
            ],
        ));
    }
}

// ---------------------------------------------------------------------------
// Counter
// ---------------------------------------------------------------------------

pub(crate) fn counter(name: &str, k: &IRType, generators: &[(String, IRType)]) -> Vec<IRClass> {
    let int = IRType::Int;
    let me_ty = IRType::Class(name.to_string());
    let dict_int = IRType::Dict(Box::new(k.clone()), Box::new(int.clone()));
    let mut m = Vec::new();
    dict_common(name, k, &int, &mut m);
    let get0 = |key: IRExpr| mcall(my("data"), "get", vec![key, crate::ir::build::int(0)]);

    m.push(method(
        "__getitem__",
        vec![me(name), param("key", k)],
        int.clone(),
        vec![ret_val(get0(v("key")))],
    ));
    m.push(method(
        "__setitem__",
        vec![me(name), param("key", k), param("count", &int)],
        IRType::None,
        vec![set_item(my("data"), v("key"), v("count"))],
    ));
    // Like dict's, but a missing element is not a KeyError.
    m.push(method(
        "__delitem__",
        vec![me(name), param("key", k)],
        IRType::None,
        vec![if_(
            cmp(v("key"), IRCompareOp::In, my("data")),
            vec![del_item(my("data"), v("key"))],
        )],
    ));

    // update / subtract from an iterable (each element counts one) or a
    // mapping (each key counts its value).
    let sources = iterable_sources(k, generators);
    for (base, op) in [("update", IROp::Add), ("subtract", IROp::Sub)] {
        for s in &sources {
            let Some(name_v) = variant(base, s) else {
                continue;
            };
            m.push(method(
                &name_v,
                vec![me(name), param("src", s)],
                IRType::None,
                vec![each(
                    "x",
                    v("src"),
                    vec![set_item(
                        my("data"),
                        v("x"),
                        bin(get0(v("x")), op.clone(), crate::ir::build::int(1)),
                    )],
                )],
            ));
        }
        if let Some(name_v) = variant(base, &dict_int) {
            m.push(method(
                &name_v,
                vec![me(name), param("src", &dict_int)],
                IRType::None,
                vec![each(
                    "x",
                    v("src"),
                    vec![set_item(
                        my("data"),
                        v("x"),
                        bin(get0(v("x")), op.clone(), idx(v("src"), v("x"))),
                    )],
                )],
            ));
        }
    }
    let mut all_sources = sources.clone();
    all_sources.push(dict_int.clone());
    from_variants(name, "update", &all_sources, &mut m);

    // most_common: by count, highest first, ties in insertion order (a stable
    // sort on the negated count, which is CPython's order).
    let pair = IRType::Tuple(vec![k.clone(), int.clone()]);
    let pairs = IRType::List(Box::new(pair.clone()));
    let opt_int = IRType::Optional(Box::new(int.clone()));
    m.push(method(
        "most_common",
        vec![me(name), param_default("n", &opt_int, none())],
        pairs.clone(),
        vec![
            assign_typed(
                "items",
                call(
                    "sorted",
                    vec![
                        mcall(my("data"), "items", Vec::new()),
                        lambda(
                            vec![param("kv", &pair)],
                            IRExpr::UnaryOp {
                                operand: Box::new(idx(v("kv"), crate::ir::build::int(1))),
                                op: crate::ir::IRUnaryOp::Neg,
                            },
                        ),
                    ],
                ),
                &pairs,
            ),
            if_(
                cmp(v("n"), IRCompareOp::Is, none()),
                vec![ret_val(v("items"))],
            ),
            if_(
                cmp(v("n"), IRCompareOp::Lt, crate::ir::build::int(0)),
                vec![ret_val(IRExpr::ListLiteral(Vec::new()))],
            ),
            ret_val(slice(v("items"), None, Some(v("n")))),
        ],
    ));
    // elements(): each element as many times as its count, in insertion
    // order, a count below one giving none.
    m.push(method(
        "elements",
        vec![me(name)],
        IRType::Generator(Box::new(k.clone())),
        vec![each(
            "x",
            my("data"),
            vec![
                assign("left", idx(my("data"), v("x"))),
                while_(
                    cmp(v("left"), IRCompareOp::Gt, crate::ir::build::int(0)),
                    vec![
                        yield_(v("x")),
                        assign("left", bin(v("left"), IROp::Sub, crate::ir::build::int(1))),
                    ],
                ),
            ],
        )],
    ));
    m.push(method(
        "total",
        vec![me(name)],
        int.clone(),
        vec![
            assign("t", crate::ir::build::int(0)),
            each("x", my("data"), vec![add_to("t", idx(my("data"), v("x")))]),
            ret_val(v("t")),
        ],
    ));

    // Comparisons treat a missing element as a count of zero.
    let other = || param("other", &me_ty);
    let all_compare = |op: IRCompareOp| {
        vec![
            each(
                "x",
                my("data"),
                vec![if_(
                    not(cmp(
                        idx(my("data"), v("x")),
                        op.clone(),
                        idx(v("other"), v("x")),
                    )),
                    vec![ret_val(boolean(false))],
                )],
            ),
            each(
                "x",
                attr(v("other"), "data"),
                vec![if_(
                    not(cmp(
                        idx(v("self"), v("x")),
                        op.clone(),
                        idx(attr(v("other"), "data"), v("x")),
                    )),
                    vec![ret_val(boolean(false))],
                )],
            ),
            ret_val(boolean(true)),
        ]
    };
    m.push(method(
        "__eq__",
        vec![me(name), other()],
        IRType::Bool,
        all_compare(IRCompareOp::Eq),
    ));
    m.push(method(
        "__le__",
        vec![me(name), other()],
        IRType::Bool,
        all_compare(IRCompareOp::LtE),
    ));
    m.push(method(
        "__ge__",
        vec![me(name), other()],
        IRType::Bool,
        all_compare(IRCompareOp::GtE),
    ));
    for (op_name, weak) in [("__lt__", IRCompareOp::LtE), ("__gt__", IRCompareOp::GtE)] {
        m.push(method(
            op_name,
            vec![me(name), other()],
            IRType::Bool,
            vec![ret_val(and(
                cmp(v("self"), weak, v("other")),
                cmp(v("self"), IRCompareOp::NotEq, v("other")),
            ))],
        ));
    }

    // The multiset operators: a new Counter keeping the positive counts.
    let keep = |count: IRExpr| {
        vec![if_(
            cmp(count.clone(), IRCompareOp::Gt, crate::ir::build::int(0)),
            vec![set_item(attr(v("r"), "data"), v("x"), count)],
        )]
    };
    let new_r = || assign("r", call(name, Vec::new()));
    let others_not_in_self = |body: Vec<IRStatement>| {
        each(
            "x",
            attr(v("other"), "data"),
            vec![if_(cmp(v("x"), IRCompareOp::NotIn, my("data")), body)],
        )
    };
    let o_count = || idx(attr(v("other"), "data"), v("x"));
    m.push(method(
        "__add__",
        vec![me(name), other()],
        me_ty.clone(),
        vec![
            new_r(),
            each(
                "x",
                my("data"),
                keep(bin(
                    idx(my("data"), v("x")),
                    IROp::Add,
                    idx(v("other"), v("x")),
                )),
            ),
            others_not_in_self(keep(o_count())),
            ret_val(v("r")),
        ],
    ));
    m.push(method(
        "__sub__",
        vec![me(name), other()],
        me_ty.clone(),
        vec![
            new_r(),
            each(
                "x",
                my("data"),
                keep(bin(
                    idx(my("data"), v("x")),
                    IROp::Sub,
                    idx(v("other"), v("x")),
                )),
            ),
            others_not_in_self(vec![if_(
                cmp(o_count(), IRCompareOp::Lt, crate::ir::build::int(0)),
                vec![set_item(
                    attr(v("r"), "data"),
                    v("x"),
                    bin(crate::ir::build::int(0), IROp::Sub, o_count()),
                )],
            )]),
            ret_val(v("r")),
        ],
    ));
    // `|` keeps the larger count, `&` the smaller.
    let pick = |larger: bool| {
        let (c, o) = (idx(my("data"), v("x")), idx(v("other"), v("x")));
        let cond = cmp(c.clone(), IRCompareOp::Lt, o.clone());
        IRExpr::Conditional {
            condition: Box::new(cond),
            body: Box::new(if larger { o.clone() } else { c.clone() }),
            orelse: Box::new(if larger { c } else { o }),
        }
    };
    m.push(method(
        "__or__",
        vec![me(name), other()],
        me_ty.clone(),
        vec![
            new_r(),
            each("x", my("data"), keep(pick(true))),
            others_not_in_self(keep(o_count())),
            ret_val(v("r")),
        ],
    ));
    m.push(method(
        "__and__",
        vec![me(name), other()],
        me_ty.clone(),
        vec![
            new_r(),
            each("x", my("data"), keep(pick(false))),
            ret_val(v("r")),
        ],
    ));
    m.push(method(
        "__pos__",
        vec![me(name)],
        me_ty.clone(),
        vec![
            new_r(),
            each("x", my("data"), keep(idx(my("data"), v("x")))),
            ret_val(v("r")),
        ],
    ));
    m.push(method(
        "__neg__",
        vec![me(name)],
        me_ty.clone(),
        vec![
            new_r(),
            each(
                "x",
                my("data"),
                vec![if_(
                    cmp(
                        idx(my("data"), v("x")),
                        IRCompareOp::Lt,
                        crate::ir::build::int(0),
                    ),
                    vec![set_item(
                        attr(v("r"), "data"),
                        v("x"),
                        bin(crate::ir::build::int(0), IROp::Sub, idx(my("data"), v("x"))),
                    )],
                )],
            ),
            ret_val(v("r")),
        ],
    ));
    // The in-place operators change the Counter and then drop what is not
    // positive. `&=` reads the other's missing counts as 0, so it takes a
    // Counter; the others take a dict (a Counter reads as one).
    m.push(method(
        "_keep_positive",
        vec![me(name)],
        me_ty.clone(),
        vec![
            assign_typed(
                "drop",
                IRExpr::Comprehension {
                    kind: IRComprehensionKind::List,
                    element: Box::new(v("x")),
                    value: None,
                    generators: vec![IRGenerator {
                        targets: vec!["x".to_string()],
                        iterable: my("data"),
                        conditions: vec![not(cmp(
                            idx(my("data"), v("x")),
                            IRCompareOp::Gt,
                            crate::ir::build::int(0),
                        ))],
                    }],
                },
                &IRType::List(Box::new(k.clone())),
            ),
            each("y", v("drop"), vec![del_item(my("data"), v("y"))]),
            ret_val(v("self")),
        ],
    ));
    let keep_positive = || ret_val(mcall(v("self"), "_keep_positive", Vec::new()));
    let o = || idx(v("other"), v("x"));
    for (op_name, body) in [
        (
            "__iadd__",
            vec![set_item(
                my("data"),
                v("x"),
                bin(get0(v("x")), IROp::Add, o()),
            )],
        ),
        (
            "__isub__",
            vec![set_item(
                my("data"),
                v("x"),
                bin(get0(v("x")), IROp::Sub, o()),
            )],
        ),
        (
            "__ior__",
            vec![if_(
                cmp(o(), IRCompareOp::Gt, get0(v("x"))),
                vec![set_item(my("data"), v("x"), o())],
            )],
        ),
    ] {
        if let Some(n) = variant(op_name, &dict_int) {
            m.push(method(
                &n,
                vec![me(name), param("other", &dict_int)],
                me_ty.clone(),
                vec![each("x", v("other"), body), keep_positive()],
            ));
        }
    }
    if let Some(n) = variant("__iand__", &me_ty) {
        m.push(method(
            &n,
            vec![me(name), param("other", &me_ty)],
            me_ty.clone(),
            vec![
                assign_typed(
                    "keys",
                    mcall(my("data"), "keys", Vec::new()),
                    &IRType::List(Box::new(k.clone())),
                ),
                each(
                    "x",
                    v("keys"),
                    vec![if_(
                        cmp(
                            idx(v("other"), v("x")),
                            IRCompareOp::Lt,
                            idx(my("data"), v("x")),
                        ),
                        vec![set_item(my("data"), v("x"), idx(v("other"), v("x")))],
                    )],
                ),
                keep_positive(),
            ],
        ));
    }

    m.push(method(
        "__repr__",
        vec![me(name)],
        IRType::String,
        vec![
            if_(
                cmp(len(my("data")), IRCompareOp::Eq, crate::ir::build::int(0)),
                vec![ret_val(text("Counter()"))],
            ),
            assign_typed("d", IRExpr::DictLiteral(Vec::new()), &dict_int),
            each(
                "kv",
                mcall(v("self"), "most_common", Vec::new()),
                vec![set_item(
                    v("d"),
                    idx(v("kv"), crate::ir::build::int(0)),
                    idx(v("kv"), crate::ir::build::int(1)),
                )],
            ),
            ret_val(bin(
                bin(text("Counter("), IROp::Add, call("repr", vec![v("d")])),
                IROp::Add,
                text(")"),
            )),
        ],
    ));
    vec![class(
        name,
        Vec::new(),
        m,
        &[
            "keys",
            "values",
            "items",
            "get",
            "pop",
            "popitem",
            "setdefault",
            "clear",
        ],
    )]
}

// ---------------------------------------------------------------------------
// defaultdict
// ---------------------------------------------------------------------------

/// What a defaultdict makes for a missing key.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Factory {
    /// `None`, or no factory: a missing key is a KeyError, as in a dict.
    Nothing,
    /// A builtin type or a class, called with no arguments: `int`, `list`,
    /// `Counter`, `Point`. Its repr is the class's.
    Type(String),
    /// A function of the module, or a lambda the pass lifted to one.
    Function(String),
}

/// The value a factory makes, as an expression, and its repr when CPython's
/// does not show an address.
fn factory_value(factory: &Factory, val: &IRType) -> Option<(IRExpr, Option<IRExpr>)> {
    let class_repr = |n: &str| text(&format!("<class '{n}'>"));
    match factory {
        Factory::Nothing => None,
        Factory::Function(f) => Some((call(f, Vec::new()), None)),
        Factory::Type(t) => Some(match (t.as_str(), val) {
            ("int", _) => (crate::ir::build::int(0), Some(class_repr("int"))),
            ("float", _) => (float(0.0), Some(class_repr("float"))),
            ("str", _) => (text(""), Some(class_repr("str"))),
            ("bool", _) => (boolean(false), Some(class_repr("bool"))),
            ("list", _) => (IRExpr::ListLiteral(Vec::new()), Some(class_repr("list"))),
            ("set", _) => (IRExpr::SetLiteral(Vec::new()), Some(class_repr("set"))),
            ("dict", _) => (IRExpr::DictLiteral(Vec::new()), Some(class_repr("dict"))),
            (_, IRType::Class(c)) => {
                let shown = match spec::parse(c) {
                    Some((kind, _)) => class_repr(&format!("collections.{}", kind.name())),
                    None => call("str", vec![v(t)]),
                };
                (call(c, Vec::new()), Some(shown))
            }
            _ => return None,
        }),
    }
}

pub(crate) fn defaultdict(name: &str, k: &IRType, val: &IRType, factory: &Factory) -> Vec<IRClass> {
    let dict_ty = IRType::Dict(Box::new(k.clone()), Box::new(val.clone()));
    let mut m = Vec::new();
    dict_common(name, k, val, &mut m);
    m.push(method(
        "__getitem__",
        vec![me(name), param("key", k)],
        val.clone(),
        vec![
            if_(
                cmp(v("key"), IRCompareOp::In, my("data")),
                vec![ret_val(idx(my("data"), v("key")))],
            ),
            ret_val(mcall(v("self"), "__missing__", vec![v("key")])),
        ],
    ));
    let made = factory_value(factory, val);
    let missing = match &made {
        None => vec![raise_expr("KeyError", v("key"))],
        Some((value, _)) => vec![
            assign_typed("value", value.clone(), val),
            set_item(my("data"), v("key"), v("value")),
            ret_val(v("value")),
        ],
    };
    m.push(method(
        "__missing__",
        vec![me(name), param("key", k)],
        val.clone(),
        missing,
    ));
    // `<from dict>` and `<from pairs>`: the mapping a defaultdict starts with.
    let pairs = IRType::List(Box::new(IRType::Tuple(vec![k.clone(), val.clone()])));
    for (src, body) in [
        (
            dict_ty.clone(),
            each(
                "x",
                v("src"),
                vec![set_item(my("data"), v("x"), idx(v("src"), v("x")))],
            ),
        ),
        (
            pairs,
            each(
                "kv",
                v("src"),
                vec![set_item(
                    my("data"),
                    idx(v("kv"), crate::ir::build::int(0)),
                    idx(v("kv"), crate::ir::build::int(1)),
                )],
            ),
        ),
    ] {
        if let Some(from) = variant(crate::ir::FROM_METHOD, &src) {
            m.push(method(
                &from,
                vec![me(name), param("src", &src)],
                IRType::Class(name.to_string()),
                vec![body, ret_val(v("self"))],
            ));
        }
    }
    // A factory that is a function shows its address in CPython's repr,
    // which nothing here reproduces, so such a defaultdict has none.
    let shown = match &made {
        None => Some(text("None")),
        Some((_, shown)) => shown.clone(),
    };
    if let Some(shown) = shown {
        m.push(method(
            "__repr__",
            vec![me(name)],
            IRType::String,
            vec![ret_val(bin(
                bin(
                    bin(
                        bin(text("defaultdict("), IROp::Add, shown),
                        IROp::Add,
                        text(", "),
                    ),
                    IROp::Add,
                    call("repr", vec![my("data")]),
                ),
                IROp::Add,
                text(")"),
            ))],
        ));
    }
    vec![class(
        name,
        Vec::new(),
        m,
        &[
            "keys",
            "values",
            "items",
            "get",
            "pop",
            "popitem",
            "setdefault",
            "clear",
            "update",
        ],
    )]
}

// ---------------------------------------------------------------------------
// OrderedDict
// ---------------------------------------------------------------------------

pub(crate) fn ordered_dict(name: &str, k: &IRType, val: &IRType) -> Vec<IRClass> {
    let dict_ty = IRType::Dict(Box::new(k.clone()), Box::new(val.clone()));
    let me_ty = IRType::Class(name.to_string());
    let pair = IRType::Tuple(vec![k.clone(), val.clone()]);
    let mut m = Vec::new();
    dict_common(name, k, val, &mut m);
    m.push(method(
        "__repr__",
        vec![me(name)],
        IRType::String,
        vec![
            if_(
                cmp(len(my("data")), IRCompareOp::Eq, crate::ir::build::int(0)),
                vec![ret_val(text("OrderedDict()"))],
            ),
            ret_val(bin(
                bin(
                    text("OrderedDict("),
                    IROp::Add,
                    call("repr", vec![my("data")]),
                ),
                IROp::Add,
                text(")"),
            )),
        ],
    ));
    // Between two OrderedDicts, equality is order-sensitive.
    m.push(method(
        "__eq__",
        vec![me(name), param("other", &me_ty)],
        IRType::Bool,
        vec![ret_val(cmp(
            mcall(my("data"), "items", Vec::new()),
            IRCompareOp::Eq,
            mcall(attr(v("other"), "data"), "items", Vec::new()),
        ))],
    ));
    m.push(method(
        "move_to_end",
        vec![
            me(name),
            param("key", k),
            param_default("last", &IRType::Bool, boolean(true)),
        ],
        IRType::None,
        vec![
            assign_typed("value", mcall(my("data"), "pop", vec![v("key")]), val),
            if_(
                v("last"),
                vec![set_item(my("data"), v("key"), v("value")), ret()],
            ),
            assign_typed(
                "moved",
                IRExpr::DictLiteral(vec![(v("key"), v("value"))]),
                &dict_ty,
            ),
            each(
                "x",
                my("data"),
                vec![set_item(v("moved"), v("x"), idx(my("data"), v("x")))],
            ),
            set_attr(v("self"), "data", v("moved")),
        ],
    ));
    m.push(method(
        "popitem",
        vec![
            me(name),
            param_default("last", &IRType::Bool, boolean(true)),
        ],
        pair.clone(),
        vec![
            if_(
                cmp(len(my("data")), IRCompareOp::Eq, crate::ir::build::int(0)),
                vec![raise_expr("KeyError", text("dictionary is empty"))],
            ),
            if_(
                v("last"),
                vec![ret_val(mcall(my("data"), "popitem", Vec::new()))],
            ),
            each(
                "x",
                my("data"),
                vec![
                    assign_typed("value", mcall(my("data"), "pop", vec![v("x")]), val),
                    ret_val(IRExpr::TupleLiteral(vec![v("x"), v("value")])),
                ],
            ),
            raise_expr("KeyError", text("dictionary is empty")),
        ],
    ));
    let pairs = IRType::List(Box::new(pair));
    for (src, body) in [
        (
            dict_ty.clone(),
            each(
                "x",
                v("src"),
                vec![set_item(my("data"), v("x"), idx(v("src"), v("x")))],
            ),
        ),
        (
            pairs,
            each(
                "kv",
                v("src"),
                vec![set_item(
                    my("data"),
                    idx(v("kv"), crate::ir::build::int(0)),
                    idx(v("kv"), crate::ir::build::int(1)),
                )],
            ),
        ),
    ] {
        if let Some(from) = variant(crate::ir::FROM_METHOD, &src) {
            m.push(method(
                &from,
                vec![me(name), param("src", &src)],
                me_ty.clone(),
                vec![body, ret_val(v("self"))],
            ));
        }
    }
    vec![class(
        name,
        Vec::new(),
        m,
        &[
            "keys",
            "values",
            "items",
            "get",
            "pop",
            "setdefault",
            "clear",
            "update",
        ],
    )]
}

// ---------------------------------------------------------------------------
// deque
// ---------------------------------------------------------------------------

pub(crate) fn deque(name: &str, t: &IRType, generators: &[(String, IRType)]) -> Vec<IRClass> {
    let list_ty = IRType::List(Box::new(t.clone()));
    let me_ty = IRType::Class(name.to_string());
    let opt_int = IRType::Optional(Box::new(IRType::Int));
    let limit = || my("limit");
    let size = || len(my("data"));
    let mut m = Vec::new();
    m.push(method(
        "__init__",
        vec![me(name), param_default("maxlen", &opt_int, none())],
        IRType::None,
        vec![
            set_my_typed("data", IRExpr::ListLiteral(Vec::new()), &list_ty),
            set_my_typed("limit", crate::ir::build::int(-1), &IRType::Int),
            if_(
                cmp(v("maxlen"), IRCompareOp::IsNot, none()),
                vec![
                    if_(
                        cmp(v("maxlen"), IRCompareOp::Lt, crate::ir::build::int(0)),
                        vec![raise_expr(
                            "ValueError",
                            text("maxlen must be non-negative"),
                        )],
                    ),
                    set_attr(v("self"), "limit", v("maxlen")),
                ],
            ),
        ],
    ));
    m.push(method(
        crate::ir::DATA_METHOD,
        vec![me(name)],
        list_ty.clone(),
        vec![ret_val(my("data"))],
    ));
    m.push(method(
        "__len__",
        vec![me(name)],
        IRType::Int,
        vec![ret_val(size())],
    ));
    let mut maxlen = method(
        "maxlen",
        vec![me(name)],
        opt_int.clone(),
        vec![
            if_(
                cmp(limit(), IRCompareOp::Lt, crate::ir::build::int(0)),
                vec![ret_val(none())],
            ),
            ret_val(limit()),
        ],
    );
    maxlen.decorators = vec!["property".to_string()];
    m.push(maxlen);
    // At maxlen, adding at one end drops from the other.
    let over = || {
        and(
            cmp(limit(), IRCompareOp::GtE, crate::ir::build::int(0)),
            cmp(size(), IRCompareOp::Gt, limit()),
        )
    };
    m.push(method(
        "append",
        vec![me(name), param("x", t)],
        IRType::None,
        vec![
            expr_stmt(mcall(my("data"), "append", vec![v("x")])),
            if_(
                over(),
                vec![expr_stmt(mcall(
                    my("data"),
                    "pop",
                    vec![crate::ir::build::int(0)],
                ))],
            ),
        ],
    ));
    m.push(method(
        "appendleft",
        vec![me(name), param("x", t)],
        IRType::None,
        vec![
            expr_stmt(mcall(
                my("data"),
                "insert",
                vec![crate::ir::build::int(0), v("x")],
            )),
            if_(
                over(),
                vec![expr_stmt(mcall(my("data"), "pop", Vec::new()))],
            ),
        ],
    ));
    let empty_check = || {
        if_(
            cmp(size(), IRCompareOp::Eq, crate::ir::build::int(0)),
            vec![raise_expr("IndexError", text("pop from an empty deque"))],
        )
    };
    m.push(method(
        "pop",
        vec![me(name)],
        t.clone(),
        vec![empty_check(), ret_val(mcall(my("data"), "pop", Vec::new()))],
    ));
    m.push(method(
        "popleft",
        vec![me(name)],
        t.clone(),
        vec![
            empty_check(),
            ret_val(mcall(my("data"), "pop", vec![crate::ir::build::int(0)])),
        ],
    ));
    // extend / extendleft, from each source type; extending a deque by
    // itself walks a copy, as CPython's does.
    let mut sources = iterable_sources(t, generators);
    sources.push(me_ty.clone());
    for (base, add) in [("extend", "append"), ("extendleft", "appendleft")] {
        for s in &sources {
            let Some(n) = variant(base, s) else {
                continue;
            };
            let walk = if *s == me_ty {
                slice(attr(v("src"), "data"), None, None)
            } else {
                v("src")
            };
            m.push(method(
                &n,
                vec![me(name), param("src", s)],
                IRType::None,
                vec![each(
                    "x",
                    walk,
                    vec![expr_stmt(mcall(v("self"), add, vec![v("x")]))],
                )],
            ));
        }
    }
    from_variants(name, "extend", &sources, &mut m);
    for s in &sources {
        if let Some(n) = variant("__iadd__", s) {
            let via = variant("extend", s).expect("named above");
            m.push(method(
                &n,
                vec![me(name), param("src", s)],
                me_ty.clone(),
                vec![
                    expr_stmt(mcall(v("self"), &via, vec![v("src")])),
                    ret_val(v("self")),
                ],
            ));
        }
    }
    m.push(method(
        "index",
        vec![me(name), param("x", t)],
        IRType::Int,
        vec![
            assign("i", crate::ir::build::int(0)),
            each(
                "y",
                my("data"),
                vec![
                    if_(cmp(v("y"), IRCompareOp::Eq, v("x")), vec![ret_val(v("i"))]),
                    add_to("i", crate::ir::build::int(1)),
                ],
            ),
            raise_expr(
                "ValueError",
                bin(
                    call("repr", vec![v("x")]),
                    IROp::Add,
                    text(" is not in deque"),
                ),
            ),
        ],
    ));
    m.push(method(
        "insert",
        vec![me(name), param("i", &IRType::Int), param("x", t)],
        IRType::None,
        vec![
            if_(
                and(
                    cmp(limit(), IRCompareOp::GtE, crate::ir::build::int(0)),
                    cmp(size(), IRCompareOp::GtE, limit()),
                ),
                vec![raise_expr(
                    "IndexError",
                    text("deque already at its maximum size"),
                )],
            ),
            expr_stmt(mcall(my("data"), "insert", vec![v("i"), v("x")])),
        ],
    ));
    m.push(method(
        "remove",
        vec![me(name), param("x", t)],
        IRType::None,
        vec![
            assign("i", crate::ir::build::int(0)),
            each(
                "y",
                my("data"),
                vec![
                    if_(
                        cmp(v("y"), IRCompareOp::Eq, v("x")),
                        vec![del_item(my("data"), v("i")), ret()],
                    ),
                    add_to("i", crate::ir::build::int(1)),
                ],
            ),
            raise_expr(
                "ValueError",
                bin(
                    call("repr", vec![v("x")]),
                    IROp::Add,
                    text(" is not in deque"),
                ),
            ),
        ],
    ));
    m.push(method(
        "rotate",
        vec![
            me(name),
            param_default("n", &IRType::Int, crate::ir::build::int(1)),
        ],
        IRType::None,
        vec![
            assign("size", size()),
            if_(
                cmp(v("size"), IRCompareOp::LtE, crate::ir::build::int(1)),
                vec![ret()],
            ),
            assign("k", bin(v("n"), IROp::Mod, v("size"))),
            if_(
                cmp(v("k"), IRCompareOp::Eq, crate::ir::build::int(0)),
                vec![ret()],
            ),
            assign("cut", bin(v("size"), IROp::Sub, v("k"))),
            set_attr(
                v("self"),
                "data",
                bin(
                    slice(my("data"), Some(v("cut")), None),
                    IROp::Add,
                    slice(my("data"), None, Some(v("cut"))),
                ),
            ),
        ],
    ));
    m.push(method(
        "copy",
        vec![me(name)],
        me_ty.clone(),
        vec![
            assign("new", call(name, Vec::new())),
            set_attr(v("new"), "limit", limit()),
            set_attr(v("new"), "data", slice(my("data"), None, None)),
            ret_val(v("new")),
        ],
    ));
    // Indexing checks against the deque's own bounds, with its own message.
    let checked = |rest: Vec<IRStatement>| {
        let mut body = vec![
            assign("n", size()),
            if_(
                cmp(v("i"), IRCompareOp::Lt, crate::ir::build::int(0)),
                vec![add_to("i", v("n"))],
            ),
            if_(
                or(
                    cmp(v("i"), IRCompareOp::Lt, crate::ir::build::int(0)),
                    cmp(v("i"), IRCompareOp::GtE, v("n")),
                ),
                vec![raise_expr("IndexError", text("deque index out of range"))],
            ),
        ];
        body.extend(rest);
        body
    };
    m.push(method(
        "__getitem__",
        vec![me(name), param("i", &IRType::Int)],
        t.clone(),
        checked(vec![ret_val(idx(my("data"), v("i")))]),
    ));
    m.push(method(
        "__setitem__",
        vec![me(name), param("i", &IRType::Int), param("x", t)],
        IRType::None,
        checked(vec![set_item(my("data"), v("i"), v("x"))]),
    ));
    m.push(method(
        "__delitem__",
        vec![me(name), param("i", &IRType::Int)],
        IRType::None,
        checked(vec![del_item(my("data"), v("i"))]),
    ));
    m.push(method(
        "__repr__",
        vec![me(name)],
        IRType::String,
        vec![
            assign("shown", call("repr", vec![my("data")])),
            if_(
                cmp(limit(), IRCompareOp::Lt, crate::ir::build::int(0)),
                vec![ret_val(bin(
                    bin(text("deque("), IROp::Add, v("shown")),
                    IROp::Add,
                    text(")"),
                ))],
            ),
            ret_val(bin(
                bin(
                    bin(
                        bin(text("deque("), IROp::Add, v("shown")),
                        IROp::Add,
                        text(", maxlen="),
                    ),
                    IROp::Add,
                    call("str", vec![limit()]),
                ),
                IROp::Add,
                text(")"),
            )),
        ],
    ));
    for (op_name, op) in [
        ("__eq__", IRCompareOp::Eq),
        ("__lt__", IRCompareOp::Lt),
        ("__le__", IRCompareOp::LtE),
        ("__gt__", IRCompareOp::Gt),
        ("__ge__", IRCompareOp::GtE),
    ] {
        m.push(method(
            op_name,
            vec![me(name), param("other", &me_ty)],
            IRType::Bool,
            vec![ret_val(cmp(my("data"), op, attr(v("other"), "data")))],
        ));
    }
    m.push(method(
        "__add__",
        vec![me(name), param("other", &me_ty)],
        me_ty.clone(),
        vec![
            assign("new", mcall(v("self"), "copy", Vec::new())),
            expr_stmt(mcall(
                v("new"),
                &variant("extend", &me_ty).expect("a class name is spelled"),
                vec![v("other")],
            )),
            ret_val(v("new")),
        ],
    ));
    m.push(method(
        "__mul__",
        vec![me(name), param("n", &IRType::Int)],
        me_ty.clone(),
        vec![
            assign("new", call(name, Vec::new())),
            set_attr(v("new"), "limit", limit()),
            assign("i", crate::ir::build::int(0)),
            while_(
                cmp(v("i"), IRCompareOp::Lt, v("n")),
                vec![
                    each(
                        "x",
                        my("data"),
                        vec![expr_stmt(mcall(v("new"), "append", vec![v("x")]))],
                    ),
                    add_to("i", crate::ir::build::int(1)),
                ],
            ),
            ret_val(v("new")),
        ],
    ));
    vec![class(name, Vec::new(), m, &["count", "reverse", "clear"])]
}

// ---------------------------------------------------------------------------
// ChainMap
// ---------------------------------------------------------------------------

pub(crate) fn chain_map(name: &str, k: &IRType, val: &IRType) -> Vec<IRClass> {
    let dict_ty = IRType::Dict(Box::new(k.clone()), Box::new(val.clone()));
    let maps_ty = IRType::List(Box::new(dict_ty.clone()));
    let me_ty = IRType::Class(name.to_string());
    let first = || idx(my("maps"), crate::ir::build::int(0));
    let not_found = || {
        raise_expr(
            "KeyError",
            bin(
                text("Key not found in the first mapping: "),
                IROp::Add,
                call("repr", vec![v("key")]),
            ),
        )
    };
    let mut m = vec![
        method(
            "__init__",
            vec![me(name)],
            IRType::None,
            vec![set_my_typed(
                "maps",
                IRExpr::ListLiteral(vec![IRExpr::DictLiteral(Vec::new())]),
                &maps_ty,
            )],
        ),
        method(
            &variant(crate::ir::FROM_METHOD, &maps_ty).expect("spelled"),
            vec![me(name), param("maps", &maps_ty)],
            me_ty.clone(),
            vec![set_attr(v("self"), "maps", v("maps")), ret_val(v("self"))],
        ),
        // Merged as iterating a ChainMap sees it: each key where the last
        // map holding it puts it, with the first map's value.
        method(
            crate::ir::VIEW_METHOD,
            vec![me(name)],
            dict_ty.clone(),
            vec![
                assign_typed("merged", IRExpr::DictLiteral(Vec::new()), &dict_ty),
                assign(
                    "i",
                    bin(len(my("maps")), IROp::Sub, crate::ir::build::int(1)),
                ),
                while_(
                    cmp(v("i"), IRCompareOp::GtE, crate::ir::build::int(0)),
                    vec![
                        assign_typed("one", idx(my("maps"), v("i")), &dict_ty),
                        each(
                            "x",
                            v("one"),
                            vec![set_item(v("merged"), v("x"), idx(v("one"), v("x")))],
                        ),
                        assign("i", bin(v("i"), IROp::Sub, crate::ir::build::int(1))),
                    ],
                ),
                ret_val(v("merged")),
            ],
        ),
        method(
            "__getitem__",
            vec![me(name), param("key", k)],
            val.clone(),
            vec![
                each(
                    "one",
                    my("maps"),
                    vec![if_(
                        cmp(v("key"), IRCompareOp::In, v("one")),
                        vec![ret_val(idx(v("one"), v("key")))],
                    )],
                ),
                raise_expr("KeyError", v("key")),
            ],
        ),
        method(
            "__setitem__",
            vec![me(name), param("key", k), param("value", val)],
            IRType::None,
            vec![set_item(first(), v("key"), v("value"))],
        ),
        method(
            "__delitem__",
            vec![me(name), param("key", k)],
            IRType::None,
            vec![
                if_(
                    cmp(v("key"), IRCompareOp::In, first()),
                    vec![del_item(first(), v("key")), ret()],
                ),
                not_found(),
            ],
        ),
        method(
            "__contains__",
            vec![me(name), param("key", k)],
            IRType::Bool,
            vec![
                each(
                    "one",
                    my("maps"),
                    vec![if_(
                        cmp(v("key"), IRCompareOp::In, v("one")),
                        vec![ret_val(boolean(true))],
                    )],
                ),
                ret_val(boolean(false)),
            ],
        ),
        method(
            "__len__",
            vec![me(name)],
            IRType::Int,
            vec![ret_val(len(mcall(
                v("self"),
                crate::ir::VIEW_METHOD,
                Vec::new(),
            )))],
        ),
        method(
            "pop",
            vec![me(name), param("key", k)],
            val.clone(),
            vec![
                if_(
                    cmp(v("key"), IRCompareOp::In, first()),
                    vec![ret_val(mcall(first(), "pop", vec![v("key")]))],
                ),
                not_found(),
            ],
        ),
        method(
            "popitem",
            vec![me(name)],
            IRType::Tuple(vec![k.clone(), val.clone()]),
            vec![
                if_(
                    cmp(len(first()), IRCompareOp::Eq, crate::ir::build::int(0)),
                    vec![raise_expr(
                        "KeyError",
                        text("No keys found in the first mapping."),
                    )],
                ),
                ret_val(mcall(first(), "popitem", Vec::new())),
            ],
        ),
        method(
            "clear",
            vec![me(name)],
            IRType::None,
            vec![expr_stmt(mcall(first(), "clear", Vec::new()))],
        ),
        method(
            "new_child",
            vec![me(name)],
            me_ty.clone(),
            vec![
                assign("new", call(name, Vec::new())),
                assign_typed(
                    "head",
                    IRExpr::ListLiteral(vec![IRExpr::DictLiteral(Vec::new())]),
                    &maps_ty,
                ),
                set_attr(v("new"), "maps", bin(v("head"), IROp::Add, my("maps"))),
                ret_val(v("new")),
            ],
        ),
        method(
            &variant("new_child", &dict_ty).expect("spelled"),
            vec![me(name), param("m", &dict_ty)],
            me_ty.clone(),
            vec![
                assign("new", call(name, Vec::new())),
                set_attr(
                    v("new"),
                    "maps",
                    bin(IRExpr::ListLiteral(vec![v("m")]), IROp::Add, my("maps")),
                ),
                ret_val(v("new")),
            ],
        ),
        method(
            "copy",
            vec![me(name)],
            me_ty.clone(),
            vec![
                assign("new", call(name, Vec::new())),
                set_attr(
                    v("new"),
                    "maps",
                    bin(
                        IRExpr::ListLiteral(vec![mcall(first(), "copy", Vec::new())]),
                        IROp::Add,
                        slice(my("maps"), Some(crate::ir::build::int(1)), None),
                    ),
                ),
                ret_val(v("new")),
            ],
        ),
        method(
            &variant("update", &dict_ty).expect("spelled"),
            vec![me(name), param("src", &dict_ty)],
            IRType::None,
            vec![each(
                "x",
                v("src"),
                vec![set_item(first(), v("x"), idx(v("src"), v("x")))],
            )],
        ),
        method(
            "__repr__",
            vec![me(name)],
            IRType::String,
            vec![
                assign("shown", text("ChainMap(")),
                assign("i", crate::ir::build::int(0)),
                each(
                    "one",
                    my("maps"),
                    vec![
                        if_(
                            cmp(v("i"), IRCompareOp::Gt, crate::ir::build::int(0)),
                            vec![add_to("shown", text(", "))],
                        ),
                        add_to("shown", call("repr", vec![v("one")])),
                        add_to("i", crate::ir::build::int(1)),
                    ],
                ),
                ret_val(bin(v("shown"), IROp::Add, text(")"))),
            ],
        ),
    ];
    let mut parents = method(
        "parents",
        vec![me(name)],
        me_ty.clone(),
        vec![
            assign("new", call(name, Vec::new())),
            assign_typed(
                "rest",
                slice(my("maps"), Some(crate::ir::build::int(1)), None),
                &maps_ty,
            ),
            if_(
                cmp(len(v("rest")), IRCompareOp::Gt, crate::ir::build::int(0)),
                vec![set_attr(v("new"), "maps", v("rest"))],
            ),
            ret_val(v("new")),
        ],
    );
    parents.decorators = vec!["property".to_string()];
    m.push(parents);
    vec![class(
        name,
        Vec::new(),
        m,
        &["keys", "values", "items", "get"],
    )]
}

/// The classes `kind` specialized by `args` is made of, named `name`.
pub(crate) fn synthesize(
    kind: Kind,
    name: &str,
    args: &[IRType],
    factory: &Factory,
    generators: &[(String, IRType)],
) -> Vec<IRClass> {
    match kind {
        Kind::Counter => counter(name, &args[0], generators),
        Kind::DefaultDict => defaultdict(name, &args[0], &args[1], factory),
        Kind::OrderedDict => ordered_dict(name, &args[0], &args[1]),
        Kind::Deque => deque(name, &args[0], generators),
        Kind::ChainMap => chain_map(name, &args[0], &args[1]),
    }
}
