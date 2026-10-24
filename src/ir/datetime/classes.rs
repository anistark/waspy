//! The `datetime` classes: `timedelta`, `date`, `time`, `datetime`,
//! `timezone`, and the `IsoCalendarDate` named tuple.

use super::*;

fn i() -> IRType {
    IRType::Int
}
fn f() -> IRType {
    IRType::Float
}
fn s() -> IRType {
    IRType::String
}
fn td() -> IRType {
    class_ty("timedelta")
}

pub(super) fn field(object: &str, name: &str) -> IRExpr {
    attr(v(object), name)
}

pub(super) fn str_of(e: IRExpr) -> IRExpr {
    call("str", vec![e])
}

/// `repr(e)`.
pub(super) fn repr_of(e: IRExpr) -> IRExpr {
    call("repr", vec![e])
}

fn raise(class: &str, message: IRExpr) -> IRStatement {
    raise_expr(class, message)
}

/// The total microseconds of the timedelta `e`, as an exact float.
pub(super) fn micros(e: IRExpr) -> IRExpr {
    bin(
        bin(
            bin(
                bin(attr(e.clone(), "days"), IROp::Mul, float(86400.0)),
                IROp::Add,
                attr(e.clone(), "seconds"),
            ),
            IROp::Mul,
            float(1e6),
        ),
        IROp::Add,
        attr(e, "microseconds"),
    )
}

/// `timedelta(microseconds=us)`.
pub(super) fn td_of_micros(us: IRExpr) -> IRExpr {
    make("timedelta", vec![float(0.0), float(0.0), us])
}

pub(super) fn all(layout: &mut crate::ir::MemoryLayout) -> Result<Vec<IRClass>> {
    let iso = crate::ir::collections::namedtuple_class(
        "datetime.IsoCalendarDate",
        "datetime.IsoCalendarDate",
        vec![
            crate::ir::collections::TupleField {
                name: "year".to_string(),
                ty: i(),
                default: None,
            },
            crate::ir::collections::TupleField {
                name: "week".to_string(),
                ty: i(),
                default: None,
            },
            crate::ir::collections::TupleField {
                name: "weekday".to_string(),
                ty: i(),
                default: None,
            },
        ],
        Vec::new(),
        Vec::new(),
        layout,
    )?;
    Ok(vec![
        timedelta(),
        date(),
        super::time::time_class(),
        super::time::datetime_class(),
        super::time::timezone_class(),
        iso,
    ])
}

// ---------------------------------------------------------------------------
// timedelta
// ---------------------------------------------------------------------------

fn timedelta() -> IRClass {
    let me_td = || me("timedelta");
    let other_td = || param("other", &td());
    let fparam = |n: &str| param_default(n, &f(), float(0.0));
    let mut m = Vec::new();
    // CPython's normalization (`_pydatetime.timedelta.__new__`), in floats,
    // which hold every value it reaches exactly: days, seconds, and
    // microseconds, with 0 <= seconds < 86400 and 0 <= microseconds < 10**6.
    let trunc = |e: IRExpr| fcall("trunc", vec![e]);
    m.push(method(
        "__init__",
        vec![
            me_td(),
            fparam("days"),
            fparam("seconds"),
            fparam("microseconds"),
            fparam("milliseconds"),
            fparam("minutes"),
            fparam("hours"),
            fparam("weeks"),
        ],
        IRType::None,
        vec![
            assign_typed(
                "d",
                bin(v("days"), IROp::Add, bin(v("weeks"), IROp::Mul, float(7.0))),
                &f(),
            ),
            assign_typed(
                "s",
                bin(
                    bin(
                        v("seconds"),
                        IROp::Add,
                        bin(v("minutes"), IROp::Mul, float(60.0)),
                    ),
                    IROp::Add,
                    bin(v("hours"), IROp::Mul, float(3600.0)),
                ),
                &f(),
            ),
            assign_typed(
                "us",
                bin(
                    v("microseconds"),
                    IROp::Add,
                    bin(v("milliseconds"), IROp::Mul, float(1000.0)),
                ),
                &f(),
            ),
            // A fraction of a day is seconds, and a fraction of a second
            // microseconds.
            assign_typed("dwhole", trunc(v("d")), &f()),
            assign_typed(
                "dsec",
                bin(
                    bin(v("d"), IROp::Sub, v("dwhole")),
                    IROp::Mul,
                    float(86400.0),
                ),
                &f(),
            ),
            assign_typed("dsecwhole", trunc(v("dsec")), &f()),
            assign_typed("secfrac", bin(v("dsec"), IROp::Sub, v("dsecwhole")), &f()),
            assign_typed("swhole", trunc(v("s")), &f()),
            assign(
                "secfrac",
                bin(v("secfrac"), IROp::Add, bin(v("s"), IROp::Sub, v("swhole"))),
            ),
            assign_typed("dd", v("dwhole"), &f()),
            assign_typed("ss", v("dsecwhole"), &f()),
            assign(
                "dd",
                bin(v("dd"), IROp::Add, floordiv(v("swhole"), float(86400.0))),
            ),
            assign(
                "ss",
                bin(v("ss"), IROp::Add, modulo(v("swhole"), float(86400.0))),
            ),
            assign_typed(
                "uu",
                fcall(
                    "round_even",
                    vec![bin(
                        v("us"),
                        IROp::Add,
                        bin(v("secfrac"), IROp::Mul, float(1e6)),
                    )],
                ),
                &f(),
            ),
            assign("ss", bin(v("ss"), IROp::Add, floordiv(v("uu"), float(1e6)))),
            assign("uu", modulo(v("uu"), float(1e6))),
            assign(
                "dd",
                bin(v("dd"), IROp::Add, floordiv(v("ss"), float(86400.0))),
            ),
            assign("ss", modulo(v("ss"), float(86400.0))),
            if_(
                or(
                    cmp(v("dd"), IRCompareOp::Gt, float(999999999.0)),
                    cmp(v("dd"), IRCompareOp::Lt, float(-999999999.0)),
                ),
                vec![raise(
                    "OverflowError",
                    concat(vec![
                        text("days="),
                        call("format", vec![v("dd"), text(".0f")]),
                        text("; must have magnitude <= 999999999"),
                    ]),
                )],
            ),
            set_my_typed("days", call("int", vec![v("dd")]), &i()),
            set_my_typed("seconds", call("int", vec![v("ss")]), &i()),
            set_my_typed("microseconds", call("int", vec![v("uu")]), &i()),
        ],
    ));
    m.push(method(
        "total_seconds",
        vec![me_td()],
        f(),
        vec![ret_val(bin(micros(v("self")), IROp::Div, float(1e6)))],
    ));
    m.push(method(
        "__repr__",
        vec![me_td()],
        s(),
        vec![
            assign_typed(
                "parts",
                IRExpr::ListLiteral(Vec::new()),
                &IRType::List(Box::new(s())),
            ),
            if_(
                cmp(my("days"), IRCompareOp::NotEq, int(0)),
                vec![expr_stmt(mcall(
                    v("parts"),
                    "append",
                    vec![bin(text("days="), IROp::Add, str_of(my("days")))],
                ))],
            ),
            if_(
                cmp(my("seconds"), IRCompareOp::NotEq, int(0)),
                vec![expr_stmt(mcall(
                    v("parts"),
                    "append",
                    vec![bin(text("seconds="), IROp::Add, str_of(my("seconds")))],
                ))],
            ),
            if_(
                cmp(my("microseconds"), IRCompareOp::NotEq, int(0)),
                vec![expr_stmt(mcall(
                    v("parts"),
                    "append",
                    vec![bin(
                        text("microseconds="),
                        IROp::Add,
                        str_of(my("microseconds")),
                    )],
                ))],
            ),
            if_(
                cmp(len(v("parts")), IRCompareOp::Eq, int(0)),
                vec![ret_val(text("datetime.timedelta(0)"))],
            ),
            ret_val(concat(vec![
                text("datetime.timedelta("),
                mcall(text(", "), "join", vec![v("parts")]),
                text(")"),
            ])),
        ],
    ));
    m.push(method(
        "__str__",
        vec![me_td()],
        s(),
        vec![
            assign("mm", floordiv(my("seconds"), int(60))),
            assign("ss", modulo(my("seconds"), int(60))),
            assign("hh", floordiv(v("mm"), int(60))),
            assign("mm", modulo(v("mm"), int(60))),
            assign_typed(
                "out",
                concat(vec![
                    str_of(v("hh")),
                    text(":"),
                    pad(v("mm"), 2),
                    text(":"),
                    pad(v("ss"), 2),
                ]),
                &s(),
            ),
            if_(
                cmp(my("days"), IRCompareOp::NotEq, int(0)),
                vec![assign(
                    "out",
                    concat(vec![
                        str_of(my("days")),
                        ifexp(
                            or(
                                cmp(my("days"), IRCompareOp::Eq, int(1)),
                                cmp(my("days"), IRCompareOp::Eq, int(-1)),
                            ),
                            text(" day, "),
                            text(" days, "),
                        ),
                        v("out"),
                    ]),
                )],
            ),
            if_(
                cmp(my("microseconds"), IRCompareOp::NotEq, int(0)),
                vec![assign(
                    "out",
                    concat(vec![v("out"), text("."), pad(my("microseconds"), 6)]),
                )],
            ),
            ret_val(v("out")),
        ],
    ));
    let td_of = |d: IRExpr, s: IRExpr, us: IRExpr| {
        make(
            "timedelta",
            vec![
                call("float", vec![d]),
                call("float", vec![s]),
                call("float", vec![us]),
            ],
        )
    };
    for (name, op) in [("__add__", IROp::Add), ("__sub__", IROp::Sub)] {
        let n = if name == "__sub__" {
            spec_variant("__sub__", &td())
        } else {
            name.to_string()
        };
        m.push(method(
            &n,
            vec![me_td(), other_td()],
            td(),
            vec![ret_val(td_of(
                bin(my("days"), op.clone(), field("other", "days")),
                bin(my("seconds"), op.clone(), field("other", "seconds")),
                bin(my("microseconds"), op, field("other", "microseconds")),
            ))],
        ));
    }
    m.push(method(
        "__neg__",
        vec![me_td()],
        td(),
        vec![ret_val(td_of(
            neg(my("days")),
            neg(my("seconds")),
            neg(my("microseconds")),
        ))],
    ));
    m.push(method(
        "__pos__",
        vec![me_td()],
        td(),
        vec![ret_val(v("self"))],
    ));
    m.push(method(
        "__abs__",
        vec![me_td()],
        td(),
        vec![
            if_(
                cmp(my("days"), IRCompareOp::Lt, int(0)),
                vec![ret_val(neg(v("self")))],
            ),
            ret_val(v("self")),
        ],
    ));
    // `td * n`, `n * td`: each part scaled, and normalized again.
    let scaled = |by: IRExpr| {
        make(
            "timedelta",
            vec![
                bin(call("float", vec![my("days")]), IROp::Mul, by.clone()),
                bin(call("float", vec![my("seconds")]), IROp::Mul, by.clone()),
                bin(call("float", vec![my("microseconds")]), IROp::Mul, by),
            ],
        )
    };
    for name in ["__mul__", "__rmul__"] {
        m.push(method(
            name,
            vec![me_td(), param("other", &f())],
            td(),
            vec![ret_val(scaled(v("other")))],
        ));
    }
    let zero_check = |what: &str, message: &str| {
        if_(
            cmp(v(what), IRCompareOp::Eq, float(0.0)),
            vec![raise("ZeroDivisionError", text(message))],
        )
    };
    m.push(method(
        &spec_variant("__truediv__", &td()),
        vec![me_td(), other_td()],
        f(),
        vec![
            assign_typed("b", micros(v("other")), &f()),
            zero_check("b", "division by zero"),
            ret_val(bin(micros(v("self")), IROp::Div, v("b"))),
        ],
    ));
    m.push(method(
        &spec_variant("__truediv__", &f()),
        vec![me_td(), param("other", &f())],
        td(),
        vec![
            zero_check("other", "integer division or modulo by zero"),
            ret_val(td_of_micros(bin(micros(v("self")), IROp::Div, v("other")))),
        ],
    ));
    m.push(method(
        &spec_variant("__floordiv__", &td()),
        vec![me_td(), other_td()],
        i(),
        vec![
            assign_typed("b", micros(v("other")), &f()),
            zero_check("b", "integer division or modulo by zero"),
            ret_val(call("int", vec![floordiv(micros(v("self")), v("b"))])),
        ],
    ));
    m.push(method(
        &spec_variant("__floordiv__", &f()),
        vec![me_td(), param("other", &f())],
        td(),
        vec![
            zero_check("other", "integer division or modulo by zero"),
            ret_val(td_of_micros(floordiv(micros(v("self")), v("other")))),
        ],
    ));
    m.push(method(
        "__mod__",
        vec![me_td(), other_td()],
        td(),
        vec![
            assign_typed("b", micros(v("other")), &f()),
            zero_check("b", "integer modulo by zero"),
            ret_val(td_of_micros(modulo(micros(v("self")), v("b")))),
        ],
    ));
    let key = |o: &str| {
        IRExpr::TupleLiteral(vec![
            field(o, "days"),
            field(o, "seconds"),
            field(o, "microseconds"),
        ])
    };
    for (name, op) in [
        ("__eq__", IRCompareOp::Eq),
        ("__lt__", IRCompareOp::Lt),
        ("__le__", IRCompareOp::LtE),
        ("__gt__", IRCompareOp::Gt),
        ("__ge__", IRCompareOp::GtE),
    ] {
        m.push(method(
            name,
            vec![me_td(), other_td()],
            IRType::Bool,
            vec![ret_val(cmp(key("self"), op, key("other")))],
        ));
    }
    m.push(method(
        "__bool__",
        vec![me_td()],
        IRType::Bool,
        vec![ret_val(or(
            or(
                cmp(my("days"), IRCompareOp::NotEq, int(0)),
                cmp(my("seconds"), IRCompareOp::NotEq, int(0)),
            ),
            cmp(my("microseconds"), IRCompareOp::NotEq, int(0)),
        ))],
    ));
    class(
        "timedelta",
        Vec::new(),
        m,
        vec![
            class_var("min", make("timedelta", vec![float(-999999999.0)])),
            class_var(
                "max",
                make(
                    "timedelta",
                    vec![float(999999999.0), float(86399.0), float(999999.0)],
                ),
            ),
            class_var(
                "resolution",
                make("timedelta", vec![float(0.0), float(0.0), float(1.0)]),
            ),
        ],
    )
}

pub(super) fn class_var(name: &str, value: IRExpr) -> IRVariable {
    IRVariable {
        name: name.to_string(),
        value,
        var_type: None,
    }
}

pub(super) fn spec_variant(base: &str, ty: &IRType) -> String {
    crate::ir::collections::spec::variant(base, ty).expect("a spelled type")
}

// ---------------------------------------------------------------------------
// date
// ---------------------------------------------------------------------------

/// The parts of a date that a datetime has too: its fields' methods, in
/// terms of `self.year`, `self.month`, `self.day`.
pub(super) fn date_methods(short: &str) -> Vec<IRFunction> {
    let me_d = || me(short);
    let ord = || fcall("ymd2ord", vec![my("year"), my("month"), my("day")]);
    vec![
        method("toordinal", vec![me_d()], i(), vec![ret_val(ord())]),
        method(
            "weekday",
            vec![me_d()],
            i(),
            vec![ret_val(modulo(bin(ord(), IROp::Add, int(6)), int(7)))],
        ),
        method(
            "isoweekday",
            vec![me_d()],
            i(),
            vec![
                assign("w", modulo(ord(), int(7))),
                if_(cmp(v("w"), IRCompareOp::Eq, int(0)), vec![ret_val(int(7))]),
                ret_val(v("w")),
            ],
        ),
        method(
            "isocalendar",
            vec![me_d()],
            IRType::Class("datetime.IsoCalendarDate".to_string()),
            vec![
                assign("year", my("year")),
                assign("week1monday", fcall("isoweek1monday", vec![v("year")])),
                assign("today", ord()),
                assign(
                    "week",
                    floordiv(bin(v("today"), IROp::Sub, v("week1monday")), int(7)),
                ),
                assign(
                    "day",
                    modulo(bin(v("today"), IROp::Sub, v("week1monday")), int(7)),
                ),
                if_else(
                    cmp(v("week"), IRCompareOp::Lt, int(0)),
                    vec![
                        assign("year", bin(v("year"), IROp::Sub, int(1))),
                        assign("week1monday", fcall("isoweek1monday", vec![v("year")])),
                        assign(
                            "week",
                            floordiv(bin(v("today"), IROp::Sub, v("week1monday")), int(7)),
                        ),
                        assign(
                            "day",
                            modulo(bin(v("today"), IROp::Sub, v("week1monday")), int(7)),
                        ),
                    ],
                    vec![if_(
                        and(
                            cmp(v("week"), IRCompareOp::GtE, int(52)),
                            cmp(
                                v("today"),
                                IRCompareOp::GtE,
                                fcall("isoweek1monday", vec![bin(v("year"), IROp::Add, int(1))]),
                            ),
                        ),
                        vec![
                            assign("year", bin(v("year"), IROp::Add, int(1))),
                            assign("week", int(0)),
                        ],
                    )],
                ),
                ret_val(call(
                    "datetime.IsoCalendarDate",
                    vec![
                        v("year"),
                        bin(v("week"), IROp::Add, int(1)),
                        bin(v("day"), IROp::Add, int(1)),
                    ],
                )),
            ],
        ),
    ]
}

/// `date + timedelta`'s ordinal arithmetic: the date `days` after `ordinal`,
/// or CPython's OverflowError.
fn date_from_ordinal_checked(ordinal: IRExpr) -> Vec<IRStatement> {
    vec![
        assign("o", ordinal),
        if_(
            or(
                cmp(v("o"), IRCompareOp::Lt, int(1)),
                cmp(v("o"), IRCompareOp::Gt, int(MAX_ORDINAL)),
            ),
            vec![raise("OverflowError", text("date value out of range"))],
        ),
        ret_val(mcall(v(&class_name("date")), "fromordinal", vec![v("o")])),
    ]
}

fn date() -> IRClass {
    let me_d = || me("date");
    let dt = class_ty("date");
    let mut m = vec![method(
        "__init__",
        vec![
            me_d(),
            param("year", &i()),
            param("month", &i()),
            param("day", &i()),
        ],
        IRType::None,
        vec![
            expr_stmt(fcall("check_date", vec![v("year"), v("month"), v("day")])),
            set_my_typed("year", v("year"), &i()),
            set_my_typed("month", v("month"), &i()),
            set_my_typed("day", v("day"), &i()),
        ],
    )];
    m.extend(date_methods("date"));
    m.push(classmethod(method(
        "fromordinal",
        vec![cls("date"), param("n", &i())],
        dt.clone(),
        vec![
            if_(
                cmp(v("n"), IRCompareOp::Lt, int(1)),
                vec![raise("ValueError", text("ordinal must be >= 1"))],
            ),
            if_(
                cmp(v("n"), IRCompareOp::Gt, int(MAX_ORDINAL)),
                vec![raise(
                    "ValueError",
                    concat(vec![
                        text("year "),
                        str_of(int(MAXYEAR + 1)),
                        text(" is out of range"),
                    ]),
                )],
            ),
            IRStatement::TupleUnpack {
                targets: vec!["y".into(), "mo".into(), "d".into()],
                value: fcall("ord2ymd", vec![v("n")]),
                starred: None,
            },
            ret_val(make("date", vec![v("y"), v("mo"), v("d")])),
        ],
    )));
    m.push(classmethod(method(
        "today",
        vec![cls("date")],
        dt.clone(),
        vec![
            assign_typed("us", call(crate::ir::DATETIME_CLOCK_FN, Vec::new()), &f()),
            ret_val(mcall(
                v(&class_name("date")),
                "fromordinal",
                vec![bin(
                    call("int", vec![floordiv(v("us"), float(86400e6))]),
                    IROp::Add,
                    int(EPOCH_ORDINAL),
                )],
            )),
        ],
    )));
    m.push(classmethod(method(
        "fromtimestamp",
        vec![cls("date"), param("t", &f())],
        dt.clone(),
        vec![ret_val(mcall(
            v(&class_name("date")),
            "fromordinal",
            vec![bin(
                call("int", vec![floordiv(v("t"), float(86400.0))]),
                IROp::Add,
                int(EPOCH_ORDINAL),
            )],
        ))],
    )));
    m.push(classmethod(method(
        "fromisocalendar",
        vec![
            cls("date"),
            param("year", &i()),
            param("week", &i()),
            param("day", &i()),
        ],
        dt.clone(),
        vec![
            if_(
                or(
                    cmp(v("year"), IRCompareOp::Lt, int(MINYEAR)),
                    cmp(v("year"), IRCompareOp::Gt, int(MAXYEAR)),
                ),
                vec![raise(
                    "ValueError",
                    concat(vec![text("Year is out of range: "), str_of(v("year"))]),
                )],
            ),
            assign("last", int(52)),
            assign(
                "first_weekday",
                modulo(
                    bin(
                        fcall("ymd2ord", vec![v("year"), int(1), int(1)]),
                        IROp::Add,
                        int(6),
                    ),
                    int(7),
                ),
            ),
            if_(
                or(
                    cmp(v("first_weekday"), IRCompareOp::Eq, int(3)),
                    and(
                        cmp(v("first_weekday"), IRCompareOp::Eq, int(2)),
                        fcall("is_leap", vec![v("year")]),
                    ),
                ),
                vec![assign("last", int(53))],
            ),
            if_(
                or(
                    cmp(v("week"), IRCompareOp::Lt, int(1)),
                    cmp(v("week"), IRCompareOp::Gt, v("last")),
                ),
                vec![raise(
                    "ValueError",
                    concat(vec![text("Invalid week: "), str_of(v("week"))]),
                )],
            ),
            if_(
                or(
                    cmp(v("day"), IRCompareOp::Lt, int(1)),
                    cmp(v("day"), IRCompareOp::Gt, int(7)),
                ),
                vec![raise(
                    "ValueError",
                    concat(vec![
                        text("Invalid day: "),
                        str_of(v("day")),
                        text(" (range is [1, 7])"),
                    ]),
                )],
            ),
            assign(
                "o",
                bin(
                    bin(
                        fcall("isoweek1monday", vec![v("year")]),
                        IROp::Add,
                        bin(bin(v("week"), IROp::Sub, int(1)), IROp::Mul, int(7)),
                    ),
                    IROp::Add,
                    bin(v("day"), IROp::Sub, int(1)),
                ),
            ),
            ret_val(mcall(v(&class_name("date")), "fromordinal", vec![v("o")])),
        ],
    )));
    m.push(classmethod(method(
        "fromisoformat",
        vec![cls("date"), param("text", &s())],
        dt.clone(),
        vec![
            assign_typed(
                "ymd",
                fcall("parse_isodate", vec![v("text"), v("text")]),
                &IRType::Tuple(vec![i(), i(), i()]),
            ),
            ret_val(make(
                "date",
                vec![
                    idx(v("ymd"), int(0)),
                    idx(v("ymd"), int(1)),
                    idx(v("ymd"), int(2)),
                ],
            )),
        ],
    )));
    m.push(method(
        "isoformat",
        vec![me_d()],
        s(),
        vec![ret_val(concat(vec![
            pad(my("year"), 4),
            text("-"),
            pad(my("month"), 2),
            text("-"),
            pad(my("day"), 2),
        ]))],
    ));
    m.push(method(
        "__str__",
        vec![me_d()],
        s(),
        vec![ret_val(mcall(v("self"), "isoformat", Vec::new()))],
    ));
    m.push(method(
        "__repr__",
        vec![me_d()],
        s(),
        vec![ret_val(concat(vec![
            text("datetime.date("),
            str_of(my("year")),
            text(", "),
            str_of(my("month")),
            text(", "),
            str_of(my("day")),
            text(")"),
        ]))],
    ));
    m.push(method(
        "ctime",
        vec![me_d()],
        s(),
        vec![ret_val(fcall(
            "strftime",
            vec![
                text("%c"),
                my("year"),
                my("month"),
                my("day"),
                int(0),
                int(0),
                int(0),
                int(0),
                text(""),
                text(""),
            ],
        ))],
    ));
    m.push(method(
        "strftime",
        vec![me_d(), param("fmt", &s())],
        s(),
        vec![ret_val(fcall(
            "strftime",
            vec![
                v("fmt"),
                my("year"),
                my("month"),
                my("day"),
                int(0),
                int(0),
                int(0),
                int(0),
                text(""),
                text(""),
            ],
        ))],
    ));
    m.push(method(
        "__format__",
        vec![me_d(), param("fmt", &s())],
        s(),
        vec![
            if_(
                cmp(len(v("fmt")), IRCompareOp::Eq, int(0)),
                vec![ret_val(str_of(v("self")))],
            ),
            ret_val(mcall(v("self"), "strftime", vec![v("fmt")])),
        ],
    ));
    let oi = || opt(&i());
    m.push(method(
        "replace",
        vec![
            me_d(),
            param_default("year", &oi(), none()),
            param_default("month", &oi(), none()),
            param_default("day", &oi(), none()),
        ],
        dt.clone(),
        vec![
            assign_typed(
                "y",
                ifexp(
                    cmp(v("year"), IRCompareOp::Is, none()),
                    my("year"),
                    v("year"),
                ),
                &i(),
            ),
            assign_typed(
                "mo",
                ifexp(
                    cmp(v("month"), IRCompareOp::Is, none()),
                    my("month"),
                    v("month"),
                ),
                &i(),
            ),
            assign_typed(
                "d",
                ifexp(cmp(v("day"), IRCompareOp::Is, none()), my("day"), v("day")),
                &i(),
            ),
            ret_val(make("date", vec![v("y"), v("mo"), v("d")])),
        ],
    ));
    for (name, op) in [
        ("__eq__", IRCompareOp::Eq),
        ("__lt__", IRCompareOp::Lt),
        ("__le__", IRCompareOp::LtE),
        ("__gt__", IRCompareOp::Gt),
        ("__ge__", IRCompareOp::GtE),
    ] {
        m.push(method(
            name,
            vec![me_d(), param("other", &dt)],
            IRType::Bool,
            vec![ret_val(cmp(
                mcall(v("self"), "toordinal", Vec::new()),
                op,
                mcall(v("other"), "toordinal", Vec::new()),
            ))],
        ));
    }
    for name in ["__add__", "__radd__"] {
        m.push(method(
            name,
            vec![me_d(), param("other", &td())],
            dt.clone(),
            date_from_ordinal_checked(bin(
                mcall(v("self"), "toordinal", Vec::new()),
                IROp::Add,
                field("other", "days"),
            )),
        ));
    }
    m.push(method(
        &spec_variant("__sub__", &td()),
        vec![me_d(), param("other", &td())],
        dt.clone(),
        date_from_ordinal_checked(bin(
            mcall(v("self"), "toordinal", Vec::new()),
            IROp::Sub,
            field("other", "days"),
        )),
    ));
    m.push(method(
        &spec_variant("__sub__", &dt),
        vec![me_d(), param("other", &dt)],
        td(),
        vec![ret_val(make(
            "timedelta",
            vec![call(
                "float",
                vec![bin(
                    mcall(v("self"), "toordinal", Vec::new()),
                    IROp::Sub,
                    mcall(v("other"), "toordinal", Vec::new()),
                )],
            )],
        ))],
    ));
    class(
        "date",
        Vec::new(),
        m,
        vec![
            class_var("min", make("date", vec![int(1), int(1), int(1)])),
            class_var("max", make("date", vec![int(9999), int(12), int(31)])),
            class_var("resolution", make("timedelta", vec![float(1.0)])),
        ],
    )
}
