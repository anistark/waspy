//! `str` methods written out in IR over the ones code generation implements
//! (`startswith`, `find`, slicing, `len`, `join`): `removeprefix`,
//! `removesuffix`, `rfind`, `rindex`, `partition`, `rpartition`, `zfill`,
//! `expandtabs`, `splitlines`, `rsplit`, and `replace` with a count, each as
//! CPython's `unicodeobject.c` answers it.
//!
//! A helper is a function `__str_<method>` added to the module just before
//! code generation (`with_str_helpers`), only when the program calls a method
//! of that name, so a merge of several files never sees two of one. Code
//! generation sends a call on a str receiver to it
//! (`compiler::dunders::rewrite_str_method`), with the method's defaults
//! filled in.

use crate::ir::build::*;
use crate::ir::{IRCompareOp, IRExpr, IRFunction, IRModule, IROp, IRStatement, IRType};
use std::collections::BTreeSet;

/// The prefix of the helpers' names; they are internal.
pub const STR_FN_PREFIX: &str = "__str_";

/// The methods written here.
pub const STR_METHODS: [&str; 11] = [
    "removeprefix",
    "removesuffix",
    "rfind",
    "rindex",
    "partition",
    "rpartition",
    "zfill",
    "expandtabs",
    "splitlines",
    "rsplit",
    "replace",
];

/// CPython's line boundaries, which `splitlines` splits at.
const LINE_BREAKS: &str = "\n\r\u{b}\u{c}\u{1c}\u{1d}\u{1e}\u{85}\u{2028}\u{2029}";

/// The module with the helpers for the `str` methods it calls, or `None`
/// when it calls none of them. `replace` takes a helper only for a call with
/// a count; the two-argument form is code generation's.
pub fn with_str_helpers(module: &IRModule) -> Option<IRModule> {
    let mut wanted: BTreeSet<&'static str> = BTreeSet::new();
    let mut probe: Vec<crate::ir::IRBody> = module
        .functions
        .iter()
        .map(|f| f.body.clone())
        .chain(
            module
                .classes
                .iter()
                .flat_map(|c| c.methods.iter().map(|m| m.body.clone())),
        )
        .collect();
    for body in &mut probe {
        let _ = crate::ir::finalize::visit_body(body, &mut |e| {
            if let IRExpr::MethodCall {
                method_name,
                arguments,
                ..
            } = e
            {
                if let Some(m) = STR_METHODS.iter().find(|m| **m == method_name.as_str()) {
                    if *m != "replace" || arguments.len() == 3 {
                        wanted.insert(m);
                    }
                }
            }
            Ok(())
        });
    }
    // The helpers a helper calls.
    if wanted.contains("rindex") || wanted.contains("rpartition") {
        wanted.insert("rfind");
    }
    if wanted.is_empty() {
        return None;
    }
    let mut module = module.clone();
    for text in [
        "",
        "0",
        " ",
        "\t",
        "\n",
        "\r",
        "+",
        "-",
        LINE_BREAKS,
        "substring not found",
        "empty separator",
    ] {
        module.memory_layout.add_string(text);
    }
    for name in wanted {
        if module.functions.iter().any(|f| f.name == helper_name(name)) {
            continue;
        }
        module.functions.push(helper(name));
    }
    Some(module)
}

/// The helper's function name for `method`.
pub fn helper_name(method: &str) -> String {
    format!("{STR_FN_PREFIX}{method}")
}

fn s() -> IRExpr {
    v("s")
}
fn add(l: IRExpr, r: IRExpr) -> IRExpr {
    bin(l, IROp::Add, r)
}
fn sub(l: IRExpr, r: IRExpr) -> IRExpr {
    bin(l, IROp::Sub, r)
}
fn eq(l: IRExpr, r: IRExpr) -> IRExpr {
    cmp(l, IRCompareOp::Eq, r)
}
fn lt(l: IRExpr, r: IRExpr) -> IRExpr {
    cmp(l, IRCompareOp::Lt, r)
}
fn ge(l: IRExpr, r: IRExpr) -> IRExpr {
    cmp(l, IRCompareOp::GtE, r)
}
fn strs() -> IRType {
    IRType::List(Box::new(IRType::String))
}
fn empty_parts(name: &str) -> IRStatement {
    assign_typed(name, IRExpr::ListLiteral(Vec::new()), &strs())
}
fn push(list: &str, value: IRExpr) -> IRStatement {
    expr_stmt(mcall(v(list), "append", vec![value]))
}
fn joined(list: &str) -> IRExpr {
    mcall(text(""), "join", vec![v(list)])
}
fn incr(name: &str, by: i32) -> IRStatement {
    assign(name, add(v(name), int(by)))
}
fn three(a: IRExpr, b: IRExpr, c: IRExpr) -> IRExpr {
    IRExpr::TupleLiteral(vec![a, b, c])
}
fn triple() -> IRType {
    IRType::Tuple(vec![IRType::String, IRType::String, IRType::String])
}

fn helper(method: &str) -> IRFunction {
    let string = IRType::String;
    let int_t = IRType::Int;
    let name = helper_name(method);
    let f = |params, ret, statements| method_fn(&name, params, ret, statements);
    match method {
        "removeprefix" => f(
            vec![param("s", &string), param("prefix", &string)],
            string.clone(),
            vec![
                if_(
                    mcall(s(), "startswith", vec![v("prefix")]),
                    vec![ret_val(slice(s(), Some(len(v("prefix"))), None))],
                ),
                ret_val(s()),
            ],
        ),
        "removesuffix" => f(
            vec![param("s", &string), param("suffix", &string)],
            string.clone(),
            vec![
                if_(
                    and(
                        cmp(len(v("suffix")), IRCompareOp::Gt, int(0)),
                        mcall(s(), "endswith", vec![v("suffix")]),
                    ),
                    vec![ret_val(slice(
                        s(),
                        None,
                        Some(sub(len(s()), len(v("suffix")))),
                    ))],
                ),
                ret_val(s()),
            ],
        ),
        // The highest position `sub` starts at, or -1.
        "rfind" => f(
            vec![param("s", &string), param("sub", &string)],
            int_t.clone(),
            vec![
                assign_typed("m", len(v("sub")), &int_t),
                assign_typed("i", sub(len(s()), v("m")), &int_t),
                while_(
                    ge(v("i"), int(0)),
                    vec![
                        if_(
                            eq(
                                slice(s(), Some(v("i")), Some(add(v("i"), v("m")))),
                                v("sub"),
                            ),
                            vec![ret_val(v("i"))],
                        ),
                        incr("i", -1),
                    ],
                ),
                ret_val(int(-1)),
            ],
        ),
        "rindex" => f(
            vec![param("s", &string), param("sub", &string)],
            int_t.clone(),
            vec![
                assign_typed(
                    "i",
                    call(&helper_name("rfind"), vec![s(), v("sub")]),
                    &int_t,
                ),
                if_(
                    lt(v("i"), int(0)),
                    vec![raise("ValueError", "substring not found")],
                ),
                ret_val(v("i")),
            ],
        ),
        "partition" | "rpartition" => {
            let right = method == "rpartition";
            let found = if right {
                call(&helper_name("rfind"), vec![s(), v("sep")])
            } else {
                mcall(s(), "find", vec![v("sep")])
            };
            let missing = if right {
                three(text(""), text(""), s())
            } else {
                three(s(), text(""), text(""))
            };
            f(
                vec![param("s", &string), param("sep", &string)],
                triple(),
                vec![
                    if_(
                        eq(len(v("sep")), int(0)),
                        vec![raise("ValueError", "empty separator")],
                    ),
                    assign_typed("i", found, &int_t),
                    if_(lt(v("i"), int(0)), vec![ret_val(missing)]),
                    ret_val(three(
                        slice(s(), None, Some(v("i"))),
                        v("sep"),
                        slice(s(), Some(add(v("i"), len(v("sep")))), None),
                    )),
                ],
            )
        }
        // Zeros after the sign, to `width`.
        "zfill" => f(
            vec![param("s", &string), param("width", &int_t)],
            string.clone(),
            vec![
                assign_typed("n", len(s()), &int_t),
                if_(ge(v("n"), v("width")), vec![ret_val(s())]),
                assign_typed(
                    "fill",
                    bin(text("0"), IROp::Mul, sub(v("width"), v("n"))),
                    &string,
                ),
                if_(
                    and(
                        cmp(v("n"), IRCompareOp::Gt, int(0)),
                        or(
                            eq(idx(s(), int(0)), text("+")),
                            eq(idx(s(), int(0)), text("-")),
                        ),
                    ),
                    vec![ret_val(add(
                        add(idx(s(), int(0)), v("fill")),
                        slice(s(), Some(int(1)), None),
                    ))],
                ),
                ret_val(add(v("fill"), s())),
            ],
        ),
        // A tab to the next multiple of `tabsize` columns; a line break
        // starts the count again.
        "expandtabs" => f(
            vec![param("s", &string), param("tabsize", &int_t)],
            string.clone(),
            vec![
                empty_parts("parts"),
                assign_typed("col", int(0), &int_t),
                for_(
                    "ch",
                    s(),
                    vec![if_else(
                        eq(v("ch"), text("\t")),
                        vec![if_(
                            cmp(v("tabsize"), IRCompareOp::Gt, int(0)),
                            vec![
                                assign_typed(
                                    "k",
                                    sub(v("tabsize"), bin(v("col"), IROp::Mod, v("tabsize"))),
                                    &int_t,
                                ),
                                push("parts", bin(text(" "), IROp::Mul, v("k"))),
                                assign("col", add(v("col"), v("k"))),
                            ],
                        )],
                        vec![
                            push("parts", v("ch")),
                            if_else(
                                or(eq(v("ch"), text("\n")), eq(v("ch"), text("\r"))),
                                vec![assign("col", int(0))],
                                vec![incr("col", 1)],
                            ),
                        ],
                    )],
                ),
                ret_val(joined("parts")),
            ],
        ),
        "splitlines" => f(
            vec![param("s", &string), param("keepends", &IRType::Bool)],
            strs(),
            vec![
                empty_parts("lines"),
                assign_typed("n", len(s()), &int_t),
                assign_typed("start", int(0), &int_t),
                assign_typed("i", int(0), &int_t),
                while_(
                    lt(v("i"), v("n")),
                    vec![
                        assign_typed("ch", idx(s(), v("i")), &string),
                        if_else(
                            cmp(v("ch"), IRCompareOp::In, text(LINE_BREAKS)),
                            vec![
                                assign_typed("eol", add(v("i"), int(1)), &int_t),
                                if_(
                                    and(
                                        eq(v("ch"), text("\r")),
                                        and(
                                            lt(v("eol"), v("n")),
                                            eq(idx(s(), v("eol")), text("\n")),
                                        ),
                                    ),
                                    vec![incr("eol", 1)],
                                ),
                                if_else(
                                    v("keepends"),
                                    vec![push(
                                        "lines",
                                        slice(s(), Some(v("start")), Some(v("eol"))),
                                    )],
                                    vec![push("lines", slice(s(), Some(v("start")), Some(v("i"))))],
                                ),
                                assign("start", v("eol")),
                                assign("i", v("eol")),
                            ],
                            vec![incr("i", 1)],
                        ),
                    ],
                ),
                if_(
                    lt(v("start"), v("n")),
                    vec![push("lines", slice(s(), Some(v("start")), None))],
                ),
                ret_val(v("lines")),
            ],
        ),
        "rsplit" => rsplit(&name),
        // `replace(old, new, count)`: at most `count` replacements, from the
        // left; a negative count is every one.
        _ => f(
            vec![
                param("s", &string),
                param("old", &string),
                param("new", &string),
                param("count", &int_t),
            ],
            string.clone(),
            vec![
                if_(
                    lt(v("count"), int(0)),
                    vec![ret_val(mcall(s(), "replace", vec![v("old"), v("new")]))],
                ),
                empty_parts("parts"),
                assign_typed("done", int(0), &int_t),
                // An empty `old` matches before each character and at the end.
                if_(
                    eq(len(v("old")), int(0)),
                    vec![
                        for_(
                            "ch",
                            s(),
                            vec![
                                if_(
                                    lt(v("done"), v("count")),
                                    vec![push("parts", v("new")), incr("done", 1)],
                                ),
                                push("parts", v("ch")),
                            ],
                        ),
                        if_(lt(v("done"), v("count")), vec![push("parts", v("new"))]),
                        ret_val(joined("parts")),
                    ],
                ),
                assign_typed("i", int(0), &int_t),
                assign_typed("m", len(v("old")), &int_t),
                while_(
                    lt(v("done"), v("count")),
                    vec![
                        assign_typed(
                            "j",
                            mcall(slice(s(), Some(v("i")), None), "find", vec![v("old")]),
                            &int_t,
                        ),
                        if_(lt(v("j"), int(0)), vec![IRStatement::Break]),
                        push("parts", slice(s(), Some(v("i")), Some(add(v("i"), v("j"))))),
                        push("parts", v("new")),
                        assign("i", add(add(v("i"), v("j")), v("m"))),
                        incr("done", 1),
                    ],
                ),
                push("parts", slice(s(), Some(v("i")), None)),
                ret_val(joined("parts")),
            ],
        ),
    }
}

fn method_fn(
    name: &str,
    params: Vec<crate::ir::IRParam>,
    ret: IRType,
    statements: Vec<IRStatement>,
) -> IRFunction {
    method(name, params, ret, statements)
}

/// `rsplit(sep, maxsplit)`: split from the right, at most `maxsplit` times
/// (every time when negative), on runs of whitespace when `sep` is None, as
/// CPython's `rsplit_whitespace` and `rsplit` do.
fn rsplit(name: &str) -> IRFunction {
    let int_t = IRType::Int;
    let space = |at: IRExpr| mcall(idx(s(), at), "isspace", Vec::new());
    let more = or(lt(v("maxsplit"), int(0)), lt(v("count"), v("maxsplit")));
    let whitespace = vec![
        assign_typed("i", sub(len(s()), int(1)), &int_t),
        while_(
            more.clone(),
            vec![
                while_(and(ge(v("i"), int(0)), space(v("i"))), vec![incr("i", -1)]),
                if_(lt(v("i"), int(0)), vec![IRStatement::Break]),
                assign_typed("j", v("i"), &int_t),
                incr("i", -1),
                while_(
                    and(ge(v("i"), int(0)), not(space(v("i")))),
                    vec![incr("i", -1)],
                ),
                push(
                    "parts",
                    slice(s(), Some(add(v("i"), int(1))), Some(add(v("j"), int(1)))),
                ),
                incr("count", 1),
            ],
        ),
        // Only when `maxsplit` ran out: what is left, without its
        // trailing whitespace.
        if_(
            ge(v("i"), int(0)),
            vec![
                while_(and(ge(v("i"), int(0)), space(v("i"))), vec![incr("i", -1)]),
                if_(
                    ge(v("i"), int(0)),
                    vec![push("parts", slice(s(), None, Some(add(v("i"), int(1)))))],
                ),
            ],
        ),
    ];
    let separator = vec![
        if_(
            eq(len(v("sep")), int(0)),
            vec![raise("ValueError", "empty separator")],
        ),
        assign_typed("m", len(v("sep")), &int_t),
        assign_typed("end", len(s()), &int_t),
        assign_typed("i", sub(v("end"), v("m")), &int_t),
        while_(
            and(ge(v("i"), int(0)), more),
            vec![if_else(
                eq(
                    slice(s(), Some(v("i")), Some(add(v("i"), v("m")))),
                    v("sep"),
                ),
                vec![
                    push(
                        "parts",
                        slice(s(), Some(add(v("i"), v("m"))), Some(v("end"))),
                    ),
                    assign("end", v("i")),
                    assign("i", sub(v("i"), v("m"))),
                    incr("count", 1),
                ],
                vec![incr("i", -1)],
            )],
        ),
        push("parts", slice(s(), None, Some(v("end")))),
    ];
    method(
        name,
        vec![
            param("s", &IRType::String),
            param("sep", &IRType::Optional(Box::new(IRType::String))),
            param("maxsplit", &int_t),
        ],
        strs(),
        vec![
            empty_parts("parts"),
            assign_typed("count", int(0), &int_t),
            if_else(
                cmp(v("sep"), IRCompareOp::Is, none()),
                whitespace,
                separator,
            ),
            expr_stmt(mcall(v("parts"), "reverse", Vec::new())),
            ret_val(v("parts")),
        ],
    )
}
