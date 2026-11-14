//! `asyncio`: the event loop, futures, and tasks, CPython 3.13's
//! `asyncio/{futures,tasks,base_events,timeouts}.py` written out in IR.
//!
//! The loop is CPython's: a queue of ready callbacks run in turns, where a
//! turn runs exactly the callbacks that were ready when it began, and a heap
//! of timers moved onto the queue as they fall due. A task is a future that
//! steps its coroutine from a callback; a coroutine waiting on a future
//! parks its task on the future, whose completion schedules the task's
//! wakeup. So which task runs when, and what each `print` interleaves with,
//! is CPython's order wherever a program can see it.
//!
//! Time is the host's monotonic clock in nanoseconds, held in a float (exact
//! to 2**53 ns, about 104 days), and a deadline is the time it was set plus
//! the delay rounded to a nanosecond, so equal delays set one after another
//! fall due in the order they were set, as CPython's do (its clock always
//! moves between the two). With nothing ready the loop waits for the next
//! timer through WASI's `poll_oneoff`, and with nothing at all to wait for
//! it waits forever, as CPython's would.
//!
//! A future holds one type of result, so `Future`, `Task`, and the class
//! every coroutine's state class derives from (`_Coro`) are written once per
//! result type a coroutine of the program has: `asyncio.Task[int]`.

use super::build::*;
use crate::ir::collections::spec::encode;
use crate::ir::{IRClass, IRCompareOp, IRExpr, IRFunction, IRModule, IROp, IRStatement, IRType};
use crate::ir::{IRExceptHandler, IRVariable};
use anyhow::Result;
use std::collections::HashMap;

/// The prefix of the runtime's functions; they are internal.
pub const ASYNCIO_FN_PREFIX: &str = "__asyncio_";

/// `__asyncio_clock()`: the host's monotonic clock, in nanoseconds, as a
/// float.
pub const CLOCK_FN: &str = "__asyncio_clock";
/// `__asyncio_ns(seconds)`: a delay in nanoseconds, rounded to the nearest.
pub const NS_FN: &str = "__asyncio_ns";
/// `__asyncio_wait(ns)`: block for that long (WASI's `poll_oneoff`).
pub const WAIT_FN: &str = "__asyncio_wait";
/// `__asyncio_idle()`: block for good, as a loop with nothing to wait for
/// does.
pub const IDLE_FN: &str = "__asyncio_idle";

/// A runtime class's name: `asyncio.<short>`.
pub(crate) fn name(short: &str) -> String {
    format!("asyncio.{short}")
}
/// A runtime function's name.
pub(crate) fn fname(short: &str) -> String {
    format!("{ASYNCIO_FN_PREFIX}{short}")
}
fn spelled(t: &IRType) -> String {
    encode(t).expect("a result type an annotation can name")
}
/// The class every coroutine resolving to `t` derives from.
pub(crate) fn coro_class(t: &IRType) -> String {
    name(&format!("_Coro[{}]", spelled(t)))
}
pub(crate) fn future_class(t: &IRType) -> String {
    name(&format!("Future[{}]", spelled(t)))
}
pub(crate) fn task_class(t: &IRType) -> String {
    name(&format!("Task[{}]", spelled(t)))
}
/// What a step of a coroutine answers: the future it waits on, or None.
pub(crate) fn waited_on() -> IRType {
    IRType::Optional(Box::new(cls("_FutureBase")))
}
/// Whether instances of a class can be awaited.
pub(crate) fn is_awaitable_class(class: &str) -> bool {
    ["Future[", "Task[", "_Coro["]
        .iter()
        .any(|p| class.starts_with(&name(p)))
        || class == name("_Gathering")
}

fn cls(short: &str) -> IRType {
    IRType::Class(name(short))
}
fn opt(t: IRType) -> IRType {
    IRType::Optional(Box::new(t))
}
fn list(t: IRType) -> IRType {
    IRType::List(Box::new(t))
}
fn me(class: &str) -> crate::ir::IRParam {
    param("self", &IRType::Class(class.to_string()))
}
fn loop_var(var: &str) -> IRExpr {
    attr(v(&name("_Loop")), var)
}
fn set_loop(var: &str, value: IRExpr) -> IRStatement {
    set_attr(v(&name("_Loop")), var, value)
}
fn f(short: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&fname(short), args)
}
fn qcall(class: &str, method: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&format!("{class}::{method}"), args)
}
fn eq(l: IRExpr, r: IRExpr) -> IRExpr {
    cmp(l, IRCompareOp::Eq, r)
}
fn ne(l: IRExpr, r: IRExpr) -> IRExpr {
    cmp(l, IRCompareOp::NotEq, r)
}
fn is_none(e: IRExpr) -> IRExpr {
    cmp(e, IRCompareOp::Is, none())
}
fn not_none(e: IRExpr) -> IRExpr {
    cmp(e, IRCompareOp::IsNot, none())
}
fn raise_new(class: &str) -> IRStatement {
    IRStatement::Raise {
        exception: Some(call(class, vec![])),
        cause: None,
    }
}
fn class(short: &str, base: Option<String>, methods: Vec<IRFunction>) -> IRClass {
    IRClass {
        name: name(short),
        bases: base.into_iter().collect(),
        methods,
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}
fn function(
    short: &str,
    params: Vec<crate::ir::IRParam>,
    ret: IRType,
    body: Vec<IRStatement>,
) -> IRFunction {
    method(&fname(short), params, ret, body)
}
fn stop_check() -> IRExpr {
    call(crate::ir::STOP_CHECK_FN, vec![])
}
fn write_err(text_expr: IRExpr) -> IRStatement {
    expr_stmt(mcall(attr(v("sys"), "stderr"), "write", vec![text_expr]))
}

/// The strings the runtime's code uses.
pub(crate) fn runtime_strings() -> Vec<&'static str> {
    vec![
        "Task-",
        "Result is not ready.",
        "Exception is not set.",
        "invalid state",
        "no running event loop",
        "asyncio.run() cannot be called from a running event loop",
        "Event loop stopped before Future completed.",
        "Timeout should be used inside a task",
        "unhandled exception during asyncio.run() shutdown\n",
        "Task exception was never retrieved\n",
        "\n",
        ": ",
        "__main__.",
        "coroutine",
        "CancelledError",
    ]
}

// ---- the program's calls ----------------------------------------------------

/// The names the program calls `asyncio`'s functions by.
pub(crate) struct Api {
    /// `import asyncio`.
    module: bool,
    /// `from asyncio import sleep [as s]`: the name, and what it names.
    names: HashMap<String, String>,
}

/// The exception classes `asyncio` names, and the builtin each is.
fn exception_class(attr: &str) -> Option<&'static str> {
    match attr {
        "CancelledError" => Some("CancelledError"),
        "TimeoutError" => Some("TimeoutError"),
        "InvalidStateError" => Some("InvalidStateError"),
        _ => None,
    }
}

impl Api {
    pub(crate) fn of(module: &IRModule) -> Api {
        let mut api = Api {
            module: false,
            names: HashMap::new(),
        };
        for imp in &module.imports {
            if imp.module != "asyncio" {
                continue;
            }
            match (&imp.name, imp.is_from_import) {
                (Some(n), true) => {
                    api.names
                        .insert(imp.alias.clone().unwrap_or_else(|| n.clone()), n.clone());
                }
                (None, false) => api.module = true,
                _ => {}
            }
        }
        api
    }

    pub(crate) fn used(&self) -> bool {
        self.module || !self.names.is_empty()
    }

    /// `asyncio.f(args)`, or `f(args)` for a name imported from it: the
    /// function's name in `asyncio`, and the arguments.
    pub(crate) fn call_of<'e>(&self, e: &'e IRExpr) -> Option<(String, &'e [IRExpr])> {
        match e {
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } if self.module && matches!(&**object, IRExpr::Variable(m) if m == "asyncio") => {
                Some((method_name.clone(), arguments.as_slice()))
            }
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } => self
                .names
                .get(function_name)
                .map(|n| (n.clone(), arguments.as_slice())),
            _ => None,
        }
    }

    /// Rewrite every asyncio call in an expression, innermost first.
    pub(crate) fn rewrite_tree(&self, e: &mut IRExpr, awaited: bool) -> Result<()> {
        self.rewrite(e, awaited)?;
        let mut err = None;
        let mut first = true;
        crate::ir::coroutines::visit_expr(e, &mut |x| {
            if first {
                first = false;
                return;
            }
            if err.is_none() {
                if let Err(problem) = self.rewrite(x, false) {
                    err = Some(problem);
                }
            }
        });
        match err {
            Some(problem) => Err(problem),
            None => Ok(()),
        }
    }

    /// Rewrite `e` if it is an asyncio call or name; `awaited` says it is
    /// what an `await` applies to.
    pub(crate) fn rewrite(&self, e: &mut IRExpr, awaited: bool) -> Result<()> {
        if let IRExpr::Attribute { object, attribute } = e {
            if self.module && matches!(&**object, IRExpr::Variable(m) if m == "asyncio") {
                let Some(class) = exception_class(attribute) else {
                    return Err(refused(format!(
                        "asyncio.{attribute} is not supported here"
                    )));
                };
                *e = v(class);
            }
            return Ok(());
        }
        if let IRExpr::Variable(n) = e {
            if let Some(class) = self.names.get(n.as_str()).and_then(|a| exception_class(a)) {
                *e = v(class);
            }
            return Ok(());
        }
        let Some((function, args)) = self.call_of(e) else {
            return Ok(());
        };
        let args = args.to_vec();
        if let Some(class) = exception_class(&function) {
            *e = call(class, args);
            return Ok(());
        }
        let refuse_keywords = |args: &[IRExpr]| -> Result<()> {
            match args.iter().find(|a| matches!(a, IRExpr::Keyword { .. })) {
                Some(IRExpr::Keyword { name, .. }) => Err(refused(format!(
                    "asyncio.{function}() with '{}' is not supported",
                    if name == crate::ir::STARRED_ARG {
                        "*".to_string()
                    } else {
                        format!("{name}=")
                    }
                ))),
                _ => Ok(()),
            }
        };
        let one = |args: Vec<IRExpr>| -> Result<IRExpr> {
            refuse_keywords(&args)?;
            match <[IRExpr; 1]>::try_from(args) {
                Ok([x]) => Ok(x),
                Err(args) => Err(refused(format!(
                    "asyncio.{function}() takes 1 argument here ({} given)",
                    args.len()
                ))),
            }
        };
        *e = match function.as_str() {
            "run" => mcall(one(args)?, "_asyncio_run", vec![]),
            "create_task" => mcall(one(args)?, "_make_task", vec![]),
            "ensure_future" => mcall(one(args)?, "_ensure", vec![]),
            "sleep" => match one(args) {
                Ok(delay) => f("sleep", vec![delay]),
                Err(_) => {
                    return Err(refused(
                        "asyncio.sleep() takes the delay alone here; its 'result' argument is \
                         not supported"
                            .to_string(),
                    ))
                }
            },
            "gather" | "wait_for" if !awaited => {
                return Err(refused(format!(
                    "asyncio.{function}() is supported only where it is awaited directly: \
                     'await asyncio.{function}(...)'"
                )))
            }
            "gather" | "wait_for" => return Ok(()),
            other => {
                return Err(refused(format!(
                    "asyncio.{other}() is not supported. Supported: run, create_task, \
                     ensure_future, gather, sleep, wait_for, and the methods of a task"
                )))
            }
        };
        Ok(())
    }
}

// ---- the runtime's coroutines ---------------------------------------------------

/// The coroutine functions the runtime writes in Python's terms, which
/// `ir::coroutines` lowers with the program's own.
pub(crate) fn coroutine_functions() -> Vec<IRFunction> {
    let future_none = || IRType::Class(future_class(&IRType::None));
    let coroutine = |short: &str, params, body| IRFunction {
        name: fname(short),
        params,
        body: crate::ir::IRBody { statements: body },
        return_type: IRType::Coroutine(Box::new(IRType::None)),
        decorators: Vec::new(),
    };
    let await_ = |x: IRExpr| expr_stmt(call(crate::ir::AWAIT_FN, vec![x]));
    // `asyncio.sleep(delay)`: one turn of the loop for a delay of zero or
    // less, else a future a timer completes.
    let mut zero = crate::ir::coroutines::bare_yield();
    zero.push(ret());
    let sleep = coroutine(
        "sleep",
        vec![param("delay", &IRType::Float)],
        vec![
            if_(cmp(v("delay"), IRCompareOp::LtE, float(0.0)), zero),
            assign_typed(
                "fut",
                call(&future_class(&IRType::None), vec![]),
                &future_none(),
            ),
            assign_typed(
                "h",
                f(
                    "call_at",
                    vec![
                        bin(
                            call(CLOCK_FN, vec![]),
                            IROp::Add,
                            call(NS_FN, vec![v("delay")]),
                        ),
                        call(&name("_SetResult"), vec![v("fut")]),
                    ],
                ),
                &cls("_Handle"),
            ),
            IRStatement::TryExcept {
                try_body: body(vec![await_(v("fut"))]),
                except_handlers: Vec::new(),
                finally_body: Some(body(vec![expr_stmt(mcall(v("h"), "cancel", vec![]))])),
            },
        ],
    );
    // `tasks._cancel_and_wait(fut)`: cancel and wait for it to finish.
    let cancel_and_wait = coroutine(
        "cancel_and_wait",
        vec![param("fut", &cls("_FutureBase"))],
        vec![
            assign_typed(
                "waiter",
                call(&future_class(&IRType::None), vec![]),
                &future_none(),
            ),
            assign_typed(
                "cb",
                call(&name("_ReleaseWaiter"), vec![v("waiter")]),
                &cls("_Callback"),
            ),
            expr_stmt(mcall(v("fut"), "_add_cb", vec![v("cb")])),
            IRStatement::TryExcept {
                try_body: body(vec![
                    expr_stmt(mcall(v("fut"), "cancel", vec![])),
                    await_(v("waiter")),
                ]),
                except_handlers: Vec::new(),
                finally_body: Some(body(vec![expr_stmt(mcall(
                    v("fut"),
                    "_remove_cb",
                    vec![v("cb")],
                ))])),
            },
        ],
    );
    vec![sleep, cancel_and_wait]
}

// ---- the runtime's classes ------------------------------------------------------

/// Every runtime class: the loop's state, handles, callbacks, the futures'
/// base, and `Future`, `Task`, and `_Coro` for each result type in `kinds`
/// (which holds None).
pub(crate) fn runtime_classes(kinds: &[IRType], bridge: bool) -> Vec<IRClass> {
    let mut out = vec![loop_state(bridge), callback(), handle(), future_base()];
    for t in kinds {
        out.push(future(t));
    }
    for t in kinds {
        out.push(task(t));
        let mut c = coro(t);
        if bridge && crosses_js(t) {
            c.methods.push(export_method(t));
            out.push(resolve_callback(t));
        }
        out.push(c);
    }
    out.extend(callbacks());
    out.push(gathering());
    out.push(timeout());
    out
}

/// A result JavaScript can be handed (`compiler::js`'s rule): a number, a
/// str, a bool, None, a JS value, and lists, tuples, and dicts of those.
pub(crate) fn crosses_js(t: &IRType) -> bool {
    match t {
        IRType::Int
        | IRType::Float
        | IRType::Bool
        | IRType::String
        | IRType::None
        | IRType::JsProxy => true,
        IRType::List(e) | IRType::VarTuple(e) | IRType::Optional(e) => {
            crosses_js(e) && **e != IRType::JsProxy
        }
        IRType::Tuple(ms) => ms.iter().all(|m| crosses_js(m) && *m != IRType::JsProxy),
        IRType::Dict(k, v) => {
            matches!(**k, IRType::String | IRType::Int) && crosses_js(v) && **v != IRType::JsProxy
        }
        _ => false,
    }
}

fn js(op: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&format!("__js_{op}"), args)
}

/// `coro._export()`: the call of an exported coroutine from JavaScript, as
/// a task the loop runs between JavaScript's turns, whose end resolves the
/// Promise the call answered (its token).
fn export_method(t: &IRType) -> IRFunction {
    let c = coro_class(t);
    let task_ty = IRType::Class(task_class(t));
    method(
        "_export",
        vec![me(&c)],
        IRType::Int,
        vec![
            set_loop("running", boolean(true)),
            set_loop("js_driven", boolean(true)),
            assign_typed("task", call(&task_class(t), vec![v("self")]), &task_ty),
            set_loop("next_token", bin(loop_var("next_token"), IROp::Add, int(1))),
            assign_typed("token", loop_var("next_token"), &IRType::Int),
            expr_stmt(mcall(
                v("task"),
                "_add_cb",
                vec![call(
                    &name(&format!("_Resolve[{}]", spelled(t))),
                    vec![v("task"), v("token")],
                )],
            )),
            ret_val(v("token")),
        ],
    )
}

/// `asyncio._Resolve[T]`: an exported coroutine's task is done, which
/// resolves (or rejects) its Promise.
fn resolve_callback(t: &IRType) -> IRClass {
    let short = format!("_Resolve[{}]", spelled(t));
    let c = name(&short);
    let task_ty = IRType::Class(task_class(t));
    let resolve = |failed: i32, value: IRExpr| {
        expr_stmt(js(
            "resolve",
            vec![my("token"), int(failed), js("export", vec![value])],
        ))
    };
    let result = if *t == IRType::None {
        vec![
            expr_stmt(mcall(my("task"), "result", vec![])),
            resolve(0, none()),
        ]
    } else {
        vec![resolve(0, mcall(my("task"), "result", vec![]))]
    };
    class(
        &short,
        Some(name("_Callback")),
        vec![
            method(
                "__init__",
                vec![
                    me(&c),
                    param("task", &task_ty),
                    param("token", &IRType::Int),
                ],
                IRType::None,
                vec![
                    set_my_typed("task", v("task"), &task_ty),
                    set_my_typed("token", v("token"), &IRType::Int),
                ],
            ),
            method(
                "run",
                vec![me(&c), param("fut", &opt(cls("_FutureBase")))],
                IRType::None,
                vec![if_else(
                    mcall(my("task"), "cancelled", vec![]),
                    vec![resolve(1, text("CancelledError"))],
                    vec![if_else(
                        attr(my("task"), "_has_exc"),
                        vec![
                            set_attr(my("task"), "_log_traceback", boolean(false)),
                            resolve(1, f("exc_line", vec![attr(my("task"), "_exc")])),
                        ],
                        result,
                    )],
                )],
            ),
        ],
    )
}

/// The functions a module using the bridge adds: a promise awaited as a
/// future, the loop run between JavaScript's turns, and the exports
/// JavaScript calls to settle a promise and to run due timers.
pub(crate) fn bridge_functions() -> Vec<IRFunction> {
    let promise_future = || IRType::Class(future_class(&IRType::JsProxy));
    let heap = || loop_var("timers");
    vec![
        // `await promise`: a future the promise settles.
        function(
            "promise",
            vec![param("p", &IRType::JsProxy)],
            promise_future(),
            vec![
                assign_typed(
                    "fut",
                    call(&future_class(&IRType::JsProxy), vec![]),
                    &promise_future(),
                ),
                set_loop("next_token", bin(loop_var("next_token"), IROp::Add, int(1))),
                assign_typed("token", loop_var("next_token"), &IRType::Int),
                set_item(loop_var("awaiting"), v("token"), v("fut")),
                expr_stmt(js("then", vec![v("p"), v("token")])),
                ret_val(v("fut")),
            ],
        ),
        // Run what is ready, and timers as they fall due, until there is
        // nothing to do now: then the milliseconds until the next timer, or
        // -1 when there is none.
        function(
            "drive",
            vec![],
            IRType::Float,
            vec![
                set_loop("running", boolean(true)),
                while_(
                    boolean(true),
                    vec![
                        while_(
                            and(
                                cmp(len(heap()), IRCompareOp::Gt, int(0)),
                                attr(idx(heap(), int(0)), "cancelled"),
                            ),
                            vec![expr_stmt(f("pop", vec![]))],
                        ),
                        if_(
                            eq(len(loop_var("ready")), int(0)),
                            vec![if_else(
                                cmp(len(heap()), IRCompareOp::Gt, int(0)),
                                vec![
                                    assign_typed(
                                        "wait",
                                        bin(
                                            attr(idx(heap(), int(0)), "when"),
                                            IROp::Sub,
                                            call(CLOCK_FN, vec![]),
                                        ),
                                        &IRType::Float,
                                    ),
                                    if_(
                                        cmp(v("wait"), IRCompareOp::Gt, float(0.0)),
                                        vec![ret_val(bin(v("wait"), IROp::Div, float(1e6)))],
                                    ),
                                ],
                                vec![ret_val(float(-1.0))],
                            )],
                        ),
                        expr_stmt(f("run_once", vec![])),
                    ],
                ),
            ],
        ),
        // A promise settled: its future done, and the loop run.
        method(
            "__waspy_settle",
            vec![
                param("token", &IRType::Int),
                param("failed", &IRType::Int),
                param("h", &IRType::Int),
            ],
            IRType::Float,
            vec![
                assign_typed("value", js("adopt", vec![v("h")]), &IRType::JsProxy),
                if_(
                    cmp(v("token"), IRCompareOp::In, loop_var("awaiting")),
                    vec![
                        assign_typed(
                            "fut",
                            mcall(loop_var("awaiting"), "pop", vec![v("token")]),
                            &promise_future(),
                        ),
                        if_(
                            not(mcall(v("fut"), "done", vec![])),
                            vec![if_else(
                                ne(v("failed"), int(0)),
                                vec![expr_stmt(mcall(
                                    v("fut"),
                                    "set_exception",
                                    vec![call("JsException", vec![call("str", vec![v("value")])])],
                                ))],
                                vec![expr_stmt(mcall(v("fut"), "set_result", vec![v("value")]))],
                            )],
                        ),
                    ],
                ),
                ret_val(f("drive", vec![])),
            ],
        ),
        // The next timer is due.
        method(
            "__waspy_drive",
            vec![],
            IRType::Float,
            vec![ret_val(f("drive", vec![]))],
        ),
    ]
}

/// `asyncio._Loop`: the event loop's state, in class variables.
fn loop_state(bridge: bool) -> IRClass {
    let var = |n: &str, value: IRExpr, ty: IRType| IRVariable {
        name: n.to_string(),
        value,
        var_type: Some(ty),
    };
    let mut c = class("_Loop", None, Vec::new());
    c.class_vars = vec![
        var("ready", IRExpr::ListLiteral(vec![]), list(cls("_Handle"))),
        var("timers", IRExpr::ListLiteral(vec![]), list(cls("_Handle"))),
        var(
            "tasks",
            IRExpr::ListLiteral(vec![]),
            list(cls("_FutureBase")),
        ),
        // The tasks that finished with an exception, which is reported if
        // nothing retrieves it by the time the loop closes.
        var(
            "failed",
            IRExpr::ListLiteral(vec![]),
            list(cls("_FutureBase")),
        ),
        var("seq", int(0), IRType::Int),
        var("names", int(0), IRType::Int),
        var("running", boolean(false), IRType::Bool),
        var("stopping", boolean(false), IRType::Bool),
        // The task stepping, when `has_current`.
        var(
            "current",
            call(&name("_FutureBase"), vec![]),
            cls("_FutureBase"),
        ),
        var("has_current", boolean(false), IRType::Bool),
    ];
    if bridge {
        // The futures promises settle, by token; the last token given; and
        // whether the loop runs between JavaScript's turns.
        c.class_vars.extend([
            var(
                "awaiting",
                IRExpr::DictLiteral(vec![]),
                IRType::Dict(
                    Box::new(IRType::Int),
                    Box::new(IRType::Class(future_class(&IRType::JsProxy))),
                ),
            ),
            var("next_token", int(0), IRType::Int),
            var("js_driven", boolean(false), IRType::Bool),
        ]);
    }
    c
}

/// `asyncio._Callback`: what a handle runs, given the future it is about.
fn callback() -> IRClass {
    let c = name("_Callback");
    class(
        "_Callback",
        None,
        vec![method(
            "run",
            vec![me(&c), param("fut", &opt(cls("_FutureBase")))],
            IRType::None,
            vec![ret()],
        )],
    )
}

/// `asyncio._Handle`: a callback scheduled to run, at a time for a timer.
fn handle() -> IRClass {
    let c = name("_Handle");
    class(
        "_Handle",
        None,
        vec![
            method(
                "__init__",
                vec![
                    me(&c),
                    param("cb", &cls("_Callback")),
                    param("arg", &opt(cls("_FutureBase"))),
                    param("when", &IRType::Float),
                    param("seq", &IRType::Int),
                ],
                IRType::None,
                vec![
                    set_my_typed("cb", v("cb"), &cls("_Callback")),
                    set_my_typed("arg", v("arg"), &opt(cls("_FutureBase"))),
                    set_my_typed("when", v("when"), &IRType::Float),
                    set_my_typed("seq", v("seq"), &IRType::Int),
                    set_my_typed("cancelled", boolean(false), &IRType::Bool),
                ],
            ),
            method(
                "cancel",
                vec![me(&c)],
                IRType::None,
                vec![set_attr(v("self"), "cancelled", boolean(true))],
            ),
        ],
    )
}

/// Future states.
const PENDING: i32 = 0;
const CANCELLED: i32 = 1;
const FINISHED: i32 = 2;

/// `asyncio._FutureBase`: what every future has whatever its result, which
/// is all the loop and the callbacks see of one.
fn future_base() -> IRClass {
    let c = name("_FutureBase");
    let state = || my("_state");
    let call_soon = |cb: IRExpr| expr_stmt(f("call_soon", vec![cb, v("self")]));
    class(
        "_FutureBase",
        None,
        vec![
            method(
                "__init__",
                vec![me(&c)],
                IRType::None,
                vec![
                    set_my_typed("_state", int(PENDING), &IRType::Int),
                    set_my_typed("_has_exc", boolean(false), &IRType::Bool),
                    set_my_typed(
                        "_callbacks",
                        IRExpr::ListLiteral(vec![]),
                        &list(cls("_Callback")),
                    ),
                    set_my_typed("_log_traceback", boolean(false), &IRType::Bool),
                ],
            ),
            method(
                "done",
                vec![me(&c)],
                IRType::Bool,
                vec![ret_val(ne(state(), int(PENDING)))],
            ),
            method(
                "cancelled",
                vec![me(&c)],
                IRType::Bool,
                vec![ret_val(eq(state(), int(CANCELLED)))],
            ),
            method(
                "_add_cb",
                vec![me(&c), param("cb", &cls("_Callback"))],
                IRType::None,
                vec![if_else(
                    ne(state(), int(PENDING)),
                    vec![call_soon(v("cb"))],
                    vec![expr_stmt(mcall(my("_callbacks"), "append", vec![v("cb")]))],
                )],
            ),
            method(
                "_remove_cb",
                vec![me(&c), param("cb", &cls("_Callback"))],
                IRType::None,
                vec![
                    assign_typed("kept", IRExpr::ListLiteral(vec![]), &list(cls("_Callback"))),
                    for_(
                        "x",
                        my("_callbacks"),
                        vec![if_(
                            cmp(v("x"), IRCompareOp::IsNot, v("cb")),
                            vec![expr_stmt(mcall(v("kept"), "append", vec![v("x")]))],
                        )],
                    ),
                    set_attr(v("self"), "_callbacks", v("kept")),
                ],
            ),
            method(
                "_schedule",
                vec![me(&c)],
                IRType::None,
                vec![
                    assign_typed("cbs", my("_callbacks"), &list(cls("_Callback"))),
                    set_attr(v("self"), "_callbacks", IRExpr::ListLiteral(vec![])),
                    for_("cb", v("cbs"), vec![call_soon(v("cb"))]),
                ],
            ),
            method(
                "_cancel_future",
                vec![me(&c)],
                IRType::Bool,
                vec![
                    set_attr(v("self"), "_log_traceback", boolean(false)),
                    if_(ne(state(), int(PENDING)), vec![ret_val(boolean(false))]),
                    set_attr(v("self"), "_state", int(CANCELLED)),
                    expr_stmt(mcall(v("self"), "_schedule", vec![])),
                    ret_val(boolean(true)),
                ],
            ),
            method(
                "cancel",
                vec![me(&c)],
                IRType::Bool,
                vec![ret_val(mcall(v("self"), "_cancel_future", vec![]))],
            ),
            method(
                "_check",
                vec![me(&c)],
                IRType::None,
                vec![
                    if_(
                        eq(state(), int(CANCELLED)),
                        vec![raise_new("CancelledError")],
                    ),
                    if_(
                        ne(state(), int(FINISHED)),
                        vec![raise("InvalidStateError", "Result is not ready.")],
                    ),
                    set_attr(v("self"), "_log_traceback", boolean(false)),
                    if_(
                        my("_has_exc"),
                        vec![IRStatement::Raise {
                            exception: Some(my("_exc")),
                            cause: None,
                        }],
                    ),
                ],
            ),
            method(
                "exception",
                vec![me(&c)],
                opt(IRType::Class("BaseException".to_string())),
                vec![
                    if_(
                        eq(state(), int(CANCELLED)),
                        vec![raise_new("CancelledError")],
                    ),
                    if_(
                        ne(state(), int(FINISHED)),
                        vec![raise("InvalidStateError", "Exception is not set.")],
                    ),
                    set_attr(v("self"), "_log_traceback", boolean(false)),
                    if_(my("_has_exc"), vec![ret_val(my("_exc"))]),
                    ret_val(none()),
                ],
            ),
            method(
                "set_exception",
                vec![
                    me(&c),
                    param("e", &IRType::Class("BaseException".to_string())),
                ],
                IRType::None,
                vec![
                    if_(
                        ne(state(), int(PENDING)),
                        vec![raise("InvalidStateError", "invalid state")],
                    ),
                    set_my_typed("_exc", v("e"), &IRType::Class("BaseException".to_string())),
                    set_attr(v("self"), "_has_exc", boolean(true)),
                    set_attr(v("self"), "_state", int(FINISHED)),
                    expr_stmt(mcall(v("self"), "_schedule", vec![])),
                    set_attr(v("self"), "_log_traceback", boolean(true)),
                ],
            ),
            // A task's, reached through the loop's callbacks.
            method("_step", vec![me(&c)], IRType::None, vec![ret()]),
            method(
                "_wakeup",
                vec![me(&c), param("fut", &opt(cls("_FutureBase")))],
                IRType::None,
                vec![ret()],
            ),
            method(
                "cancelling",
                vec![me(&c)],
                IRType::Int,
                vec![ret_val(int(0))],
            ),
            method("uncancel", vec![me(&c)], IRType::Int, vec![ret_val(int(0))]),
        ],
    )
}

/// `asyncio.Future[T]`.
fn future(t: &IRType) -> IRClass {
    let c = future_class(t);
    let none_result = *t == IRType::None;
    let mut methods = vec![
        method(
            "__init__",
            vec![me(&c)],
            IRType::None,
            vec![expr_stmt(qcall(
                &name("_FutureBase"),
                "__init__",
                vec![v("self")],
            ))],
        ),
        method(
            "_aw_next",
            vec![me(&c)],
            waited_on(),
            vec![
                if_(
                    ne(my("_state"), int(PENDING)),
                    vec![raise_new("StopIteration")],
                ),
                ret_val(v("self")),
            ],
        ),
        method(
            "_aw_throw",
            vec![
                me(&c),
                param("e", &IRType::Class("BaseException".to_string())),
            ],
            waited_on(),
            vec![IRStatement::Raise {
                exception: Some(v("e")),
                cause: None,
            }],
        ),
        method(
            "_ensure",
            vec![me(&c)],
            IRType::Class(c.clone()),
            vec![ret_val(v("self"))],
        ),
    ];
    if none_result {
        methods.extend([
            method(
                "result",
                vec![me(&c)],
                IRType::None,
                vec![expr_stmt(mcall(v("self"), "_check", vec![]))],
            ),
            method(
                "_aw_result",
                vec![me(&c)],
                IRType::None,
                vec![expr_stmt(mcall(v("self"), "_check", vec![]))],
            ),
            method(
                "_set_none",
                vec![me(&c)],
                IRType::None,
                vec![
                    if_(
                        ne(my("_state"), int(PENDING)),
                        vec![raise("InvalidStateError", "invalid state")],
                    ),
                    set_attr(v("self"), "_state", int(FINISHED)),
                    expr_stmt(mcall(v("self"), "_schedule", vec![])),
                ],
            ),
        ]);
    } else {
        methods.extend([
            method(
                "result",
                vec![me(&c)],
                t.clone(),
                vec![
                    expr_stmt(mcall(v("self"), "_check", vec![])),
                    ret_val(my("_result")),
                ],
            ),
            method(
                "_aw_result",
                vec![me(&c)],
                t.clone(),
                vec![ret_val(mcall(v("self"), "result", vec![]))],
            ),
            method(
                "set_result",
                vec![me(&c), param("value", t)],
                IRType::None,
                vec![
                    if_(
                        ne(my("_state"), int(PENDING)),
                        vec![raise("InvalidStateError", "invalid state")],
                    ),
                    set_my_typed("_result", v("value"), t),
                    set_attr(v("self"), "_state", int(FINISHED)),
                    expr_stmt(mcall(v("self"), "_schedule", vec![])),
                ],
            ),
        ]);
    }
    IRClass {
        name: c,
        bases: vec![name("_FutureBase")],
        methods,
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// `asyncio.Task[T]`: a future stepping a coroutine from the loop.
fn task(t: &IRType) -> IRClass {
    let c = task_class(t);
    let coro_ty = IRType::Class(coro_class(t));
    let base = || IRType::Class("BaseException".to_string());
    let finished = if *t == IRType::None {
        vec![
            expr_stmt(mcall(my("_coro"), "_aw_result", vec![])),
            expr_stmt(mcall(v("self"), "_set_none", vec![])),
        ]
    } else {
        vec![expr_stmt(mcall(
            v("self"),
            "set_result",
            vec![mcall(my("_coro"), "_aw_result", vec![])],
        ))]
    };
    // `Task.__step`: resume (or throw into) the coroutine, then act on what
    // it did: finished, raised, or waits on a future (or on nothing).
    let run = method(
        "_run",
        vec![
            me(&c),
            param("thrown", &IRType::Bool),
            param("exc", &base()),
        ],
        IRType::None,
        vec![
            if_(
                ne(my("_state"), int(PENDING)),
                vec![raise("InvalidStateError", "invalid state")],
            ),
            assign_typed("throwing", v("thrown"), &IRType::Bool),
            assign_typed("error", v("exc"), &base()),
            if_(
                my("_must_cancel"),
                vec![
                    if_(
                        or(
                            not(v("throwing")),
                            not(call("isinstance", vec![v("error"), v("CancelledError")])),
                        ),
                        vec![
                            assign("error", call("CancelledError", vec![])),
                            assign("throwing", boolean(true)),
                        ],
                    ),
                    set_attr(v("self"), "_must_cancel", boolean(false)),
                ],
            ),
            set_attr(v("self"), "_waiter", none()),
            set_loop("current", v("self")),
            set_loop("has_current", boolean(true)),
            IRStatement::TryExcept {
                try_body: body(vec![
                    expr_stmt(stop_check()),
                    if_else(
                        v("throwing"),
                        vec![assign_typed(
                            "sig",
                            mcall(my("_coro"), "_aw_throw", vec![v("error")]),
                            &waited_on(),
                        )],
                        vec![assign_typed(
                            "sig",
                            mcall(my("_coro"), "_aw_next", vec![]),
                            &waited_on(),
                        )],
                    ),
                    if_else(
                        stop_check(),
                        vec![if_else(
                            my("_must_cancel"),
                            vec![
                                set_attr(v("self"), "_must_cancel", boolean(false)),
                                expr_stmt(mcall(v("self"), "_cancel_future", vec![])),
                            ],
                            finished,
                        )],
                        vec![if_else(
                            is_none(v("sig")),
                            vec![expr_stmt(f(
                                "call_soon",
                                vec![call(&name("_Step"), vec![v("self")]), none()],
                            ))],
                            vec![
                                assign_typed(
                                    "waited",
                                    call(crate::ir::SOME_FN, vec![v("sig"), text("_add_cb")]),
                                    &cls("_FutureBase"),
                                ),
                                expr_stmt(mcall(
                                    v("waited"),
                                    "_add_cb",
                                    vec![call(&name("_Wakeup"), vec![v("self")])],
                                )),
                                set_attr(v("self"), "_waiter", v("waited")),
                                if_(
                                    my("_must_cancel"),
                                    vec![if_(
                                        mcall(v("waited"), "cancel", vec![]),
                                        vec![set_attr(v("self"), "_must_cancel", boolean(false))],
                                    )],
                                ),
                            ],
                        )],
                    ),
                ]),
                except_handlers: vec![
                    IRExceptHandler {
                        exception_types: vec!["CancelledError".to_string()],
                        name: None,
                        body: crate::ir::IRBody {
                            statements: vec![expr_stmt(mcall(v("self"), "_cancel_future", vec![]))],
                        },
                    },
                    IRExceptHandler {
                        exception_types: vec![
                            "SystemExit".to_string(),
                            "KeyboardInterrupt".to_string(),
                        ],
                        name: Some("fatal".to_string()),
                        body: crate::ir::IRBody {
                            statements: vec![
                                expr_stmt(mcall(v("self"), "set_exception", vec![v("fatal")])),
                                set_loop("has_current", boolean(false)),
                                IRStatement::Raise {
                                    exception: None,
                                    cause: None,
                                },
                            ],
                        },
                    },
                    IRExceptHandler {
                        exception_types: vec!["BaseException".to_string()],
                        name: Some("failure".to_string()),
                        body: crate::ir::IRBody {
                            statements: vec![
                                expr_stmt(mcall(v("self"), "set_exception", vec![v("failure")])),
                                expr_stmt(mcall(loop_var("failed"), "append", vec![v("self")])),
                            ],
                        },
                    },
                ],
                finally_body: None,
            },
            set_loop("has_current", boolean(false)),
            // A finished task is no longer the loop's to cancel.
            if_(
                ne(my("_state"), int(PENDING)),
                vec![expr_stmt(mcall(
                    loop_var("tasks"),
                    "remove",
                    vec![v("self")],
                ))],
            ),
        ],
    );
    IRClass {
        name: c.clone(),
        bases: vec![future_class(t)],
        methods: vec![
            method(
                "__init__",
                vec![me(&c), param("coro", &coro_ty)],
                IRType::None,
                vec![
                    expr_stmt(qcall(&name("_FutureBase"), "__init__", vec![v("self")])),
                    set_my_typed("_coro", v("coro"), &coro_ty),
                    set_my_typed("_waiter", none(), &opt(cls("_FutureBase"))),
                    set_my_typed("_must_cancel", boolean(false), &IRType::Bool),
                    set_my_typed("_cancels", int(0), &IRType::Int),
                    set_loop("names", bin(loop_var("names"), IROp::Add, int(1))),
                    set_my_typed(
                        "_name",
                        bin(
                            text("Task-"),
                            IROp::Add,
                            call("str", vec![loop_var("names")]),
                        ),
                        &IRType::String,
                    ),
                    expr_stmt(f(
                        "call_soon",
                        vec![call(&name("_Step"), vec![v("self")]), none()],
                    )),
                    expr_stmt(mcall(loop_var("tasks"), "append", vec![v("self")])),
                ],
            ),
            method(
                "get_name",
                vec![me(&c)],
                IRType::String,
                vec![ret_val(my("_name"))],
            ),
            method(
                "cancel",
                vec![me(&c)],
                IRType::Bool,
                vec![
                    set_attr(v("self"), "_log_traceback", boolean(false)),
                    if_(
                        ne(my("_state"), int(PENDING)),
                        vec![ret_val(boolean(false))],
                    ),
                    set_attr(
                        v("self"),
                        "_cancels",
                        bin(my("_cancels"), IROp::Add, int(1)),
                    ),
                    if_(
                        not_none(my("_waiter")),
                        vec![if_(
                            mcall(
                                call(crate::ir::SOME_FN, vec![my("_waiter"), text("cancel")]),
                                "cancel",
                                vec![],
                            ),
                            vec![ret_val(boolean(true))],
                        )],
                    ),
                    set_attr(v("self"), "_must_cancel", boolean(true)),
                    ret_val(boolean(true)),
                ],
            ),
            method(
                "cancelling",
                vec![me(&c)],
                IRType::Int,
                vec![ret_val(my("_cancels"))],
            ),
            method(
                "uncancel",
                vec![me(&c)],
                IRType::Int,
                vec![
                    if_(
                        cmp(my("_cancels"), IRCompareOp::Gt, int(0)),
                        vec![
                            set_attr(
                                v("self"),
                                "_cancels",
                                bin(my("_cancels"), IROp::Sub, int(1)),
                            ),
                            if_(
                                eq(my("_cancels"), int(0)),
                                vec![set_attr(v("self"), "_must_cancel", boolean(false))],
                            ),
                        ],
                    ),
                    ret_val(my("_cancels")),
                ],
            ),
            method(
                "_step",
                vec![me(&c)],
                IRType::None,
                vec![expr_stmt(mcall(
                    v("self"),
                    "_run",
                    vec![
                        boolean(false),
                        call("RuntimeError", vec![text("coroutine")]),
                    ],
                ))],
            ),
            method(
                "_wakeup",
                vec![me(&c), param("fut", &opt(cls("_FutureBase")))],
                IRType::None,
                vec![
                    IRStatement::TryExcept {
                        try_body: body(vec![expr_stmt(mcall(
                            call(crate::ir::SOME_FN, vec![v("fut"), text("_check")]),
                            "_check",
                            vec![],
                        ))]),
                        except_handlers: vec![IRExceptHandler {
                            exception_types: vec!["BaseException".to_string()],
                            name: Some("e".to_string()),
                            body: crate::ir::IRBody {
                                statements: vec![
                                    expr_stmt(mcall(
                                        v("self"),
                                        "_run",
                                        vec![boolean(true), v("e")],
                                    )),
                                    ret(),
                                ],
                            },
                        }],
                        finally_body: None,
                    },
                    expr_stmt(mcall(v("self"), "_step", vec![])),
                ],
            ),
            run,
        ],
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// `asyncio._Coro[T]`: what every coroutine resolving to `T` derives from,
/// with what `asyncio.run()`, `create_task()`, and `ensure_future()` do with
/// one.
fn coro(t: &IRType) -> IRClass {
    let c = coro_class(t);
    let task_ty = IRType::Class(task_class(t));
    let base = || IRType::Class("BaseException".to_string());
    let unreachable = || vec![raise("RuntimeError", "coroutine")];
    let mut guarded = vec![expr_stmt(f("run_until_complete", vec![v("task")]))];
    let result_read = mcall(v("task"), "result", vec![]);
    let finish = if *t == IRType::None {
        guarded.push(expr_stmt(result_read));
        vec![ret()]
    } else {
        guarded.push(assign_typed("r", result_read, t));
        vec![ret_val(v("r"))]
    };
    let mut run_body = vec![
        if_(
            loop_var("running"),
            vec![raise(
                "RuntimeError",
                "asyncio.run() cannot be called from a running event loop",
            )],
        ),
        expr_stmt(f("reset", vec![])),
        set_loop("running", boolean(true)),
        assign_typed("task", call(&task_class(t), vec![v("self")]), &task_ty),
        IRStatement::TryExcept {
            try_body: body(guarded),
            except_handlers: Vec::new(),
            finally_body: Some(body(vec![expr_stmt(f("shutdown", vec![]))])),
        },
    ];
    run_body.extend(finish);
    IRClass {
        name: c.clone(),
        bases: Vec::new(),
        methods: vec![
            method("_aw_next", vec![me(&c)], waited_on(), unreachable()),
            method(
                "_aw_throw",
                vec![me(&c), param("e", &base())],
                waited_on(),
                vec![
                    set_my_typed(crate::ir::coroutines::THROW, v("e"), &base()),
                    set_my_typed(crate::ir::coroutines::THROWN, boolean(true), &IRType::Bool),
                    ret_val(mcall(v("self"), "_aw_next", vec![])),
                ],
            ),
            method("_aw_result", vec![me(&c)], t.clone(), unreachable()),
            method(
                "_make_task",
                vec![me(&c)],
                task_ty.clone(),
                vec![
                    if_(
                        not(loop_var("running")),
                        vec![raise("RuntimeError", "no running event loop")],
                    ),
                    ret_val(call(&task_class(t), vec![v("self")])),
                ],
            ),
            method(
                "_ensure",
                vec![me(&c)],
                task_ty.clone(),
                vec![ret_val(mcall(v("self"), "_make_task", vec![]))],
            ),
            method("_asyncio_run", vec![me(&c)], t.clone(), run_body),
        ],
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// The callbacks the loop runs.
fn callbacks() -> Vec<IRClass> {
    let base = Some(name("_Callback"));
    let fut = || param("fut", &opt(cls("_FutureBase")));
    let holder = |short: &str, field: &str, ty: IRType, run: Vec<IRStatement>| {
        let c = name(short);
        class(
            short,
            base.clone(),
            vec![
                method(
                    "__init__",
                    vec![me(&c), param(field, &ty)],
                    IRType::None,
                    vec![set_my_typed(field, v(field), &ty)],
                ),
                method("run", vec![me(&c), fut()], IRType::None, run),
            ],
        )
    };
    let future_none = IRType::Class(future_class(&IRType::None));
    vec![
        // A task's next step.
        holder(
            "_Step",
            "task",
            cls("_FutureBase"),
            vec![expr_stmt(mcall(my("task"), "_step", vec![]))],
        ),
        // A task's wakeup once the future it waits on is done.
        holder(
            "_Wakeup",
            "task",
            cls("_FutureBase"),
            vec![expr_stmt(mcall(my("task"), "_wakeup", vec![v("fut")]))],
        ),
        // `futures._set_result_unless_cancelled(fut, None)`: a sleep's timer.
        holder(
            "_SetResult",
            "target",
            future_none.clone(),
            vec![
                if_(mcall(my("target"), "cancelled", vec![]), vec![ret()]),
                expr_stmt(mcall(my("target"), "_set_none", vec![])),
            ],
        ),
        // `tasks._release_waiter`.
        holder(
            "_ReleaseWaiter",
            "waiter",
            future_none,
            vec![if_(
                not(mcall(my("waiter"), "done", vec![])),
                vec![expr_stmt(mcall(my("waiter"), "_set_none", vec![]))],
            )],
        ),
        // A gathering future's child is done.
        holder(
            "_GatherDone",
            "outer",
            cls("_Gathering"),
            vec![expr_stmt(mcall(my("outer"), "_child_done", vec![v("fut")]))],
        ),
        // A timeout's deadline.
        holder(
            "_TimeoutFire",
            "timeout",
            cls("_Timeout"),
            vec![expr_stmt(mcall(my("timeout"), "_on_timeout", vec![]))],
        ),
        // `run_until_complete`'s stop.
        class(
            "_StopLoop",
            base.clone(),
            vec![method(
                "run",
                vec![me(&name("_StopLoop")), fut()],
                IRType::None,
                vec![set_loop("stopping", boolean(true))],
            )],
        ),
    ]
}

/// `asyncio._Gathering`: `gather()`'s future (`_GatheringFuture`), done when
/// every child is, or failed with the first child that fails. What the
/// children resolved to is read off them by the code awaiting it.
fn gathering() -> IRClass {
    let c = name("_Gathering");
    let base_future = future_class(&IRType::None);
    let child = || cls("_FutureBase");
    IRClass {
        name: c.clone(),
        bases: vec![base_future],
        methods: vec![
            method(
                "__init__",
                vec![me(&c)],
                IRType::None,
                vec![
                    expr_stmt(qcall(&name("_FutureBase"), "__init__", vec![v("self")])),
                    set_my_typed("_children", IRExpr::ListLiteral(vec![]), &list(child())),
                    set_my_typed("_early", IRExpr::ListLiteral(vec![]), &list(child())),
                    set_my_typed("_nfuts", int(0), &IRType::Int),
                    set_my_typed("_nfinished", int(0), &IRType::Int),
                    set_my_typed("_started", boolean(false), &IRType::Bool),
                    set_my_typed("_return_exceptions", boolean(false), &IRType::Bool),
                    set_my_typed("_cancel_requested", boolean(false), &IRType::Bool),
                ],
            ),
            // One argument of `gather()`, in order: a repeated future is one
            // child, listed twice.
            method(
                "_add",
                vec![me(&c), param("fut", &child())],
                IRType::None,
                vec![
                    assign_typed("again", boolean(false), &IRType::Bool),
                    for_(
                        "x",
                        my("_children"),
                        vec![if_(
                            cmp(v("x"), IRCompareOp::Is, v("fut")),
                            vec![assign("again", boolean(true))],
                        )],
                    ),
                    expr_stmt(mcall(my("_children"), "append", vec![v("fut")])),
                    if_(v("again"), vec![ret()]),
                    set_attr(v("self"), "_nfuts", bin(my("_nfuts"), IROp::Add, int(1))),
                    if_else(
                        mcall(v("fut"), "done", vec![]),
                        vec![expr_stmt(mcall(my("_early"), "append", vec![v("fut")]))],
                        vec![expr_stmt(mcall(
                            v("fut"),
                            "_add_cb",
                            vec![call(&name("_GatherDone"), vec![v("self")])],
                        ))],
                    ),
                ],
            ),
            method(
                "_start",
                vec![me(&c)],
                IRType::None,
                vec![
                    set_attr(v("self"), "_started", boolean(true)),
                    assign_typed("early", my("_early"), &list(child())),
                    set_attr(v("self"), "_early", IRExpr::ListLiteral(vec![])),
                    for_(
                        "x",
                        v("early"),
                        vec![expr_stmt(mcall(v("self"), "_child_done", vec![v("x")]))],
                    ),
                ],
            ),
            method(
                "_child_done",
                vec![me(&c), param("done", &opt(child()))],
                IRType::None,
                vec![
                    assign_typed(
                        "fut",
                        call(crate::ir::SOME_FN, vec![v("done"), text("done")]),
                        &child(),
                    ),
                    set_attr(
                        v("self"),
                        "_nfinished",
                        bin(my("_nfinished"), IROp::Add, int(1)),
                    ),
                    if_(
                        or(not(my("_started")), mcall(v("self"), "done", vec![])),
                        vec![
                            if_(
                                not(mcall(v("fut"), "cancelled", vec![])),
                                vec![set_attr(v("fut"), "_log_traceback", boolean(false))],
                            ),
                            ret(),
                        ],
                    ),
                    if_(
                        not(my("_return_exceptions")),
                        vec![
                            if_(
                                mcall(v("fut"), "cancelled", vec![]),
                                vec![
                                    expr_stmt(mcall(
                                        v("self"),
                                        "set_exception",
                                        vec![call("CancelledError", vec![])],
                                    )),
                                    ret(),
                                ],
                            ),
                            if_(
                                attr(v("fut"), "_has_exc"),
                                vec![
                                    set_attr(v("fut"), "_log_traceback", boolean(false)),
                                    expr_stmt(mcall(
                                        v("self"),
                                        "set_exception",
                                        vec![attr(v("fut"), "_exc")],
                                    )),
                                    ret(),
                                ],
                            ),
                        ],
                    ),
                    if_(
                        eq(my("_nfinished"), my("_nfuts")),
                        vec![
                            for_(
                                "x",
                                my("_children"),
                                vec![if_(
                                    not(mcall(v("x"), "cancelled", vec![])),
                                    vec![set_attr(v("x"), "_log_traceback", boolean(false))],
                                )],
                            ),
                            if_else(
                                my("_cancel_requested"),
                                vec![expr_stmt(mcall(
                                    v("self"),
                                    "set_exception",
                                    vec![call("CancelledError", vec![])],
                                ))],
                                vec![expr_stmt(mcall(v("self"), "_set_none", vec![]))],
                            ),
                        ],
                    ),
                ],
            ),
            method(
                "cancel",
                vec![me(&c)],
                IRType::Bool,
                vec![
                    if_(
                        mcall(v("self"), "done", vec![]),
                        vec![ret_val(boolean(false))],
                    ),
                    assign_typed("any", boolean(false), &IRType::Bool),
                    for_(
                        "x",
                        my("_children"),
                        vec![if_(
                            mcall(v("x"), "cancel", vec![]),
                            vec![assign("any", boolean(true))],
                        )],
                    ),
                    if_(
                        v("any"),
                        vec![set_attr(v("self"), "_cancel_requested", boolean(true))],
                    ),
                    ret_val(v("any")),
                ],
            ),
        ],
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// `asyncio._Timeout`: `timeouts.Timeout`, the deadline `wait_for()` waits
/// under, which cancels the task waiting at it when it passes.
fn timeout() -> IRClass {
    let c = name("_Timeout");
    let base = || IRType::Class("BaseException".to_string());
    let task = || call(crate::ir::SOME_FN, vec![my("_task"), text("cancel")]);
    let close = vec![if_(
        not_none(my("_handler")),
        vec![
            expr_stmt(mcall(
                call(crate::ir::SOME_FN, vec![my("_handler"), text("cancel")]),
                "cancel",
                vec![],
            )),
            set_attr(v("self"), "_handler", none()),
        ],
    )];
    let mut exit_ok = close.clone();
    exit_ok.push(if_else(
        eq(my("_state"), int(2)),
        vec![
            set_attr(v("self"), "_state", int(3)),
            expr_stmt(mcall(task(), "uncancel", vec![])),
        ],
        vec![if_(
            eq(my("_state"), int(1)),
            vec![set_attr(v("self"), "_state", int(4))],
        )],
    ));
    let mut exit_failed = close;
    exit_failed.push(if_else(
        eq(my("_state"), int(2)),
        vec![
            set_attr(v("self"), "_state", int(3)),
            if_(
                and(
                    cmp(
                        mcall(task(), "uncancel", vec![]),
                        IRCompareOp::LtE,
                        my("_cancelling"),
                    ),
                    call("isinstance", vec![v("e"), v("CancelledError")]),
                ),
                vec![IRStatement::Raise {
                    exception: Some(call("TimeoutError", vec![])),
                    cause: Some(v("e")),
                }],
            ),
        ],
        vec![if_(
            eq(my("_state"), int(1)),
            vec![set_attr(v("self"), "_state", int(4))],
        )],
    ));
    class(
        "_Timeout",
        None,
        vec![
            method(
                "__init__",
                vec![me(&c), param("when", &IRType::Float)],
                IRType::None,
                vec![
                    set_my_typed("_when", v("when"), &IRType::Float),
                    set_my_typed("_state", int(0), &IRType::Int),
                    set_my_typed("_handler", none(), &opt(cls("_Handle"))),
                    set_my_typed("_task", none(), &opt(cls("_FutureBase"))),
                    set_my_typed("_cancelling", int(0), &IRType::Int),
                ],
            ),
            method(
                "_enter",
                vec![me(&c)],
                IRType::None,
                vec![
                    set_attr(v("self"), "_state", int(1)),
                    if_(
                        not(loop_var("has_current")),
                        vec![raise(
                            "RuntimeError",
                            "Timeout should be used inside a task",
                        )],
                    ),
                    assign_typed("t", loop_var("current"), &cls("_FutureBase")),
                    set_attr(v("self"), "_task", v("t")),
                    set_attr(
                        v("self"),
                        "_cancelling",
                        mcall(v("t"), "cancelling", vec![]),
                    ),
                    if_else(
                        cmp(my("_when"), IRCompareOp::LtE, call(CLOCK_FN, vec![])),
                        vec![set_attr(
                            v("self"),
                            "_handler",
                            f(
                                "call_soon",
                                vec![call(&name("_TimeoutFire"), vec![v("self")]), none()],
                            ),
                        )],
                        vec![set_attr(
                            v("self"),
                            "_handler",
                            f(
                                "call_at",
                                vec![my("_when"), call(&name("_TimeoutFire"), vec![v("self")])],
                            ),
                        )],
                    ),
                ],
            ),
            method(
                "_on_timeout",
                vec![me(&c)],
                IRType::None,
                vec![
                    expr_stmt(mcall(task(), "cancel", vec![])),
                    set_attr(v("self"), "_state", int(2)),
                    set_attr(v("self"), "_handler", none()),
                ],
            ),
            method("_exit_ok", vec![me(&c)], IRType::None, exit_ok),
            method(
                "_exit_failed",
                vec![me(&c), param("e", &base())],
                IRType::None,
                exit_failed,
            ),
        ],
    )
}

// ---- the runtime's functions ----------------------------------------------------

/// Nothing ready and no timer: wait for good, as CPython's loop would, but
/// for a promise awaited inside `asyncio.run()`, which only JavaScript can
/// settle and which this loop, running inside a call from JavaScript,
/// cannot hand control back for.
fn idle(bridge: bool) -> Vec<IRStatement> {
    let mut out = Vec::new();
    if bridge {
        out.push(if_(
            cmp(len(loop_var("awaiting")), IRCompareOp::Gt, int(0)),
            vec![raise(
                "RuntimeError",
                "a JavaScript promise is awaited inside asyncio.run(), which cannot hand \
                 control back to JavaScript to settle it. Hint: export the coroutine and \
                 await it from JavaScript",
            )],
        ));
    }
    out.push(expr_stmt(call(IDLE_FN, vec![])));
    out
}

/// The loop: scheduling, the timer heap, a turn, running until a future is
/// done, and what `asyncio.run()` does around that.
pub(crate) fn runtime_functions(bridge: bool) -> Vec<IRFunction> {
    let h = || cls("_Handle");
    let heap = || loop_var("timers");
    let swap = |a: IRExpr, b: IRExpr| {
        vec![
            assign_typed("t", idx(heap(), a.clone()), &h()),
            set_item(heap(), a, idx(heap(), b.clone())),
            set_item(heap(), b, v("t")),
        ]
    };
    let before = |a: IRExpr, b: IRExpr| f("before", vec![a, b]);
    let mut up = vec![assign_typed(
        "p",
        bin(bin(v("i"), IROp::Sub, int(1)), IROp::FloorDiv, int(2)),
        &IRType::Int,
    )];
    let mut swapped = swap(v("i"), v("p"));
    swapped.push(assign("i", v("p")));
    up.push(if_else(
        before(idx(heap(), v("i")), idx(heap(), v("p"))),
        swapped,
        vec![IRStatement::Break],
    ));
    let mut down_swap = swap(v("i"), v("m"));
    down_swap.push(assign("i", v("m")));
    let report = |what: &str, exc: IRExpr| {
        vec![
            write_err(text(what)),
            write_err(bin(f("exc_line", vec![exc]), IROp::Add, text("\n"))),
        ]
    };
    let mut never_retrieved = vec![set_attr(v("t"), "_log_traceback", boolean(false))];
    never_retrieved.extend(report(
        "Task exception was never retrieved\n",
        attr(v("t"), "_exc"),
    ));
    let mut leftover = vec![if_(
        mcall(v("t"), "cancelled", vec![]),
        vec![IRStatement::Continue],
    )];
    let mut unhandled = vec![set_attr(v("t"), "_log_traceback", boolean(false))];
    unhandled.extend(report(
        "unhandled exception during asyncio.run() shutdown\n",
        attr(v("t"), "_exc"),
    ));
    leftover.push(if_(attr(v("t"), "_has_exc"), unhandled));
    vec![
        function(
            "call_soon",
            vec![
                param("cb", &cls("_Callback")),
                param("arg", &opt(cls("_FutureBase"))),
            ],
            h(),
            vec![
                assign_typed(
                    "h",
                    call(
                        &name("_Handle"),
                        vec![v("cb"), v("arg"), float(0.0), int(0)],
                    ),
                    &h(),
                ),
                expr_stmt(mcall(loop_var("ready"), "append", vec![v("h")])),
                ret_val(v("h")),
            ],
        ),
        function(
            "call_at",
            vec![
                param("when", &IRType::Float),
                param("cb", &cls("_Callback")),
            ],
            h(),
            vec![
                set_loop("seq", bin(loop_var("seq"), IROp::Add, int(1))),
                assign_typed(
                    "h",
                    call(
                        &name("_Handle"),
                        vec![v("cb"), none(), v("when"), loop_var("seq")],
                    ),
                    &h(),
                ),
                expr_stmt(f("push", vec![v("h")])),
                ret_val(v("h")),
            ],
        ),
        function(
            "before",
            vec![param("a", &h()), param("b", &h())],
            IRType::Bool,
            vec![ret_val(or(
                cmp(attr(v("a"), "when"), IRCompareOp::Lt, attr(v("b"), "when")),
                and(
                    eq(attr(v("a"), "when"), attr(v("b"), "when")),
                    cmp(attr(v("a"), "seq"), IRCompareOp::Lt, attr(v("b"), "seq")),
                ),
            ))],
        ),
        function(
            "push",
            vec![param("h", &h())],
            IRType::None,
            vec![
                expr_stmt(mcall(heap(), "append", vec![v("h")])),
                assign_typed("i", bin(len(heap()), IROp::Sub, int(1)), &IRType::Int),
                while_(cmp(v("i"), IRCompareOp::Gt, int(0)), up),
            ],
        ),
        function(
            "pop",
            vec![],
            h(),
            vec![
                assign_typed("top", idx(heap(), int(0)), &h()),
                assign_typed("last", mcall(heap(), "pop", vec![]), &h()),
                assign_typed("n", len(heap()), &IRType::Int),
                if_(
                    cmp(v("n"), IRCompareOp::Gt, int(0)),
                    vec![
                        set_item(heap(), int(0), v("last")),
                        assign_typed("i", int(0), &IRType::Int),
                        while_(
                            boolean(true),
                            vec![
                                assign_typed(
                                    "l",
                                    bin(bin(v("i"), IROp::Mul, int(2)), IROp::Add, int(1)),
                                    &IRType::Int,
                                ),
                                assign_typed("r", bin(v("l"), IROp::Add, int(1)), &IRType::Int),
                                assign_typed("m", v("i"), &IRType::Int),
                                if_(
                                    and(
                                        cmp(v("l"), IRCompareOp::Lt, v("n")),
                                        before(idx(heap(), v("l")), idx(heap(), v("m"))),
                                    ),
                                    vec![assign("m", v("l"))],
                                ),
                                if_(
                                    and(
                                        cmp(v("r"), IRCompareOp::Lt, v("n")),
                                        before(idx(heap(), v("r")), idx(heap(), v("m"))),
                                    ),
                                    vec![assign("m", v("r"))],
                                ),
                                if_(eq(v("m"), v("i")), vec![IRStatement::Break]),
                                if_(boolean(true), down_swap),
                            ],
                        ),
                    ],
                ),
                ret_val(v("top")),
            ],
        ),
        // `BaseEventLoop._run_once`.
        function(
            "run_once",
            vec![],
            IRType::None,
            vec![
                while_(
                    and(
                        cmp(len(heap()), IRCompareOp::Gt, int(0)),
                        attr(idx(heap(), int(0)), "cancelled"),
                    ),
                    vec![expr_stmt(f("pop", vec![]))],
                ),
                if_(
                    and(
                        eq(len(loop_var("ready")), int(0)),
                        not(loop_var("stopping")),
                    ),
                    vec![if_else(
                        cmp(len(heap()), IRCompareOp::Gt, int(0)),
                        vec![
                            assign_typed(
                                "wait",
                                bin(
                                    attr(idx(heap(), int(0)), "when"),
                                    IROp::Sub,
                                    call(CLOCK_FN, vec![]),
                                ),
                                &IRType::Float,
                            ),
                            if_(
                                cmp(v("wait"), IRCompareOp::Gt, float(0.0)),
                                vec![expr_stmt(call(WAIT_FN, vec![v("wait")]))],
                            ),
                        ],
                        idle(bridge),
                    )],
                ),
                // Timers due by now (and the clock's resolution) join the
                // ready callbacks.
                assign_typed(
                    "end",
                    bin(call(CLOCK_FN, vec![]), IROp::Add, float(1.0)),
                    &IRType::Float,
                ),
                while_(
                    cmp(len(heap()), IRCompareOp::Gt, int(0)),
                    vec![
                        if_(
                            cmp(
                                attr(idx(heap(), int(0)), "when"),
                                IRCompareOp::GtE,
                                v("end"),
                            ),
                            vec![IRStatement::Break],
                        ),
                        expr_stmt(mcall(loop_var("ready"), "append", vec![f("pop", vec![])])),
                    ],
                ),
                // Exactly what was ready as the turn began.
                assign_typed("n", len(loop_var("ready")), &IRType::Int),
                assign_typed("i", int(0), &IRType::Int),
                while_(
                    cmp(v("i"), IRCompareOp::Lt, v("n")),
                    vec![
                        assign_typed("h", mcall(loop_var("ready"), "pop", vec![int(0)]), &h()),
                        assign("i", bin(v("i"), IROp::Add, int(1))),
                        if_(
                            not(attr(v("h"), "cancelled")),
                            vec![expr_stmt(mcall(
                                attr(v("h"), "cb"),
                                "run",
                                vec![attr(v("h"), "arg")],
                            ))],
                        ),
                    ],
                ),
            ],
        ),
        // `BaseEventLoop.run_until_complete`, minus the result.
        function(
            "run_until_complete",
            vec![param("fut", &cls("_FutureBase"))],
            IRType::None,
            vec![
                assign_typed("stop", call(&name("_StopLoop"), vec![]), &cls("_Callback")),
                expr_stmt(mcall(v("fut"), "_add_cb", vec![v("stop")])),
                while_(
                    boolean(true),
                    vec![
                        expr_stmt(f("run_once", vec![])),
                        if_(loop_var("stopping"), vec![IRStatement::Break]),
                    ],
                ),
                set_loop("stopping", boolean(false)),
                expr_stmt(mcall(v("fut"), "_remove_cb", vec![v("stop")])),
                if_(
                    not(mcall(v("fut"), "done", vec![])),
                    vec![raise(
                        "RuntimeError",
                        "Event loop stopped before Future completed.",
                    )],
                ),
            ],
        ),
        function(
            "reset",
            vec![],
            IRType::None,
            vec![
                set_loop("ready", IRExpr::ListLiteral(vec![])),
                set_loop("timers", IRExpr::ListLiteral(vec![])),
                set_loop("tasks", IRExpr::ListLiteral(vec![])),
                set_loop("failed", IRExpr::ListLiteral(vec![])),
                set_loop("stopping", boolean(false)),
                set_loop("has_current", boolean(false)),
            ],
        ),
        // `Runner.close()`: cancel what is left and wait for it, then the
        // two shutdown steps, each of which runs a task of its own.
        function(
            "shutdown",
            vec![],
            IRType::None,
            vec![
                assign_typed(
                    "pending",
                    IRExpr::ListLiteral(vec![]),
                    &list(cls("_FutureBase")),
                ),
                for_(
                    "t",
                    loop_var("tasks"),
                    vec![if_(
                        not(mcall(v("t"), "done", vec![])),
                        vec![expr_stmt(mcall(v("pending"), "append", vec![v("t")]))],
                    )],
                ),
                if_(
                    cmp(len(v("pending")), IRCompareOp::Gt, int(0)),
                    vec![
                        for_(
                            "t",
                            v("pending"),
                            vec![expr_stmt(mcall(v("t"), "cancel", vec![]))],
                        ),
                        assign_typed("g", call(&name("_Gathering"), vec![]), &cls("_Gathering")),
                        set_attr(v("g"), "_return_exceptions", boolean(true)),
                        for_(
                            "t",
                            v("pending"),
                            vec![expr_stmt(mcall(v("g"), "_add", vec![v("t")]))],
                        ),
                        expr_stmt(mcall(v("g"), "_start", vec![])),
                        expr_stmt(f("run_until_complete", vec![v("g")])),
                        for_("t", v("pending"), leftover),
                    ],
                ),
                // CPython reports a task's exception nothing retrieved as
                // the task is reclaimed; here, as the loop closes.
                for_(
                    "t",
                    loop_var("failed"),
                    vec![if_(attr(v("t"), "_log_traceback"), never_retrieved)],
                ),
                set_loop("names", bin(loop_var("names"), IROp::Add, int(2))),
                set_loop("running", boolean(false)),
                expr_stmt(f("reset", vec![])),
            ],
        ),
        // The last line of CPython's report of an exception.
        function(
            "exc_line",
            vec![param("e", &IRType::Class("BaseException".to_string()))],
            IRType::String,
            vec![
                assign_typed(
                    "shown",
                    call("str", vec![call("type", vec![v("e")])]),
                    &IRType::String,
                ),
                assign_typed(
                    "name",
                    IRExpr::Slicing {
                        container: Box::new(v("shown")),
                        start: Some(Box::new(int(8))),
                        end: Some(Box::new(int(-2))),
                        step: None,
                    },
                    &IRType::String,
                ),
                if_(
                    mcall(v("name"), "startswith", vec![text("__main__.")]),
                    vec![assign(
                        "name",
                        IRExpr::Slicing {
                            container: Box::new(v("name")),
                            start: Some(Box::new(int(9))),
                            end: None,
                            step: None,
                        },
                    )],
                ),
                assign_typed("said", call("str", vec![v("e")]), &IRType::String),
                if_(eq(len(v("said")), int(0)), vec![ret_val(v("name"))]),
                ret_val(bin(
                    bin(v("name"), IROp::Add, text(": ")),
                    IROp::Add,
                    v("said"),
                )),
            ],
        ),
    ]
}
