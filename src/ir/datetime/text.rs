//! The text of `datetime`'s values: `_format_time`, `_format_offset`,
//! `strftime` (the C locale's), the ISO 8601 parsing of `fromisoformat`, and
//! `strptime`.

use super::*;

fn i() -> IRType {
    IRType::Int
}
fn s() -> IRType {
    IRType::String
}

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn names(list: &[&str], abbreviated: bool) -> IRExpr {
    IRExpr::ListLiteral(
        list.iter()
            .map(|n| text(if abbreviated { &n[..3] } else { n }))
            .collect(),
    )
}

fn ch(e: IRExpr, at: IRExpr) -> IRExpr {
    idx(e, at)
}

fn digit(c: IRExpr) -> IRExpr {
    and(
        cmp(c.clone(), IRCompareOp::GtE, text("0")),
        cmp(c, IRCompareOp::LtE, text("9")),
    )
}

fn invalid(orig: &str) -> IRStatement {
    raise_expr(
        "ValueError",
        concat(vec![
            text("Invalid isoformat string: "),
            call("repr", vec![v(orig)]),
        ]),
    )
}

pub(super) fn functions() -> Vec<IRFunction> {
    let mut out = Vec::new();
    // `_format_offset(off, sep)`: `+HH<sep>MM`, then `<sep>SS` and
    // `.ffffff` when they are not zero.
    out.push(function(
        "format_offset",
        vec![param("off", &class_ty("timedelta")), param("sep", &s())],
        s(),
        vec![
            assign(
                "t",
                bin(
                    bin(attr(v("off"), "days"), IROp::Mul, int(86400)),
                    IROp::Add,
                    attr(v("off"), "seconds"),
                ),
            ),
            assign("us", attr(v("off"), "microseconds")),
            assign_typed("sign", text("+"), &s()),
            if_(
                cmp(attr(v("off"), "days"), IRCompareOp::Lt, int(0)),
                vec![
                    assign("sign", text("-")),
                    assign("t", neg(v("t"))),
                    assign("us", neg(v("us"))),
                    if_(
                        cmp(v("us"), IRCompareOp::Lt, int(0)),
                        vec![
                            assign("us", bin(v("us"), IROp::Add, int(1000000))),
                            assign("t", bin(v("t"), IROp::Sub, int(1))),
                        ],
                    ),
                ],
            ),
            assign_typed(
                "text",
                concat(vec![
                    v("sign"),
                    pad(floordiv(v("t"), int(3600)), 2),
                    v("sep"),
                    pad(modulo(floordiv(v("t"), int(60)), int(60)), 2),
                ]),
                &s(),
            ),
            if_(
                or(
                    cmp(modulo(v("t"), int(60)), IRCompareOp::NotEq, int(0)),
                    cmp(v("us"), IRCompareOp::NotEq, int(0)),
                ),
                vec![
                    assign(
                        "text",
                        concat(vec![v("text"), v("sep"), pad(modulo(v("t"), int(60)), 2)]),
                    ),
                    if_(
                        cmp(v("us"), IRCompareOp::NotEq, int(0)),
                        vec![assign(
                            "text",
                            concat(vec![v("text"), text("."), pad(v("us"), 6)]),
                        )],
                    ),
                ],
            ),
            ret_val(v("text")),
        ],
    ));
    // `_format_time(hh, mm, ss, us, timespec)`.
    out.push(function(
        "format_time",
        vec![
            param("hh", &i()),
            param("mm", &i()),
            param("ss", &i()),
            param("us", &i()),
            param("timespec", &s()),
        ],
        s(),
        {
            let hms = || {
                concat(vec![
                    pad(v("hh"), 2),
                    text(":"),
                    pad(v("mm"), 2),
                    text(":"),
                    pad(v("ss"), 2),
                ])
            };
            vec![
                assign_typed("spec", v("timespec"), &s()),
                if_(
                    cmp(v("spec"), IRCompareOp::Eq, text("auto")),
                    vec![assign(
                        "spec",
                        ifexp(
                            cmp(v("us"), IRCompareOp::NotEq, int(0)),
                            text("microseconds"),
                            text("seconds"),
                        ),
                    )],
                ),
                if_(
                    cmp(v("spec"), IRCompareOp::Eq, text("hours")),
                    vec![ret_val(pad(v("hh"), 2))],
                ),
                if_(
                    cmp(v("spec"), IRCompareOp::Eq, text("minutes")),
                    vec![ret_val(concat(vec![
                        pad(v("hh"), 2),
                        text(":"),
                        pad(v("mm"), 2),
                    ]))],
                ),
                if_(
                    cmp(v("spec"), IRCompareOp::Eq, text("seconds")),
                    vec![ret_val(hms())],
                ),
                if_(
                    cmp(v("spec"), IRCompareOp::Eq, text("milliseconds")),
                    vec![ret_val(concat(vec![
                        hms(),
                        text("."),
                        pad(floordiv(v("us"), int(1000)), 3),
                    ]))],
                ),
                if_(
                    cmp(v("spec"), IRCompareOp::Eq, text("microseconds")),
                    vec![ret_val(concat(vec![hms(), text("."), pad(v("us"), 6)]))],
                ),
                raise_expr("ValueError", text("Unknown timespec value")),
            ]
        },
    ));
    out.push(strftime());
    out.extend(iso_parsers());
    out.extend(super::time::functions());
    out
}

/// `strftime(fmt)` of the fields, in the C locale, with `%z` and `%Z` given
/// (empty for a naive value).
pub(super) fn strftime() -> IRFunction {
    let d = |name: &str| v(name);
    let put = |e: IRExpr| assign("out", bin(v("out"), IROp::Add, e));
    let directive = |k: &str, body: IRExpr| {
        if_(
            cmp(v("k"), IRCompareOp::Eq, text(k)),
            vec![put(body), IRStatement::Continue],
        )
    };
    let hour12 = || {
        ifexp(
            cmp(modulo(d("hh"), int(12)), IRCompareOp::Eq, int(0)),
            int(12),
            modulo(d("hh"), int(12)),
        )
    };
    // `%e`: the day, padded with a space.
    let day_space = || {
        ifexp(
            cmp(d("d"), IRCompareOp::Lt, int(10)),
            bin(text(" "), IROp::Add, call("str", vec![d("d")])),
            call("str", vec![d("d")]),
        )
    };
    let c_format = || {
        concat(vec![
            idx(names(&WEEKDAYS, true), v("wd")),
            text(" "),
            idx(names(&MONTHS, true), bin(d("mo"), IROp::Sub, int(1))),
            text(" "),
            day_space(),
            text(" "),
            pad(d("hh"), 2),
            text(":"),
            pad(d("mi"), 2),
            text(":"),
            pad(d("ss"), 2),
            text(" "),
            pad(d("y"), 4),
        ])
    };
    let mut loop_body = vec![
        assign_typed("c", ch(v("fmt"), v("i")), &s()),
        assign("i", bin(v("i"), IROp::Add, int(1))),
        if_(
            or(
                cmp(v("c"), IRCompareOp::NotEq, text("%")),
                cmp(v("i"), IRCompareOp::GtE, v("n")),
            ),
            vec![put(v("c")), IRStatement::Continue],
        ),
        assign_typed("k", ch(v("fmt"), v("i")), &s()),
        assign("i", bin(v("i"), IROp::Add, int(1))),
    ];
    // `%:z`, the offset with colons.
    loop_body.push(if_(
        and(
            cmp(v("k"), IRCompareOp::Eq, text(":")),
            and(
                cmp(v("i"), IRCompareOp::Lt, v("n")),
                cmp(ch(v("fmt"), v("i")), IRCompareOp::Eq, text("z")),
            ),
        ),
        vec![
            assign("i", bin(v("i"), IROp::Add, int(1))),
            if_(
                cmp(len(v("utcoff")), IRCompareOp::Gt, int(0)),
                vec![
                    put(concat(vec![
                        slice(v("utcoff"), None, Some(int(3))),
                        text(":"),
                        slice(v("utcoff"), Some(int(3)), Some(int(5))),
                    ])),
                    if_(
                        cmp(len(v("utcoff")), IRCompareOp::Gt, int(5)),
                        vec![put(bin(
                            text(":"),
                            IROp::Add,
                            slice(v("utcoff"), Some(int(5)), None),
                        ))],
                    ),
                ],
            ),
            IRStatement::Continue,
        ],
    ));
    // Sunday-first and Monday-first week numbers, as C computes them.
    let sunday_wd = || modulo(bin(v("wd"), IROp::Add, int(1)), int(7));
    for (k, body) in [
        ("a", idx(names(&WEEKDAYS, true), v("wd"))),
        ("A", idx(names(&WEEKDAYS, false), v("wd"))),
        ("w", call("str", vec![sunday_wd()])),
        ("u", call("str", vec![bin(v("wd"), IROp::Add, int(1))])),
        ("d", pad(d("d"), 2)),
        ("e", day_space()),
        (
            "b",
            idx(names(&MONTHS, true), bin(d("mo"), IROp::Sub, int(1))),
        ),
        (
            "h",
            idx(names(&MONTHS, true), bin(d("mo"), IROp::Sub, int(1))),
        ),
        (
            "B",
            idx(names(&MONTHS, false), bin(d("mo"), IROp::Sub, int(1))),
        ),
        ("m", pad(d("mo"), 2)),
        ("y", pad(modulo(d("y"), int(100)), 2)),
        ("Y", pad(d("y"), 4)),
        ("C", pad(floordiv(d("y"), int(100)), 2)),
        ("H", pad(d("hh"), 2)),
        ("I", pad(hour12(), 2)),
        (
            "p",
            ifexp(
                cmp(d("hh"), IRCompareOp::Lt, int(12)),
                text("AM"),
                text("PM"),
            ),
        ),
        ("M", pad(d("mi"), 2)),
        ("S", pad(d("ss"), 2)),
        ("f", pad(d("us"), 6)),
        ("z", v("utcoff")),
        ("Z", v("zname")),
        ("j", pad(bin(v("yday"), IROp::Add, int(1)), 3)),
        (
            "U",
            pad(
                floordiv(
                    bin(bin(v("yday"), IROp::Add, int(7)), IROp::Sub, sunday_wd()),
                    int(7),
                ),
                2,
            ),
        ),
        (
            "W",
            pad(
                floordiv(
                    bin(bin(v("yday"), IROp::Add, int(7)), IROp::Sub, v("wd")),
                    int(7),
                ),
                2,
            ),
        ),
        ("G", pad(v("isoyear"), 4)),
        ("V", pad(v("isoweek"), 2)),
        ("c", c_format()),
        (
            "x",
            concat(vec![
                pad(d("mo"), 2),
                text("/"),
                pad(d("d"), 2),
                text("/"),
                pad(modulo(d("y"), int(100)), 2),
            ]),
        ),
        (
            "X",
            concat(vec![
                pad(d("hh"), 2),
                text(":"),
                pad(d("mi"), 2),
                text(":"),
                pad(d("ss"), 2),
            ]),
        ),
        (
            "D",
            concat(vec![
                pad(d("mo"), 2),
                text("/"),
                pad(d("d"), 2),
                text("/"),
                pad(modulo(d("y"), int(100)), 2),
            ]),
        ),
        (
            "F",
            concat(vec![
                pad(d("y"), 4),
                text("-"),
                pad(d("mo"), 2),
                text("-"),
                pad(d("d"), 2),
            ]),
        ),
        (
            "T",
            concat(vec![
                pad(d("hh"), 2),
                text(":"),
                pad(d("mi"), 2),
                text(":"),
                pad(d("ss"), 2),
            ]),
        ),
        (
            "R",
            concat(vec![pad(d("hh"), 2), text(":"), pad(d("mi"), 2)]),
        ),
        ("n", text("\n")),
        ("t", text("\t")),
        ("%", text("%")),
    ] {
        loop_body.push(directive(k, body));
    }
    // Anything else is kept as written, as the C library keeps it.
    loop_body.push(put(bin(text("%"), IROp::Add, v("k"))));
    function(
        "strftime",
        vec![
            param("fmt", &s()),
            param("y", &i()),
            param("mo", &i()),
            param("d", &i()),
            param("hh", &i()),
            param("mi", &i()),
            param("ss", &i()),
            param("us", &i()),
            param("utcoff", &s()),
            param("zname", &s()),
        ],
        s(),
        vec![
            assign_typed("out", text(""), &s()),
            assign("ord", fcall("ymd2ord", vec![d("y"), d("mo"), d("d")])),
            assign("wd", modulo(bin(v("ord"), IROp::Add, int(6)), int(7))),
            assign(
                "yday",
                bin(
                    v("ord"),
                    IROp::Sub,
                    fcall("ymd2ord", vec![d("y"), int(1), int(1)]),
                ),
            ),
            // The ISO year and week, for %G and %V.
            assign("isoyear", d("y")),
            assign("w1", fcall("isoweek1monday", vec![v("isoyear")])),
            assign(
                "isoweek",
                floordiv(bin(v("ord"), IROp::Sub, v("w1")), int(7)),
            ),
            if_else(
                cmp(v("isoweek"), IRCompareOp::Lt, int(0)),
                vec![
                    assign("isoyear", bin(v("isoyear"), IROp::Sub, int(1))),
                    assign("w1", fcall("isoweek1monday", vec![v("isoyear")])),
                    assign(
                        "isoweek",
                        floordiv(bin(v("ord"), IROp::Sub, v("w1")), int(7)),
                    ),
                ],
                vec![if_(
                    and(
                        cmp(v("isoweek"), IRCompareOp::GtE, int(52)),
                        cmp(
                            v("ord"),
                            IRCompareOp::GtE,
                            fcall("isoweek1monday", vec![bin(v("isoyear"), IROp::Add, int(1))]),
                        ),
                    ),
                    vec![
                        assign("isoyear", bin(v("isoyear"), IROp::Add, int(1))),
                        assign("isoweek", int(0)),
                    ],
                )],
            ),
            assign("isoweek", bin(v("isoweek"), IROp::Add, int(1))),
            assign("i", int(0)),
            assign("n", len(v("fmt"))),
            while_(cmp(v("i"), IRCompareOp::Lt, v("n")), loop_body),
            ret_val(v("out")),
        ],
    )
}

/// `int(text[a:b])` when every character there is a digit, else -1.
fn digits_at(text_var: &str, a: IRExpr, b: IRExpr) -> IRExpr {
    fcall("digits", vec![v(text_var), a, b])
}

/// The ISO 8601 parsers `fromisoformat` uses: CPython's
/// `_parse_isoformat_date` and `_parse_isoformat_time`.
fn iso_parsers() -> Vec<IRFunction> {
    let tuple3 = IRType::Tuple(vec![i(), i(), i()]);
    vec![
        function(
            "iso_week_ok",
            vec![param("year", &i()), param("week", &i()), param("day", &i())],
            IRType::Bool,
            vec![
                if_(
                    or(
                        cmp(v("year"), IRCompareOp::Lt, int(MINYEAR)),
                        cmp(v("year"), IRCompareOp::Gt, int(MAXYEAR)),
                    ),
                    vec![ret_val(boolean(false))],
                ),
                assign(
                    "first",
                    modulo(
                        bin(
                            fcall("ymd2ord", vec![v("year"), int(1), int(1)]),
                            IROp::Add,
                            int(6),
                        ),
                        int(7),
                    ),
                ),
                assign("last", int(52)),
                if_(
                    or(
                        cmp(v("first"), IRCompareOp::Eq, int(3)),
                        and(
                            cmp(v("first"), IRCompareOp::Eq, int(2)),
                            fcall("is_leap", vec![v("year")]),
                        ),
                    ),
                    vec![assign("last", int(53))],
                ),
                ret_val(and(
                    and(
                        cmp(v("week"), IRCompareOp::GtE, int(1)),
                        cmp(v("week"), IRCompareOp::LtE, v("last")),
                    ),
                    and(
                        cmp(v("day"), IRCompareOp::GtE, int(1)),
                        cmp(v("day"), IRCompareOp::LtE, int(7)),
                    ),
                )),
            ],
        ),
        function(
            "digits",
            vec![param("text", &s()), param("a", &i()), param("b", &i())],
            i(),
            vec![
                if_(
                    or(
                        cmp(v("a"), IRCompareOp::GtE, v("b")),
                        cmp(v("b"), IRCompareOp::Gt, len(v("text"))),
                    ),
                    vec![ret_val(int(-1))],
                ),
                assign("k", v("a")),
                while_(
                    cmp(v("k"), IRCompareOp::Lt, v("b")),
                    vec![
                        if_(not(digit(ch(v("text"), v("k")))), vec![ret_val(int(-1))]),
                        assign("k", bin(v("k"), IROp::Add, int(1))),
                    ],
                ),
                ret_val(call(
                    "int",
                    vec![slice(v("text"), Some(v("a")), Some(v("b")))],
                )),
            ],
        ),
        // A whole date: `YYYY-MM-DD`, `YYYYMMDD`, `YYYY-Www[-D]`, or
        // `YYYYWww[D]`.
        function(
            "parse_isodate",
            vec![param("text", &s()), param("orig", &s())],
            tuple3.clone(),
            vec![
                assign("n", len(v("text"))),
                if_(cmp(v("n"), IRCompareOp::Lt, int(7)), vec![invalid("orig")]),
                assign("year", digits_at("text", int(0), int(4))),
                if_(
                    cmp(v("year"), IRCompareOp::Lt, int(0)),
                    vec![invalid("orig")],
                ),
                assign_typed("dash", ch(v("text"), int(4)), &s()),
                assign(
                    "p",
                    ifexp(cmp(v("dash"), IRCompareOp::Eq, text("-")), int(5), int(4)),
                ),
                // An ISO week date.
                if_(
                    cmp(ch(v("text"), v("p")), IRCompareOp::Eq, text("W")),
                    vec![
                        assign(
                            "week",
                            digits_at(
                                "text",
                                bin(v("p"), IROp::Add, int(1)),
                                bin(v("p"), IROp::Add, int(3)),
                            ),
                        ),
                        if_(
                            cmp(v("week"), IRCompareOp::Lt, int(0)),
                            vec![invalid("orig")],
                        ),
                        assign("day", int(1)),
                        assign("q", bin(v("p"), IROp::Add, int(3))),
                        if_(
                            cmp(v("q"), IRCompareOp::Lt, v("n")),
                            vec![
                                if_(
                                    cmp(v("dash"), IRCompareOp::Eq, text("-")),
                                    vec![
                                        if_(
                                            cmp(
                                                ch(v("text"), v("q")),
                                                IRCompareOp::NotEq,
                                                text("-"),
                                            ),
                                            vec![invalid("orig")],
                                        ),
                                        assign("q", bin(v("q"), IROp::Add, int(1))),
                                    ],
                                ),
                                assign(
                                    "day",
                                    digits_at("text", v("q"), bin(v("q"), IROp::Add, int(1))),
                                ),
                                if_(
                                    cmp(v("day"), IRCompareOp::Lt, int(0)),
                                    vec![invalid("orig")],
                                ),
                                assign("q", bin(v("q"), IROp::Add, int(1))),
                            ],
                        ),
                        if_(
                            cmp(v("q"), IRCompareOp::NotEq, v("n")),
                            vec![invalid("orig")],
                        ),
                        // A week date that does not exist is an invalid
                        // string here, where fromisocalendar() says why.
                        if_(
                            not(fcall("iso_week_ok", vec![v("year"), v("week"), v("day")])),
                            vec![invalid("orig")],
                        ),
                        assign_typed(
                            "found",
                            mcall(
                                v(&class_name("date")),
                                "fromisocalendar",
                                vec![v("year"), v("week"), v("day")],
                            ),
                            &class_ty("date"),
                        ),
                        ret_val(IRExpr::TupleLiteral(vec![
                            attr(v("found"), "year"),
                            attr(v("found"), "month"),
                            attr(v("found"), "day"),
                        ])),
                    ],
                ),
                assign(
                    "month",
                    digits_at("text", v("p"), bin(v("p"), IROp::Add, int(2))),
                ),
                assign("q", bin(v("p"), IROp::Add, int(2))),
                if_(
                    cmp(v("dash"), IRCompareOp::Eq, text("-")),
                    vec![
                        if_(
                            or(
                                cmp(v("q"), IRCompareOp::GtE, v("n")),
                                cmp(ch(v("text"), v("q")), IRCompareOp::NotEq, text("-")),
                            ),
                            vec![invalid("orig")],
                        ),
                        assign("q", bin(v("q"), IROp::Add, int(1))),
                    ],
                ),
                assign(
                    "day",
                    digits_at("text", v("q"), bin(v("q"), IROp::Add, int(2))),
                ),
                if_(
                    or(
                        or(
                            cmp(v("month"), IRCompareOp::Lt, int(0)),
                            cmp(v("day"), IRCompareOp::Lt, int(0)),
                        ),
                        cmp(bin(v("q"), IROp::Add, int(2)), IRCompareOp::NotEq, v("n")),
                    ),
                    vec![invalid("orig")],
                ),
                expr_stmt(fcall("check_date", vec![v("year"), v("month"), v("day")])),
                ret_val(IRExpr::TupleLiteral(vec![v("year"), v("month"), v("day")])),
            ],
        ),
        // A whole time: `HH[:MM[:SS[.fff[fff]]]]` (or without the colons),
        // then `Z` or `+HH:MM[:SS[.ffffff]]`. The fields, then whether an
        // offset was given, and the offset in microseconds.
        function(
            "parse_isotime",
            vec![param("text", &s()), param("orig", &s())],
            IRType::Tuple(vec![i(), i(), i(), i(), i(), IRType::Float]),
            vec![
                assign("n", len(v("text"))),
                // Where the offset starts.
                assign("cut", v("n")),
                assign("k", int(0)),
                while_(
                    cmp(v("k"), IRCompareOp::Lt, v("n")),
                    vec![
                        assign_typed("c", ch(v("text"), v("k")), &s()),
                        if_(
                            or(
                                or(
                                    cmp(v("c"), IRCompareOp::Eq, text("+")),
                                    cmp(v("c"), IRCompareOp::Eq, text("-")),
                                ),
                                cmp(v("c"), IRCompareOp::Eq, text("Z")),
                            ),
                            vec![assign("cut", v("k")), IRStatement::Break],
                        ),
                        assign("k", bin(v("k"), IROp::Add, int(1))),
                    ],
                ),
                assign_typed(
                    "fields",
                    fcall(
                        "parse_hms",
                        vec![slice(v("text"), None, Some(v("cut"))), v("orig")],
                    ),
                    &IRType::Tuple(vec![i(), i(), i(), i()]),
                ),
                assign("aware", int(0)),
                assign_typed("off", float(0.0), &IRType::Float),
                if_(
                    cmp(v("cut"), IRCompareOp::Lt, v("n")),
                    vec![
                        assign("aware", int(1)),
                        if_else(
                            cmp(ch(v("text"), v("cut")), IRCompareOp::Eq, text("Z")),
                            vec![if_(
                                cmp(bin(v("cut"), IROp::Add, int(1)), IRCompareOp::NotEq, v("n")),
                                vec![invalid("orig")],
                            )],
                            vec![
                                assign_typed(
                                    "tzf",
                                    fcall(
                                        "parse_hms",
                                        vec![
                                            slice(
                                                v("text"),
                                                Some(bin(v("cut"), IROp::Add, int(1))),
                                                None,
                                            ),
                                            v("orig"),
                                        ],
                                    ),
                                    &IRType::Tuple(vec![i(), i(), i(), i()]),
                                ),
                                assign(
                                    "off",
                                    bin(
                                        bin(
                                            call(
                                                "float",
                                                vec![bin(
                                                    bin(
                                                        bin(
                                                            idx(v("tzf"), int(0)),
                                                            IROp::Mul,
                                                            int(3600),
                                                        ),
                                                        IROp::Add,
                                                        bin(
                                                            idx(v("tzf"), int(1)),
                                                            IROp::Mul,
                                                            int(60),
                                                        ),
                                                    ),
                                                    IROp::Add,
                                                    idx(v("tzf"), int(2)),
                                                )],
                                            ),
                                            IROp::Mul,
                                            float(1e6),
                                        ),
                                        IROp::Add,
                                        call("float", vec![idx(v("tzf"), int(3))]),
                                    ),
                                ),
                                if_(
                                    cmp(ch(v("text"), v("cut")), IRCompareOp::Eq, text("-")),
                                    vec![assign("off", neg(v("off")))],
                                ),
                            ],
                        ),
                    ],
                ),
                ret_val(IRExpr::TupleLiteral(vec![
                    idx(v("fields"), int(0)),
                    idx(v("fields"), int(1)),
                    idx(v("fields"), int(2)),
                    idx(v("fields"), int(3)),
                    v("aware"),
                    v("off"),
                ])),
            ],
        ),
        // CPython's `_parse_hh_mm_ss_ff`: hours, then minutes and seconds,
        // with or without colons, then a fraction of 1 to 6 (or more, the
        // rest dropped) digits after `.` or `,`.
        function(
            "parse_hms",
            vec![param("text", &s()), param("orig", &s())],
            IRType::Tuple(vec![i(), i(), i(), i()]),
            vec![
                assign("n", len(v("text"))),
                assign_typed(
                    "comps",
                    IRExpr::ListLiteral(vec![int(0), int(0), int(0), int(0)]),
                    &IRType::List(Box::new(i())),
                ),
                assign("pos", int(0)),
                assign("comp", int(0)),
                assign("colons", int(-1)),
                while_(
                    cmp(v("comp"), IRCompareOp::Lt, int(3)),
                    vec![
                        assign(
                            "value",
                            digits_at("text", v("pos"), bin(v("pos"), IROp::Add, int(2))),
                        ),
                        if_(
                            cmp(v("value"), IRCompareOp::Lt, int(0)),
                            vec![invalid("orig")],
                        ),
                        set_item(v("comps"), v("comp"), v("value")),
                        assign("pos", bin(v("pos"), IROp::Add, int(2))),
                        assign("comp", bin(v("comp"), IROp::Add, int(1))),
                        if_(
                            or(
                                cmp(v("pos"), IRCompareOp::GtE, v("n")),
                                cmp(v("comp"), IRCompareOp::GtE, int(3)),
                            ),
                            vec![IRStatement::Break],
                        ),
                        assign_typed("sep", ch(v("text"), v("pos")), &s()),
                        if_(
                            or(
                                cmp(v("sep"), IRCompareOp::Eq, text(".")),
                                cmp(v("sep"), IRCompareOp::Eq, text(",")),
                            ),
                            vec![IRStatement::Break],
                        ),
                        assign(
                            "has",
                            ifexp(cmp(v("sep"), IRCompareOp::Eq, text(":")), int(1), int(0)),
                        ),
                        if_(
                            cmp(v("colons"), IRCompareOp::Lt, int(0)),
                            vec![assign("colons", v("has"))],
                        ),
                        if_(
                            cmp(v("colons"), IRCompareOp::NotEq, v("has")),
                            vec![invalid("orig")],
                        ),
                        if_(
                            cmp(v("has"), IRCompareOp::Eq, int(1)),
                            vec![assign("pos", bin(v("pos"), IROp::Add, int(1)))],
                        ),
                    ],
                ),
                if_(
                    cmp(v("pos"), IRCompareOp::Lt, v("n")),
                    vec![
                        assign_typed("sep", ch(v("text"), v("pos")), &s()),
                        if_(
                            or(
                                cmp(v("comp"), IRCompareOp::LtE, int(2)),
                                and(
                                    cmp(v("sep"), IRCompareOp::NotEq, text(".")),
                                    cmp(v("sep"), IRCompareOp::NotEq, text(",")),
                                ),
                            ),
                            vec![invalid("orig")],
                        ),
                        assign("pos", bin(v("pos"), IROp::Add, int(1))),
                        assign("width", bin(v("n"), IROp::Sub, v("pos"))),
                        if_(
                            cmp(v("width"), IRCompareOp::Gt, int(6)),
                            vec![assign("width", int(6))],
                        ),
                        assign(
                            "frac",
                            digits_at("text", v("pos"), bin(v("pos"), IROp::Add, v("width"))),
                        ),
                        if_(
                            cmp(v("frac"), IRCompareOp::Lt, int(0)),
                            vec![invalid("orig")],
                        ),
                        assign("scale", v("width")),
                        while_(
                            cmp(v("scale"), IRCompareOp::Lt, int(6)),
                            vec![
                                assign("frac", bin(v("frac"), IROp::Mul, int(10))),
                                assign("scale", bin(v("scale"), IROp::Add, int(1))),
                            ],
                        ),
                        set_item(v("comps"), int(3), v("frac")),
                        assign("rest", bin(v("pos"), IROp::Add, v("width"))),
                        if_(
                            and(
                                cmp(v("rest"), IRCompareOp::Lt, v("n")),
                                cmp(
                                    digits_at("text", v("rest"), v("n")),
                                    IRCompareOp::Lt,
                                    int(0),
                                ),
                            ),
                            vec![invalid("orig")],
                        ),
                    ],
                ),
                ret_val(IRExpr::TupleLiteral(vec![
                    idx(v("comps"), int(0)),
                    idx(v("comps"), int(1)),
                    idx(v("comps"), int(2)),
                    idx(v("comps"), int(3)),
                ])),
            ],
        ),
    ]
}
