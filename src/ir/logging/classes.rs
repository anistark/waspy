//! The `logging` classes, CPython's `logging/__init__.py` written out in IR:
//! the logger hierarchy, the handlers, `Formatter`, `Filter`, `LogRecord`,
//! and the module's state.

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
fn os() -> IRType {
    IRType::Optional(Box::new(IRType::String))
}
pub(super) fn ty(short: &str) -> IRType {
    IRType::Class(class_name(short))
}
fn oty(short: &str) -> IRType {
    IRType::Optional(Box::new(ty(short)))
}
fn list_of(short: &str) -> IRType {
    IRType::List(Box::new(ty(short)))
}
fn me(short: &str) -> IRParam {
    param("self", &ty(short))
}
fn state(var: &str) -> IRExpr {
    attr(v(&class_name("_State")), var)
}
fn set_state(var: &str, value: IRExpr) -> IRStatement {
    set_attr(v(&class_name("_State")), var, value)
}
fn super_init(args: Vec<IRExpr>) -> IRStatement {
    expr_stmt(mcall(call("super", Vec::new()), "__init__", args))
}
fn fcall(name: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&fname(name), args)
}
fn class(
    short: &str,
    base: Option<&str>,
    methods: Vec<IRFunction>,
    vars: Vec<IRVariable>,
) -> IRClass {
    IRClass {
        name: class_name(short),
        bases: base.map(class_name).into_iter().collect(),
        methods,
        class_vars: vars,
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}
fn var(name: &str, value: IRExpr, ty: Option<IRType>) -> IRVariable {
    IRVariable {
        name: name.to_string(),
        value,
        var_type: ty,
    }
}
fn variant(base: &str, ty: &IRType) -> String {
    crate::ir::collections::spec::variant(base, ty).expect("a spelled type")
}
/// `setLevel(int)` and `setLevel(str)`, the latter by its name.
fn set_level(short: &str) -> Vec<IRFunction> {
    vec![
        method(
            &variant("setLevel", &i()),
            vec![me(short), param("level", &i())],
            IRType::None,
            vec![set_attr(v("self"), "level", v("level"))],
        ),
        method(
            &variant("setLevel", &s()),
            vec![me(short), param("level", &s())],
            IRType::None,
            vec![set_attr(
                v("self"),
                "level",
                fcall("level_of", vec![v("level")]),
            )],
        ),
    ]
}
/// `addFilter`, `removeFilter`, and `filter`, as `Filterer` gives them.
fn filterer(short: &str) -> Vec<IRFunction> {
    vec![
        method(
            "addFilter",
            vec![me(short), param("filter", &ty("Filter"))],
            IRType::None,
            vec![if_(
                cmp(v("filter"), IRCompareOp::NotIn, my("filters")),
                vec![expr_stmt(mcall(my("filters"), "append", vec![v("filter")]))],
            )],
        ),
        method(
            "removeFilter",
            vec![me(short), param("filter", &ty("Filter"))],
            IRType::None,
            vec![if_(
                cmp(v("filter"), IRCompareOp::In, my("filters")),
                vec![expr_stmt(mcall(my("filters"), "remove", vec![v("filter")]))],
            )],
        ),
        method(
            "filter",
            vec![me(short), param("record", &ty("LogRecord"))],
            b(),
            vec![
                for_(
                    "f",
                    my("filters"),
                    vec![if_(
                        not(mcall(v("f"), "filter", vec![v("record")])),
                        vec![ret_val(boolean(false))],
                    )],
                ),
                ret_val(boolean(true)),
            ],
        ),
    ]
}

pub(super) fn all() -> Vec<IRClass> {
    vec![
        state_class(),
        logger(),
        root_logger(),
        record(),
        filter(),
        formatter(),
        handler(),
        stream_handler(),
        file_handler(),
        null_handler(),
        stderr_handler(),
    ]
}

/// The module's state, as class variables: they are defined before any of
/// the program's, so a logger made by a module definition finds them.
fn state_class() -> IRClass {
    class(
        "_State",
        None,
        Vec::new(),
        vec![
            var(
                "start_us",
                call(crate::ir::DATETIME_CLOCK_FN, Vec::new()),
                Some(f()),
            ),
            var(
                "root",
                call(&class_name("RootLogger"), vec![int(30)]),
                Some(ty("Logger")),
            ),
            var(
                "loggers",
                IRExpr::DictLiteral(Vec::new()),
                Some(IRType::Dict(Box::new(s()), Box::new(ty("Logger")))),
            ),
            var("disable", int(0), Some(i())),
            var(
                "last_resort",
                call(&class_name("_StderrHandler"), vec![int(30)]),
                Some(ty("Handler")),
            ),
            var(
                "default_formatter",
                call(&class_name("Formatter"), Vec::new()),
                Some(ty("Formatter")),
            ),
        ],
    )
}

fn logger() -> IRClass {
    let me_l = || me("Logger");
    let rec = || param("record", &ty("LogRecord"));
    let mut m = vec![method(
        "__init__",
        vec![
            me_l(),
            param("name", &s()),
            param_default("level", &i(), int(0)),
        ],
        IRType::None,
        vec![
            set_my_typed("name", v("name"), &s()),
            set_my_typed("level", v("level"), &i()),
            set_my_typed("parent", none(), &oty("Logger")),
            set_my_typed("propagate", boolean(true), &b()),
            set_my_typed(
                "handlers",
                IRExpr::ListLiteral(Vec::new()),
                &list_of("Handler"),
            ),
            set_my_typed("disabled", boolean(false), &b()),
            set_my_typed(
                "filters",
                IRExpr::ListLiteral(Vec::new()),
                &list_of("Filter"),
            ),
        ],
    )];
    m.extend(set_level("Logger"));
    m.extend(filterer("Logger"));
    m.push(method(
        "getEffectiveLevel",
        vec![me_l()],
        i(),
        vec![
            assign_typed("logger", v("self"), &oty("Logger")),
            while_(
                cmp(v("logger"), IRCompareOp::IsNot, none()),
                vec![
                    if_(
                        cmp(attr(v("logger"), "level"), IRCompareOp::NotEq, int(0)),
                        vec![ret_val(attr(v("logger"), "level"))],
                    ),
                    assign("logger", attr(v("logger"), "parent")),
                ],
            ),
            ret_val(int(0)),
        ],
    ));
    m.push(method(
        "isEnabledFor",
        vec![me_l(), param("level", &i())],
        b(),
        vec![
            if_(my("disabled"), vec![ret_val(boolean(false))]),
            if_(
                cmp(state("disable"), IRCompareOp::GtE, v("level")),
                vec![ret_val(boolean(false))],
            ),
            ret_val(cmp(
                v("level"),
                IRCompareOp::GtE,
                mcall(v("self"), "getEffectiveLevel", Vec::new()),
            )),
        ],
    ));
    let log_params = || {
        vec![
            me_l(),
            param("level", &i()),
            param("msg", &s()),
            param("text", &s()),
            param("line", &i()),
            param("func", &s()),
        ]
    };
    let make_record = || {
        call(
            &class_name("LogRecord"),
            vec![
                my("name"),
                v("level"),
                v("msg"),
                v("text"),
                v("line"),
                v("func"),
            ],
        )
    };
    m.push(method(
        LOG_METHOD,
        log_params(),
        IRType::None,
        vec![expr_stmt(mcall(v("self"), "handle", vec![make_record()]))],
    ));
    // The form a logging call takes where it is an expression: its message
    // made whether or not it is logged.
    m.push(method(
        LOG_NOW_METHOD,
        log_params(),
        IRType::None,
        vec![if_(
            mcall(v("self"), "isEnabledFor", vec![v("level")]),
            vec![expr_stmt(mcall(v("self"), "handle", vec![make_record()]))],
        )],
    ));
    m.push(method(
        "handle",
        vec![me_l(), rec()],
        IRType::None,
        vec![
            if_(my("disabled"), vec![ret()]),
            if_(
                not(mcall(v("self"), "filter", vec![v("record")])),
                vec![ret()],
            ),
            expr_stmt(mcall(v("self"), "callHandlers", vec![v("record")])),
        ],
    ));
    m.push(method(
        "callHandlers",
        vec![me_l(), rec()],
        IRType::None,
        vec![
            assign_typed("c", v("self"), &oty("Logger")),
            assign("found", int(0)),
            while_(
                cmp(v("c"), IRCompareOp::IsNot, none()),
                vec![
                    for_(
                        "h",
                        attr(v("c"), "handlers"),
                        vec![
                            add_to("found", int(1)),
                            if_(
                                cmp(
                                    attr(v("record"), "levelno"),
                                    IRCompareOp::GtE,
                                    attr(v("h"), "level"),
                                ),
                                vec![expr_stmt(mcall(v("h"), "handle", vec![v("record")]))],
                            ),
                        ],
                    ),
                    if_else(
                        not(attr(v("c"), "propagate")),
                        vec![assign("c", none())],
                        vec![assign("c", attr(v("c"), "parent"))],
                    ),
                ],
            ),
            if_(
                cmp(v("found"), IRCompareOp::Eq, int(0)),
                vec![
                    assign_typed("last", state("last_resort"), &ty("Handler")),
                    if_(
                        cmp(
                            attr(v("record"), "levelno"),
                            IRCompareOp::GtE,
                            attr(v("last"), "level"),
                        ),
                        vec![expr_stmt(mcall(v("last"), "handle", vec![v("record")]))],
                    ),
                ],
            ),
        ],
    ));
    m.push(method(
        "addHandler",
        vec![me_l(), param("hdlr", &ty("Handler"))],
        IRType::None,
        vec![if_(
            cmp(v("hdlr"), IRCompareOp::NotIn, my("handlers")),
            vec![expr_stmt(mcall(my("handlers"), "append", vec![v("hdlr")]))],
        )],
    ));
    m.push(method(
        "removeHandler",
        vec![me_l(), param("hdlr", &ty("Handler"))],
        IRType::None,
        vec![if_(
            cmp(v("hdlr"), IRCompareOp::In, my("handlers")),
            vec![expr_stmt(mcall(my("handlers"), "remove", vec![v("hdlr")]))],
        )],
    ));
    m.push(method(
        "hasHandlers",
        vec![me_l()],
        b(),
        vec![
            assign_typed("c", v("self"), &oty("Logger")),
            while_(
                cmp(v("c"), IRCompareOp::IsNot, none()),
                vec![
                    if_(
                        cmp(len(attr(v("c"), "handlers")), IRCompareOp::Gt, int(0)),
                        vec![ret_val(boolean(true))],
                    ),
                    if_else(
                        not(attr(v("c"), "propagate")),
                        vec![assign("c", none())],
                        vec![assign("c", attr(v("c"), "parent"))],
                    ),
                ],
            ),
            ret_val(boolean(false)),
        ],
    ));
    m.push(method(
        "getChild",
        vec![me_l(), param("suffix", &s())],
        ty("Logger"),
        vec![
            assign_typed("name", v("suffix"), &s()),
            if_(
                cmp(v("self"), IRCompareOp::IsNot, state("root")),
                vec![assign(
                    "name",
                    concat(vec![my("name"), text("."), v("suffix")]),
                )],
            ),
            ret_val(fcall("get_logger", vec![v("name")])),
        ],
    ));
    m.push(method(
        "__repr__",
        vec![me_l()],
        s(),
        vec![ret_val(concat(vec![
            text("<"),
            attr(call("type", vec![v("self")]), "__name__"),
            text(" "),
            my("name"),
            text(" ("),
            fcall(
                "level_name",
                vec![mcall(v("self"), "getEffectiveLevel", Vec::new())],
            ),
            text(")>"),
        ]))],
    ));
    class("Logger", None, m, Vec::new())
}

fn root_logger() -> IRClass {
    class(
        "RootLogger",
        Some("Logger"),
        vec![method(
            "__init__",
            vec![me("RootLogger"), param("level", &i())],
            IRType::None,
            vec![super_init(vec![text("root"), v("level")])],
        )],
        Vec::new(),
    )
}

fn record() -> IRClass {
    let us = || my("_us");
    class(
        "LogRecord",
        None,
        vec![
            method(
                "__init__",
                vec![
                    me("LogRecord"),
                    param("name", &s()),
                    param("level", &i()),
                    param("msg", &s()),
                    param("text", &s()),
                    param("line", &i()),
                    param("func", &s()),
                ],
                IRType::None,
                vec![
                    set_my_typed("name", v("name"), &s()),
                    set_my_typed("msg", v("msg"), &s()),
                    set_my_typed("_text", v("text"), &s()),
                    set_my_typed("levelno", v("level"), &i()),
                    set_my_typed("levelname", fcall("level_name", vec![v("level")]), &s()),
                    set_my_typed("lineno", v("line"), &i()),
                    set_my_typed("funcName", v("func"), &s()),
                    set_my_typed("_us", call(crate::ir::DATETIME_CLOCK_FN, Vec::new()), &f()),
                    set_my_typed("created", bin(us(), IROp::Div, float(1e6)), &f()),
                    set_my_typed(
                        "msecs",
                        bin(
                            bin(
                                bin(us(), IROp::Mod, float(1e6)),
                                IROp::FloorDiv,
                                float(1000.0),
                            ),
                            IROp::Add,
                            float(0.0),
                        ),
                        &f(),
                    ),
                    set_my_typed(
                        "relativeCreated",
                        bin(
                            bin(us(), IROp::Sub, state("start_us")),
                            IROp::Div,
                            float(1000.0),
                        ),
                        &f(),
                    ),
                    // What CPython on a WASI host answers.
                    set_my_typed("process", int(42), &i()),
                    set_my_typed("processName", text("MainProcess"), &s()),
                    set_my_typed("threadName", text("MainThread"), &s()),
                    set_my_typed("taskName", none(), &os()),
                    set_my_typed("message", text(""), &s()),
                    set_my_typed("asctime", text(""), &s()),
                ],
            ),
            method(
                "getMessage",
                vec![me("LogRecord")],
                s(),
                vec![ret_val(my("_text"))],
            ),
        ],
        Vec::new(),
    )
}

fn filter() -> IRClass {
    class(
        "Filter",
        None,
        vec![
            method(
                "__init__",
                vec![me("Filter"), param_default("name", &s(), text(""))],
                IRType::None,
                vec![
                    set_my_typed("name", v("name"), &s()),
                    set_my_typed("nlen", len(v("name")), &i()),
                ],
            ),
            method(
                "filter",
                vec![me("Filter"), param("record", &ty("LogRecord"))],
                b(),
                vec![
                    if_(
                        cmp(my("nlen"), IRCompareOp::Eq, int(0)),
                        vec![ret_val(boolean(true))],
                    ),
                    if_(
                        cmp(my("name"), IRCompareOp::Eq, attr(v("record"), "name")),
                        vec![ret_val(boolean(true))],
                    ),
                    if_(
                        not(mcall(
                            attr(v("record"), "name"),
                            "startswith",
                            vec![my("name")],
                        )),
                        vec![ret_val(boolean(false))],
                    ),
                    ret_val(cmp(
                        idx(attr(v("record"), "name"), my("nlen")),
                        IRCompareOp::Eq,
                        text("."),
                    )),
                ],
            ),
        ],
        Vec::new(),
    )
}

fn formatter() -> IRClass {
    let me_f = || me("Formatter");
    let rec = || param("record", &ty("LogRecord"));
    // The record's time, as the local (UTC) time `strftime` renders.
    let time_fields = vec![
        assign_typed("us", attr(v("record"), "_us"), &f()),
        IRStatement::TupleUnpack {
            targets: vec!["y".into(), "mo".into(), "d".into()],
            value: call(
                &crate::ir::datetime::function_name("ord2ymd"),
                vec![bin(
                    call("int", vec![bin(v("us"), IROp::FloorDiv, float(86400e6))]),
                    IROp::Add,
                    int(crate::ir::datetime::EPOCH_ORDINAL),
                )],
            ),
            starred: None,
        },
        assign(
            "secs",
            call(
                "int",
                vec![bin(
                    bin(v("us"), IROp::Mod, float(86400e6)),
                    IROp::FloorDiv,
                    float(1e6),
                )],
            ),
        ),
    ];
    let strftime = |fmt: IRExpr| {
        call(
            &crate::ir::datetime::function_name("strftime"),
            vec![
                fmt,
                v("y"),
                v("mo"),
                v("d"),
                bin(v("secs"), IROp::FloorDiv, int(3600)),
                bin(bin(v("secs"), IROp::FloorDiv, int(60)), IROp::Mod, int(60)),
                bin(v("secs"), IROp::Mod, int(60)),
                int(0),
                text("+0000"),
                text("UTC"),
            ],
        )
    };
    let mut format_time = time_fields;
    format_time.extend([
        if_(
            cmp(v("datefmt"), IRCompareOp::IsNot, none()),
            vec![ret_val(strftime(v("datefmt")))],
        ),
        assign_typed("t", strftime(my("default_time_format")), &s()),
        assign_typed("msec", my("default_msec_format"), &os()),
        if_(
            cmp(v("msec"), IRCompareOp::Is, none()),
            vec![ret_val(v("t"))],
        ),
        assign_typed("mf", v("msec"), &s()),
        assign_typed(
            "ms",
            call(
                &crate::ir::datetime::function_name("pad"),
                vec![call("int", vec![attr(v("record"), "msecs")]), int(3)],
            ),
            &s(),
        ),
        if_(
            cmp(v("mf"), IRCompareOp::Eq, text("%s,%03d")),
            vec![ret_val(concat(vec![v("t"), text(","), v("ms")]))],
        ),
        if_(
            cmp(v("mf"), IRCompareOp::Eq, text("%s.%03d")),
            vec![ret_val(concat(vec![v("t"), text("."), v("ms")]))],
        ),
        raise_expr(
            "NotImplementedError",
            text("a Formatter's default_msec_format other than '%s,%03d' or '%s.%03d'"),
        ),
    ]);
    let uses_time = or(
        or(
            cmp(text("%(asctime)"), IRCompareOp::In, my("_fmt")),
            cmp(text("{asctime"), IRCompareOp::In, my("_fmt")),
        ),
        cmp(text("$asctime"), IRCompareOp::In, my("_fmt")),
    );
    let uses_time = or(
        uses_time,
        cmp(text("${asctime}"), IRCompareOp::In, my("_fmt")),
    );
    class(
        "Formatter",
        None,
        vec![
            method(
                "__init__",
                vec![
                    me_f(),
                    param_default("fmt", &os(), none()),
                    param_default("datefmt", &os(), none()),
                    param_default("style", &s(), text("%")),
                    param_default("validate", &b(), boolean(true)),
                ],
                IRType::None,
                vec![
                    if_(
                        and(
                            and(
                                cmp(v("style"), IRCompareOp::NotEq, text("%")),
                                cmp(v("style"), IRCompareOp::NotEq, text("{")),
                            ),
                            cmp(v("style"), IRCompareOp::NotEq, text("$")),
                        ),
                        vec![raise_expr(
                            "ValueError",
                            text("Style must be one of: %,{,$"),
                        )],
                    ),
                    assign_typed("f", fcall("default_format", vec![v("style")]), &s()),
                    if_(
                        cmp(v("fmt"), IRCompareOp::IsNot, none()),
                        vec![
                            assign_typed("given", v("fmt"), &s()),
                            if_(
                                cmp(len(v("given")), IRCompareOp::Gt, int(0)),
                                vec![assign("f", v("given"))],
                            ),
                        ],
                    ),
                    set_my_typed("_fmt", v("f"), &s()),
                    set_my_typed("datefmt", v("datefmt"), &os()),
                    set_my_typed("_style", v("style"), &s()),
                    set_my_typed(
                        "_id",
                        fcall("format_id", vec![v("f"), v("style"), v("validate")]),
                        &i(),
                    ),
                    set_my_typed("default_time_format", text("%Y-%m-%d %H:%M:%S"), &s()),
                    set_my_typed("default_msec_format", text("%s,%03d"), &os()),
                ],
            ),
            method("usesTime", vec![me_f()], b(), vec![ret_val(uses_time)]),
            method(
                "formatTime",
                vec![me_f(), rec(), param_default("datefmt", &os(), none())],
                s(),
                format_time,
            ),
            method(
                "formatMessage",
                vec![me_f(), rec()],
                s(),
                vec![ret_val(fcall("render", vec![my("_id"), v("record")]))],
            ),
            method(
                "format",
                vec![me_f(), rec()],
                s(),
                vec![
                    set_attr(
                        v("record"),
                        "message",
                        mcall(v("record"), "getMessage", Vec::new()),
                    ),
                    if_(
                        mcall(v("self"), "usesTime", Vec::new()),
                        vec![set_attr(
                            v("record"),
                            "asctime",
                            mcall(v("self"), "formatTime", vec![v("record"), my("datefmt")]),
                        )],
                    ),
                    ret_val(mcall(v("self"), "formatMessage", vec![v("record")])),
                ],
            ),
        ],
        Vec::new(),
    )
}

fn handler_init() -> Vec<IRStatement> {
    vec![
        set_my_typed("level", v("level"), &i()),
        set_my_typed("formatter", none(), &oty("Formatter")),
        set_my_typed(
            "filters",
            IRExpr::ListLiteral(Vec::new()),
            &list_of("Filter"),
        ),
    ]
}

fn handler() -> IRClass {
    let me_h = || me("Handler");
    let rec = || param("record", &ty("LogRecord"));
    let mut m = vec![method(
        "__init__",
        vec![me_h(), param_default("level", &i(), int(0))],
        IRType::None,
        handler_init(),
    )];
    m.extend(set_level("Handler"));
    m.extend(filterer("Handler"));
    m.extend([
        method(
            "setFormatter",
            vec![me_h(), param("fmt", &oty("Formatter"))],
            IRType::None,
            vec![set_attr(v("self"), "formatter", v("fmt"))],
        ),
        method(
            "format",
            vec![me_h(), rec()],
            s(),
            vec![
                assign_typed("fmt", state("default_formatter"), &ty("Formatter")),
                if_(
                    cmp(my("formatter"), IRCompareOp::IsNot, none()),
                    vec![ret_val(mcall(my("formatter"), "format", vec![v("record")]))],
                ),
                ret_val(mcall(v("fmt"), "format", vec![v("record")])),
            ],
        ),
        method(
            "handle",
            vec![me_h(), rec()],
            b(),
            vec![
                assign("rv", mcall(v("self"), "filter", vec![v("record")])),
                if_(
                    v("rv"),
                    vec![expr_stmt(mcall(v("self"), "emit", vec![v("record")]))],
                ),
                ret_val(v("rv")),
            ],
        ),
        method(
            "emit",
            vec![me_h(), rec()],
            IRType::None,
            vec![raise_expr(
                "NotImplementedError",
                text("emit must be implemented by Handler subclasses"),
            )],
        ),
        method("flush", vec![me_h()], IRType::None, vec![ret()]),
        method("close", vec![me_h()], IRType::None, vec![ret()]),
        method(
            "__repr__",
            vec![me_h()],
            s(),
            vec![ret_val(concat(vec![
                text("<"),
                attr(call("type", vec![v("self")]), "__name__"),
                text(" ("),
                fcall("level_name", vec![my("level")]),
                text(")>"),
            ]))],
        ),
    ]);
    class("Handler", None, m, Vec::new())
}

/// The record, formatted and terminated, written to stream `fd`.
fn write_to(fd: IRExpr) -> Vec<IRStatement> {
    let write = |stream: &str| {
        expr_stmt(mcall(
            attr(v("sys"), stream),
            "write",
            vec![bin(v("msg"), IROp::Add, my("terminator"))],
        ))
    };
    vec![
        assign_typed("msg", mcall(v("self"), "format", vec![v("record")]), &s()),
        if_else(
            cmp(fd, IRCompareOp::Eq, int(1)),
            vec![write("stdout")],
            vec![write("stderr")],
        ),
    ]
}

fn stream_handler() -> IRClass {
    let me_s = || me("StreamHandler");
    class(
        "StreamHandler",
        Some("Handler"),
        vec![
            method(
                "__init__",
                vec![me_s(), param_default("fd", &i(), int(2))],
                IRType::None,
                vec![
                    super_init(vec![int(0)]),
                    set_my_typed("_fd", v("fd"), &i()),
                    set_my_typed("terminator", text("\n"), &s()),
                ],
            ),
            method(
                "emit",
                vec![me_s(), param("record", &ty("LogRecord"))],
                IRType::None,
                write_to(my("_fd")),
            ),
            method(
                "__repr__",
                vec![me_s()],
                s(),
                vec![ret_val(concat(vec![
                    text("<"),
                    attr(call("type", vec![v("self")]), "__name__"),
                    ifexp(
                        cmp(my("_fd"), IRCompareOp::Eq, int(1)),
                        text(" <stdout> ("),
                        text(" <stderr> ("),
                    ),
                    fcall("level_name", vec![my("level")]),
                    text(")>"),
                ]))],
            ),
        ],
        Vec::new(),
    )
}

fn ifexp(c: IRExpr, a: IRExpr, b: IRExpr) -> IRExpr {
    IRExpr::Conditional {
        condition: Box::new(c),
        body: Box::new(a),
        orelse: Box::new(b),
    }
}

/// A `FileHandler` opens its file as CPython does (truncating it for mode
/// "w"), then appends each record, so the file holds what CPython's would.
fn file_handler() -> IRClass {
    let me_fh = || me("FileHandler");
    // `with open(...) as fh:` as the converter writes it out.
    let open = |mode: IRExpr, body: Vec<IRStatement>| {
        let mut statements = vec![assign("fh", call("open", vec![my("_path"), mode]))];
        statements.extend(body);
        statements.push(expr_stmt(mcall(v("fh"), "close", Vec::new())));
        if_(boolean(true), statements)
    };
    class(
        "FileHandler",
        Some("StreamHandler"),
        vec![
            method(
                "__init__",
                vec![
                    me_fh(),
                    param("filename", &s()),
                    param_default("mode", &s(), text("a")),
                    param_default("encoding", &os(), none()),
                    param_default("delay", &b(), boolean(false)),
                ],
                IRType::None,
                vec![
                    super_init(vec![int(0)]),
                    set_my_typed("_path", v("filename"), &s()),
                    set_my_typed("mode", v("mode"), &s()),
                    set_my_typed("_opened", boolean(false), &b()),
                    if_(
                        not(v("delay")),
                        vec![expr_stmt(mcall(v("self"), "_open", Vec::new()))],
                    ),
                ],
            ),
            // Opened as CPython opens it: created, and truncated for "w".
            method(
                "_open",
                vec![me_fh()],
                IRType::None,
                vec![
                    if_else(
                        mcall(my("mode"), "startswith", vec![text("w")]),
                        vec![open(text("w"), Vec::new())],
                        vec![if_else(
                            mcall(my("mode"), "startswith", vec![text("a")]),
                            vec![open(text("a"), Vec::new())],
                            vec![raise_expr(
                                "NotImplementedError",
                                concat(vec![
                                    text("a FileHandler's mode other than 'a' or 'w': "),
                                    call("repr", vec![my("mode")]),
                                ]),
                            )],
                        )],
                    ),
                    set_attr(v("self"), "_opened", boolean(true)),
                ],
            ),
            method(
                "emit",
                vec![me_fh(), param("record", &ty("LogRecord"))],
                IRType::None,
                vec![
                    if_(
                        not(my("_opened")),
                        vec![expr_stmt(mcall(v("self"), "_open", Vec::new()))],
                    ),
                    assign_typed("msg", mcall(v("self"), "format", vec![v("record")]), &s()),
                    open(
                        text("a"),
                        vec![expr_stmt(mcall(
                            v("fh"),
                            "write",
                            vec![bin(v("msg"), IROp::Add, my("terminator"))],
                        ))],
                    ),
                ],
            ),
        ],
        Vec::new(),
    )
}

fn null_handler() -> IRClass {
    class(
        "NullHandler",
        Some("Handler"),
        vec![
            method(
                "handle",
                vec![me("NullHandler"), param("record", &ty("LogRecord"))],
                b(),
                vec![ret_val(boolean(true))],
            ),
            method(
                "emit",
                vec![me("NullHandler"), param("record", &ty("LogRecord"))],
                IRType::None,
                vec![ret()],
            ),
        ],
        Vec::new(),
    )
}

/// `logging.lastResort`: the record's message, to `sys.stderr`, when no
/// handler was found for it.
fn stderr_handler() -> IRClass {
    class(
        "_StderrHandler",
        Some("Handler"),
        vec![
            method(
                "__init__",
                vec![me("_StderrHandler"), param("level", &i())],
                IRType::None,
                vec![
                    super_init(vec![v("level")]),
                    set_my_typed("terminator", text("\n"), &s()),
                ],
            ),
            method(
                "emit",
                vec![me("_StderrHandler"), param("record", &ty("LogRecord"))],
                IRType::None,
                write_to(int(2)),
            ),
        ],
        Vec::new(),
    )
}

/// The module's functions.
pub(super) fn functions() -> Vec<IRFunction> {
    let logger = ty("Logger");
    let loggers = || state("loggers");
    vec![
        function("level_name", vec![param("level", &i())], s(), {
            let mut body: Vec<IRStatement> = [
                (50, "CRITICAL"),
                (40, "ERROR"),
                (30, "WARNING"),
                (20, "INFO"),
                (10, "DEBUG"),
                (0, "NOTSET"),
            ]
            .into_iter()
            .map(|(n, name)| {
                if_(
                    cmp(v("level"), IRCompareOp::Eq, int(n)),
                    vec![ret_val(text(name))],
                )
            })
            .collect();
            body.push(ret_val(bin(
                text("Level "),
                IROp::Add,
                call("str", vec![v("level")]),
            )));
            body
        }),
        function("level_of", vec![param("name", &s())], i(), {
            let mut body: Vec<IRStatement> = [
                ("CRITICAL", 50),
                ("FATAL", 50),
                ("ERROR", 40),
                ("WARNING", 30),
                ("WARN", 30),
                ("INFO", 20),
                ("DEBUG", 10),
                ("NOTSET", 0),
            ]
            .into_iter()
            .map(|(name, n)| {
                if_(
                    cmp(v("name"), IRCompareOp::Eq, text(name)),
                    vec![ret_val(int(n))],
                )
            })
            .collect();
            body.push(raise_expr(
                "ValueError",
                bin(
                    text("Unknown level: "),
                    IROp::Add,
                    call("repr", vec![v("name")]),
                ),
            ));
            body
        }),
        function(
            "default_format",
            vec![param("style", &s())],
            s(),
            vec![
                if_(
                    cmp(v("style"), IRCompareOp::Eq, text("{")),
                    vec![ret_val(text("{message}"))],
                ),
                if_(
                    cmp(v("style"), IRCompareOp::Eq, text("$")),
                    vec![ret_val(text("${message}"))],
                ),
                ret_val(text("%(message)s")),
            ],
        ),
        function(
            "basic_format",
            vec![param("style", &s())],
            s(),
            vec![
                if_(
                    cmp(v("style"), IRCompareOp::Eq, text("{")),
                    vec![ret_val(text("{levelname}:{name}:{message}"))],
                ),
                if_(
                    cmp(v("style"), IRCompareOp::Eq, text("$")),
                    vec![ret_val(text("${levelname}:${name}:${message}"))],
                ),
                ret_val(text(BASIC_FORMAT)),
            ],
        ),
        // `logging.getLogger(name)`: the logger of that name, made the first
        // time, between its nearest existing ancestor and descendants.
        function(
            "get_logger",
            vec![param_default(
                "name",
                &IRType::Optional(Box::new(s())),
                none(),
            )],
            logger.clone(),
            vec![
                if_(
                    cmp(v("name"), IRCompareOp::Is, none()),
                    vec![ret_val(state("root"))],
                ),
                assign_typed("n", v("name"), &s()),
                if_(
                    or(
                        cmp(len(v("n")), IRCompareOp::Eq, int(0)),
                        cmp(v("n"), IRCompareOp::Eq, text("root")),
                    ),
                    vec![ret_val(state("root"))],
                ),
                if_(
                    cmp(v("n"), IRCompareOp::In, loggers()),
                    vec![ret_val(idx(loggers(), v("n")))],
                ),
                assign_typed("lg", call(&class_name("Logger"), vec![v("n")]), &logger),
                // Existing loggers below it now have it as their parent.
                assign("k", len(v("n"))),
                for_(
                    "key",
                    loggers(),
                    vec![
                        assign_typed("c", idx(loggers(), v("key")), &logger),
                        if_(
                            and(
                                mcall(
                                    v("key"),
                                    "startswith",
                                    vec![bin(v("n"), IROp::Add, text("."))],
                                ),
                                cmp(
                                    slice(attr(attr(v("c"), "parent"), "name"), None, Some(v("k"))),
                                    IRCompareOp::NotEq,
                                    v("n"),
                                ),
                            ),
                            vec![set_attr(v("c"), "parent", v("lg"))],
                        ),
                    ],
                ),
                // Its parent is the nearest existing ancestor.
                assign_typed("parent", state("root"), &logger),
                assign("dot", fcall("last_dot", vec![v("n")])),
                while_(
                    cmp(v("dot"), IRCompareOp::Gt, int(0)),
                    vec![
                        assign_typed("sub", slice(v("n"), None, Some(v("dot"))), &s()),
                        if_(
                            cmp(v("sub"), IRCompareOp::In, loggers()),
                            vec![
                                assign("parent", idx(loggers(), v("sub"))),
                                IRStatement::Break,
                            ],
                        ),
                        assign("dot", fcall("last_dot", vec![v("sub")])),
                    ],
                ),
                set_attr(v("lg"), "parent", v("parent")),
                set_item(loggers(), v("n"), v("lg")),
                ret_val(v("lg")),
            ],
        ),
        // The root logger, configured with `basicConfig()`'s defaults when
        // it has no handler, as the module's logging functions do first.
        function(
            "module_root",
            Vec::new(),
            ty("Logger"),
            vec![
                if_(
                    cmp(
                        len(attr(state("root"), "handlers")),
                        IRCompareOp::Eq,
                        int(0),
                    ),
                    vec![expr_stmt(fcall(
                        "basic_config",
                        vec![
                            int(0),
                            boolean(false),
                            text(BASIC_FORMAT),
                            none(),
                            text("%"),
                            int(2),
                            none(),
                            text("a"),
                            boolean(false),
                            IRExpr::ListLiteral(Vec::new()),
                            boolean(false),
                        ],
                    ))],
                ),
                ret_val(state("root")),
            ],
        ),
        function(
            "basic_config",
            vec![
                param("level", &i()),
                param("has_level", &b()),
                param("fmt", &s()),
                param("datefmt", &os()),
                param("style", &s()),
                param("fd", &i()),
                param("filename", &os()),
                param("filemode", &s()),
                param("force", &b()),
                param("handlers", &list_of("Handler")),
                param("has_handlers", &b()),
            ],
            IRType::None,
            vec![
                assign_typed("root", state("root"), &logger),
                if_(
                    v("force"),
                    vec![
                        assign_typed(
                            "old",
                            slice(attr(v("root"), "handlers"), None, None),
                            &list_of("Handler"),
                        ),
                        for_(
                            "h",
                            v("old"),
                            vec![
                                expr_stmt(mcall(v("root"), "removeHandler", vec![v("h")])),
                                expr_stmt(mcall(v("h"), "close", Vec::new())),
                            ],
                        ),
                    ],
                ),
                if_(
                    cmp(len(attr(v("root"), "handlers")), IRCompareOp::Gt, int(0)),
                    vec![ret()],
                ),
                assign_typed("hs", IRExpr::ListLiteral(Vec::new()), &list_of("Handler")),
                if_else(
                    v("has_handlers"),
                    vec![assign("hs", v("handlers"))],
                    vec![
                        assign_typed("path", text(""), &s()),
                        if_(
                            cmp(v("filename"), IRCompareOp::IsNot, none()),
                            vec![assign("path", v("filename"))],
                        ),
                        if_else(
                            cmp(len(v("path")), IRCompareOp::Gt, int(0)),
                            vec![expr_stmt(mcall(
                                v("hs"),
                                "append",
                                vec![call(
                                    &class_name("FileHandler"),
                                    vec![v("path"), v("filemode")],
                                )],
                            ))],
                            vec![expr_stmt(mcall(
                                v("hs"),
                                "append",
                                vec![call(&class_name("StreamHandler"), vec![v("fd")])],
                            ))],
                        ),
                    ],
                ),
                if_(
                    and(
                        and(
                            cmp(v("style"), IRCompareOp::NotEq, text("%")),
                            cmp(v("style"), IRCompareOp::NotEq, text("{")),
                        ),
                        cmp(v("style"), IRCompareOp::NotEq, text("$")),
                    ),
                    vec![raise_expr(
                        "ValueError",
                        text("Style must be one of: %,{,$"),
                    )],
                ),
                assign_typed(
                    "fs",
                    call(
                        &class_name("Formatter"),
                        vec![v("fmt"), v("datefmt"), v("style")],
                    ),
                    &ty("Formatter"),
                ),
                for_(
                    "h",
                    v("hs"),
                    vec![
                        if_(
                            cmp(attr(v("h"), "formatter"), IRCompareOp::Is, none()),
                            vec![expr_stmt(mcall(v("h"), "setFormatter", vec![v("fs")]))],
                        ),
                        expr_stmt(mcall(v("root"), "addHandler", vec![v("h")])),
                    ],
                ),
                if_(
                    v("has_level"),
                    vec![set_attr(v("root"), "level", v("level"))],
                ),
            ],
        ),
        function(
            "disable",
            vec![param("level", &i())],
            IRType::None,
            vec![set_state("disable", v("level"))],
        ),
        function("shutdown", Vec::new(), IRType::None, vec![ret()]),
        // `s.rfind(".")`.
        function(
            "last_dot",
            vec![param("s", &s())],
            i(),
            vec![
                assign("k", bin(len(v("s")), IROp::Sub, int(1))),
                while_(
                    cmp(v("k"), IRCompareOp::GtE, int(0)),
                    vec![
                        if_(
                            cmp(idx(v("s"), v("k")), IRCompareOp::Eq, text(".")),
                            vec![ret_val(v("k"))],
                        ),
                        assign("k", bin(v("k"), IROp::Sub, int(1))),
                    ],
                ),
                ret_val(int(-1)),
            ],
        ),
    ]
}
