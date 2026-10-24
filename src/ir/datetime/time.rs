//! `time`, `datetime`, and `timezone`, and `strptime`.

use super::classes::{
    class_var, date_methods, field, micros, repr_of, spec_variant, str_of, td_of_micros,
};
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
fn b() -> IRType {
    IRType::Bool
}
fn td() -> IRType {
    class_ty("timedelta")
}
fn tz() -> IRType {
    class_ty("timezone")
}
fn otz() -> IRType {
    opt(&tz())
}
fn ints(n: usize) -> IRType {
    IRType::Tuple(vec![IRType::Int; n])
}

fn kw(name: &str, value: IRExpr) -> IRExpr {
    IRExpr::Keyword {
        name: name.to_string(),
        value: Box::new(value),
    }
}

fn is_none(e: IRExpr) -> IRExpr {
    cmp(e, IRCompareOp::Is, none())
}

fn not_none(e: IRExpr) -> IRExpr {
    cmp(e, IRCompareOp::IsNot, none())
}

fn naive() -> IRExpr {
    is_none(my("tzinfo"))
}

/// `self.tzinfo` and `other.tzinfo` are not both None or both set.
fn mixed() -> IRExpr {
    cmp(
        is_none(my("tzinfo")),
        IRCompareOp::NotEq,
        is_none(field("other", "tzinfo")),
    )
}

/// `x if arg is None else arg`, for `replace`.
fn kept(arg: &str, own: IRExpr) -> IRExpr {
    ifexp(is_none(v(arg)), own, v(arg))
}

fn seconds_of_day() -> IRExpr {
    bin(
        bin(
            bin(my("hour"), IROp::Mul, int(3600)),
            IROp::Add,
            bin(my("minute"), IROp::Mul, int(60)),
        ),
        IROp::Add,
        my("second"),
    )
}

/// The microseconds since midnight, as an exact float.
fn us_of_day() -> IRExpr {
    bin(
        bin(call("float", vec![seconds_of_day()]), IROp::Mul, float(1e6)),
        IROp::Add,
        call("float", vec![my("microsecond")]),
    )
}

/// The methods `time` and `datetime` share around their `tzinfo`; `arg` is
/// what CPython passes the tzinfo's methods (None from a time, the datetime
/// itself from a datetime).
fn tz_methods(short: &str, arg: fn() -> IRExpr) -> Vec<IRFunction> {
    let me_t = || me(short);
    vec![
        // The offset in microseconds, 0 when naive.
        method(
            "_offus",
            vec![me_t()],
            f(),
            vec![
                if_(naive(), vec![ret_val(float(0.0))]),
                ret_val(micros(attr(my("tzinfo"), "_offset"))),
            ],
        ),
        method(
            "utcoffset",
            vec![me_t()],
            opt(&td()),
            vec![
                if_(naive(), vec![ret_val(none())]),
                ret_val(mcall(my("tzinfo"), "utcoffset", vec![arg()])),
            ],
        ),
        method(
            "tzname",
            vec![me_t()],
            opt(&s()),
            vec![
                if_(naive(), vec![ret_val(none())]),
                ret_val(mcall(my("tzinfo"), "tzname", vec![arg()])),
            ],
        ),
        method("dst", vec![me_t()], IRType::None, vec![ret()]),
        // `%z` and `%Z` of strftime.
        method(
            "_zfmt",
            vec![me_t()],
            s(),
            vec![
                if_(naive(), vec![ret_val(text(""))]),
                ret_val(fcall(
                    "format_offset",
                    vec![attr(my("tzinfo"), "_offset"), text("")],
                )),
            ],
        ),
        method(
            "_zname",
            vec![me_t()],
            s(),
            vec![
                if_(naive(), vec![ret_val(text(""))]),
                ret_val(mcall(my("tzinfo"), "tzname", vec![arg()])),
            ],
        ),
        method(
            "__format__",
            vec![me_t(), param("fmt", &s())],
            s(),
            vec![
                if_(
                    cmp(len(v("fmt")), IRCompareOp::Eq, int(0)),
                    vec![ret_val(str_of(v("self")))],
                ),
                ret_val(mcall(v("self"), "strftime", vec![v("fmt")])),
            ],
        ),
    ]
}

/// `__eq__` and the orderings, through `self._cmp(other)`; a naive value and
/// an aware one are unequal, and cannot be ordered.
fn comparisons(short: &str, plural: &str) -> Vec<IRFunction> {
    let ty = class_ty(short);
    let mut m = vec![method(
        "__eq__",
        vec![me(short), param("other", &ty)],
        b(),
        vec![
            if_(mixed(), vec![ret_val(boolean(false))]),
            ret_val(cmp(
                mcall(v("self"), "_cmp", vec![v("other")]),
                IRCompareOp::Eq,
                int(0),
            )),
        ],
    )];
    for (name, op) in [
        ("__lt__", IRCompareOp::Lt),
        ("__le__", IRCompareOp::LtE),
        ("__gt__", IRCompareOp::Gt),
        ("__ge__", IRCompareOp::GtE),
    ] {
        m.push(method(
            name,
            vec![me(short), param("other", &ty)],
            b(),
            vec![
                if_(
                    mixed(),
                    vec![raise_expr(
                        "TypeError",
                        text(&format!(
                            "can't compare offset-naive and offset-aware {plural}"
                        )),
                    )],
                ),
                ret_val(cmp(mcall(v("self"), "_cmp", vec![v("other")]), op, int(0))),
            ],
        ));
    }
    m
}

/// `-1`, `0`, or `1` as `a` sorts before, with, or after `b`, two keys of
/// `(seconds, microseconds)`.
fn three_way(a: IRExpr, b: IRExpr) -> Vec<IRStatement> {
    vec![
        assign_typed("a", a, &IRType::Tuple(vec![f(), f()])),
        assign_typed("b", b, &IRType::Tuple(vec![f(), f()])),
        if_(
            cmp(idx(v("a"), int(0)), IRCompareOp::Lt, idx(v("b"), int(0))),
            vec![ret_val(int(-1))],
        ),
        if_(
            cmp(idx(v("a"), int(0)), IRCompareOp::Gt, idx(v("b"), int(0))),
            vec![ret_val(int(1))],
        ),
        if_(
            cmp(idx(v("a"), int(1)), IRCompareOp::Lt, idx(v("b"), int(1))),
            vec![ret_val(int(-1))],
        ),
        if_(
            cmp(idx(v("a"), int(1)), IRCompareOp::Gt, idx(v("b"), int(1))),
            vec![ret_val(int(1))],
        ),
        ret_val(int(0)),
    ]
}

/// The `(seconds, microseconds)` of the instant `secs` (a whole number of
/// seconds, as a float) and `self.microsecond` name, the UTC offset taken
/// off. Both stay exact in floats.
fn key_body(secs: IRExpr) -> Vec<IRStatement> {
    vec![
        assign_typed("t", secs, &f()),
        assign_typed("off", mcall(v("self"), "_offus", Vec::new()), &f()),
        assign("t", bin(v("t"), IROp::Sub, floordiv(v("off"), float(1e6)))),
        assign_typed(
            "u",
            bin(
                call("float", vec![my("microsecond")]),
                IROp::Sub,
                modulo(v("off"), float(1e6)),
            ),
            &f(),
        ),
        if_(
            cmp(v("u"), IRCompareOp::Lt, float(0.0)),
            vec![
                assign("u", bin(v("u"), IROp::Add, float(1e6))),
                assign("t", bin(v("t"), IROp::Sub, float(1.0))),
            ],
        ),
        ret_val(IRExpr::TupleLiteral(vec![v("t"), v("u")])),
    ]
}

// ---------------------------------------------------------------------------
// timezone
// ---------------------------------------------------------------------------

pub(super) fn timezone_class() -> IRClass {
    let me_z = || me("timezone");
    let odt = || param("dt", &opt(&class_ty("datetime")));
    let m = vec![
        method(
            "__init__",
            vec![
                me_z(),
                param("offset", &td()),
                param_default("name", &opt(&s()), none()),
            ],
            IRType::None,
            vec![
                assign_typed("m", micros(v("offset")), &f()),
                if_(
                    or(
                        cmp(v("m"), IRCompareOp::LtE, float(-86400e6)),
                        cmp(v("m"), IRCompareOp::GtE, float(86400e6)),
                    ),
                    vec![raise_expr(
                        "ValueError",
                        concat(vec![
                            text(
                                "offset must be a timedelta strictly between \
                                 -timedelta(hours=24) and timedelta(hours=24), not ",
                            ),
                            repr_of(v("offset")),
                            text("."),
                        ]),
                    )],
                ),
                set_my_typed("_offset", v("offset"), &td()),
                set_my_typed("_named", boolean(false), &b()),
                set_my_typed("_name", text(""), &s()),
                if_(
                    not_none(v("name")),
                    vec![
                        set_attr(v("self"), "_named", boolean(true)),
                        set_attr(v("self"), "_name", v("name")),
                    ],
                ),
            ],
        ),
        method(
            "utcoffset",
            vec![me_z(), odt()],
            td(),
            vec![ret_val(my("_offset"))],
        ),
        method(
            "tzname",
            vec![me_z(), odt()],
            s(),
            vec![
                if_(my("_named"), vec![ret_val(my("_name"))]),
                if_(
                    cmp(micros(my("_offset")), IRCompareOp::Eq, float(0.0)),
                    vec![ret_val(text("UTC"))],
                ),
                ret_val(bin(
                    text("UTC"),
                    IROp::Add,
                    fcall("format_offset", vec![my("_offset"), text(":")]),
                )),
            ],
        ),
        method("dst", vec![me_z(), odt()], IRType::None, vec![ret()]),
        method(
            "__repr__",
            vec![me_z()],
            s(),
            vec![
                if_(
                    my("_named"),
                    vec![ret_val(concat(vec![
                        text("datetime.timezone("),
                        repr_of(my("_offset")),
                        text(", "),
                        repr_of(my("_name")),
                        text(")"),
                    ]))],
                ),
                if_(
                    cmp(micros(my("_offset")), IRCompareOp::Eq, float(0.0)),
                    vec![ret_val(text("datetime.timezone.utc"))],
                ),
                ret_val(concat(vec![
                    text("datetime.timezone("),
                    repr_of(my("_offset")),
                    text(")"),
                ])),
            ],
        ),
        method(
            "__str__",
            vec![me_z()],
            s(),
            vec![ret_val(mcall(v("self"), "tzname", vec![none()]))],
        ),
        method(
            "__eq__",
            vec![me_z(), param("other", &tz())],
            b(),
            vec![ret_val(cmp(
                micros(my("_offset")),
                IRCompareOp::Eq,
                micros(field("other", "_offset")),
            ))],
        ),
    ];
    class(
        "timezone",
        Vec::new(),
        m,
        vec![
            class_var(
                "utc",
                make("timezone", vec![make("timedelta", vec![float(0.0)])]),
            ),
            class_var(
                "min",
                make(
                    "timezone",
                    vec![make("timedelta", vec![float(-1.0), float(60.0)])],
                ),
            ),
            class_var(
                "max",
                make(
                    "timezone",
                    vec![make("timedelta", vec![float(0.0), float(86340.0)])],
                ),
            ),
        ],
    )
}

// ---------------------------------------------------------------------------
// time
// ---------------------------------------------------------------------------

fn time_fields() -> Vec<IRStatement> {
    vec![
        set_my_typed("hour", v("hour"), &i()),
        set_my_typed("minute", v("minute"), &i()),
        set_my_typed("second", v("second"), &i()),
        set_my_typed("microsecond", v("microsecond"), &i()),
        set_my_typed("tzinfo", v("tzinfo"), &otz()),
        set_my_typed("fold", v("fold"), &i()),
    ]
}

fn time_params() -> Vec<IRParam> {
    vec![
        param_default("hour", &i(), int(0)),
        param_default("minute", &i(), int(0)),
        param_default("second", &i(), int(0)),
        param_default("microsecond", &i(), int(0)),
        param_default("tzinfo", &otz(), none()),
        IRParam {
            keyword_only: true,
            ..param_default("fold", &i(), int(0))
        },
    ]
}

/// `replace`'s parameters for the time fields; `tzinfo_given` says whether
/// `tzinfo` was passed at all (the pass adds it where `tzinfo=` is written).
fn replace_time_params() -> Vec<IRParam> {
    let oi = || opt(&i());
    vec![
        param_default("hour", &oi(), none()),
        param_default("minute", &oi(), none()),
        param_default("second", &oi(), none()),
        param_default("microsecond", &oi(), none()),
        param_default("tzinfo", &otz(), none()),
        param_default("fold", &oi(), none()),
        param_default(TZINFO_GIVEN, &b(), boolean(false)),
    ]
}

/// The name of `replace`'s and `combine`'s hidden parameter.
pub(super) const TZINFO_GIVEN: &str = "tzinfo_given";

fn replace_time_fields() -> Vec<IRStatement> {
    vec![
        assign_typed("h", kept("hour", my("hour")), &i()),
        assign_typed("mi", kept("minute", my("minute")), &i()),
        assign_typed("se", kept("second", my("second")), &i()),
        assign_typed("us", kept("microsecond", my("microsecond")), &i()),
        assign_typed("z", my("tzinfo"), &otz()),
        if_(v(TZINFO_GIVEN), vec![assign("z", v("tzinfo"))]),
        assign_typed("fo", kept("fold", my("fold")), &i()),
    ]
}

/// The fields of a time's repr after the hour and minute.
fn repr_seconds() -> Vec<IRStatement> {
    vec![if_else(
        cmp(my("microsecond"), IRCompareOp::NotEq, int(0)),
        vec![assign(
            "r",
            concat(vec![
                v("r"),
                text(", "),
                str_of(my("second")),
                text(", "),
                str_of(my("microsecond")),
            ]),
        )],
        vec![if_(
            cmp(my("second"), IRCompareOp::NotEq, int(0)),
            vec![assign(
                "r",
                concat(vec![v("r"), text(", "), str_of(my("second"))]),
            )],
        )],
    )]
}

fn repr_tzinfo() -> IRStatement {
    if_(
        not_none(my("tzinfo")),
        vec![assign(
            "r",
            concat(vec![
                v("r"),
                text(", tzinfo="),
                mcall(my("tzinfo"), "__repr__", Vec::new()),
            ]),
        )],
    )
}

fn repr_fold() -> IRStatement {
    if_(
        cmp(my("fold"), IRCompareOp::NotEq, int(0)),
        vec![assign("r", bin(v("r"), IROp::Add, text(", fold=1")))],
    )
}

/// The offset of an aware value in its isoformat, else nothing.
fn iso_offset() -> IRStatement {
    if_(
        not_none(my("tzinfo")),
        vec![assign(
            "out",
            bin(
                v("out"),
                IROp::Add,
                fcall(
                    "format_offset",
                    vec![attr(my("tzinfo"), "_offset"), text(":")],
                ),
            ),
        )],
    )
}

/// A timezone of `micros` microseconds when `aware`, else None, into `z`.
fn zone_from(aware: IRExpr, offset: IRExpr) -> Vec<IRStatement> {
    vec![
        assign_typed("z", none(), &otz()),
        if_(
            aware,
            vec![assign("z", make("timezone", vec![td_of_micros(offset)]))],
        ),
    ]
}

pub(super) fn time_class() -> IRClass {
    let me_t = || me("time");
    let ty = class_ty("time");
    let mut params = vec![me_t()];
    params.extend(time_params());
    let mut init = vec![expr_stmt(fcall(
        "check_time",
        vec![
            v("hour"),
            v("minute"),
            v("second"),
            v("microsecond"),
            v("fold"),
        ],
    ))];
    init.extend(time_fields());
    let mut m = vec![method("__init__", params, IRType::None, init)];
    m.extend(tz_methods("time", none));
    m.push(method(
        "isoformat",
        vec![me_t(), param_default("timespec", &s(), text("auto"))],
        s(),
        vec![
            assign_typed(
                "out",
                fcall(
                    "format_time",
                    vec![
                        my("hour"),
                        my("minute"),
                        my("second"),
                        my("microsecond"),
                        v("timespec"),
                    ],
                ),
                &s(),
            ),
            iso_offset(),
            ret_val(v("out")),
        ],
    ));
    m.push(method(
        "__str__",
        vec![me_t()],
        s(),
        vec![ret_val(mcall(v("self"), "isoformat", vec![text("auto")]))],
    ));
    let mut repr = vec![assign_typed(
        "r",
        concat(vec![
            text("datetime.time("),
            str_of(my("hour")),
            text(", "),
            str_of(my("minute")),
        ]),
        &s(),
    )];
    repr.extend(repr_seconds());
    repr.push(repr_tzinfo());
    repr.push(repr_fold());
    repr.push(ret_val(bin(v("r"), IROp::Add, text(")"))));
    m.push(method("__repr__", vec![me_t()], s(), repr));
    m.push(method(
        "strftime",
        vec![me_t(), param("fmt", &s())],
        s(),
        vec![ret_val(fcall(
            "strftime",
            vec![
                v("fmt"),
                int(1900),
                int(1),
                int(1),
                my("hour"),
                my("minute"),
                my("second"),
                my("microsecond"),
                mcall(v("self"), "_zfmt", Vec::new()),
                mcall(v("self"), "_zname", Vec::new()),
            ],
        ))],
    ));
    let mut params = vec![me_t()];
    params.extend(replace_time_params());
    let mut body = replace_time_fields();
    body.push(ret_val(make(
        "time",
        vec![
            v("h"),
            v("mi"),
            v("se"),
            v("us"),
            v("z"),
            kw("fold", v("fo")),
        ],
    )));
    m.push(method("replace", params, ty.clone(), body));
    m.push(method(
        "_key",
        vec![me_t()],
        IRType::Tuple(vec![f(), f()]),
        key_body(call("float", vec![seconds_of_day()])),
    ));
    m.push(method(
        "_cmp",
        vec![me_t(), param("other", &ty)],
        i(),
        three_way(
            mcall(v("self"), "_key", Vec::new()),
            mcall(v("other"), "_key", Vec::new()),
        ),
    ));
    m.extend(comparisons("time", "times"));
    let mut body = vec![
        assign_typed("t", v("text"), &s()),
        if_(
            and(
                cmp(len(v("t")), IRCompareOp::Gt, int(0)),
                cmp(idx(v("t"), int(0)), IRCompareOp::Eq, text("T")),
            ),
            vec![assign("t", slice(v("t"), Some(int(1)), None))],
        ),
        assign_typed(
            "p",
            fcall("parse_isotime", vec![v("t"), v("text")]),
            &IRType::Tuple(vec![i(), i(), i(), i(), i(), f()]),
        ),
    ];
    body.extend(zone_from(
        cmp(idx(v("p"), int(4)), IRCompareOp::Eq, int(1)),
        idx(v("p"), int(5)),
    ));
    body.push(ret_val(make(
        "time",
        vec![
            idx(v("p"), int(0)),
            idx(v("p"), int(1)),
            idx(v("p"), int(2)),
            idx(v("p"), int(3)),
            v("z"),
        ],
    )));
    m.push(classmethod(method(
        "fromisoformat",
        vec![cls("time"), param("text", &s())],
        ty.clone(),
        body,
    )));
    class(
        "time",
        Vec::new(),
        m,
        vec![
            class_var("min", make("time", vec![int(0), int(0)])),
            class_var(
                "max",
                make("time", vec![int(23), int(59), int(59), int(999999)]),
            ),
            class_var(
                "resolution",
                make("timedelta", vec![float(0.0), float(0.0), float(1.0)]),
            ),
        ],
    )
}

// ---------------------------------------------------------------------------
// datetime
// ---------------------------------------------------------------------------

fn date_part() -> Vec<IRExpr> {
    vec![my("year"), my("month"), my("day")]
}

pub(super) fn datetime_class() -> IRClass {
    let me_dt = || me("datetime");
    let ty = class_ty("datetime");
    let ord = || mcall(v("self"), "toordinal", Vec::new());
    let mut params = vec![
        me_dt(),
        param("year", &i()),
        param("month", &i()),
        param("day", &i()),
    ];
    params.extend(time_params());
    let mut init = vec![
        expr_stmt(fcall("check_date", vec![v("year"), v("month"), v("day")])),
        expr_stmt(fcall(
            "check_time",
            vec![
                v("hour"),
                v("minute"),
                v("second"),
                v("microsecond"),
                v("fold"),
            ],
        )),
        set_my_typed("year", v("year"), &i()),
        set_my_typed("month", v("month"), &i()),
        set_my_typed("day", v("day"), &i()),
    ];
    init.extend(time_fields());
    let mut m = vec![method("__init__", params, IRType::None, init)];
    m.extend(date_methods("datetime"));
    m.extend(tz_methods("datetime", || v("self")));
    m.push(method(
        "date",
        vec![me_dt()],
        class_ty("date"),
        vec![ret_val(make("date", date_part()))],
    ));
    let time_of = |z: IRExpr| {
        make(
            "time",
            vec![
                my("hour"),
                my("minute"),
                my("second"),
                my("microsecond"),
                z,
                kw("fold", my("fold")),
            ],
        )
    };
    m.push(method(
        "time",
        vec![me_dt()],
        class_ty("time"),
        vec![ret_val(time_of(none()))],
    ));
    m.push(method(
        "timetz",
        vec![me_dt()],
        class_ty("time"),
        vec![ret_val(time_of(my("tzinfo")))],
    ));
    m.push(method(
        "isoformat",
        vec![
            me_dt(),
            param_default("sep", &s(), text("T")),
            param_default("timespec", &s(), text("auto")),
        ],
        s(),
        vec![
            assign_typed(
                "out",
                concat(vec![
                    pad(my("year"), 4),
                    text("-"),
                    pad(my("month"), 2),
                    text("-"),
                    pad(my("day"), 2),
                    v("sep"),
                    fcall(
                        "format_time",
                        vec![
                            my("hour"),
                            my("minute"),
                            my("second"),
                            my("microsecond"),
                            v("timespec"),
                        ],
                    ),
                ]),
                &s(),
            ),
            iso_offset(),
            ret_val(v("out")),
        ],
    ));
    m.push(method(
        "__str__",
        vec![me_dt()],
        s(),
        vec![ret_val(mcall(
            v("self"),
            "isoformat",
            vec![text(" "), text("auto")],
        ))],
    ));
    let mut repr = vec![assign_typed(
        "r",
        concat(vec![
            text("datetime.datetime("),
            str_of(my("year")),
            text(", "),
            str_of(my("month")),
            text(", "),
            str_of(my("day")),
            text(", "),
            str_of(my("hour")),
            text(", "),
            str_of(my("minute")),
        ]),
        &s(),
    )];
    repr.extend(repr_seconds());
    repr.push(repr_fold());
    repr.push(repr_tzinfo());
    repr.push(ret_val(bin(v("r"), IROp::Add, text(")"))));
    m.push(method("__repr__", vec![me_dt()], s(), repr));
    let strftime_of = |fmt: IRExpr, us: IRExpr, zf: IRExpr, zn: IRExpr| {
        let mut args = vec![fmt];
        args.extend(date_part());
        args.extend([my("hour"), my("minute"), my("second"), us, zf, zn]);
        fcall("strftime", args)
    };
    m.push(method(
        "ctime",
        vec![me_dt()],
        s(),
        vec![ret_val(strftime_of(text("%c"), int(0), text(""), text("")))],
    ));
    m.push(method(
        "strftime",
        vec![me_dt(), param("fmt", &s())],
        s(),
        vec![ret_val(strftime_of(
            v("fmt"),
            my("microsecond"),
            mcall(v("self"), "_zfmt", Vec::new()),
            mcall(v("self"), "_zname", Vec::new()),
        ))],
    ));
    // Whole seconds since the epoch, then the microseconds over 10**6:
    // CPython's sum for a naive value, and its correctly rounded quotient for
    // an aware one, which the sum also gives, since a count of microseconds
    // is never halfway between two doubles of this size.
    m.push(method(
        "timestamp",
        vec![me_dt()],
        f(),
        vec![
            assign_typed(
                "k",
                mcall(v("self"), "_key", Vec::new()),
                &IRType::Tuple(vec![f(), f()]),
            ),
            ret_val(bin(
                bin(
                    idx(v("k"), int(0)),
                    IROp::Sub,
                    bin(float(EPOCH_ORDINAL as f64), IROp::Mul, float(86400.0)),
                ),
                IROp::Add,
                bin(idx(v("k"), int(1)), IROp::Div, float(1e6)),
            )),
        ],
    ));
    let mut params = vec![
        me_dt(),
        param_default("year", &opt(&i()), none()),
        param_default("month", &opt(&i()), none()),
        param_default("day", &opt(&i()), none()),
    ];
    params.extend(replace_time_params());
    let mut body = vec![
        assign_typed("y", kept("year", my("year")), &i()),
        assign_typed("mo", kept("month", my("month")), &i()),
        assign_typed("d", kept("day", my("day")), &i()),
    ];
    body.extend(replace_time_fields());
    body.push(ret_val(make(
        "datetime",
        vec![
            v("y"),
            v("mo"),
            v("d"),
            v("h"),
            v("mi"),
            v("se"),
            v("us"),
            v("z"),
            kw("fold", v("fo")),
        ],
    )));
    m.push(method("replace", params, ty.clone(), body));
    // The datetime `days` and `us` microseconds after this one, in zone `z`.
    m.push(method(
        "_plus",
        vec![
            me_dt(),
            param("days", &i()),
            param("us", &f()),
            param("z", &otz()),
        ],
        ty.clone(),
        vec![
            assign("o", bin(ord(), IROp::Add, v("days"))),
            assign_typed("u", bin(us_of_day(), IROp::Add, v("us")), &f()),
            assign(
                "o",
                bin(
                    v("o"),
                    IROp::Add,
                    call("int", vec![floordiv(v("u"), float(86400e6))]),
                ),
            ),
            assign("u", modulo(v("u"), float(86400e6))),
            if_(
                or(
                    cmp(v("o"), IRCompareOp::Lt, int(1)),
                    cmp(v("o"), IRCompareOp::Gt, int(MAX_ORDINAL)),
                ),
                vec![raise_expr("OverflowError", text("date value out of range"))],
            ),
            ret_val(fcall("from_ordinal_us", vec![v("o"), v("u"), v("z")])),
        ],
    ));
    let delta_us = |o: &str| {
        bin(
            bin(
                call("float", vec![field(o, "seconds")]),
                IROp::Mul,
                float(1e6),
            ),
            IROp::Add,
            call("float", vec![field(o, "microseconds")]),
        )
    };
    for name in ["__add__", "__radd__"] {
        m.push(method(
            name,
            vec![me_dt(), param("other", &td())],
            ty.clone(),
            vec![ret_val(mcall(
                v("self"),
                "_plus",
                vec![field("other", "days"), delta_us("other"), my("tzinfo")],
            ))],
        ));
    }
    m.push(method(
        &spec_variant("__sub__", &td()),
        vec![me_dt(), param("other", &td())],
        ty.clone(),
        vec![ret_val(mcall(
            v("self"),
            "_plus",
            vec![
                neg(field("other", "days")),
                neg(delta_us("other")),
                my("tzinfo"),
            ],
        ))],
    ));
    m.push(method(
        &spec_variant("__sub__", &ty),
        vec![me_dt(), param("other", &ty)],
        td(),
        vec![
            if_(
                mixed(),
                vec![raise_expr(
                    "TypeError",
                    text("can't subtract offset-naive and offset-aware datetimes"),
                )],
            ),
            assign(
                "days",
                bin(ord(), IROp::Sub, mcall(v("other"), "toordinal", Vec::new())),
            ),
            assign_typed(
                "us",
                bin(
                    bin(us_of_day(), IROp::Sub, mcall(v("other"), "_us", Vec::new())),
                    IROp::Sub,
                    bin(
                        mcall(v("self"), "_offus", Vec::new()),
                        IROp::Sub,
                        mcall(v("other"), "_offus", Vec::new()),
                    ),
                ),
                &f(),
            ),
            ret_val(make(
                "timedelta",
                vec![call("float", vec![v("days")]), float(0.0), v("us")],
            )),
        ],
    ));
    m.push(method(
        "_us",
        vec![me_dt()],
        f(),
        vec![ret_val(us_of_day())],
    ));
    m.push(method(
        "astimezone",
        vec![me_dt(), param_default("tz", &otz(), none())],
        ty.clone(),
        vec![
            // A WASI host's local time is UTC, under that name.
            assign_typed("target", v("tz"), &otz()),
            if_(
                is_none(v("tz")),
                vec![assign(
                    "target",
                    make(
                        "timezone",
                        vec![make("timedelta", vec![float(0.0)]), text("UTC")],
                    ),
                )],
            ),
            if_(
                cmp(my("tzinfo"), IRCompareOp::Is, v("target")),
                vec![ret_val(v("self"))],
            ),
            ret_val(mcall(
                v("self"),
                "_plus",
                vec![
                    int(0),
                    bin(
                        micros(attr(v("target"), "_offset")),
                        IROp::Sub,
                        mcall(v("self"), "_offus", Vec::new()),
                    ),
                    v("target"),
                ],
            )),
        ],
    ));
    m.push(method(
        "_key",
        vec![me_dt()],
        IRType::Tuple(vec![f(), f()]),
        key_body(bin(
            bin(call("float", vec![ord()]), IROp::Mul, float(86400.0)),
            IROp::Add,
            call("float", vec![seconds_of_day()]),
        )),
    ));
    m.push(method(
        "_cmp",
        vec![me_dt(), param("other", &ty)],
        i(),
        three_way(
            mcall(v("self"), "_key", Vec::new()),
            mcall(v("other"), "_key", Vec::new()),
        ),
    ));
    m.extend(comparisons("datetime", "datetimes"));
    let clock = || call(crate::ir::DATETIME_CLOCK_FN, Vec::new());
    m.push(classmethod(method(
        "now",
        vec![cls("datetime"), param_default("tz", &otz(), none())],
        ty.clone(),
        vec![ret_val(fcall("from_us", vec![clock(), v("tz")]))],
    )));
    m.push(classmethod(method(
        "utcnow",
        vec![cls("datetime")],
        ty.clone(),
        vec![ret_val(fcall("from_us", vec![clock(), none()]))],
    )));
    m.push(classmethod(method(
        "today",
        vec![cls("datetime")],
        ty.clone(),
        vec![ret_val(fcall("from_us", vec![clock(), none()]))],
    )));
    // CPython's rounding of a timestamp: the fraction to microseconds,
    // halves to even.
    let stamp_us = || {
        vec![
            assign_typed("whole", fcall("trunc", vec![v("t")]), &f()),
            assign_typed(
                "us",
                bin(
                    bin(v("whole"), IROp::Mul, float(1e6)),
                    IROp::Add,
                    fcall(
                        "round_even",
                        vec![bin(
                            bin(v("t"), IROp::Sub, v("whole")),
                            IROp::Mul,
                            float(1e6),
                        )],
                    ),
                ),
                &f(),
            ),
        ]
    };
    let mut body = stamp_us();
    body.push(ret_val(fcall("from_us", vec![v("us"), v("tz")])));
    m.push(classmethod(method(
        "fromtimestamp",
        vec![
            cls("datetime"),
            param("t", &f()),
            param_default("tz", &otz(), none()),
        ],
        ty.clone(),
        body,
    )));
    let mut body = stamp_us();
    body.push(ret_val(fcall("from_us", vec![v("us"), none()])));
    m.push(classmethod(method(
        "utcfromtimestamp",
        vec![cls("datetime"), param("t", &f())],
        ty.clone(),
        body,
    )));
    let from_date = |d: IRExpr| {
        vec![
            assign_typed("dd", d, &class_ty("date")),
            ret_val(make(
                "datetime",
                vec![
                    attr(v("dd"), "year"),
                    attr(v("dd"), "month"),
                    attr(v("dd"), "day"),
                ],
            )),
        ]
    };
    m.push(classmethod(method(
        "fromordinal",
        vec![cls("datetime"), param("n", &i())],
        ty.clone(),
        from_date(mcall(v(&class_name("date")), "fromordinal", vec![v("n")])),
    )));
    m.push(classmethod(method(
        "fromisocalendar",
        vec![
            cls("datetime"),
            param("year", &i()),
            param("week", &i()),
            param("day", &i()),
        ],
        ty.clone(),
        from_date(mcall(
            v(&class_name("date")),
            "fromisocalendar",
            vec![v("year"), v("week"), v("day")],
        )),
    )));
    m.push(classmethod(method(
        "combine",
        vec![
            cls("datetime"),
            param("date", &class_ty("date")),
            param("time", &class_ty("time")),
            param_default("tzinfo", &otz(), none()),
            param_default(TZINFO_GIVEN, &b(), boolean(false)),
        ],
        ty.clone(),
        vec![
            assign_typed("z", field("time", "tzinfo"), &otz()),
            if_(v(TZINFO_GIVEN), vec![assign("z", v("tzinfo"))]),
            ret_val(make(
                "datetime",
                vec![
                    field("date", "year"),
                    field("date", "month"),
                    field("date", "day"),
                    field("time", "hour"),
                    field("time", "minute"),
                    field("time", "second"),
                    field("time", "microsecond"),
                    v("z"),
                    kw("fold", field("time", "fold")),
                ],
            )),
        ],
    )));
    let invalid = || {
        raise_expr(
            "ValueError",
            concat(vec![text("Invalid isoformat string: "), repr_of(v("text"))]),
        )
    };
    let mut body = vec![
        if_(
            cmp(len(v("text")), IRCompareOp::Lt, int(7)),
            vec![invalid()],
        ),
        assign("cut", fcall("iso_separator", vec![v("text")])),
        if_(cmp(v("cut"), IRCompareOp::Lt, int(0)), vec![invalid()]),
        assign_typed(
            "ymd",
            fcall(
                "parse_isodate",
                vec![slice(v("text"), None, Some(v("cut"))), v("text")],
            ),
            &ints(3),
        ),
        assign_typed(
            "p",
            IRExpr::TupleLiteral(vec![int(0), int(0), int(0), int(0), int(0), float(0.0)]),
            &IRType::Tuple(vec![i(), i(), i(), i(), i(), f()]),
        ),
        if_(
            cmp(v("cut"), IRCompareOp::Lt, len(v("text"))),
            vec![assign(
                "p",
                fcall(
                    "parse_isotime",
                    vec![
                        slice(v("text"), Some(bin(v("cut"), IROp::Add, int(1))), None),
                        v("text"),
                    ],
                ),
            )],
        ),
    ];
    body.extend(zone_from(
        cmp(idx(v("p"), int(4)), IRCompareOp::Eq, int(1)),
        idx(v("p"), int(5)),
    ));
    body.push(ret_val(make(
        "datetime",
        vec![
            idx(v("ymd"), int(0)),
            idx(v("ymd"), int(1)),
            idx(v("ymd"), int(2)),
            idx(v("p"), int(0)),
            idx(v("p"), int(1)),
            idx(v("p"), int(2)),
            idx(v("p"), int(3)),
            v("z"),
        ],
    )));
    m.push(classmethod(method(
        "fromisoformat",
        vec![cls("datetime"), param("text", &s())],
        ty.clone(),
        body,
    )));
    m.push(classmethod(method(
        "strptime",
        vec![cls("datetime"), param("data", &s()), param("fmt", &s())],
        ty.clone(),
        vec![
            assign_typed(
                "r",
                fcall("strptime", vec![v("data"), v("fmt")]),
                &strptime_result(),
            ),
            assign_typed("z", none(), &otz()),
            if_(
                cmp(idx(v("r"), int(7)), IRCompareOp::Eq, int(1)),
                vec![
                    assign_typed(
                        "off",
                        make(
                            "timedelta",
                            vec![float(0.0), idx(v("r"), int(8)), idx(v("r"), int(9))],
                        ),
                        &td(),
                    ),
                    if_else(
                        cmp(len(idx(v("r"), int(10))), IRCompareOp::Gt, int(0)),
                        vec![assign(
                            "z",
                            make("timezone", vec![v("off"), idx(v("r"), int(10))]),
                        )],
                        vec![assign("z", make("timezone", vec![v("off")]))],
                    ),
                ],
            ),
            ret_val(make(
                "datetime",
                (0..7)
                    .map(|k| idx(v("r"), int(k)))
                    .chain([v("z")])
                    .collect(),
            )),
        ],
    )));
    class(
        "datetime",
        Vec::new(),
        m,
        vec![
            class_var("min", make("datetime", vec![int(1), int(1), int(1)])),
            class_var(
                "max",
                make(
                    "datetime",
                    vec![
                        int(9999),
                        int(12),
                        int(31),
                        int(23),
                        int(59),
                        int(59),
                        int(999999),
                    ],
                ),
            ),
            class_var(
                "resolution",
                make("timedelta", vec![float(0.0), float(0.0), float(1.0)]),
            ),
        ],
    )
}

/// The fields `strptime` finds: the date and time, whether an offset was
/// given, its seconds and microseconds, and the `%Z` name.
fn strptime_result() -> IRType {
    IRType::Tuple(vec![i(), i(), i(), i(), i(), i(), i(), i(), f(), f(), s()])
}

// ---------------------------------------------------------------------------
// The module functions behind the classes
// ---------------------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const WEEKDAYS: [&str; 7] = [
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

fn lowered(list: &[&str], abbreviated: bool) -> IRExpr {
    IRExpr::ListLiteral(
        list.iter()
            .map(|n| text(if abbreviated { &n[..3] } else { n }))
            .collect(),
    )
}

fn list_of(ty: IRType) -> IRType {
    IRType::List(Box::new(ty))
}

fn isspace(e: IRExpr) -> IRExpr {
    mcall(e, "isspace", Vec::new())
}

fn lower(e: IRExpr) -> IRExpr {
    mcall(e, "lower", Vec::new())
}

fn append(list: &str, e: IRExpr) -> IRStatement {
    expr_stmt(mcall(v(list), "append", vec![e]))
}

fn inc(var: &str, by: IRExpr) -> IRStatement {
    assign(var, bin(v(var), IROp::Add, by))
}

/// The directives `strptime` reads, and those it spells out in others (in
/// the C locale).
pub(super) const STRPTIME_DIRECTIVES: &str = "dejfHkIlmMSUWwuyYzAaBbpPZ%";
pub(super) const STRPTIME_COMPOSITES: &[(char, &str)] = &[
    ('c', "%a %b %d %H:%M:%S %Y"),
    ('x', "%m/%d/%y"),
    ('X', "%H:%M:%S"),
    ('T', "%H:%M:%S"),
    ('R', "%H:%M"),
    ('r', "%I:%M:%S %p"),
];

/// The format's tokens: a directive (`%d`), a character, or `" "` for a
/// run of whitespace.
fn tokens_of(spelled: &str) -> IRExpr {
    let mut out = Vec::new();
    let mut chars = spelled.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let k = chars.next().expect("a directive");
            out.push(text(&format!("%{k}")));
        } else if c == ' ' {
            out.push(text(" "));
        } else {
            out.push(text(&c.to_string()));
        }
    }
    IRExpr::ListLiteral(out)
}

pub(super) fn functions() -> Vec<IRFunction> {
    let mut out = vec![
        // A datetime from an ordinal and microseconds since midnight.
        function(
            "from_ordinal_us",
            vec![param("o", &i()), param("u", &f()), param("z", &otz())],
            class_ty("datetime"),
            vec![
                IRStatement::TupleUnpack {
                    targets: vec!["y".into(), "mo".into(), "d".into()],
                    value: fcall("ord2ymd", vec![v("o")]),
                    starred: None,
                },
                assign("secs", call("int", vec![floordiv(v("u"), float(1e6))])),
                ret_val(make(
                    "datetime",
                    vec![
                        v("y"),
                        v("mo"),
                        v("d"),
                        floordiv(v("secs"), int(3600)),
                        modulo(floordiv(v("secs"), int(60)), int(60)),
                        modulo(v("secs"), int(60)),
                        call("int", vec![modulo(v("u"), float(1e6))]),
                        v("z"),
                    ],
                )),
            ],
        ),
        // The datetime `us` microseconds after the epoch, as the local time
        // (UTC) or the time in `z`.
        function(
            "from_us",
            vec![param("us", &f()), param("z", &otz())],
            class_ty("datetime"),
            vec![
                assign_typed("t", v("us"), &f()),
                if_(
                    not_none(v("z")),
                    vec![assign(
                        "t",
                        bin(v("t"), IROp::Add, micros(attr(v("z"), "_offset"))),
                    )],
                ),
                ret_val(fcall(
                    "from_ordinal_us",
                    vec![
                        bin(
                            call("int", vec![floordiv(v("t"), float(86400e6))]),
                            IROp::Add,
                            int(EPOCH_ORDINAL),
                        ),
                        modulo(v("t"), float(86400e6)),
                        v("z"),
                    ],
                )),
            ],
        ),
        iso_separator(),
    ];
    out.extend(strptime_functions());
    out
}

/// CPython's `_find_isoformat_datetime_separator`, with -1 for an invalid
/// string.
fn iso_separator() -> IRFunction {
    let digit_at = |k: IRExpr| {
        and(
            cmp(idx(v("text"), k.clone()), IRCompareOp::GtE, text("0")),
            cmp(idx(v("text"), k), IRCompareOp::LtE, text("9")),
        )
    };
    function(
        "iso_separator",
        vec![param("text", &s())],
        i(),
        vec![
            assign("n", len(v("text"))),
            if_(cmp(v("n"), IRCompareOp::Eq, int(7)), vec![ret_val(int(7))]),
            if_(
                cmp(idx(v("text"), int(4)), IRCompareOp::Eq, text("-")),
                vec![
                    if_(
                        cmp(idx(v("text"), int(5)), IRCompareOp::Eq, text("W")),
                        vec![
                            if_(
                                and(
                                    cmp(v("n"), IRCompareOp::Gt, int(8)),
                                    cmp(idx(v("text"), int(8)), IRCompareOp::Eq, text("-")),
                                ),
                                vec![
                                    if_(
                                        cmp(v("n"), IRCompareOp::Eq, int(9)),
                                        vec![ret_val(int(-1))],
                                    ),
                                    if_(
                                        and(
                                            cmp(v("n"), IRCompareOp::Gt, int(10)),
                                            digit_at(int(10)),
                                        ),
                                        vec![ret_val(int(8))],
                                    ),
                                    ret_val(int(10)),
                                ],
                            ),
                            ret_val(int(8)),
                        ],
                    ),
                    ret_val(int(10)),
                ],
            ),
            if_(
                cmp(idx(v("text"), int(4)), IRCompareOp::Eq, text("W")),
                vec![
                    assign("k", int(7)),
                    while_(
                        and(cmp(v("k"), IRCompareOp::Lt, v("n")), digit_at(v("k"))),
                        vec![inc("k", int(1))],
                    ),
                    if_(cmp(v("k"), IRCompareOp::Lt, int(9)), vec![ret_val(v("k"))]),
                    if_(
                        cmp(modulo(v("k"), int(2)), IRCompareOp::Eq, int(0)),
                        vec![ret_val(int(7))],
                    ),
                    ret_val(int(8)),
                ],
            ),
            ret_val(int(8)),
        ],
    )
}

/// `strptime`, as CPython's `_strptime` reads a string: the format becomes
/// tokens, the tokens are matched as its regular expression would match them
/// (each directive's alternatives in the expression's order, backtracking
/// when a later token fails), and the matched text is then read field by
/// field.
fn strptime_functions() -> Vec<IRFunction> {
    let ls = || list_of(s());
    let li = || list_of(i());
    let mut composite = Vec::new();
    for (k, spelled) in STRPTIME_COMPOSITES {
        composite.push(if_(
            cmp(v("k"), IRCompareOp::Eq, text(&k.to_string())),
            vec![
                for_("t", tokens_of(spelled), vec![append("toks", v("t"))]),
                IRStatement::Continue,
            ],
        ));
    }
    let mut tokenize_loop = vec![
        assign_typed("c", idx(v("fmt"), v("i")), &s()),
        if_(
            isspace(v("c")),
            vec![
                while_(
                    and(
                        cmp(v("i"), IRCompareOp::Lt, v("n")),
                        isspace(idx(v("fmt"), v("i"))),
                    ),
                    vec![inc("i", int(1))],
                ),
                append("toks", text(" ")),
                IRStatement::Continue,
            ],
        ),
        inc("i", int(1)),
        if_(
            cmp(v("c"), IRCompareOp::NotEq, text("%")),
            vec![append("toks", v("c")), IRStatement::Continue],
        ),
        if_(
            or(
                cmp(v("i"), IRCompareOp::GtE, v("n")),
                isspace(idx(v("fmt"), v("i"))),
            ),
            vec![raise_expr(
                "ValueError",
                concat(vec![text("stray % in format '"), v("fmt"), text("'")]),
            )],
        ),
        assign_typed("k", idx(v("fmt"), v("i")), &s()),
        inc("i", int(1)),
        if_(
            cmp(v("k"), IRCompareOp::Eq, text("%")),
            vec![append("toks", text("%")), IRStatement::Continue],
        ),
    ];
    tokenize_loop.extend(composite);
    tokenize_loop.push(if_(
        cmp(v("k"), IRCompareOp::In, text(STRPTIME_DIRECTIVES)),
        vec![
            append("toks", bin(text("%"), IROp::Add, v("k"))),
            IRStatement::Continue,
        ],
    ));
    tokenize_loop.push(raise_expr(
        "ValueError",
        concat(vec![
            text("'"),
            v("k"),
            text("' is a bad directive in format '"),
            v("fmt"),
            text("'"),
        ]),
    ));

    // The ends a token can match at `j`, in the order the expression tries
    // them.
    let numeric = |names: &[&str], maxw: i32, minw: i32, lo: i32, hi: i32, space: Option<i32>| {
        let cond = names
            .iter()
            .map(|n| cmp(v("t"), IRCompareOp::Eq, text(n)))
            .reduce(or)
            .expect("a directive");
        if_(
            cond,
            vec![ret_val(fcall(
                "numeric_ends",
                vec![
                    v("data"),
                    v("j"),
                    int(maxw),
                    int(minw),
                    int(lo),
                    int(hi),
                    int(space.unwrap_or(-1)),
                ],
            ))],
        )
    };
    let named = |names: &[&str], list: IRExpr| {
        let cond = names
            .iter()
            .map(|n| cmp(v("t"), IRCompareOp::Eq, text(n)))
            .reduce(or)
            .expect("a directive");
        if_(
            cond,
            vec![ret_val(fcall("name_ends", vec![v("data"), v("j"), list]))],
        )
    };
    let candidates_body = vec![
        assign("m", len(v("data"))),
        assign_typed("out", IRExpr::ListLiteral(Vec::new()), &li()),
        if_(
            cmp(v("t"), IRCompareOp::Eq, text(" ")),
            vec![
                assign("k", v("j")),
                while_(
                    and(
                        cmp(v("k"), IRCompareOp::Lt, v("m")),
                        isspace(idx(v("data"), v("k"))),
                    ),
                    vec![inc("k", int(1))],
                ),
                while_(
                    cmp(v("k"), IRCompareOp::Gt, v("j")),
                    vec![append("out", v("k")), inc("k", int(-1))],
                ),
                ret_val(v("out")),
            ],
        ),
        if_(
            cmp(len(v("t")), IRCompareOp::Eq, int(1)),
            vec![
                if_(
                    and(
                        cmp(v("j"), IRCompareOp::Lt, v("m")),
                        cmp(
                            lower(idx(v("data"), v("j"))),
                            IRCompareOp::Eq,
                            lower(v("t")),
                        ),
                    ),
                    vec![append("out", bin(v("j"), IROp::Add, int(1)))],
                ),
                ret_val(v("out")),
            ],
        ),
        numeric(&["%d", "%e"], 2, 1, 1, 31, Some(1)),
        numeric(&["%H", "%k"], 2, 1, 0, 23, Some(0)),
        numeric(&["%I", "%l"], 2, 1, 1, 12, Some(1)),
        numeric(&["%j"], 3, 1, 1, 366, None),
        numeric(&["%m"], 2, 1, 1, 12, None),
        numeric(&["%M"], 2, 1, 0, 59, None),
        numeric(&["%S"], 2, 1, 0, 61, None),
        numeric(&["%U", "%W"], 2, 1, 0, 53, None),
        numeric(&["%w"], 1, 1, 0, 6, None),
        numeric(&["%u"], 1, 1, 1, 7, None),
        numeric(&["%y"], 2, 2, 0, 99, None),
        numeric(&["%Y"], 4, 4, 0, 9999, None),
        numeric(&["%f"], 6, 1, 0, 999999, None),
        named(&["%B"], lowered(&MONTHS, false)),
        named(&["%b"], lowered(&MONTHS, true)),
        named(&["%A"], lowered(&WEEKDAYS, false)),
        named(&["%a"], lowered(&WEEKDAYS, true)),
        named(
            &["%p", "%P"],
            IRExpr::ListLiteral(vec![text("am"), text("pm")]),
        ),
        named(&["%Z"], IRExpr::ListLiteral(vec![text("utc"), text("gmt")])),
        // `[+-]\d\d:?[0-5]\d(:?[0-5]\d(\.\d{1,6})?)?|(?-i:Z)`: the optional
        // parts present first, then without them, then `Z`.
        ret_val(fcall("offset_ends", vec![v("data"), v("j")])),
    ];
    let digit = |e: IRExpr| {
        and(
            cmp(e.clone(), IRCompareOp::GtE, text("0")),
            cmp(e, IRCompareOp::LtE, text("9")),
        )
    };
    let sixty = |at: IRExpr| {
        and(
            cmp(bin(at.clone(), IROp::Add, int(2)), IRCompareOp::LtE, v("m")),
            cmp(
                fcall(
                    "digits",
                    vec![v("data"), at.clone(), bin(at, IROp::Add, int(2))],
                ),
                IRCompareOp::GtE,
                int(0),
            ),
        )
    };
    let below_sixty = |at: IRExpr| {
        and(
            sixty(at.clone()),
            cmp(idx(v("data"), at), IRCompareOp::LtE, text("5")),
        )
    };
    let offset_ends = vec![
        assign("m", len(v("data"))),
        assign_typed("out", IRExpr::ListLiteral(Vec::new()), &li()),
        if_(
            and(
                and(
                    cmp(v("j"), IRCompareOp::Lt, v("m")),
                    or(
                        cmp(idx(v("data"), v("j")), IRCompareOp::Eq, text("+")),
                        cmp(idx(v("data"), v("j")), IRCompareOp::Eq, text("-")),
                    ),
                ),
                sixty(bin(v("j"), IROp::Add, int(1))),
            ),
            vec![
                assign("p", bin(v("j"), IROp::Add, int(3))),
                if_(
                    and(
                        cmp(v("p"), IRCompareOp::Lt, v("m")),
                        cmp(idx(v("data"), v("p")), IRCompareOp::Eq, text(":")),
                    ),
                    vec![inc("p", int(1))],
                ),
                if_(
                    below_sixty(v("p")),
                    vec![
                        assign("q", bin(v("p"), IROp::Add, int(2))),
                        assign("r", v("q")),
                        if_(
                            and(
                                cmp(v("r"), IRCompareOp::Lt, v("m")),
                                cmp(idx(v("data"), v("r")), IRCompareOp::Eq, text(":")),
                            ),
                            vec![inc("r", int(1))],
                        ),
                        if_(
                            below_sixty(v("r")),
                            vec![
                                assign("e", bin(v("r"), IROp::Add, int(2))),
                                if_(
                                    and(
                                        and(
                                            cmp(
                                                bin(v("e"), IROp::Add, int(1)),
                                                IRCompareOp::Lt,
                                                v("m"),
                                            ),
                                            cmp(idx(v("data"), v("e")), IRCompareOp::Eq, text(".")),
                                        ),
                                        digit(idx(v("data"), bin(v("e"), IROp::Add, int(1)))),
                                    ),
                                    vec![
                                        assign("w", int(0)),
                                        while_(
                                            and(
                                                and(
                                                    cmp(v("w"), IRCompareOp::Lt, int(6)),
                                                    cmp(
                                                        bin(
                                                            bin(v("e"), IROp::Add, int(1)),
                                                            IROp::Add,
                                                            v("w"),
                                                        ),
                                                        IRCompareOp::Lt,
                                                        v("m"),
                                                    ),
                                                ),
                                                digit(idx(
                                                    v("data"),
                                                    bin(
                                                        bin(v("e"), IROp::Add, int(1)),
                                                        IROp::Add,
                                                        v("w"),
                                                    ),
                                                )),
                                            ),
                                            vec![inc("w", int(1))],
                                        ),
                                        while_(
                                            cmp(v("w"), IRCompareOp::GtE, int(1)),
                                            vec![
                                                append(
                                                    "out",
                                                    bin(
                                                        bin(v("e"), IROp::Add, int(1)),
                                                        IROp::Add,
                                                        v("w"),
                                                    ),
                                                ),
                                                inc("w", int(-1)),
                                            ],
                                        ),
                                    ],
                                ),
                                append("out", v("e")),
                            ],
                        ),
                        append("out", v("q")),
                    ],
                ),
            ],
        ),
        if_(
            and(
                cmp(v("j"), IRCompareOp::Lt, v("m")),
                cmp(idx(v("data"), v("j")), IRCompareOp::Eq, text("Z")),
            ),
            vec![append("out", bin(v("j"), IROp::Add, int(1)))],
        ),
        ret_val(v("out")),
    ];
    let numeric_ends = vec![
        assign("m", len(v("data"))),
        assign_typed("out", IRExpr::ListLiteral(Vec::new()), &li()),
        assign("w", v("maxw")),
        while_(
            cmp(v("w"), IRCompareOp::GtE, v("minw")),
            vec![
                assign(
                    "n",
                    fcall(
                        "digits",
                        vec![v("data"), v("j"), bin(v("j"), IROp::Add, v("w"))],
                    ),
                ),
                if_(
                    and(
                        cmp(v("n"), IRCompareOp::GtE, v("lo")),
                        cmp(v("n"), IRCompareOp::LtE, v("hi")),
                    ),
                    vec![append("out", bin(v("j"), IROp::Add, v("w")))],
                ),
                inc("w", int(-1)),
            ],
        ),
        // ` [1-9]` and ` \d`, the forms `%c`'s day and hour take.
        if_(
            and(
                and(
                    cmp(v("space"), IRCompareOp::GtE, int(0)),
                    cmp(bin(v("j"), IROp::Add, int(2)), IRCompareOp::LtE, v("m")),
                ),
                cmp(idx(v("data"), v("j")), IRCompareOp::Eq, text(" ")),
            ),
            vec![
                assign(
                    "n",
                    fcall(
                        "digits",
                        vec![
                            v("data"),
                            bin(v("j"), IROp::Add, int(1)),
                            bin(v("j"), IROp::Add, int(2)),
                        ],
                    ),
                ),
                if_(
                    cmp(v("n"), IRCompareOp::GtE, v("space")),
                    vec![append("out", bin(v("j"), IROp::Add, int(2)))],
                ),
            ],
        ),
        ret_val(v("out")),
    ];
    let name_ends = vec![
        assign_typed("out", IRExpr::ListLiteral(Vec::new()), &li()),
        for_(
            "name",
            v("names"),
            vec![if_(
                cmp(
                    lower(slice(
                        v("data"),
                        Some(v("j")),
                        Some(bin(v("j"), IROp::Add, len(v("name")))),
                    )),
                    IRCompareOp::Eq,
                    v("name"),
                ),
                vec![append("out", bin(v("j"), IROp::Add, len(v("name"))))],
            )],
        ),
        ret_val(v("out")),
    ];
    let matcher = vec![
        if_(
            cmp(v("ti"), IRCompareOp::Eq, len(v("toks"))),
            vec![ret_val(v("j"))],
        ),
        for_(
            "e",
            fcall("ends", vec![v("data"), idx(v("toks"), v("ti")), v("j")]),
            vec![
                assign(
                    "r",
                    fcall(
                        "match",
                        vec![
                            v("data"),
                            v("toks"),
                            bin(v("ti"), IROp::Add, int(1)),
                            v("e"),
                            v("starts"),
                            v("stops"),
                        ],
                    ),
                ),
                if_(
                    cmp(v("r"), IRCompareOp::GtE, int(0)),
                    vec![
                        set_item(v("starts"), v("ti"), v("j")),
                        set_item(v("stops"), v("ti"), v("e")),
                        ret_val(v("r")),
                    ],
                ),
            ],
        ),
        ret_val(int(-1)),
    ];
    vec![
        function(
            "tokens",
            vec![param("fmt", &s())],
            ls(),
            vec![
                assign_typed("toks", IRExpr::ListLiteral(Vec::new()), &ls()),
                assign("i", int(0)),
                assign("n", len(v("fmt"))),
                while_(cmp(v("i"), IRCompareOp::Lt, v("n")), tokenize_loop),
                ret_val(v("toks")),
            ],
        ),
        function(
            "numeric_ends",
            vec![
                param("data", &s()),
                param("j", &i()),
                param("maxw", &i()),
                param("minw", &i()),
                param("lo", &i()),
                param("hi", &i()),
                param("space", &i()),
            ],
            li(),
            numeric_ends,
        ),
        function(
            "name_ends",
            vec![param("data", &s()), param("j", &i()), param("names", &ls())],
            li(),
            name_ends,
        ),
        function(
            "offset_ends",
            vec![param("data", &s()), param("j", &i())],
            li(),
            offset_ends,
        ),
        function(
            "ends",
            vec![param("data", &s()), param("t", &s()), param("j", &i())],
            li(),
            candidates_body,
        ),
        function(
            "match",
            vec![
                param("data", &s()),
                param("toks", &ls()),
                param("ti", &i()),
                param("j", &i()),
                param("starts", &li()),
                param("stops", &li()),
            ],
            i(),
            matcher,
        ),
        function(
            "julian_of_week",
            vec![
                param("year", &i()),
                param("week", &i()),
                param("weekday", &i()),
                param("monday_first", &b()),
            ],
            i(),
            vec![
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
                assign("dow", v("weekday")),
                if_(
                    not(v("monday_first")),
                    vec![
                        assign("first", modulo(bin(v("first"), IROp::Add, int(1)), int(7))),
                        assign("dow", modulo(bin(v("dow"), IROp::Add, int(1)), int(7))),
                    ],
                ),
                if_(
                    cmp(v("week"), IRCompareOp::Eq, int(0)),
                    vec![ret_val(bin(
                        bin(int(1), IROp::Add, v("dow")),
                        IROp::Sub,
                        v("first"),
                    ))],
                ),
                ret_val(bin(
                    bin(
                        bin(
                            int(1),
                            IROp::Add,
                            modulo(bin(int(7), IROp::Sub, v("first")), int(7)),
                        ),
                        IROp::Add,
                        bin(int(7), IROp::Mul, bin(v("week"), IROp::Sub, int(1))),
                    ),
                    IROp::Add,
                    v("dow"),
                )),
            ],
        ),
        strptime(),
    ]
}

fn strptime() -> IRFunction {
    let piece = || {
        slice(
            v("data"),
            Some(idx(v("starts"), v("ti"))),
            Some(idx(v("stops"), v("ti"))),
        )
    };
    let on = |names: &[&str], then: Vec<IRStatement>| {
        let cond = names
            .iter()
            .map(|n| cmp(v("t"), IRCompareOp::Eq, text(n)))
            .reduce(or)
            .expect("a directive");
        let mut then = then;
        then.push(IRStatement::Continue);
        if_(cond, then)
    };
    let as_int = || call("int", vec![v("g")]);
    let index_in = |list: IRExpr| mcall(list, "index", vec![lower(v("g"))]);
    let fields = vec![
        assign_typed("t", idx(v("toks"), v("ti")), &s()),
        if_(
            cmp(len(v("t")), IRCompareOp::NotEq, int(2)),
            vec![IRStatement::Continue],
        ),
        assign_typed("g", piece(), &s()),
        on(
            &["%y"],
            vec![
                assign("year", as_int()),
                assign(
                    "year",
                    bin(
                        v("year"),
                        IROp::Add,
                        ifexp(
                            cmp(v("year"), IRCompareOp::LtE, int(68)),
                            int(2000),
                            int(1900),
                        ),
                    ),
                ),
                assign("dated", boolean(true)),
            ],
        ),
        on(
            &["%Y"],
            vec![assign("year", as_int()), assign("dated", boolean(true))],
        ),
        on(&["%m"], vec![assign("month", as_int())]),
        on(
            &["%B"],
            vec![assign(
                "month",
                bin(index_in(lowered(&MONTHS, false)), IROp::Add, int(1)),
            )],
        ),
        on(
            &["%b"],
            vec![assign(
                "month",
                bin(index_in(lowered(&MONTHS, true)), IROp::Add, int(1)),
            )],
        ),
        on(&["%d", "%e"], vec![assign("day", as_int())]),
        on(&["%H", "%k"], vec![assign("hour", as_int())]),
        on(
            &["%I", "%l"],
            vec![
                assign("hour", as_int()),
                if_else(
                    or(
                        cmp(v("ampm"), IRCompareOp::Eq, text("")),
                        cmp(v("ampm"), IRCompareOp::Eq, text("am")),
                    ),
                    vec![if_(
                        cmp(v("hour"), IRCompareOp::Eq, int(12)),
                        vec![assign("hour", int(0))],
                    )],
                    vec![if_(
                        cmp(v("hour"), IRCompareOp::NotEq, int(12)),
                        vec![inc("hour", int(12))],
                    )],
                ),
            ],
        ),
        on(&["%M"], vec![assign("minute", as_int())]),
        on(&["%S"], vec![assign("second", as_int())]),
        on(
            &["%f"],
            vec![assign(
                "frac",
                call(
                    "int",
                    vec![bin(
                        v("g"),
                        IROp::Add,
                        bin(text("0"), IROp::Mul, bin(int(6), IROp::Sub, len(v("g")))),
                    )],
                ),
            )],
        ),
        on(
            &["%A"],
            vec![assign("weekday", index_in(lowered(&WEEKDAYS, false)))],
        ),
        on(
            &["%a"],
            vec![assign("weekday", index_in(lowered(&WEEKDAYS, true)))],
        ),
        on(
            &["%w"],
            vec![
                assign("weekday", as_int()),
                assign(
                    "weekday",
                    ifexp(
                        cmp(v("weekday"), IRCompareOp::Eq, int(0)),
                        int(6),
                        bin(v("weekday"), IROp::Sub, int(1)),
                    ),
                ),
            ],
        ),
        on(
            &["%u"],
            vec![assign("weekday", bin(as_int(), IROp::Sub, int(1)))],
        ),
        on(&["%j"], vec![assign("julian", as_int())]),
        on(
            &["%U", "%W"],
            vec![
                assign("week", as_int()),
                assign("monday_first", cmp(v("t"), IRCompareOp::Eq, text("%W"))),
            ],
        ),
        on(
            &["%z"],
            vec![
                assign("aware", int(1)),
                if_(
                    cmp(v("g"), IRCompareOp::Eq, text("Z")),
                    vec![IRStatement::Continue],
                ),
                assign_typed("z", v("g"), &s()),
                if_(
                    cmp(idx(v("z"), int(3)), IRCompareOp::Eq, text(":")),
                    vec![
                        assign(
                            "z",
                            bin(
                                slice(v("z"), None, Some(int(3))),
                                IROp::Add,
                                slice(v("z"), Some(int(4)), None),
                            ),
                        ),
                        if_(
                            cmp(len(v("z")), IRCompareOp::Gt, int(5)),
                            vec![
                                if_(
                                    cmp(idx(v("z"), int(5)), IRCompareOp::NotEq, text(":")),
                                    vec![raise_expr(
                                        "ValueError",
                                        bin(text("Inconsistent use of : in "), IROp::Add, v("g")),
                                    )],
                                ),
                                assign(
                                    "z",
                                    bin(
                                        slice(v("z"), None, Some(int(5))),
                                        IROp::Add,
                                        slice(v("z"), Some(int(6)), None),
                                    ),
                                ),
                            ],
                        ),
                    ],
                ),
                assign_typed(
                    "secs",
                    bin(
                        bin(
                            bin(
                                call("int", vec![slice(v("z"), Some(int(1)), Some(int(3)))]),
                                IROp::Mul,
                                int(3600),
                            ),
                            IROp::Add,
                            bin(
                                call("int", vec![slice(v("z"), Some(int(3)), Some(int(5)))]),
                                IROp::Mul,
                                int(60),
                            ),
                        ),
                        IROp::Add,
                        ifexp(
                            cmp(len(v("z")), IRCompareOp::GtE, int(7)),
                            call("int", vec![slice(v("z"), Some(int(5)), Some(int(7)))]),
                            int(0),
                        ),
                    ),
                    &i(),
                ),
                assign_typed("rest", slice(v("z"), Some(int(8)), None), &s()),
                assign(
                    "zfrac",
                    call(
                        "int",
                        vec![bin(
                            v("rest"),
                            IROp::Add,
                            bin(text("0"), IROp::Mul, bin(int(6), IROp::Sub, len(v("rest")))),
                        )],
                    ),
                ),
                if_(
                    cmp(idx(v("z"), int(0)), IRCompareOp::Eq, text("-")),
                    vec![
                        assign("secs", neg(v("secs"))),
                        assign("zfrac", neg(v("zfrac"))),
                    ],
                ),
                assign("off_s", call("float", vec![v("secs")])),
                assign("off_us", call("float", vec![v("zfrac")])),
            ],
        ),
        on(&["%Z"], vec![assign("zname", v("g"))]),
    ];
    let mismatch = || {
        raise_expr(
            "ValueError",
            concat(vec![
                text("time data "),
                repr_of(v("data")),
                text(" does not match format "),
                repr_of(v("fmt")),
            ]),
        )
    };
    function(
        "strptime",
        vec![param("data", &s()), param("fmt", &s())],
        strptime_result(),
        vec![
            assign_typed("toks", fcall("tokens", vec![v("fmt")]), &list_of(s())),
            assign_typed(
                "starts",
                bin(IRExpr::ListLiteral(vec![int(0)]), IROp::Mul, len(v("toks"))),
                &list_of(i()),
            ),
            assign_typed(
                "stops",
                bin(IRExpr::ListLiteral(vec![int(0)]), IROp::Mul, len(v("toks"))),
                &list_of(i()),
            ),
            assign(
                "end",
                fcall(
                    "match",
                    vec![
                        v("data"),
                        v("toks"),
                        int(0),
                        int(0),
                        v("starts"),
                        v("stops"),
                    ],
                ),
            ),
            if_(cmp(v("end"), IRCompareOp::Lt, int(0)), vec![mismatch()]),
            if_(
                cmp(v("end"), IRCompareOp::NotEq, len(v("data"))),
                vec![raise_expr(
                    "ValueError",
                    bin(
                        text("unconverted data remains: "),
                        IROp::Add,
                        slice(v("data"), Some(v("end")), None),
                    ),
                )],
            ),
            assign("year", int(1900)),
            assign("dated", boolean(false)),
            assign("month", int(1)),
            assign("day", int(1)),
            assign("hour", int(0)),
            assign("minute", int(0)),
            assign("second", int(0)),
            assign("frac", int(0)),
            assign("weekday", int(-1)),
            assign("julian", int(-1)),
            assign("week", int(-1)),
            assign("monday_first", boolean(false)),
            assign("aware", int(0)),
            assign_typed("off_s", float(0.0), &f()),
            assign_typed("off_us", float(0.0), &f()),
            assign_typed("zname", text(""), &s()),
            // `%p`, which `%I` reads wherever it is.
            assign_typed("ampm", text(""), &s()),
            for_(
                "ti",
                range(len(v("toks"))),
                vec![if_(
                    or(
                        cmp(idx(v("toks"), v("ti")), IRCompareOp::Eq, text("%p")),
                        cmp(idx(v("toks"), v("ti")), IRCompareOp::Eq, text("%P")),
                    ),
                    vec![assign("ampm", lower(piece()))],
                )],
            ),
            for_("ti", range(len(v("toks"))), fields),
            assign("leap_fix", boolean(false)),
            if_(
                not(v("dated")),
                vec![if_(
                    and(
                        cmp(v("month"), IRCompareOp::Eq, int(2)),
                        cmp(v("day"), IRCompareOp::Eq, int(29)),
                    ),
                    vec![assign("year", int(1904)), assign("leap_fix", boolean(true))],
                )],
            ),
            if_(
                and(
                    cmp(v("julian"), IRCompareOp::Lt, int(0)),
                    and(
                        cmp(v("weekday"), IRCompareOp::GtE, int(0)),
                        cmp(v("week"), IRCompareOp::GtE, int(0)),
                    ),
                ),
                vec![
                    assign(
                        "julian",
                        fcall(
                            "julian_of_week",
                            vec![v("year"), v("week"), v("weekday"), v("monday_first")],
                        ),
                    ),
                    if_(
                        cmp(v("julian"), IRCompareOp::LtE, int(0)),
                        vec![
                            inc("year", int(-1)),
                            inc(
                                "julian",
                                ifexp(fcall("is_leap", vec![v("year")]), int(366), int(365)),
                            ),
                        ],
                    ),
                ],
            ),
            if_else(
                cmp(v("julian"), IRCompareOp::Lt, int(0)),
                vec![expr_stmt(fcall(
                    "check_date",
                    vec![v("year"), v("month"), v("day")],
                ))],
                vec![
                    expr_stmt(fcall("check_date", vec![v("year"), int(1), int(1)])),
                    assign_typed(
                        "found",
                        mcall(
                            v(&class_name("date")),
                            "fromordinal",
                            vec![bin(
                                bin(v("julian"), IROp::Sub, int(1)),
                                IROp::Add,
                                fcall("ymd2ord", vec![v("year"), int(1), int(1)]),
                            )],
                        ),
                        &class_ty("date"),
                    ),
                    assign("year", attr(v("found"), "year")),
                    assign("month", attr(v("found"), "month")),
                    assign("day", attr(v("found"), "day")),
                ],
            ),
            if_(v("leap_fix"), vec![assign("year", int(1900))]),
            ret_val(IRExpr::TupleLiteral(vec![
                v("year"),
                v("month"),
                v("day"),
                v("hour"),
                v("minute"),
                v("second"),
                v("frac"),
                v("aware"),
                v("off_s"),
                v("off_us"),
                v("zname"),
            ])),
        ],
    )
}
