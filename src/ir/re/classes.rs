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
/// `finditer`'s iterator, which CPython names `callable_iterator`.
pub(crate) const FIND_ITER: &str = "_FindIter";
/// `Pattern.groupindex`'s class, which CPython names `mappingproxy`.
pub(crate) const GROUP_INDEX: &str = "_GroupIndex";

/// What a pattern matches, str or bytes. Each has classes of its own (a
/// bytes pattern's are `re._B...`, which print as CPython's `re.Pattern` and
/// `re.Match`) and functions of its own (`__re_b_...`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Text {
    Str,
    Bytes,
}

impl Text {
    /// The subject's type, and a group's.
    pub(super) fn ty(self) -> IRType {
        match self {
            Text::Str => IRType::String,
            Text::Bytes => IRType::Bytes,
        }
    }
    fn opt(self) -> IRType {
        IRType::Optional(Box::new(self.ty()))
    }
    fn list(self) -> IRType {
        IRType::List(Box::new(self.ty()))
    }
    fn list_opt(self) -> IRType {
        IRType::List(Box::new(self.opt()))
    }
    /// A class's short name: `Match`, or a bytes pattern's `_BMatch`.
    pub(super) fn short(self, short: &str) -> String {
        match self {
            Text::Str => short.to_string(),
            Text::Bytes => format!("_B{}", short.trim_start_matches('_')),
        }
    }
    pub(super) fn class(self, short: &str) -> String {
        class_name(&self.short(short))
    }
    pub(super) fn cty(self, short: &str) -> IRType {
        IRType::Class(self.class(short))
    }
    fn me(self, short: &str) -> IRParam {
        param("self", &self.cty(short))
    }
    fn fname(self, name: &str) -> String {
        match self {
            Text::Str => fname(name),
            Text::Bytes => fname(&format!("b_{name}")),
        }
    }
    fn fcall(self, name: &str, args: Vec<IRExpr>) -> IRExpr {
        call(&self.fname(name), args)
    }
    /// The empty str or bytes.
    fn empty(self) -> IRExpr {
        match self {
            Text::Str => text(""),
            Text::Bytes => bytes_const(b""),
        }
    }
}

pub(super) fn bytes_const(b: &[u8]) -> IRExpr {
    IRExpr::Const(crate::ir::IRConstant::Bytes(b.to_vec()))
}

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

pub(super) fn all(cache: Vec<IRVariable>, with_functions: bool, texts: &[Text]) -> Vec<IRClass> {
    let mut out = Vec::new();
    for &t in texts {
        out.extend([
            subject(t),
            template(t),
            pattern(t, with_functions),
            pattern0(t),
            pattern1(t),
            pattern_n(t),
            matched(t),
            find_iter(t),
        ]);
    }
    out.extend([
        group_index(),
        IRClass {
            name: class_name("_Cache"),
            bases: Vec::new(),
            methods: Vec::new(),
            class_vars: cache,
            metaclass: None,
            position: 0,
            deferred_vars: Vec::new(),
        },
    ]);
    out
}

/// A subject string as the matcher reads it: its characters and their codes,
/// and their lowercases (and those lowercases' uppercases) when the pattern
/// ignores case.
fn subject(t: Text) -> IRClass {
    if t == Text::Bytes {
        return bytes_subject();
    }
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

/// `finditer`'s iterator: each `next()` searches on from the last match, as
/// CPython's scanner does, the search after an empty match not matching
/// empty where it starts.
fn find_iter(t: Text) -> IRClass {
    let short = t.short(FIND_ITER);
    let short = short.as_str();
    let subj_ty = t.cty("_Subject");
    let stop = || IRStatement::Raise {
        exception: Some(v("StopIteration")),
        cause: None,
    };
    class(
        short,
        None,
        vec![
            method(
                "__init__",
                vec![
                    me(short),
                    param("re", &t.cty("Pattern")),
                    param("subj", &subj_ty),
                    param("pos", &i()),
                    param("endpos", &i()),
                ],
                IRType::None,
                vec![
                    set_my_typed("_re", v("re"), &t.cty("Pattern")),
                    set_my_typed("_subj", v("subj"), &subj_ty),
                    set_my_typed("_pos", v("pos"), &i()),
                    set_my_typed("_endpos", v("endpos"), &i()),
                    set_my_typed("_at", v("pos"), &i()),
                    set_my_typed("_must", boolean(false), &b()),
                    set_my_typed("_done", lt(v("endpos"), v("pos")), &b()),
                ],
            ),
            method(
                "__iter__",
                vec![me(short)],
                ty(short),
                vec![ret_val(v("self"))],
            ),
            method(
                "__next__",
                vec![me(short)],
                t.cty("Match"),
                vec![
                    if_(
                        or(my("_done"), cmp(my("_at"), IRCompareOp::Gt, my("_endpos"))),
                        vec![stop()],
                    ),
                    assign_typed(
                        "r",
                        mcall(
                            my("_re"),
                            "_find",
                            vec![my("_subj"), my("_at"), my("_endpos"), my("_must")],
                        ),
                        &li(),
                    ),
                    if_(
                        eq(len(v("r")), int(0)),
                        vec![set_attr(v("self"), "_done", boolean(true)), stop()],
                    ),
                    set_attr(
                        v("self"),
                        "_must",
                        eq(idx(v("r"), int(1)), idx(v("r"), int(0))),
                    ),
                    set_attr(v("self"), "_at", idx(v("r"), int(1))),
                    ret_val(call(
                        &t.class("Match"),
                        vec![
                            my("_re"),
                            attr(my("_subj"), "text"),
                            my("_pos"),
                            my("_endpos"),
                            v("r"),
                        ],
                    )),
                ],
            ),
        ],
    )
}

/// `Pattern.groupindex`: a read-only view of the group names' numbers, a
/// `mappingproxy` of a dict, which reads as that dict and cannot be changed.
fn group_index() -> IRClass {
    let short = GROUP_INDEX;
    let dict_ty = IRType::Dict(Box::new(s()), Box::new(i()));
    let mut c = class(
        short,
        None,
        vec![
            method(
                "__init__",
                vec![me(short), param("names", &ls()), param("indices", &li())],
                IRType::None,
                vec![
                    set_my_typed("_d", IRExpr::DictLiteral(Vec::new()), &dict_ty),
                    for_(
                        "k",
                        range(len(v("names"))),
                        vec![set_item(
                            my("_d"),
                            idx(v("names"), v("k")),
                            idx(v("indices"), v("k")),
                        )],
                    ),
                ],
            ),
            method(
                crate::ir::VIEW_METHOD,
                vec![me(short)],
                dict_ty.clone(),
                vec![ret_val(mcall(my("_d"), "copy", Vec::new()))],
            ),
            method(
                "__getitem__",
                vec![me(short), param("key", &s())],
                i(),
                vec![ret_val(idx(my("_d"), v("key")))],
            ),
            method(
                "__len__",
                vec![me(short)],
                i(),
                vec![ret_val(len(my("_d")))],
            ),
            method(
                "__contains__",
                vec![me(short), param("key", &s())],
                b(),
                vec![ret_val(cmp(v("key"), IRCompareOp::In, my("_d")))],
            ),
            // str() is the dict's, repr() names the proxy.
            method(
                "__str__",
                vec![me(short)],
                s(),
                vec![ret_val(call("str", vec![my("_d")]))],
            ),
            method(
                "__repr__",
                vec![me(short)],
                s(),
                vec![ret_val(concat(vec![
                    text("mappingproxy("),
                    call("repr", vec![my("_d")]),
                    text(")"),
                ]))],
            ),
        ],
    );
    c.class_vars.push(IRVariable {
        name: crate::ir::DELEGATES_VAR.to_string(),
        value: IRExpr::TupleLiteral(
            ["get", "keys", "values", "items", "copy"]
                .iter()
                .map(|d| text(d))
                .collect(),
        ),
        var_type: None,
    });
    c
}

/// A bytes subject as the matcher reads it: its bytes as codes. A bytes
/// pattern matches by ASCII's classes and case, so it never reads the
/// characters (each the latin-1 one, for the sets that are given one) or the
/// lowercases.
fn bytes_subject() -> IRClass {
    let short = Text::Bytes.short("_Subject");
    class(
        &short,
        None,
        vec![method(
            "__init__",
            vec![me(&short), param("text", &IRType::Bytes)],
            IRType::None,
            vec![
                set_my_typed("text", v("text"), &IRType::Bytes),
                set_my_typed("n", len(v("text")), &i()),
                set_my_typed("codes", IRExpr::ListLiteral(Vec::new()), &li()),
                set_my_typed("chars", IRExpr::ListLiteral(Vec::new()), &ls()),
                for_(
                    "k",
                    range(my("n")),
                    vec![
                        assign("c", idx(v("text"), v("k"))),
                        append(my("codes"), v("c")),
                        append(my("chars"), call("chr", vec![v("c")])),
                    ],
                ),
                set_my_typed("low", IRExpr::ListLiteral(Vec::new()), &li()),
                set_my_typed("up", IRExpr::ListLiteral(Vec::new()), &li()),
            ],
        )],
    )
}

/// A replacement template: text, or a group (`-1` for text) in turn.
fn template(t: Text) -> IRClass {
    let short = t.short("_Template");
    class(
        &short,
        None,
        vec![method(
            "__init__",
            vec![me(&short)],
            IRType::None,
            vec![
                set_my_typed("texts", IRExpr::ListLiteral(Vec::new()), &t.list()),
                set_my_typed("groups", IRExpr::ListLiteral(Vec::new()), &li()),
            ],
        )],
    )
}

fn pattern_params(t: Text) -> Vec<IRParam> {
    vec![
        t.me("Pattern"),
        param("pattern", &t.ty()),
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

fn new_subject(t: Text, string: IRExpr) -> IRExpr {
    match t {
        Text::Str => call(
            &class_name("_Subject"),
            vec![string, my("_lower"), my("_upper")],
        ),
        Text::Bytes => call(&t.class("_Subject"), vec![string]),
    }
}

fn search_params(t: Text, short: &str) -> Vec<IRParam> {
    vec![
        t.me(short),
        param("string", &t.ty()),
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
fn pattern(t: Text, with_functions: bool) -> IRClass {
    let me_p = || t.me("Pattern");
    let opt_match = IRType::Optional(Box::new(t.cty("Match")));
    let subj_ty = t.cty("_Subject");
    let made = |r: &str| {
        call(
            &t.class("Match"),
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
        pattern_params(t),
        IRType::None,
        vec![
            set_my_typed("pattern", v("pattern"), &t.ty()),
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
        let mut body = vec![assign_typed("subj", new_subject(t, v("string")), &subj_ty)];
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
            search_params(t, "Pattern"),
            opt_match.clone(),
            body,
        ));
    }
    let mut body = vec![assign_typed("subj", new_subject(t, v("string")), &subj_ty)];
    body.extend(clamp());
    body.push(ret_val(call(
        &t.class(FIND_ITER),
        vec![v("self"), v("subj"), v("pos"), v("endpos")],
    )));
    m.push(method(
        "finditer",
        search_params(t, "Pattern"),
        t.cty(FIND_ITER),
        body,
    ));
    // sub and subn, by a template or a function.
    let by_template = |r: IRExpr| t.fcall("expand", vec![v("t"), v("string"), r]);
    let by_function = |r: IRExpr| IRExpr::FunctionCall {
        function_name: "repl".to_string(),
        arguments: vec![call(
            &t.class("Match"),
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
                params: vec![t.cty("Match")],
                return_type: Box::new(t.ty()),
            }
        } else {
            t.ty()
        };
        let mut body = Vec::new();
        if !fun {
            body.push(assign_typed(
                "t",
                t.fcall(
                    "template",
                    vec![v("repl"), my("_ngroups"), my("_names"), my("_indices")],
                ),
                &t.cty("_Template"),
            ));
        }
        body.extend([
            assign_typed("subj", new_subject(t, v("string")), &subj_ty),
            assign_typed("pieces", IRExpr::ListLiteral(Vec::new()), &t.list()),
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
            assign_typed(
                "result",
                mcall(t.empty(), "join", vec![v("pieces")]),
                &t.ty(),
            ),
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
                param("string", &t.ty()),
                param_default("count", &i(), int(0)),
            ],
            if counted {
                IRType::Tuple(vec![t.ty(), i()])
            } else {
                t.ty()
            },
            body,
        ));
    }
    // A new mappingproxy at each read, as CPython's getter makes.
    let mut groupindex = method(
        "groupindex",
        vec![me_p()],
        ty(GROUP_INDEX),
        vec![ret_val(call(
            &class_name(GROUP_INDEX),
            vec![my("_names"), my("_indices")],
        ))],
    );
    groupindex.decorators = vec!["property".to_string()];
    m.push(groupindex);
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
    class(&t.short("Pattern"), None, m)
}

/// A group's text in match `r` of `string`, or None.
fn group_of(t: Text, r: IRExpr, g: IRExpr) -> IRExpr {
    t.fcall("group", vec![v("string"), r, g])
}

/// `findall` and `split` for a pattern with `groups` groups (2 for several).
fn finders(t: Text, short: &str, groups: usize) -> Vec<IRFunction> {
    let subj_ty = t.cty("_Subject");
    let item_ty = match groups {
        0 | 1 => t.ty(),
        _ => IRType::VarTuple(Box::new(t.ty())),
    };
    let item = |r: IRExpr| match groups {
        0 => slice(
            v("string"),
            Some(idx(r.clone(), int(0))),
            Some(idx(r, int(1))),
        ),
        1 => t.fcall("group_or", vec![v("string"), r, int(1), t.empty()]),
        _ => call(
            crate::ir::AS_TUPLE_FN,
            vec![t.fcall(
                "groups_list",
                vec![v("string"), r, my("_ngroups"), t.empty()],
            )],
        ),
    };
    let mut findall = vec![assign_typed("subj", new_subject(t, v("string")), &subj_ty)];
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
    let piece_ty = if groups == 0 { t.ty() } else { t.opt() };
    let mut split = vec![
        assign_typed("subj", new_subject(t, v("string")), &subj_ty),
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
                        vec![append(v("out"), group_of(t, v("r"), add(v("g"), int(1))))],
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
            search_params(t, short),
            IRType::List(Box::new(item_ty)),
            findall,
        ),
        method(
            "split",
            vec![
                t.me(short),
                param("string", &t.ty()),
                param_default("maxsplit", &i(), int(0)),
            ],
            IRType::List(Box::new(piece_ty)),
            split,
        ),
    ]
}

fn subclass_init(t: Text, short: &str) -> IRFunction {
    let mut params = pattern_params(t);
    params[0] = t.me(short);
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

fn pattern0(t: Text) -> IRClass {
    let mut m = vec![subclass_init(t, PATTERN0)];
    m.extend(finders(t, PATTERN0, 0));
    class(&t.short(PATTERN0), Some(&t.short("Pattern")), m)
}
fn pattern1(t: Text) -> IRClass {
    let mut m = vec![subclass_init(t, PATTERN1)];
    m.extend(finders(t, PATTERN1, 1));
    class(&t.short(PATTERN1), Some(&t.short("Pattern")), m)
}
fn pattern_n(t: Text) -> IRClass {
    let mut m = vec![subclass_init(t, PATTERN_N)];
    m.extend(finders(t, PATTERN_N, 2));
    class(&t.short(PATTERN_N), Some(&t.short("Pattern")), m)
}

fn matched(t: Text) -> IRClass {
    let me_m = || t.me("Match");
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
    let group_text = || t.fcall("group", vec![my("string"), marks(), v("g")]);
    let mut m = vec![
        method(
            "__init__",
            vec![
                me_m(),
                param("re", &t.cty("Pattern")),
                param("string", &t.ty()),
                param("pos", &i()),
                param("endpos", &i()),
                param("marks", &li()),
            ],
            IRType::None,
            vec![
                set_my_typed("re", v("re"), &t.cty("Pattern")),
                set_my_typed("string", v("string"), &t.ty()),
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
            t.ty(),
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
            t.opt(),
            vec![check_index(), ret_val(group_text())],
        ));
        m.push(method(
            &variant(name, &s()),
            vec![me_m(), param("g", &s())],
            t.opt(),
            vec![ret_val(t.fcall(
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
        vec![me_m(), param_default("default", &t.opt(), none())],
        IRType::VarTuple(Box::new(t.opt())),
        vec![
            assign_typed("out", IRExpr::ListLiteral(Vec::new()), &t.list_opt()),
            for_(
                "g",
                range(sub(attr(my("re"), "_ngroups"), int(1))),
                vec![
                    assign_typed(
                        "t",
                        t.fcall("group", vec![my("string"), marks(), add(v("g"), int(1))]),
                        &t.opt(),
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
        vec![me_m(), param_default("default", &t.opt(), none())],
        IRType::Dict(Box::new(s()), Box::new(t.opt())),
        vec![
            assign_typed(
                "out",
                IRExpr::DictLiteral(Vec::new()),
                &IRType::Dict(Box::new(s()), Box::new(t.opt())),
            ),
            assign("k", int(0)),
            while_(
                lt(v("k"), len(attr(my("re"), "_names"))),
                vec![
                    assign_typed(
                        "t",
                        t.fcall(
                            "group",
                            vec![
                                my("string"),
                                marks(),
                                idx(attr(my("re"), "_indices"), v("k")),
                            ],
                        ),
                        &t.opt(),
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
        vec![me_m(), param("template", &t.ty())],
        t.ty(),
        vec![
            assign_typed(
                "t",
                t.fcall(
                    "template",
                    vec![
                        v("template"),
                        attr(my("re"), "_ngroups"),
                        attr(my("re"), "_names"),
                        attr(my("re"), "_indices"),
                    ],
                ),
                &t.cty("_Template"),
            ),
            ret_val(t.fcall("expand", vec![v("t"), my("string"), marks()])),
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
    class(&t.short("Match"), None, m)
}

/// The module's functions behind the classes, for the kinds of text its
/// patterns match.
pub(super) fn functions(texts: &[Text]) -> Vec<IRFunction> {
    let mut out = vec![
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
        // `s.isascii()`.
        function(
            "ascii_only",
            vec![param("text", &s())],
            b(),
            vec![
                for_(
                    "c",
                    v("text"),
                    vec![if_(
                        cmp(call("ord", vec![v("c")]), IRCompareOp::Gt, int(127)),
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
    ];
    for &t in texts {
        out.extend(text_functions(t));
        if t == Text::Bytes {
            out.extend([bytes_template(), bytes_escape(), latin1()]);
        }
    }
    out
}

fn tfunction(
    t: Text,
    name: &str,
    params: Vec<IRParam>,
    ret: IRType,
    statements: Vec<IRStatement>,
) -> IRFunction {
    IRFunction {
        name: t.fname(name),
        params,
        body: crate::ir::IRBody { statements },
        return_type: ret,
        decorators: Vec::new(),
    }
}

/// A match's groups and its replacement by a template, over the text the
/// pattern matches.
fn text_functions(t: Text) -> Vec<IRFunction> {
    let r = || param("r", &li());
    vec![
        // Group `g`'s text in a match, or None when it did not take part.
        tfunction(
            t,
            "group",
            vec![param("string", &t.ty()), r(), param("g", &i())],
            t.opt(),
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
        tfunction(
            t,
            "group_or",
            vec![
                param("string", &t.ty()),
                r(),
                param("g", &i()),
                param("default", &t.ty()),
            ],
            t.ty(),
            vec![
                assign_typed(
                    "t",
                    t.fcall("group", vec![v("string"), v("r"), v("g")]),
                    &t.opt(),
                ),
                if_(
                    cmp(v("t"), IRCompareOp::Is, none()),
                    vec![ret_val(v("default"))],
                ),
                assign_typed("got", v("t"), &t.ty()),
                ret_val(v("got")),
            ],
        ),
        tfunction(
            t,
            "groups_list",
            vec![
                param("string", &t.ty()),
                r(),
                param("ngroups", &i()),
                param("default", &t.ty()),
            ],
            t.list(),
            vec![
                assign_typed("out", IRExpr::ListLiteral(Vec::new()), &t.list()),
                for_(
                    "g",
                    range(sub(v("ngroups"), int(1))),
                    vec![append(
                        v("out"),
                        t.fcall(
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
        tfunction(
            t,
            "expand",
            vec![
                param("t", &t.cty("_Template")),
                param("string", &t.ty()),
                r(),
            ],
            t.ty(),
            vec![
                assign_typed("pieces", IRExpr::ListLiteral(Vec::new()), &t.list()),
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
                                    t.fcall("group", vec![v("string"), v("r"), v("g")]),
                                    &t.opt(),
                                ),
                                if_(
                                    cmp(v("got"), IRCompareOp::IsNot, none()),
                                    vec![
                                        assign_typed("text", v("got"), &t.ty()),
                                        append(v("pieces"), v("text")),
                                    ],
                                ),
                            ],
                        ),
                        inc("k", int(1)),
                    ],
                ),
                ret_val(mcall(t.empty(), "join", vec![v("pieces")])),
            ],
        ),
    ]
}

/// The characters `re.escape` puts a backslash before (CPython 3.13's).
const SPECIAL_CHARS: &str = "()[]{}?*+-|^$\\.&~# \t\n\r\x0b\x0c";

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
                        cmp(v("c"), IRCompareOp::In, text(SPECIAL_CHARS)),
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
                    // A bytes template's names are ASCII, quoted as ascii()
                    // quotes them.
                    if_(
                        or(
                            not(fcall("identifier", vec![v("name")])),
                            and(v("ascii_names"), not(fcall("ascii_only", vec![v("name")]))),
                        ),
                        vec![at_pos(
                            concat(vec![
                                text("bad character in group name "),
                                IRExpr::Conditional {
                                    condition: Box::new(v("ascii_names")),
                                    body: Box::new(call("ascii", vec![v("name")])),
                                    orelse: Box::new(call("repr", vec![v("name")])),
                                },
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
            param_default("ascii_names", &b(), boolean(false)),
        ],
        ty("_Template"),
        body,
    )
}

/// A bytes template, as CPython parses one: its latin-1 decoding parsed as
/// a str template is (its names ASCII), each literal piece encoded back.
fn bytes_template() -> IRFunction {
    let t = Text::Bytes;
    tfunction(
        t,
        "template",
        vec![
            param("repl", &IRType::Bytes),
            param("ngroups", &i()),
            param("names", &ls()),
            param("indices", &li()),
        ],
        t.cty("_Template"),
        vec![
            assign_typed(
                "parsed",
                fcall(
                    "template",
                    vec![
                        fcall("latin1", vec![v("repl")]),
                        v("ngroups"),
                        v("names"),
                        v("indices"),
                        boolean(true),
                    ],
                ),
                &ty("_Template"),
            ),
            assign_typed(
                "out",
                call(&t.class("_Template"), Vec::new()),
                &t.cty("_Template"),
            ),
            for_(
                "k",
                range(len(attr(v("parsed"), "texts"))),
                vec![
                    assign_typed("piece", idx(attr(v("parsed"), "texts"), v("k")), &s()),
                    assign_typed("codes", IRExpr::ListLiteral(Vec::new()), &li()),
                    for_(
                        "c",
                        v("piece"),
                        vec![append(v("codes"), call("ord", vec![v("c")]))],
                    ),
                    append(attr(v("out"), "texts"), call("bytes", vec![v("codes")])),
                    append(
                        attr(v("out"), "groups"),
                        idx(attr(v("parsed"), "groups"), v("k")),
                    ),
                ],
            ),
            ret_val(v("out")),
        ],
    )
}

/// `str(b, 'latin-1')`: each byte the character of its code.
fn latin1() -> IRFunction {
    function(
        "latin1",
        vec![param("b", &IRType::Bytes)],
        s(),
        vec![
            assign_typed("pieces", IRExpr::ListLiteral(Vec::new()), &ls()),
            for_(
                "k",
                range(len(v("b"))),
                vec![append(v("pieces"), call("chr", vec![idx(v("b"), v("k"))]))],
            ),
            ret_val(mcall(text(""), "join", vec![v("pieces")])),
        ],
    )
}

/// `re.escape(b)` of bytes: the special characters' bytes each after a
/// backslash.
fn bytes_escape() -> IRFunction {
    let t = Text::Bytes;
    tfunction(
        t,
        "escape",
        vec![param("text", &IRType::Bytes)],
        IRType::Bytes,
        vec![
            assign_typed("pieces", IRExpr::ListLiteral(Vec::new()), &t.list()),
            for_(
                "k",
                range(len(v("text"))),
                vec![
                    if_(
                        cmp(
                            call("chr", vec![idx(v("text"), v("k"))]),
                            IRCompareOp::In,
                            text(SPECIAL_CHARS),
                        ),
                        vec![append(v("pieces"), bytes_const(b"\\"))],
                    ),
                    append(
                        v("pieces"),
                        slice(v("text"), Some(v("k")), Some(add(v("k"), int(1)))),
                    ),
                ],
            ),
            ret_val(mcall(t.empty(), "join", vec![v("pieces")])),
        ],
    )
}
