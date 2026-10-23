//! `re.Pattern`, `re.Match`, and what they are made of, as `_sre.c` gives
//! them: the scans of `search`, `findall`, `finditer`, `split`, and `sub`
//! (each search after an empty match told not to match empty at its start,
//! as CPython's `must_advance` does), the groups of a match, and the
//! replacement template.
//!
//! A pattern's `findall` and `split` answer by how many groups it has, so a
//! pattern is an instance of one of three subclasses of `re.Pattern`: none,
//! one, or several.

use super::{class_name, fname};
use crate::ir::build::*;
use crate::ir::{
    IRClass, IRCompareOp, IRExpr, IRFunction, IROp, IRParam, IRStatement, IRType, IRVariable,
};

pub(super) const PATTERN0: &str = "_Pattern0";
pub(super) const PATTERN1: &str = "_Pattern1";
pub(super) const PATTERN_N: &str = "_PatternN";

fn i() -> IRType {
    IRType::Int
}
fn s() -> IRType {
    IRType::String
}
fn b() -> IRType {
    IRType::Bool
}
fn li() -> IRType {
    IRType::List(Box::new(i()))
}
fn ls() -> IRType {
    IRType::List(Box::new(s()))
}
fn os() -> IRType {
    IRType::Optional(Box::new(s()))
}
fn los() -> IRType {
    IRType::List(Box::new(os()))
}
fn lli() -> IRType {
    IRType::List(Box::new(li()))
}
pub(super) fn ty(short: &str) -> IRType {
    IRType::Class(class_name(short))
}
fn me(short: &str) -> IRParam {
    param("self", &ty(short))
}
fn fcall(name: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&fname(name), args)
}
fn add(a: IRExpr, b: IRExpr) -> IRExpr {
    bin(a, IROp::Add, b)
}
fn sub(a: IRExpr, b: IRExpr) -> IRExpr {
    bin(a, IROp::Sub, b)
}
fn mul(a: IRExpr, b: IRExpr) -> IRExpr {
    bin(a, IROp::Mul, b)
}
fn eq(a: IRExpr, b: IRExpr) -> IRExpr {
    cmp(a, IRCompareOp::Eq, b)
}
fn ne(a: IRExpr, b: IRExpr) -> IRExpr {
    cmp(a, IRCompareOp::NotEq, b)
}
fn lt(a: IRExpr, b: IRExpr) -> IRExpr {
    cmp(a, IRCompareOp::Lt, b)
}
fn le(a: IRExpr, b: IRExpr) -> IRExpr {
    cmp(a, IRCompareOp::LtE, b)
}
fn ge(a: IRExpr, b: IRExpr) -> IRExpr {
    cmp(a, IRCompareOp::GtE, b)
}
fn inc(var: &str, by: IRExpr) -> IRStatement {
    assign(var, add(v(var), by))
}
fn append(list: IRExpr, e: IRExpr) -> IRStatement {
    expr_stmt(mcall(list, "append", vec![e]))
}
fn concat(parts: Vec<IRExpr>) -> IRExpr {
    parts.into_iter().reduce(add).unwrap_or_else(|| text(""))
}
fn class(short: &str, base: Option<&str>, methods: Vec<IRFunction>) -> IRClass {
    IRClass {
        name: class_name(short),
        bases: base.map(class_name).into_iter().collect(),
        methods,
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}
fn function(
    name: &str,
    params: Vec<IRParam>,
    ret: IRType,
    statements: Vec<IRStatement>,
) -> IRFunction {
    IRFunction {
        name: fname(name),
        params,
        body: crate::ir::IRBody { statements },
        return_type: ret,
        decorators: Vec::new(),
    }
}
fn variant(base: &str, ty: &IRType) -> String {
    crate::ir::collections::spec::variant(base, ty).expect("a spelled type")
}
fn raise_error(message: IRExpr) -> IRStatement {
    raise_expr("PatternError", message)
}
/// `sys.maxsize`, as an `int` here holds it.
const MAXSIZE: i32 = i32::MAX;

pub(super) fn all(cache: Vec<IRVariable>, with_functions: bool) -> Vec<IRClass> {
    vec![
        subject(),
        template(),
        pattern(with_functions),
        pattern0(),
        pattern1(),
        pattern_n(),
        matched(),
        IRClass {
            name: class_name("_Cache"),
            bases: Vec::new(),
            methods: Vec::new(),
            class_vars: cache,
            metaclass: None,
            position: 0,
            deferred_vars: Vec::new(),
        },
    ]
}

/// A subject string as the matcher reads it: its characters and their codes,
/// and their lowercases (and those lowercases' uppercases) when the pattern
/// ignores case.
fn subject() -> IRClass {
    class(
        "_Subject",
        None,
        vec![method(
            "__init__",
            vec![
                me("_Subject"),
                param("text", &s()),
                param("lower", &b()),
                param("upper", &b()),
            ],
            IRType::None,
            vec![
                set_my_typed("text", v("text"), &s()),
                set_my_typed("chars", IRExpr::ListLiteral(Vec::new()), &ls()),
                for_("c", v("text"), vec![append(my("chars"), v("c"))]),
                set_my_typed("n", len(my("chars")), &i()),
                set_my_typed("codes", IRExpr::ListLiteral(Vec::new()), &li()),
                set_my_typed("low", IRExpr::ListLiteral(Vec::new()), &li()),
                set_my_typed("up", IRExpr::ListLiteral(Vec::new()), &li()),
                for_(
                    "c",
                    my("chars"),
                    vec![append(my("codes"), call("ord", vec![v("c")]))],
                ),
                if_(
                    v("lower"),
                    vec![for_(
                        "c",
                        my("chars"),
                        vec![
                            assign_typed(
                                "lc",
                                idx(mcall(v("c"), "lower", Vec::new()), int(0)),
                                &s(),
                            ),
                            append(my("low"), call("ord", vec![v("lc")])),
                            if_(
                                v("upper"),
                                vec![append(
                                    my("up"),
                                    call(
                                        "ord",
                                        vec![idx(mcall(v("lc"), "upper", Vec::new()), int(0))],
                                    ),
                                )],
                            ),
                        ],
                    )],
                ),
            ],
        )],
    )
}

/// A replacement template: text, or a group (`-1` for text) in turn.
fn template() -> IRClass {
    class(
        "_Template",
        None,
        vec![method(
            "__init__",
            vec![me("_Template")],
            IRType::None,
            vec![
                set_my_typed("texts", IRExpr::ListLiteral(Vec::new()), &ls()),
                set_my_typed("groups", IRExpr::ListLiteral(Vec::new()), &li()),
            ],
        )],
    )
}

fn pattern_params() -> Vec<IRParam> {
    vec![
        me("Pattern"),
        param("pattern", &s()),
        param("flags", &i()),
        param("groups", &i()),
        param("prog", &li()),
        param("nregs", &i()),
        param("first", &i()),
        param("lower", &b()),
        param("upper", &b()),
        param("names", &ls()),
        param("indices", &li()),
    ]
}

/// The clamped `pos` and `endpos` of a search, as `state_init` clamps them.
fn clamp() -> Vec<IRStatement> {
    vec![
        assign("n", attr(v("subj"), "n")),
        if_(lt(v("pos"), int(0)), vec![assign("pos", int(0))]),
        if_(
            cmp(v("pos"), IRCompareOp::Gt, v("n")),
            vec![assign("pos", v("n"))],
        ),
        if_(lt(v("endpos"), int(0)), vec![assign("endpos", int(0))]),
        if_(
            cmp(v("endpos"), IRCompareOp::Gt, v("n")),
            vec![assign("endpos", v("n"))],
        ),
    ]
}

fn new_subject(string: IRExpr) -> IRExpr {
    call(
        &class_name("_Subject"),
        vec![string, my("_lower"), my("_upper")],
    )
}

fn search_params(short: &str) -> Vec<IRParam> {
    vec![
        me(short),
        param("string", &s()),
        param_default("pos", &i(), int(0)),
        param_default("endpos", &i(), int(MAXSIZE)),
    ]
}

fn run(start: IRExpr, end: IRExpr, all: IRExpr, must: IRExpr) -> IRExpr {
    fcall(
        "run",
        vec![
            my("_prog"),
            attr(v("subj"), "codes"),
            attr(v("subj"), "chars"),
            attr(v("subj"), "low"),
            attr(v("subj"), "up"),
            start,
            end,
            my("_ngroups"),
            my("_nregs"),
            all,
            must,
        ],
    )
}

/// `with_functions`: `sub` and `subn` with a function for the replacement,
/// made only for a program that calls them, as calling a function value
/// needs the program's closures.
fn pattern(with_functions: bool) -> IRClass {
    let me_p = || me("Pattern");
    let opt_match = IRType::Optional(Box::new(ty("Match")));
    let subj_ty = ty("_Subject");
    let made = |r: &str| {
        call(
            &class_name("Match"),
            vec![
                v("self"),
                attr(v("subj"), "text"),
                v("pos"),
                v("endpos"),
                v(r),
            ],
        )
    };
    let mut m = vec![method(
        "__init__",
        pattern_params(),
        IRType::None,
        vec![
            set_my_typed("pattern", v("pattern"), &s()),
            set_my_typed("flags", v("flags"), &i()),
            set_my_typed("groups", sub(v("groups"), int(1)), &i()),
            set_my_typed("_ngroups", v("groups"), &i()),
            set_my_typed("_prog", v("prog"), &li()),
            set_my_typed("_nregs", v("nregs"), &i()),
            set_my_typed("_first", v("first"), &i()),
            set_my_typed("_lower", v("lower"), &b()),
            set_my_typed("_upper", v("upper"), &b()),
            set_my_typed("_names", v("names"), &ls()),
            set_my_typed("_indices", v("indices"), &li()),
        ],
    )];
    // The first match at or after `pos`, before `end`.
    m.push(method(
        "_find",
        vec![
            me_p(),
            param("subj", &subj_ty),
            param("pos", &i()),
            param("end", &i()),
            param("must", &b()),
        ],
        li(),
        vec![
            assign("p", v("pos")),
            while_(
                le(v("p"), v("end")),
                vec![
                    if_(
                        or(
                            lt(my("_first"), int(0)),
                            and(
                                lt(v("p"), v("end")),
                                eq(idx(attr(v("subj"), "codes"), v("p")), my("_first")),
                            ),
                        ),
                        vec![
                            assign_typed(
                                "r",
                                run(
                                    v("p"),
                                    v("end"),
                                    boolean(false),
                                    and(v("must"), eq(v("p"), v("pos"))),
                                ),
                                &li(),
                            ),
                            if_(
                                cmp(len(v("r")), IRCompareOp::Gt, int(0)),
                                vec![ret_val(v("r"))],
                            ),
                        ],
                    ),
                    inc("p", int(1)),
                ],
            ),
            ret_val(IRExpr::ListLiteral(Vec::new())),
        ],
    ));
    // The successive matches, at most `limit` of them (0 for all), each
    // search after an empty match not matching empty where it starts.
    m.push(method(
        "_scan",
        vec![
            me_p(),
            param("subj", &subj_ty),
            param("pos", &i()),
            param("endpos", &i()),
            param("limit", &i()),
        ],
        lli(),
        vec![
            assign_typed("out", IRExpr::ListLiteral(Vec::new()), &lli()),
            assign("p", v("pos")),
            assign("must", boolean(false)),
            while_(
                and(
                    le(v("p"), v("endpos")),
                    or(eq(v("limit"), int(0)), lt(len(v("out")), v("limit"))),
                ),
                vec![
                    assign_typed(
                        "r",
                        mcall(
                            v("self"),
                            "_find",
                            vec![v("subj"), v("p"), v("endpos"), v("must")],
                        ),
                        &li(),
                    ),
                    if_(eq(len(v("r")), int(0)), vec![IRStatement::Break]),
                    append(v("out"), v("r")),
                    assign("must", eq(idx(v("r"), int(1)), idx(v("r"), int(0)))),
                    assign("p", idx(v("r"), int(1))),
                ],
            ),
            ret_val(v("out")),
        ],
    ));
    for (name, anchored, all) in [
        ("search", false, false),
        ("match", true, false),
        ("fullmatch", true, true),
    ] {
        let mut body = vec![assign_typed("subj", new_subject(v("string")), &subj_ty)];
        body.extend(clamp());
        let found = if anchored {
            run(v("pos"), v("endpos"), boolean(all), boolean(false))
        } else {
            mcall(
                v("self"),
                "_find",
                vec![v("subj"), v("pos"), v("endpos"), boolean(false)],
            )
        };
        body.push(if_(lt(v("endpos"), v("pos")), vec![ret_val(none())]));
        body.push(assign_typed("r", found, &li()));
        body.push(if_(eq(len(v("r")), int(0)), vec![ret_val(none())]));
        body.push(ret_val(made("r")));
        m.push(method(
            name,
            search_params("Pattern"),
            opt_match.clone(),
            body,
        ));
    }
    let mut body = vec![assign_typed("subj", new_subject(v("string")), &subj_ty)];
    body.extend(clamp());
    body.extend([
        assign_typed(
            "out",
            IRExpr::ListLiteral(Vec::new()),
            &IRType::List(Box::new(ty("Match"))),
        ),
        if_(lt(v("endpos"), v("pos")), vec![ret_val(v("out"))]),
        for_(
            "r",
            mcall(
                v("self"),
                "_scan",
                vec![v("subj"), v("pos"), v("endpos"), int(0)],
            ),
            vec![append(v("out"), made("r"))],
        ),
        ret_val(v("out")),
    ]);
    m.push(method(
        "finditer",
        search_params("Pattern"),
        IRType::List(Box::new(ty("Match"))),
        body,
    ));
    // sub and subn, by a template or a function.
    let by_template = |r: IRExpr| fcall("expand", vec![v("t"), v("string"), r]);
    let by_function = |r: IRExpr| IRExpr::FunctionCall {
        function_name: "repl".to_string(),
        arguments: vec![call(
            &class_name("Match"),
            vec![v("self"), v("string"), int(0), len(v("string")), r],
        )],
    };
    for (name, fun, counted) in [
        ("sub", false, false),
        ("subn", false, true),
        (super::SUB_FN, true, false),
        (super::SUBN_FN, true, true),
    ] {
        if fun && !with_functions {
            continue;
        }
        let repl_ty = if fun {
            IRType::Callable {
                params: vec![ty("Match")],
                return_type: Box::new(s()),
            }
        } else {
            s()
        };
        let mut body = Vec::new();
        if !fun {
            body.push(assign_typed(
                "t",
                fcall(
                    "template",
                    vec![v("repl"), my("_ngroups"), my("_names"), my("_indices")],
                ),
                &ty("_Template"),
            ));
        }
        body.extend([
            assign_typed("subj", new_subject(v("string")), &subj_ty),
            assign_typed("pieces", IRExpr::ListLiteral(Vec::new()), &ls()),
            assign("last", int(0)),
            assign("n", int(0)),
            for_(
                "r",
                mcall(
                    v("self"),
                    "_scan",
                    vec![v("subj"), int(0), attr(v("subj"), "n"), v("count")],
                ),
                vec![
                    if_(
                        lt(v("last"), idx(v("r"), int(0))),
                        vec![append(
                            v("pieces"),
                            slice(v("string"), Some(v("last")), Some(idx(v("r"), int(0)))),
                        )],
                    ),
                    append(
                        v("pieces"),
                        if fun {
                            by_function(v("r"))
                        } else {
                            by_template(v("r"))
                        },
                    ),
                    assign("last", idx(v("r"), int(1))),
                    inc("n", int(1)),
                ],
            ),
            if_(
                lt(v("last"), attr(v("subj"), "n")),
                vec![append(
                    v("pieces"),
                    slice(v("string"), Some(v("last")), None),
                )],
            ),
            assign_typed("result", mcall(text(""), "join", vec![v("pieces")]), &s()),
        ]);
        if counted {
            body.push(ret_val(IRExpr::TupleLiteral(vec![v("result"), v("n")])));
        } else {
            body.push(ret_val(v("result")));
        }
        m.push(method(
            name,
            vec![
                me_p(),
                param("repl", &repl_ty),
                param("string", &s()),
                param_default("count", &i(), int(0)),
            ],
            if counted {
                IRType::Tuple(vec![s(), i()])
            } else {
                s()
            },
            body,
        ));
    }
    m.push(method(
        "__repr__",
        vec![me_p()],
        s(),
        vec![
            assign_typed("flags", fcall("flag_names", vec![my("flags")]), &s()),
            assign_typed(
                "r",
                slice(call("repr", vec![my("pattern")]), None, Some(int(200))),
                &s(),
            ),
            if_(
                eq(len(v("flags")), int(0)),
                vec![ret_val(concat(vec![
                    text("re.compile("),
                    v("r"),
                    text(")"),
                ]))],
            ),
            ret_val(concat(vec![
                text("re.compile("),
                v("r"),
                text(", "),
                v("flags"),
                text(")"),
            ])),
        ],
    ));
    class("Pattern", None, m)
}

/// A group's text in match `r` of `string`, or None.
fn group_of(r: IRExpr, g: IRExpr) -> IRExpr {
    fcall("group", vec![v("string"), r, g])
}

/// `findall` and `split` for a pattern with `groups` groups (2 for several).
fn finders(short: &str, groups: usize) -> Vec<IRFunction> {
    let subj_ty = ty("_Subject");
    let item_ty = match groups {
        0 | 1 => s(),
        _ => IRType::VarTuple(Box::new(s())),
    };
    let item = |r: IRExpr| match groups {
        0 => slice(
            v("string"),
            Some(idx(r.clone(), int(0))),
            Some(idx(r, int(1))),
        ),
        1 => fcall("group_or", vec![v("string"), r, int(1), text("")]),
        _ => call(
            crate::ir::AS_TUPLE_FN,
            vec![fcall(
                "groups_list",
                vec![v("string"), r, my("_ngroups"), text("")],
            )],
        ),
    };
    let mut findall = vec![assign_typed("subj", new_subject(v("string")), &subj_ty)];
    findall.extend(clamp());
    findall.extend([
        assign_typed(
            "out",
            IRExpr::ListLiteral(Vec::new()),
            &IRType::List(Box::new(item_ty.clone())),
        ),
        if_(lt(v("endpos"), v("pos")), vec![ret_val(v("out"))]),
        for_(
            "r",
            mcall(
                v("self"),
                "_scan",
                vec![v("subj"), v("pos"), v("endpos"), int(0)],
            ),
            vec![append(v("out"), item(v("r")))],
        ),
        ret_val(v("out")),
    ]);
    let piece_ty = if groups == 0 { s() } else { os() };
    let mut split = vec![
        assign_typed("subj", new_subject(v("string")), &subj_ty),
        assign_typed(
            "out",
            IRExpr::ListLiteral(Vec::new()),
            &IRType::List(Box::new(piece_ty.clone())),
        ),
        assign("last", int(0)),
        for_(
            "r",
            mcall(
                v("self"),
                "_scan",
                vec![v("subj"), int(0), attr(v("subj"), "n"), v("maxsplit")],
            ),
            {
                let mut each = vec![append(
                    v("out"),
                    slice(v("string"), Some(v("last")), Some(idx(v("r"), int(0)))),
                )];
                if groups > 0 {
                    each.push(for_(
                        "g",
                        range(sub(my("_ngroups"), int(1))),
                        vec![append(v("out"), group_of(v("r"), add(v("g"), int(1))))],
                    ));
                }
                each.push(assign("last", idx(v("r"), int(1))));
                each
            },
        ),
        append(v("out"), slice(v("string"), Some(v("last")), None)),
        ret_val(v("out")),
    ];
    // A negative maxsplit splits nothing.
    split.insert(
        0,
        if_(
            lt(v("maxsplit"), int(0)),
            vec![ret_val(IRExpr::ListLiteral(vec![v("string")]))],
        ),
    );
    vec![
        method(
            "findall",
            search_params(short),
            IRType::List(Box::new(item_ty)),
            findall,
        ),
        method(
            "split",
            vec![
                me(short),
                param("string", &s()),
                param_default("maxsplit", &i(), int(0)),
            ],
            IRType::List(Box::new(piece_ty)),
            split,
        ),
    ]
}

fn subclass_init(short: &str) -> IRFunction {
    let mut params = pattern_params();
    params[0] = me(short);
    let args = params.iter().skip(1).map(|p| v(&p.name)).collect();
    method(
        "__init__",
        params,
        IRType::None,
        vec![expr_stmt(mcall(
            call("super", Vec::new()),
            "__init__",
            args,
        ))],
    )
}

fn pattern0() -> IRClass {
    let mut m = vec![subclass_init(PATTERN0)];
    m.extend(finders(PATTERN0, 0));
    class(PATTERN0, Some("Pattern"), m)
}
fn pattern1() -> IRClass {
    let mut m = vec![subclass_init(PATTERN1)];
    m.extend(finders(PATTERN1, 1));
    class(PATTERN1, Some("Pattern"), m)
}
fn pattern_n() -> IRClass {
    let mut m = vec![subclass_init(PATTERN_N)];
    m.extend(finders(PATTERN_N, 2));
    class(PATTERN_N, Some("Pattern"), m)
}

fn matched() -> IRClass {
    let me_m = || me("Match");
    let marks = || my("_marks");
    // The group a name or number is, or IndexError.
    let index_of_name = vec![
        assign("k", int(0)),
        while_(
            lt(v("k"), len(attr(my("re"), "_names"))),
            vec![
                if_(
                    eq(idx(attr(my("re"), "_names"), v("k")), v("g")),
                    vec![ret_val(idx(attr(my("re"), "_indices"), v("k")))],
                ),
                inc("k", int(1)),
            ],
        ),
        raise_expr("IndexError", text("no such group")),
    ];
    let check_index = || {
        if_(
            or(lt(v("g"), int(0)), ge(v("g"), attr(my("re"), "_ngroups"))),
            vec![raise_expr("IndexError", text("no such group"))],
        )
    };
    let group_text = || fcall("group", vec![my("string"), marks(), v("g")]);
    let mut m = vec![
        method(
            "__init__",
            vec![
                me_m(),
                param("re", &ty("Pattern")),
                param("string", &s()),
                param("pos", &i()),
                param("endpos", &i()),
                param("marks", &li()),
            ],
            IRType::None,
            vec![
                set_my_typed("re", v("re"), &ty("Pattern")),
                set_my_typed("string", v("string"), &s()),
                set_my_typed("pos", v("pos"), &i()),
                set_my_typed("endpos", v("endpos"), &i()),
                set_my_typed("_marks", v("marks"), &li()),
                assign("last", idx(v("marks"), sub(len(v("marks")), int(1)))),
                set_my_typed("lastindex", none(), &IRType::Optional(Box::new(i()))),
                set_my_typed("lastgroup", none(), &os()),
                if_(
                    ge(v("last"), int(1)),
                    vec![
                        set_attr(v("self"), "lastindex", v("last")),
                        assign("k", int(0)),
                        while_(
                            lt(v("k"), len(attr(v("re"), "_names"))),
                            vec![
                                if_(
                                    eq(idx(attr(v("re"), "_indices"), v("k")), v("last")),
                                    vec![set_attr(
                                        v("self"),
                                        "lastgroup",
                                        idx(attr(v("re"), "_names"), v("k")),
                                    )],
                                ),
                                inc("k", int(1)),
                            ],
                        ),
                    ],
                ),
            ],
        ),
        method("_index", vec![me_m(), param("g", &s())], i(), index_of_name),
        method(
            "group",
            vec![me_m()],
            s(),
            vec![ret_val(slice(
                my("string"),
                Some(idx(marks(), int(0))),
                Some(idx(marks(), int(1))),
            ))],
        ),
    ];
    for name in ["group", "__getitem__"] {
        m.push(method(
            &variant(name, &i()),
            vec![me_m(), param("g", &i())],
            os(),
            vec![check_index(), ret_val(group_text())],
        ));
        m.push(method(
            &variant(name, &s()),
            vec![me_m(), param("g", &s())],
            os(),
            vec![ret_val(fcall(
                "group",
                vec![
                    my("string"),
                    marks(),
                    mcall(v("self"), "_index", vec![v("g")]),
                ],
            ))],
        ));
    }
    m.push(method(
        "groups",
        vec![me_m(), param_default("default", &os(), none())],
        IRType::VarTuple(Box::new(os())),
        vec![
            assign_typed("out", IRExpr::ListLiteral(Vec::new()), &los()),
            for_(
                "g",
                range(sub(attr(my("re"), "_ngroups"), int(1))),
                vec![
                    assign_typed(
                        "t",
                        fcall("group", vec![my("string"), marks(), add(v("g"), int(1))]),
                        &os(),
                    ),
                    if_(
                        cmp(v("t"), IRCompareOp::Is, none()),
                        vec![assign("t", v("default"))],
                    ),
                    append(v("out"), v("t")),
                ],
            ),
            ret_val(call(crate::ir::AS_TUPLE_FN, vec![v("out")])),
        ],
    ));
    m.push(method(
        "groupdict",
        vec![me_m(), param_default("default", &os(), none())],
        IRType::Dict(Box::new(s()), Box::new(os())),
        vec![
            assign_typed(
                "out",
                IRExpr::DictLiteral(Vec::new()),
                &IRType::Dict(Box::new(s()), Box::new(os())),
            ),
            assign("k", int(0)),
            while_(
                lt(v("k"), len(attr(my("re"), "_names"))),
                vec![
                    assign_typed(
                        "t",
                        fcall(
                            "group",
                            vec![
                                my("string"),
                                marks(),
                                idx(attr(my("re"), "_indices"), v("k")),
                            ],
                        ),
                        &os(),
                    ),
                    if_(
                        cmp(v("t"), IRCompareOp::Is, none()),
                        vec![assign("t", v("default"))],
                    ),
                    set_item(v("out"), idx(attr(my("re"), "_names"), v("k")), v("t")),
                    inc("k", int(1)),
                ],
            ),
            ret_val(v("out")),
        ],
    ));
    // start, end, span: of group 0, a number, or a name.
    let at = |k: i32| idx(marks(), add(mul(int(2), v("g")), int(k)));
    for (name, value, ret) in [
        ("start", at(0), i()),
        ("end", at(1), i()),
        (
            "span",
            IRExpr::TupleLiteral(vec![at(0), at(1)]),
            IRType::Tuple(vec![i(), i()]),
        ),
    ] {
        m.push(method(
            name,
            vec![me_m()],
            ret.clone(),
            vec![assign("g", int(0)), ret_val(value.clone())],
        ));
        m.push(method(
            &variant(name, &i()),
            vec![me_m(), param("g", &i())],
            ret.clone(),
            vec![check_index(), ret_val(value.clone())],
        ));
        m.push(method(
            &variant(name, &s()),
            vec![me_m(), param("name", &s())],
            ret,
            vec![
                assign("g", mcall(v("self"), "_index", vec![v("name")])),
                ret_val(value),
            ],
        ));
    }
    m.push(method(
        "expand",
        vec![me_m(), param("template", &s())],
        s(),
        vec![
            assign_typed(
                "t",
                fcall(
                    "template",
                    vec![
                        v("template"),
                        attr(my("re"), "_ngroups"),
                        attr(my("re"), "_names"),
                        attr(my("re"), "_indices"),
                    ],
                ),
                &ty("_Template"),
            ),
            ret_val(fcall("expand", vec![v("t"), my("string"), marks()])),
        ],
    ));
    m.push(method(
        "__repr__",
        vec![me_m()],
        s(),
        vec![ret_val(concat(vec![
            text("<re.Match object; span=("),
            call("str", vec![idx(marks(), int(0))]),
            text(", "),
            call("str", vec![idx(marks(), int(1))]),
            text("), match="),
            slice(
                call("repr", vec![mcall(v("self"), "group", Vec::new())]),
                None,
                Some(int(50)),
            ),
            text(">"),
        ]))],
    ));
    class("Match", None, m)
}

/// The module's functions behind the classes.
pub(super) fn functions() -> Vec<IRFunction> {
    let r = || param("r", &li());
    vec![
        // Group `g`'s text in a match, or None when it did not take part.
        function(
            "group",
            vec![param("string", &s()), r(), param("g", &i())],
            os(),
            vec![
                assign("p", idx(v("r"), mul(int(2), v("g")))),
                assign("e", idx(v("r"), add(mul(int(2), v("g")), int(1)))),
                if_(
                    or(lt(v("p"), int(0)), lt(v("e"), v("p"))),
                    vec![ret_val(none())],
                ),
                ret_val(slice(v("string"), Some(v("p")), Some(v("e")))),
            ],
        ),
        function(
            "group_or",
            vec![
                param("string", &s()),
                r(),
                param("g", &i()),
                param("default", &s()),
            ],
            s(),
            vec![
                assign_typed(
                    "t",
                    fcall("group", vec![v("string"), v("r"), v("g")]),
                    &os(),
                ),
                if_(
                    cmp(v("t"), IRCompareOp::Is, none()),
                    vec![ret_val(v("default"))],
                ),
                assign_typed("got", v("t"), &s()),
                ret_val(v("got")),
            ],
        ),
        function(
            "groups_list",
            vec![
                param("string", &s()),
                r(),
                param("ngroups", &i()),
                param("default", &s()),
            ],
            ls(),
            vec![
                assign_typed("out", IRExpr::ListLiteral(Vec::new()), &ls()),
                for_(
                    "g",
                    range(sub(v("ngroups"), int(1))),
                    vec![append(
                        v("out"),
                        fcall(
                            "group_or",
                            vec![v("string"), v("r"), add(v("g"), int(1)), v("default")],
                        ),
                    )],
                ),
                ret_val(v("out")),
            ],
        ),
        // A match's replacement by a template: its text, and each group's
        // (nothing for a group that did not take part).
        function(
            "expand",
            vec![param("t", &ty("_Template")), param("string", &s()), r()],
            s(),
            vec![
                assign_typed("pieces", IRExpr::ListLiteral(Vec::new()), &ls()),
                assign("k", int(0)),
                while_(
                    lt(v("k"), len(attr(v("t"), "texts"))),
                    vec![
                        assign("g", idx(attr(v("t"), "groups"), v("k"))),
                        if_else(
                            lt(v("g"), int(0)),
                            vec![append(v("pieces"), idx(attr(v("t"), "texts"), v("k")))],
                            vec![
                                assign_typed(
                                    "got",
                                    fcall("group", vec![v("string"), v("r"), v("g")]),
                                    &os(),
                                ),
                                if_(
                                    cmp(v("got"), IRCompareOp::IsNot, none()),
                                    vec![
                                        assign_typed("text", v("got"), &s()),
                                        append(v("pieces"), v("text")),
                                    ],
                                ),
                            ],
                        ),
                        inc("k", int(1)),
                    ],
                ),
                ret_val(mcall(text(""), "join", vec![v("pieces")])),
            ],
        ),
        template_parser(),
        escape(),
        flag_names(),
        // The value of a string of octal digits.
        function(
            "octal",
            vec![param("digits", &s())],
            i(),
            vec![
                assign("value", int(0)),
                for_(
                    "c",
                    v("digits"),
                    vec![assign(
                        "value",
                        add(
                            mul(v("value"), int(8)),
                            sub(call("ord", vec![v("c")]), int(48)),
                        ),
                    )],
                ),
                ret_val(v("value")),
            ],
        ),
        // `s.isdecimal() and s.isascii()`.
        function(
            "ascii_digits",
            vec![param("text", &s())],
            b(),
            vec![
                if_(eq(len(v("text")), int(0)), vec![ret_val(boolean(false))]),
                for_(
                    "c",
                    v("text"),
                    vec![if_(
                        or(
                            lt(v("c"), text("0")),
                            cmp(v("c"), IRCompareOp::Gt, text("9")),
                        ),
                        vec![ret_val(boolean(false))],
                    )],
                ),
                ret_val(boolean(true)),
            ],
        ),
        // `s.isidentifier()`, by letters, digits, and the underscore.
        function(
            "identifier",
            vec![param("text", &s())],
            b(),
            vec![
                if_(eq(len(v("text")), int(0)), vec![ret_val(boolean(false))]),
                assign_typed("first", idx(v("text"), int(0)), &s()),
                if_(
                    and(
                        not(mcall(v("first"), "isalpha", Vec::new())),
                        ne(v("first"), text("_")),
                    ),
                    vec![ret_val(boolean(false))],
                ),
                for_(
                    "c",
                    v("text"),
                    vec![if_(
                        and(
                            not(mcall(v("c"), "isalnum", Vec::new())),
                            ne(v("c"), text("_")),
                        ),
                        vec![ret_val(boolean(false))],
                    )],
                ),
                ret_val(boolean(true)),
            ],
        ),
    ]
}

/// `re.escape(s)`: CPython 3.13's special characters, each after a
/// backslash.
fn escape() -> IRFunction {
    function(
        "escape",
        vec![param("text", &s())],
        s(),
        vec![
            assign_typed("pieces", IRExpr::ListLiteral(Vec::new()), &ls()),
            for_(
                "c",
                v("text"),
                vec![
                    if_(
                        cmp(
                            v("c"),
                            IRCompareOp::In,
                            text("()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c"),
                        ),
                        vec![append(v("pieces"), text("\\"))],
                    ),
                    append(v("pieces"), v("c")),
                ],
            ),
            ret_val(mcall(text(""), "join", vec![v("pieces")])),
        ],
    )
}

/// The flags of `Pattern.__repr__`: `re.IGNORECASE|re.MULTILINE`, without
/// UNICODE (a str pattern's default), the rest in hex.
fn flag_names() -> IRFunction {
    let mut body = vec![
        assign_typed("names", IRExpr::ListLiteral(Vec::new()), &ls()),
        assign("rest", v("flags")),
        // UNICODE alone is a str pattern's default.
        if_(
            eq(bin(v("rest"), IROp::BitAnd, int(4 | 32 | 256)), int(32)),
            vec![assign("rest", bin(v("rest"), IROp::BitAnd, int(!32)))],
        ),
    ];
    for (bit, name) in [
        (2, "re.IGNORECASE"),
        (4, "re.LOCALE"),
        (8, "re.MULTILINE"),
        (16, "re.DOTALL"),
        (32, "re.UNICODE"),
        (64, "re.VERBOSE"),
        (128, "re.DEBUG"),
        (256, "re.ASCII"),
    ] {
        body.push(if_(
            ne(bin(v("rest"), IROp::BitAnd, int(bit)), int(0)),
            vec![
                append(v("names"), text(name)),
                assign("rest", bin(v("rest"), IROp::BitAnd, int(!bit))),
            ],
        ));
    }
    body.push(ret_val(mcall(text("|"), "join", vec![v("names")])));
    function("flag_names", vec![param("flags", &i())], s(), body)
}

/// CPython's `parse_template` at run time: the template's text and group
/// references, `re.error` (or IndexError, for an unknown name) for a bad
/// one, at the position CPython gives.
fn template_parser() -> IRFunction {
    let ch = |k: IRExpr| idx(v("repl"), k);
    let digit = |e: IRExpr| and(ge(e.clone(), text("0")), le(e, text("9")));
    let octal = |e: IRExpr| and(ge(e.clone(), text("0")), le(e, text("7")));
    let at_pos = |msg: IRExpr, pos: IRExpr| {
        raise_error(concat(vec![
            msg,
            text(" at position "),
            call("str", vec![pos]),
        ]))
    };
    let flush = || {
        if_(
            cmp(len(v("lit")), IRCompareOp::Gt, int(0)),
            vec![
                append(attr(v("t"), "texts"), v("lit")),
                append(attr(v("t"), "groups"), int(-1)),
                assign("lit", text("")),
            ],
        )
    };
    let add_group = |g: IRExpr, pos: IRExpr| {
        vec![
            if_(
                ge(g.clone(), v("ngroups")),
                vec![at_pos(
                    concat(vec![
                        text("invalid group reference "),
                        call("str", vec![g.clone()]),
                    ]),
                    pos,
                )],
            ),
            flush(),
            append(attr(v("t"), "texts"), text("")),
            append(attr(v("t"), "groups"), g),
        ]
    };
    let escapes: Vec<IRStatement> = [
        ("n", "\n"),
        ("t", "\t"),
        ("r", "\r"),
        ("f", "\x0c"),
        ("v", "\x0b"),
        ("a", "\x07"),
        ("b", "\x08"),
        ("\\", "\\"),
    ]
    .into_iter()
    .map(|(c, value)| {
        if_(
            eq(v("c"), text(c)),
            vec![inc("lit", text(value)), IRStatement::Continue],
        )
    })
    .collect();
    let mut escape_body = vec![
        assign_typed("c", ch(v("i")), &s()),
        inc("i", int(1)),
        // \g<name> or \g<number>
        if_(eq(v("c"), text("g")), {
            let mut g = vec![
                if_(
                    or(ge(v("i"), v("n")), ne(ch(v("i")), text("<"))),
                    vec![at_pos(text("missing <"), v("i"))],
                ),
                inc("i", int(1)),
                assign("close", v("i")),
                while_(
                    and(lt(v("close"), v("n")), ne(ch(v("close")), text(">"))),
                    vec![inc("close", int(1))],
                ),
                if_(
                    eq(v("close"), v("i")),
                    vec![at_pos(
                        text("missing group name"),
                        IRExpr::Conditional {
                            condition: Box::new(lt(v("close"), v("n"))),
                            body: Box::new(v("i")),
                            orelse: Box::new(v("i")),
                        },
                    )],
                ),
                if_(
                    ge(v("close"), v("n")),
                    vec![at_pos(text("missing >, unterminated name"), v("i"))],
                ),
                assign_typed(
                    "name",
                    slice(v("repl"), Some(v("i")), Some(v("close"))),
                    &s(),
                ),
                assign("i", add(v("close"), int(1))),
                assign("g", int(-1)),
            ];
            g.push(if_else(
                fcall("ascii_digits", vec![v("name")]),
                vec![assign("g", call("int", vec![v("name")]))],
                vec![
                    if_(
                        not(fcall("identifier", vec![v("name")])),
                        vec![at_pos(
                            concat(vec![
                                text("bad character in group name "),
                                call("repr", vec![v("name")]),
                            ]),
                            sub(sub(v("i"), len(v("name"))), int(1)),
                        )],
                    ),
                    assign("k", int(0)),
                    while_(
                        lt(v("k"), len(v("names"))),
                        vec![
                            if_(
                                eq(idx(v("names"), v("k")), v("name")),
                                vec![assign("g", idx(v("indices"), v("k")))],
                            ),
                            inc("k", int(1)),
                        ],
                    ),
                    if_(
                        lt(v("g"), int(0)),
                        vec![raise_expr(
                            "IndexError",
                            concat(vec![
                                text("unknown group name "),
                                call("repr", vec![v("name")]),
                            ]),
                        )],
                    ),
                ],
            ));
            g.extend(add_group(v("g"), sub(sub(v("i"), len(v("name"))), int(1))));
            g.push(IRStatement::Continue);
            g
        }),
        // \0, \0o, \0oo: an octal escape.
        if_(
            eq(v("c"), text("0")),
            vec![
                assign_typed("digits", text("0"), &s()),
                if_(
                    and(lt(v("i"), v("n")), octal(ch(v("i")))),
                    vec![
                        inc("digits", ch(v("i"))),
                        inc("i", int(1)),
                        if_(
                            and(lt(v("i"), v("n")), octal(ch(v("i")))),
                            vec![inc("digits", ch(v("i"))), inc("i", int(1))],
                        ),
                    ],
                ),
                inc(
                    "lit",
                    call(
                        "chr",
                        vec![bin(
                            fcall("octal", vec![v("digits")]),
                            IROp::BitAnd,
                            int(255),
                        )],
                    ),
                ),
                IRStatement::Continue,
            ],
        ),
        // \1 ... \99 is a group; three octal digits are a character.
        if_(digit(v("c")), {
            let mut d = vec![
                assign_typed("digits", v("c"), &s()),
                assign("start", sub(v("i"), int(1))),
                assign("isoctal", boolean(false)),
                if_(
                    and(lt(v("i"), v("n")), digit(ch(v("i")))),
                    vec![
                        inc("digits", ch(v("i"))),
                        inc("i", int(1)),
                        if_(
                            and(
                                and(octal(v("c")), octal(idx(v("digits"), int(1)))),
                                and(lt(v("i"), v("n")), octal(ch(v("i")))),
                            ),
                            vec![
                                inc("digits", ch(v("i"))),
                                inc("i", int(1)),
                                assign("isoctal", boolean(true)),
                                assign("value", fcall("octal", vec![v("digits")])),
                                if_(
                                    cmp(v("value"), IRCompareOp::Gt, int(0o377)),
                                    vec![at_pos(
                                        concat(vec![
                                            text("octal escape value \\"),
                                            v("digits"),
                                            text(" outside of range 0-0o377"),
                                        ]),
                                        sub(v("i"), int(4)),
                                    )],
                                ),
                                inc("lit", call("chr", vec![v("value")])),
                            ],
                        ),
                    ],
                ),
            ];
            let mut group = add_group(call("int", vec![v("digits")]), v("start"));
            group.push(IRStatement::Continue);
            d.push(if_(not(v("isoctal")), group));
            d.push(IRStatement::Continue);
            d
        }),
    ];
    escape_body.extend(escapes);
    escape_body.push(if_(
        or(
            and(ge(v("c"), text("a")), le(v("c"), text("z"))),
            and(ge(v("c"), text("A")), le(v("c"), text("Z"))),
        ),
        vec![at_pos(
            concat(vec![text("bad escape \\"), v("c")]),
            sub(v("i"), int(2)),
        )],
    ));
    escape_body.push(inc("lit", concat(vec![text("\\"), v("c")])));
    let body = vec![
        assign_typed(
            "t",
            call(&class_name("_Template"), Vec::new()),
            &ty("_Template"),
        ),
        assign_typed("lit", text(""), &s()),
        assign("n", len(v("repl"))),
        assign("i", int(0)),
        while_(
            lt(v("i"), v("n")),
            vec![
                assign_typed("c", ch(v("i")), &s()),
                inc("i", int(1)),
                if_(
                    ne(v("c"), text("\\")),
                    vec![inc("lit", v("c")), IRStatement::Continue],
                ),
                if_(
                    ge(v("i"), v("n")),
                    vec![at_pos(
                        text("bad escape (end of pattern)"),
                        sub(v("n"), int(1)),
                    )],
                ),
                if_(boolean(true), escape_body),
            ],
        ),
        flush(),
        ret_val(v("t")),
    ];
    function(
        "template",
        vec![
            param("repl", &s()),
            param("ngroups", &i()),
            param("names", &ls()),
            param("indices", &li()),
        ],
        ty("_Template"),
        body,
    )
}
