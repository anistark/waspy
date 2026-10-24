//! The proleptic Gregorian calendar, as CPython's `datetime` computes it:
//! `_is_leap`, `_days_before_year`, `_days_in_month`, `_days_before_month`,
//! `_ymd2ord`, `_ord2ymd`, and `_isoweek1monday`, each a function of the
//! module (`__datetime_<name>`).

use super::*;

pub(super) fn functions() -> Vec<IRFunction> {
    let i = IRType::Int;
    let days_in_month_table = || {
        IRExpr::ListLiteral(
            [-1, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
                .into_iter()
                .map(int)
                .collect(),
        )
    };
    let days_before_month_table = || {
        IRExpr::ListLiteral(
            [-1, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
                .into_iter()
                .map(int)
                .collect(),
        )
    };
    let leap = |y: IRExpr| fcall("is_leap", vec![y]);
    vec![
        // year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
        function(
            "is_leap",
            vec![param("year", &i)],
            IRType::Bool,
            vec![ret_val(and(
                cmp(modulo(v("year"), int(4)), IRCompareOp::Eq, int(0)),
                or(
                    cmp(modulo(v("year"), int(100)), IRCompareOp::NotEq, int(0)),
                    cmp(modulo(v("year"), int(400)), IRCompareOp::Eq, int(0)),
                ),
            ))],
        ),
        function(
            "days_before_year",
            vec![param("year", &i)],
            i.clone(),
            vec![
                assign("y", bin(v("year"), IROp::Sub, int(1))),
                ret_val(bin(
                    bin(
                        bin(
                            bin(v("y"), IROp::Mul, int(365)),
                            IROp::Add,
                            floordiv(v("y"), int(4)),
                        ),
                        IROp::Sub,
                        floordiv(v("y"), int(100)),
                    ),
                    IROp::Add,
                    floordiv(v("y"), int(400)),
                )),
            ],
        ),
        function(
            "days_in_month",
            vec![param("year", &i), param("month", &i)],
            i.clone(),
            vec![
                if_(
                    and(cmp(v("month"), IRCompareOp::Eq, int(2)), leap(v("year"))),
                    vec![ret_val(int(29))],
                ),
                ret_val(idx(days_in_month_table(), v("month"))),
            ],
        ),
        function(
            "days_before_month",
            vec![param("year", &i), param("month", &i)],
            i.clone(),
            vec![
                assign("before", idx(days_before_month_table(), v("month"))),
                if_(
                    and(cmp(v("month"), IRCompareOp::Gt, int(2)), leap(v("year"))),
                    vec![add_to("before", int(1))],
                ),
                ret_val(v("before")),
            ],
        ),
        function(
            "ymd2ord",
            vec![param("year", &i), param("month", &i), param("day", &i)],
            i.clone(),
            vec![ret_val(bin(
                bin(
                    fcall("days_before_year", vec![v("year")]),
                    IROp::Add,
                    fcall("days_before_month", vec![v("year"), v("month")]),
                ),
                IROp::Add,
                v("day"),
            ))],
        ),
        // The year, month, and day of ordinal `n` (day 1 is 0001-01-01).
        function(
            "ord2ymd",
            vec![param("n", &i)],
            IRType::Tuple(vec![i.clone(), i.clone(), i.clone()]),
            vec![
                assign("n", bin(v("n"), IROp::Sub, int(1))),
                assign("n400", floordiv(v("n"), int(146097))),
                assign("n", modulo(v("n"), int(146097))),
                assign(
                    "year",
                    bin(bin(v("n400"), IROp::Mul, int(400)), IROp::Add, int(1)),
                ),
                assign("n100", floordiv(v("n"), int(36524))),
                assign("n", modulo(v("n"), int(36524))),
                assign("n4", floordiv(v("n"), int(1461))),
                assign("n", modulo(v("n"), int(1461))),
                assign("n1", floordiv(v("n"), int(365))),
                assign("n", modulo(v("n"), int(365))),
                add_to(
                    "year",
                    bin(
                        bin(
                            bin(v("n100"), IROp::Mul, int(100)),
                            IROp::Add,
                            bin(v("n4"), IROp::Mul, int(4)),
                        ),
                        IROp::Add,
                        v("n1"),
                    ),
                ),
                if_(
                    or(
                        cmp(v("n1"), IRCompareOp::Eq, int(4)),
                        cmp(v("n100"), IRCompareOp::Eq, int(4)),
                    ),
                    vec![ret_val(IRExpr::TupleLiteral(vec![
                        bin(v("year"), IROp::Sub, int(1)),
                        int(12),
                        int(31),
                    ]))],
                ),
                assign(
                    "leapyear",
                    and(
                        cmp(v("n1"), IRCompareOp::Eq, int(3)),
                        or(
                            cmp(v("n4"), IRCompareOp::NotEq, int(24)),
                            cmp(v("n100"), IRCompareOp::Eq, int(3)),
                        ),
                    ),
                ),
                assign(
                    "month",
                    bin(bin(v("n"), IROp::Add, int(50)), IROp::RShift, int(5)),
                ),
                assign("preceding", idx(days_before_month_table(), v("month"))),
                if_(
                    and(cmp(v("month"), IRCompareOp::Gt, int(2)), v("leapyear")),
                    vec![add_to("preceding", int(1))],
                ),
                if_(
                    cmp(v("preceding"), IRCompareOp::Gt, v("n")),
                    vec![
                        assign("month", bin(v("month"), IROp::Sub, int(1))),
                        assign(
                            "preceding",
                            bin(
                                v("preceding"),
                                IROp::Sub,
                                idx(days_in_month_table(), v("month")),
                            ),
                        ),
                        if_(
                            and(cmp(v("month"), IRCompareOp::Eq, int(2)), v("leapyear")),
                            vec![assign("preceding", bin(v("preceding"), IROp::Sub, int(1)))],
                        ),
                    ],
                ),
                assign("n", bin(v("n"), IROp::Sub, v("preceding"))),
                ret_val(IRExpr::TupleLiteral(vec![
                    v("year"),
                    v("month"),
                    bin(v("n"), IROp::Add, int(1)),
                ])),
            ],
        ),
        // The ordinal of the Monday of ISO week 1 of `year`.
        function(
            "isoweek1monday",
            vec![param("year", &i)],
            i.clone(),
            vec![
                assign(
                    "firstday",
                    fcall("ymd2ord", vec![v("year"), int(1), int(1)]),
                ),
                assign(
                    "firstweekday",
                    modulo(bin(v("firstday"), IROp::Add, int(6)), int(7)),
                ),
                assign(
                    "week1monday",
                    bin(v("firstday"), IROp::Sub, v("firstweekday")),
                ),
                if_(
                    cmp(v("firstweekday"), IRCompareOp::Gt, int(3)),
                    vec![add_to("week1monday", int(7))],
                ),
                ret_val(v("week1monday")),
            ],
        ),
        // CPython's `_check_date_fields`.
        function(
            "check_date",
            vec![param("year", &i), param("month", &i), param("day", &i)],
            IRType::None,
            vec![
                if_(
                    or(
                        cmp(v("year"), IRCompareOp::Lt, int(MINYEAR)),
                        cmp(v("year"), IRCompareOp::Gt, int(MAXYEAR)),
                    ),
                    vec![raise_expr(
                        "ValueError",
                        concat(vec![
                            text("year "),
                            call("str", vec![v("year")]),
                            text(" is out of range"),
                        ]),
                    )],
                ),
                if_(
                    or(
                        cmp(v("month"), IRCompareOp::Lt, int(1)),
                        cmp(v("month"), IRCompareOp::Gt, int(12)),
                    ),
                    vec![raise_expr("ValueError", text("month must be in 1..12"))],
                ),
                if_(
                    or(
                        cmp(v("day"), IRCompareOp::Lt, int(1)),
                        cmp(
                            v("day"),
                            IRCompareOp::Gt,
                            fcall("days_in_month", vec![v("year"), v("month")]),
                        ),
                    ),
                    vec![raise_expr(
                        "ValueError",
                        text("day is out of range for month"),
                    )],
                ),
            ],
        ),
        // CPython's `_check_time_fields`.
        function(
            "check_time",
            vec![
                param("hour", &i),
                param("minute", &i),
                param("second", &i),
                param("microsecond", &i),
                param("fold", &i),
            ],
            IRType::None,
            {
                let range = |name: &str, hi: i32| {
                    if_(
                        or(
                            cmp(v(name), IRCompareOp::Lt, int(0)),
                            cmp(v(name), IRCompareOp::Gt, int(hi)),
                        ),
                        vec![raise_expr(
                            "ValueError",
                            text(&format!("{name} must be in 0..{hi}")),
                        )],
                    )
                };
                vec![
                    range("hour", 23),
                    range("minute", 59),
                    range("second", 59),
                    range("microsecond", 999999),
                    if_(
                        and(
                            cmp(v("fold"), IRCompareOp::NotEq, int(0)),
                            cmp(v("fold"), IRCompareOp::NotEq, int(1)),
                        ),
                        vec![raise_expr("ValueError", text("fold must be either 0 or 1"))],
                    ),
                ]
            },
        ),
        // Round a float to an integral float, halves to even, as `round()`.
        function(
            "round_even",
            vec![param("x", &IRType::Float)],
            IRType::Float,
            vec![
                assign("f", floordiv(v("x"), float(1.0))),
                assign("r", bin(v("x"), IROp::Sub, v("f"))),
                if_(
                    cmp(v("r"), IRCompareOp::Gt, float(0.5)),
                    vec![ret_val(bin(v("f"), IROp::Add, float(1.0)))],
                ),
                if_(
                    cmp(v("r"), IRCompareOp::Lt, float(0.5)),
                    vec![ret_val(v("f"))],
                ),
                if_(
                    cmp(modulo(v("f"), float(2.0)), IRCompareOp::Eq, float(1.0)),
                    vec![ret_val(bin(v("f"), IROp::Add, float(1.0)))],
                ),
                ret_val(v("f")),
            ],
        ),
        // Toward zero, as `math.modf` splits.
        function(
            "trunc",
            vec![param("x", &IRType::Float)],
            IRType::Float,
            vec![
                if_(
                    cmp(v("x"), IRCompareOp::GtE, float(0.0)),
                    vec![ret_val(floordiv(v("x"), float(1.0)))],
                ),
                ret_val(neg(floordiv(neg(v("x")), float(1.0)))),
            ],
        ),
        // `"%0*d" % (width, n)` for a non-negative int.
        function(
            "pad",
            vec![param("n", &i), param("width", &i)],
            IRType::String,
            vec![
                assign_typed("s", call("str", vec![v("n")]), &IRType::String),
                while_(
                    cmp(len(v("s")), IRCompareOp::Lt, v("width")),
                    vec![assign("s", bin(text("0"), IROp::Add, v("s")))],
                ),
                ret_val(v("s")),
            ],
        ),
    ]
}
