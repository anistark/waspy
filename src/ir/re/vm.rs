//! The matcher: the compiled program run over a subject's characters with an
//! explicit backtracking stack, as `sre`'s `SRE(match)` walks its pattern.
//!
//! The stack holds entries of three words, `kind a b`: a choice to resume
//! (`pc`, position), an undo of a mark, a repeat's count or its last start,
//! or `lastindex`, and a barrier an atomic group or a lookaround set. Undo
//! entries are replayed as the matcher backtracks past them, so a mark set on
//! a path that failed is unset again, as `sre`'s mark stack does.

use super::compile::{at, op};
use super::fname;
use crate::ir::build::*;
use crate::ir::{IRCompareOp, IRExpr, IRFunction, IROp, IRStatement, IRType};

const CHOICE: i32 = 1;
const UNDO_MARK: i32 = 2;
const UNDO_COUNT: i32 = 3;
const UNDO_LAST: i32 = 4;
const UNDO_LASTINDEX: i32 = 5;
const BARRIER: i32 = 6;
/// A barrier's resume for an atomic group, and for a positive lookaround.
const ATOMIC_MARK: i32 = -2;
const POSITIVE: i32 = -1;

fn li() -> IRType {
    IRType::List(Box::new(IRType::Int))
}
fn ls() -> IRType {
    IRType::List(Box::new(IRType::String))
}
fn function(
    name: &str,
    params: Vec<crate::ir::IRParam>,
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
fn fcall(name: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&fname(name), args)
}
fn add(a: IRExpr, b: IRExpr) -> IRExpr {
    bin(a, IROp::Add, b)
}
fn sub(a: IRExpr, b: IRExpr) -> IRExpr {
    bin(a, IROp::Sub, b)
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
fn prog(k: IRExpr) -> IRExpr {
    idx(v("prog"), k)
}
fn operand(n: i32) -> IRExpr {
    prog(add(v("pc"), int(n)))
}
fn inc(var: &str, by: IRExpr) -> IRStatement {
    assign(var, add(v(var), by))
}
fn push(kind: i32, a: IRExpr, b: IRExpr) -> Vec<IRStatement> {
    vec![
        expr_stmt(mcall(v("stack"), "append", vec![int(kind)])),
        expr_stmt(mcall(v("stack"), "append", vec![a])),
        expr_stmt(mcall(v("stack"), "append", vec![b])),
    ]
}
/// `if op == code: <body>` where the body ends in `continue` on success and
/// falls through to the backtracking at the loop's end on failure.
fn on(code: i32, body: Vec<IRStatement>) -> IRStatement {
    if_(eq(v("op"), int(code)), body)
}
fn next(by: i32) -> Vec<IRStatement> {
    vec![inc("pc", int(by)), IRStatement::Continue]
}
fn ascii_lower(x: IRExpr) -> IRExpr {
    IRExpr::Conditional {
        condition: Box::new(and(ge(x.clone(), int(65)), le(x.clone(), int(90)))),
        body: Box::new(add(x.clone(), int(32))),
        orelse: Box::new(x),
    }
}

pub(super) fn functions() -> Vec<IRFunction> {
    vec![category(), word(), in_set(), run()]
}

/// A category, of the character `ch` whose code is `x`.
fn category() -> IRFunction {
    let ascii_word = |x: IRExpr| {
        or(
            or(
                and(ge(x.clone(), int(48)), le(x.clone(), int(57))),
                and(ge(x.clone(), int(65)), le(x.clone(), int(90))),
            ),
            or(
                and(ge(x.clone(), int(97)), le(x.clone(), int(122))),
                eq(x, int(95)),
            ),
        )
    };
    let ascii_space = |x: IRExpr| {
        or(
            eq(x.clone(), int(32)),
            and(ge(x.clone(), int(9)), le(x, int(13))),
        )
    };
    let mut body = Vec::new();
    for (code, test) in [
        (0, mcall(v("ch"), "isdecimal", Vec::new())),
        (2, mcall(v("ch"), "isspace", Vec::new())),
        (
            4,
            or(mcall(v("ch"), "isalnum", Vec::new()), eq(v("x"), int(95))),
        ),
        (6, and(ge(v("x"), int(48)), le(v("x"), int(57)))),
        (8, ascii_space(v("x"))),
        (10, ascii_word(v("x"))),
    ] {
        body.push(if_(eq(v("cat"), int(code)), vec![ret_val(test.clone())]));
        body.push(if_(eq(v("cat"), int(code + 1)), vec![ret_val(not(test))]));
    }
    body.push(ret_val(boolean(false)));
    function(
        "category",
        vec![
            param("cat", &IRType::Int),
            param("x", &IRType::Int),
            param("ch", &IRType::String),
        ],
        IRType::Bool,
        body,
    )
}

/// Whether the character at `i` is a word character, as `\b` reads it.
fn word() -> IRFunction {
    function(
        "word",
        vec![
            param("chars", &ls()),
            param("codes", &li()),
            param("i", &IRType::Int),
            param("ascii", &IRType::Bool),
        ],
        IRType::Bool,
        vec![
            if_(
                v("ascii"),
                vec![ret_val(fcall(
                    "category",
                    vec![int(10), idx(v("codes"), v("i")), idx(v("chars"), v("i"))],
                ))],
            ),
            ret_val(fcall(
                "category",
                vec![int(4), idx(v("codes"), v("i")), idx(v("chars"), v("i"))],
            )),
        ],
    )
}

/// The set at `pc` has `x` (its `uni` ranges also `xu`; its categories read
/// `ch`).
fn in_set() -> IRFunction {
    function(
        "in_set",
        vec![
            param("prog", &li()),
            param("pc", &IRType::Int),
            param("x", &IRType::Int),
            param("xu", &IRType::Int),
            param("ch", &IRType::String),
        ],
        IRType::Bool,
        vec![
            assign("negate", eq(prog(add(v("pc"), int(2))), int(1))),
            assign("k", add(v("pc"), int(4))),
            assign("n", prog(v("k"))),
            inc("k", int(1)),
            assign("hit", boolean(false)),
            assign("i", int(0)),
            while_(
                lt(v("i"), v("n")),
                vec![
                    if_(
                        and(
                            le(prog(v("k")), v("x")),
                            le(v("x"), prog(add(v("k"), int(1)))),
                        ),
                        vec![assign("hit", boolean(true))],
                    ),
                    inc("k", int(2)),
                    inc("i", int(1)),
                ],
            ),
            assign("n", prog(v("k"))),
            inc("k", int(1)),
            assign("i", int(0)),
            while_(
                lt(v("i"), v("n")),
                vec![
                    if_(
                        and(
                            not(v("hit")),
                            fcall("category", vec![prog(v("k")), v("x"), v("ch")]),
                        ),
                        vec![assign("hit", boolean(true))],
                    ),
                    inc("k", int(1)),
                    inc("i", int(1)),
                ],
            ),
            assign("n", prog(v("k"))),
            inc("k", int(1)),
            assign("i", int(0)),
            while_(
                lt(v("i"), v("n")),
                vec![
                    assign("lo", prog(v("k"))),
                    assign("hi", prog(add(v("k"), int(1)))),
                    if_(
                        or(
                            and(le(v("lo"), v("x")), le(v("x"), v("hi"))),
                            and(le(v("lo"), v("xu")), le(v("xu"), v("hi"))),
                        ),
                        vec![assign("hit", boolean(true))],
                    ),
                    inc("k", int(2)),
                    inc("i", int(1)),
                ],
            ),
            ret_val(ne(v("hit"), v("negate"))),
        ],
    )
}

/// Run the program from `start`, the subject ending at `end`. The marks
/// (group 0's first, -1 where unset) and `lastindex` follow, or nothing
/// when it does not match. `match_all` is fullmatch's must-end-at-`end`,
/// `must_advance` findall's and sub's no-empty-match-at-`start`.
fn run() -> IRFunction {
    let sp = || v("sp");
    let end = || v("end");
    let codes_at = |k: IRExpr| idx(v("codes"), k);
    let in_range = || lt(sp(), end());
    let mut ops: Vec<IRStatement> = Vec::new();
    ops.push(on(
        op::MATCH,
        vec![if_(
            not(or(
                and(v("match_all"), ne(sp(), end())),
                and(v("must_advance"), eq(sp(), v("start"))),
            )),
            vec![
                set_item(v("marks"), int(0), v("start")),
                set_item(v("marks"), int(1), sp()),
                expr_stmt(mcall(v("marks"), "append", vec![v("lastindex")])),
                ret_val(v("marks")),
            ],
        )],
    ));
    let char_op = |code: i32, test: IRExpr| {
        on(
            code,
            vec![if_(
                and(in_range(), test),
                vec![inc("sp", int(1)), inc("pc", int(2)), IRStatement::Continue],
            )],
        )
    };
    ops.push(char_op(op::CHAR, eq(codes_at(sp()), operand(1))));
    ops.push(char_op(op::CHAR_I, eq(idx(v("low"), sp()), operand(1))));
    ops.push(char_op(
        op::CHAR_IA,
        eq(ascii_lower(codes_at(sp())), operand(1)),
    ));
    ops.push(char_op(op::NOTCHAR, ne(codes_at(sp()), operand(1))));
    ops.push(char_op(op::NOTCHAR_I, ne(idx(v("low"), sp()), operand(1))));
    ops.push(char_op(
        op::NOTCHAR_IA,
        ne(ascii_lower(codes_at(sp())), operand(1)),
    ));
    ops.push(on(
        op::ANY,
        vec![if_(
            and(in_range(), ne(codes_at(sp()), int(10))),
            vec![inc("sp", int(1)), inc("pc", int(1)), IRStatement::Continue],
        )],
    ));
    ops.push(on(
        op::ANYALL,
        vec![if_(
            in_range(),
            vec![inc("sp", int(1)), inc("pc", int(1)), IRStatement::Continue],
        )],
    ));
    // The set is tested on the character, its lowercase, or its ASCII
    // lowercase, as its mode says.
    ops.push(on(
        op::SET,
        vec![if_(
            in_range(),
            vec![
                assign("mode", operand(3)),
                assign("x", codes_at(sp())),
                assign("xu", v("x")),
                if_(
                    eq(v("mode"), int(1)),
                    vec![
                        assign("x", idx(v("low"), sp())),
                        if_(gt_zero_len(v("up")), vec![assign("xu", idx(v("up"), sp()))]),
                    ],
                ),
                if_(
                    eq(v("mode"), int(2)),
                    vec![assign("x", ascii_lower(codes_at(sp())))],
                ),
                assign_typed("ch", idx(v("chars"), sp()), &IRType::String),
                if_(
                    fcall("in_set", vec![v("prog"), v("pc"), v("x"), v("xu"), v("ch")]),
                    vec![
                        inc("sp", int(1)),
                        inc("pc", operand(1)),
                        IRStatement::Continue,
                    ],
                ),
            ],
        )],
    ));
    let mut split = push(CHOICE, operand(2), sp());
    split.push(assign("pc", operand(1)));
    split.push(IRStatement::Continue);
    ops.push(on(op::SPLIT, split));
    ops.push(on(
        op::JMP,
        vec![assign("pc", operand(1)), IRStatement::Continue],
    ));
    let mut save = vec![assign("k", operand(1))];
    save.extend(push(UNDO_MARK, v("k"), idx(v("marks"), v("k"))));
    save.push(set_item(v("marks"), v("k"), sp()));
    let mut closes = push(UNDO_LASTINDEX, int(0), v("lastindex"));
    closes.push(assign("lastindex", bin(v("k"), IROp::FloorDiv, int(2))));
    save.push(if_(eq(bin(v("k"), IROp::Mod, int(2)), int(1)), closes));
    save.extend(next(2));
    ops.push(on(op::SAVE, save));
    // Assertions.
    let word_at =
        |i: IRExpr, ascii: bool| fcall("word", vec![v("chars"), v("codes"), i, boolean(ascii)]);
    let boundary = |ascii: bool| {
        vec![
            assign(
                "that",
                and(
                    cmp(sp(), IRCompareOp::Gt, int(0)),
                    word_at(sub(sp(), int(1)), ascii),
                ),
            ),
            assign("this", and(in_range(), word_at(sp(), ascii))),
        ]
    };
    let mut assertion = vec![assign("kind", operand(1)), assign("ok", boolean(false))];
    assertion.push(if_(
        or(
            eq(v("kind"), int(at::BEGINNING)),
            eq(v("kind"), int(at::BEGINNING_STRING)),
        ),
        vec![assign("ok", eq(sp(), int(0)))],
    ));
    assertion.push(if_(
        eq(v("kind"), int(at::BEGINNING_LINE)),
        vec![assign(
            "ok",
            or(eq(sp(), int(0)), eq(codes_at(sub(sp(), int(1))), int(10))),
        )],
    ));
    assertion.push(if_(
        eq(v("kind"), int(at::END)),
        vec![assign(
            "ok",
            or(
                eq(sp(), end()),
                and(eq(add(sp(), int(1)), end()), eq(codes_at(sp()), int(10))),
            ),
        )],
    ));
    assertion.push(if_(
        eq(v("kind"), int(at::END_LINE)),
        vec![assign(
            "ok",
            or(eq(sp(), end()), eq(codes_at(sp()), int(10))),
        )],
    ));
    assertion.push(if_(
        eq(v("kind"), int(at::END_STRING)),
        vec![assign("ok", eq(sp(), end()))],
    ));
    for (kind, ascii, negate) in [
        (at::BOUNDARY, false, false),
        (at::NON_BOUNDARY, false, true),
        (at::ASCII_BOUNDARY, true, false),
        (at::ASCII_NON_BOUNDARY, true, true),
    ] {
        let mut b = boundary(ascii);
        // Nothing is a boundary, or not one, in an empty subject.
        b.push(assign(
            "ok",
            and(
                ne(end(), int(0)),
                if negate {
                    eq(v("this"), v("that"))
                } else {
                    ne(v("this"), v("that"))
                },
            ),
        ));
        assertion.push(if_(eq(v("kind"), int(kind)), b));
    }
    assertion.push(if_(v("ok"), next(2)));
    ops.push(on(op::AT, assertion));
    // Back references.
    for (code, read) in [
        (op::GROUPREF, "codes"),
        (op::GROUPREF_I, "low"),
        (op::GROUPREF_IA, "ascii"),
    ] {
        let at = |k: IRExpr| -> IRExpr {
            if read == "ascii" {
                ascii_lower(codes_at(k))
            } else {
                idx(v(read), k)
            }
        };
        ops.push(on(
            code,
            vec![
                assign("g", operand(1)),
                assign("p", idx(v("marks"), bin(int(2), IROp::Mul, v("g")))),
                assign(
                    "e",
                    idx(v("marks"), add(bin(int(2), IROp::Mul, v("g")), int(1))),
                ),
                if_(
                    and(
                        and(ge(v("p"), int(0)), ge(v("e"), v("p"))),
                        le(add(sp(), sub(v("e"), v("p"))), end()),
                    ),
                    vec![
                        assign("same", boolean(true)),
                        assign("i", int(0)),
                        while_(
                            lt(v("i"), sub(v("e"), v("p"))),
                            vec![
                                if_(
                                    ne(at(add(v("p"), v("i"))), at(add(sp(), v("i")))),
                                    vec![assign("same", boolean(false)), IRStatement::Break],
                                ),
                                inc("i", int(1)),
                            ],
                        ),
                        if_(
                            v("same"),
                            vec![
                                inc("sp", sub(v("e"), v("p"))),
                                inc("pc", int(2)),
                                IRStatement::Continue,
                            ],
                        ),
                    ],
                ),
            ],
        ));
    }
    // Repeats.
    let counts = |r: IRExpr| idx(v("counts"), r);
    let lasts = |r: IRExpr| idx(v("lasts"), r);
    let mut repeat = vec![assign("r", operand(1))];
    repeat.extend(push(UNDO_COUNT, v("r"), counts(v("r"))));
    repeat.push(set_item(v("counts"), v("r"), int(-1)));
    repeat.extend(push(UNDO_LAST, v("r"), lasts(v("r"))));
    repeat.push(set_item(v("lasts"), v("r"), int(-1)));
    repeat.extend(next(2));
    ops.push(on(op::REPEAT, repeat));
    let set_count = |c: IRExpr| {
        let mut s = push(UNDO_COUNT, v("r"), counts(v("r")));
        s.push(set_item(v("counts"), v("r"), c));
        s
    };
    let set_last = || {
        let mut s = push(UNDO_LAST, v("r"), lasts(v("r")));
        s.push(set_item(v("lasts"), v("r"), sp()));
        s
    };
    let below_max = || or(lt(v("max"), int(0)), lt(v("c"), v("max")));
    // MAX_UNTIL r min max body exit
    let mut until = vec![
        assign("r", operand(1)),
        assign("min", operand(2)),
        assign("max", operand(3)),
        assign("c", add(counts(v("r")), int(1))),
    ];
    let mut short = set_count(v("c"));
    short.push(assign("pc", operand(4)));
    short.push(IRStatement::Continue);
    until.push(if_(lt(v("c"), v("min")), short));
    let mut again = push(CHOICE, operand(5), sp());
    again.extend(set_count(v("c")));
    again.extend(set_last());
    again.push(assign("pc", operand(4)));
    again.push(IRStatement::Continue);
    until.push(if_(and(below_max(), ne(sp(), lasts(v("r")))), again));
    until.push(assign("pc", operand(5)));
    until.push(IRStatement::Continue);
    ops.push(on(op::MAX_UNTIL, until));
    // MIN_UNTIL r min max more body exit
    let mut until = vec![
        assign("r", operand(1)),
        assign("min", operand(2)),
        assign("c", add(counts(v("r")), int(1))),
    ];
    let mut short = set_count(v("c"));
    short.push(assign("pc", operand(5)));
    short.push(IRStatement::Continue);
    until.push(if_(lt(v("c"), v("min")), short));
    until.extend(push(CHOICE, operand(4), sp()));
    until.push(assign("pc", operand(6)));
    until.push(IRStatement::Continue);
    ops.push(on(op::MIN_UNTIL, until));
    // LAZY_MORE r max body: after the rest failed.
    let mut more = vec![
        assign("r", operand(1)),
        assign("max", operand(2)),
        assign("c", add(counts(v("r")), int(1))),
    ];
    let mut go = set_count(v("c"));
    go.extend(set_last());
    go.push(assign("pc", operand(3)));
    go.push(IRStatement::Continue);
    more.push(if_(and(below_max(), ne(sp(), lasts(v("r")))), go));
    ops.push(on(op::LAZY_MORE, more));
    // Atomic groups and lookarounds.
    let mut atomic = push(BARRIER, int(ATOMIC_MARK), sp());
    atomic.extend(next(1));
    ops.push(on(op::ATOMIC, atomic));
    // The innermost barrier, and the entries above it kept but its choices.
    let find_barrier = || {
        vec![
            assign("bi", sub(len(v("stack")), int(3))),
            while_(
                ne(idx(v("stack"), v("bi")), int(BARRIER)),
                vec![assign("bi", sub(v("bi"), int(3)))],
            ),
        ]
    };
    let cut = || {
        vec![
            assign_typed("kept", IRExpr::ListLiteral(Vec::new()), &li()),
            assign("j", add(v("bi"), int(3))),
            while_(
                lt(v("j"), len(v("stack"))),
                vec![
                    if_(
                        ne(idx(v("stack"), v("j")), int(CHOICE)),
                        vec![
                            expr_stmt(mcall(v("kept"), "append", vec![idx(v("stack"), v("j"))])),
                            expr_stmt(mcall(
                                v("kept"),
                                "append",
                                vec![idx(v("stack"), add(v("j"), int(1)))],
                            )),
                            expr_stmt(mcall(
                                v("kept"),
                                "append",
                                vec![idx(v("stack"), add(v("j"), int(2)))],
                            )),
                        ],
                    ),
                    inc("j", int(3)),
                ],
            ),
            while_(
                cmp(len(v("stack")), IRCompareOp::Gt, v("bi")),
                vec![expr_stmt(mcall(v("stack"), "pop", Vec::new()))],
            ),
            for_(
                "w",
                v("kept"),
                vec![expr_stmt(mcall(v("stack"), "append", vec![v("w")]))],
            ),
        ]
    };
    let mut atomic_end = find_barrier();
    atomic_end.extend(cut());
    atomic_end.extend(next(1));
    ops.push(on(op::ATOMIC_END, atomic_end));
    // LOOK negate width end
    let mut look = vec![
        assign("negate", eq(operand(1), int(1))),
        assign("width", operand(2)),
        assign("fits", or(lt(v("width"), int(0)), ge(sp(), v("width")))),
    ];
    // A lookbehind before the subject's start fails, which a negative one
    // takes as success.
    look.push(if_(
        not(v("fits")),
        vec![if_(
            v("negate"),
            vec![assign("pc", operand(3)), IRStatement::Continue],
        )],
    ));
    let mut enter = push(
        BARRIER,
        IRExpr::Conditional {
            condition: Box::new(v("negate")),
            body: Box::new(operand(3)),
            orelse: Box::new(int(POSITIVE)),
        },
        sp(),
    );
    enter.push(if_(
        ge(v("width"), int(0)),
        vec![assign("sp", sub(sp(), v("width")))],
    ));
    enter.extend(next(4));
    look.push(if_(v("fits"), enter));
    ops.push(on(op::LOOK, look));
    let mut look_end = find_barrier();
    // A positive lookaround matched: back to where it started, its choices
    // dropped. A negative one matched: it fails, its marks undone.
    let mut positive = vec![assign("sp", idx(v("stack"), add(v("bi"), int(2))))];
    positive.extend(cut());
    positive.extend(next(1));
    look_end.push(if_(
        eq(idx(v("stack"), add(v("bi"), int(1))), int(POSITIVE)),
        positive,
    ));
    look_end.push(while_(
        ge(len(v("stack")), add(v("bi"), int(3))),
        vec![
            assign("b", mcall(v("stack"), "pop", Vec::new())),
            assign("a", mcall(v("stack"), "pop", Vec::new())),
            assign("kind", mcall(v("stack"), "pop", Vec::new())),
            undo(),
        ],
    ));
    ops.push(on(op::LOOK_END, look_end));
    // COND g no
    let group_set = |g: IRExpr| {
        let p = idx(v("marks"), bin(int(2), IROp::Mul, g.clone()));
        let e = idx(v("marks"), add(bin(int(2), IROp::Mul, g), int(1)));
        and(ge(p.clone(), int(0)), ge(e, p))
    };
    ops.push(on(
        op::COND,
        vec![
            if_(group_set(operand(1)), next(3)),
            assign("pc", operand(2)),
            IRStatement::Continue,
        ],
    ));
    // FAIL falls through to the backtracking.
    let backtrack = vec![
        assign("resumed", boolean(false)),
        while_(
            cmp(len(v("stack")), IRCompareOp::Gt, int(0)),
            vec![
                assign("b", mcall(v("stack"), "pop", Vec::new())),
                assign("a", mcall(v("stack"), "pop", Vec::new())),
                assign("kind", mcall(v("stack"), "pop", Vec::new())),
                if_(
                    eq(v("kind"), int(CHOICE)),
                    vec![
                        assign("pc", v("a")),
                        assign("sp", v("b")),
                        assign("resumed", boolean(true)),
                        IRStatement::Break,
                    ],
                ),
                // A negative lookaround whose pattern failed: it holds.
                if_(
                    and(eq(v("kind"), int(BARRIER)), ge(v("a"), int(0))),
                    vec![
                        assign("pc", v("a")),
                        assign("sp", v("b")),
                        assign("resumed", boolean(true)),
                        IRStatement::Break,
                    ],
                ),
                undo(),
            ],
        ),
        if_(
            not(v("resumed")),
            vec![ret_val(IRExpr::ListLiteral(Vec::new()))],
        ),
    ];
    let mut loop_body = vec![assign("op", prog(v("pc")))];
    loop_body.extend(ops);
    loop_body.extend(backtrack);
    function(
        "run",
        vec![
            param("prog", &li()),
            param("codes", &li()),
            param("chars", &ls()),
            param("low", &li()),
            param("up", &li()),
            param("start", &IRType::Int),
            param("end", &IRType::Int),
            param("groups", &IRType::Int),
            param("nregs", &IRType::Int),
            param("match_all", &IRType::Bool),
            param("must_advance", &IRType::Bool),
        ],
        li(),
        vec![
            assign_typed(
                "marks",
                bin(
                    IRExpr::ListLiteral(vec![int(-1)]),
                    IROp::Mul,
                    bin(int(2), IROp::Mul, v("groups")),
                ),
                &li(),
            ),
            assign_typed(
                "counts",
                bin(IRExpr::ListLiteral(vec![int(-1)]), IROp::Mul, v("nregs")),
                &li(),
            ),
            assign_typed(
                "lasts",
                bin(IRExpr::ListLiteral(vec![int(-1)]), IROp::Mul, v("nregs")),
                &li(),
            ),
            assign_typed("stack", IRExpr::ListLiteral(Vec::new()), &li()),
            assign("lastindex", int(-1)),
            assign("pc", int(0)),
            assign("sp", v("start")),
            while_(boolean(true), loop_body),
            ret_val(IRExpr::ListLiteral(Vec::new())),
        ],
    )
}

fn gt_zero_len(e: IRExpr) -> IRExpr {
    cmp(len(e), IRCompareOp::Gt, int(0))
}

/// Replay an undo entry `kind a b`.
fn undo() -> IRStatement {
    if_(
        boolean(true),
        vec![
            if_(
                eq(v("kind"), int(UNDO_MARK)),
                vec![set_item(v("marks"), v("a"), v("b"))],
            ),
            if_(
                eq(v("kind"), int(UNDO_COUNT)),
                vec![set_item(v("counts"), v("a"), v("b"))],
            ),
            if_(
                eq(v("kind"), int(UNDO_LAST)),
                vec![set_item(v("lasts"), v("a"), v("b"))],
            ),
            if_(
                eq(v("kind"), int(UNDO_LASTINDEX)),
                vec![assign("lastindex", v("b"))],
            ),
        ],
    )
}
