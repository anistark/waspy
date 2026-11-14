//! `asyncio`'s synchronization primitives, queues, and timeouts: CPython
//! 3.13's `asyncio/locks.py` (`Lock`, `Event`, `Semaphore`,
//! `BoundedSemaphore`), `asyncio/queues.py` (`Queue`, `LifoQueue`, and
//! `PriorityQueue`, written once per element type), and the
//! `asyncio.timeout()` context manager of `asyncio/timeouts.py`, written out
//! in IR on the runtime of `ir::asyncio`.
//!
//! A coroutine method (`acquire()`, `wait()`, `put()`, `get()`, `join()`,
//! `__aenter__()`, `__aexit__()`) answers the coroutine of a runtime
//! coroutine function (`__asyncio_lock_acquire(lock)`), which the coroutine
//! pass lowers with the program's own, so awaiting one, making a task of one,
//! or handing one to `wait_for()` works as it does for any coroutine. A
//! waiter is a `Future[None]` the waking side completes, as CPython's is a
//! future whose result nobody reads, so which task wakes when is CPython's.
//!
//! Only what the program names is written: a module that never makes a lock
//! carries none of this.

use super::asyncio::{
    eq, fname, future_class, is_none, list, me, not_none, opt, qcall, raise_new, spelled, Api,
    CLOCK_FN, NS_FN,
};
use super::build::*;
use crate::ir::{IRClass, IRCompareOp, IRExceptHandler, IRExpr, IRFunction, IRModule, IROp};
use crate::ir::{IRStatement, IRType};

pub(crate) const LOCK: &str = "asyncio.locks.Lock";
pub(crate) const EVENT: &str = "asyncio.locks.Event";
pub(crate) const SEMAPHORE: &str = "asyncio.locks.Semaphore";
pub(crate) const BOUNDED: &str = "asyncio.locks.BoundedSemaphore";
pub(crate) const QUEUE_EMPTY: &str = "asyncio.queues.QueueEmpty";
pub(crate) const QUEUE_FULL: &str = "asyncio.queues.QueueFull";
pub(crate) const QUEUE_SHUTDOWN: &str = "asyncio.queues.QueueShutDown";
pub(crate) const TASKGROUP: &str = "asyncio.taskgroups.TaskGroup";
const GROUP_DONE: &str = "asyncio.taskgroups._Done";
/// The queue kinds, each a class of `asyncio.queues` per element type.
pub(crate) const QUEUE_KINDS: [&str; 3] = ["Queue", "LifoQueue", "PriorityQueue"];

/// The class a queue of the kind holding `t` is.
pub(crate) fn queue_class(kind: &str, t: &IRType) -> String {
    format!("asyncio.queues.{kind}[{}]", spelled(t))
}

/// The class an annotation names in `asyncio` (`asyncio.Lock`), when it is
/// one of these.
pub(crate) fn annotated_class(attr: &str) -> Option<String> {
    Some(match attr {
        "Lock" => LOCK.to_string(),
        "Event" => EVENT.to_string(),
        "Semaphore" => SEMAPHORE.to_string(),
        "BoundedSemaphore" => BOUNDED.to_string(),
        "Timeout" => super::asyncio::name("_Timeout"),
        "TaskGroup" => TASKGROUP.to_string(),
        "QueueEmpty" => QUEUE_EMPTY.to_string(),
        "QueueFull" => QUEUE_FULL.to_string(),
        "QueueShutDown" => QUEUE_SHUTDOWN.to_string(),
        _ => return None,
    })
}

/// The exception classes these modules raise, by the name `asyncio` gives
/// them.
pub(crate) fn exception_class(attr: &str) -> Option<&'static str> {
    match attr {
        "QueueEmpty" => Some(QUEUE_EMPTY),
        "QueueFull" => Some(QUEUE_FULL),
        "QueueShutDown" => Some(QUEUE_SHUTDOWN),
        _ => None,
    }
}

fn future_none() -> IRType {
    IRType::Class(future_class(&IRType::None))
}
fn waiters() -> IRType {
    list(future_none())
}
fn obj(class: &str) -> IRType {
    IRType::Class(class.to_string())
}
fn base_exc() -> IRType {
    IRType::Class("BaseException".to_string())
}
/// The coroutine class a runtime coroutine function makes.
fn coro_of(function: &str) -> IRType {
    IRType::Class(crate::ir::coroutines::state_class(function))
}
fn awaited(e: IRExpr) -> IRStatement {
    expr_stmt(call(crate::ir::AWAIT_FN, vec![e]))
}
fn reraise() -> IRStatement {
    IRStatement::Raise {
        exception: None,
        cause: None,
    }
}
fn try_finally(body: Vec<IRStatement>, finally: Vec<IRStatement>) -> IRStatement {
    IRStatement::TryExcept {
        try_body: crate::ir::build::body(body),
        except_handlers: Vec::new(),
        finally_body: Some(crate::ir::build::body(finally)),
    }
}
fn handler(class: &str, statements: Vec<IRStatement>) -> IRExceptHandler {
    IRExceptHandler {
        exception_types: vec![class.to_string()],
        name: None,
        body: crate::ir::IRBody { statements },
    }
}
fn coroutine(
    short: &str,
    params: Vec<crate::ir::IRParam>,
    result: IRType,
    body: Vec<IRStatement>,
) -> IRFunction {
    let mut f = method(
        &fname(short),
        params,
        IRType::Coroutine(Box::new(result)),
        body,
    );
    f.decorators = Vec::new();
    f
}
/// A method answering the coroutine of the runtime coroutine function
/// `short`, given the receiver and the method's other parameters.
fn coroutine_method(
    name: &str,
    class: &str,
    params: Vec<crate::ir::IRParam>,
    short: &str,
    passed: Vec<IRExpr>,
) -> IRFunction {
    let mut all = vec![me(class)];
    all.extend(params);
    let mut args = vec![v("self")];
    args.extend(passed);
    method(
        name,
        all,
        coro_of(&fname(short)),
        vec![ret_val(call(&fname(short), args))],
    )
}
fn plain_class(name: &str, base: Option<&str>, methods: Vec<IRFunction>) -> IRClass {
    IRClass {
        name: name.to_string(),
        bases: base.map(str::to_string).into_iter().collect(),
        methods,
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}
/// `self._remove(waiters, fut)`: let go of a waiter, if it is still listed
/// (`deque.remove`, whose ValueError CPython catches where it may be gone).
fn remove_method(class: &str) -> IRFunction {
    method(
        "_drop",
        vec![
            me(class),
            param("ws", &waiters()),
            param("fut", &future_none()),
        ],
        IRType::None,
        vec![
            assign_typed("i", int(0), &IRType::Int),
            while_(
                cmp(v("i"), IRCompareOp::Lt, len(v("ws"))),
                vec![
                    if_(
                        cmp(idx(v("ws"), v("i")), IRCompareOp::Is, v("fut")),
                        vec![expr_stmt(mcall(v("ws"), "pop", vec![v("i")])), ret()],
                    ),
                    assign("i", bin(v("i"), IROp::Add, int(1))),
                ],
            ),
        ],
    )
}
/// The context-manager methods of a lock or a semaphore
/// (`_ContextManagerMixin`): acquire on the way in, release on the way out.
fn mixin_methods(class: &str, stem: &str) -> Vec<IRFunction> {
    let opt_exc = || opt(base_exc());
    vec![
        coroutine_method(
            "__aenter__",
            class,
            vec![],
            &format!("{stem}_aenter"),
            vec![],
        ),
        coroutine_method(
            "__aexit__",
            class,
            vec![
                param("t", &opt_exc()),
                param("e", &opt_exc()),
                param("tb", &opt_exc()),
            ],
            &format!("{stem}_aexit"),
            vec![],
        ),
    ]
}
fn mixin_coroutines(class: &str, stem: &str) -> Vec<IRFunction> {
    vec![
        coroutine(
            &format!("{stem}_aenter"),
            vec![param("x", &obj(class))],
            IRType::None,
            vec![awaited(mcall(v("x"), "acquire", vec![]))],
        ),
        coroutine(
            &format!("{stem}_aexit"),
            vec![param("x", &obj(class))],
            IRType::Bool,
            vec![
                expr_stmt(mcall(v("x"), "release", vec![])),
                ret_val(boolean(false)),
            ],
        ),
    ]
}

// ---- locks.py ---------------------------------------------------------------------

fn lock() -> IRClass {
    let c = LOCK;
    let mut methods = vec![
        method(
            "__init__",
            vec![me(c)],
            IRType::None,
            vec![
                set_my_typed("_waiters", IRExpr::ListLiteral(vec![]), &waiters()),
                set_my_typed("_locked", boolean(false), &IRType::Bool),
            ],
        ),
        method(
            "locked",
            vec![me(c)],
            IRType::Bool,
            vec![ret_val(my("_locked"))],
        ),
        // No waiter that is not cancelled.
        method(
            "_free",
            vec![me(c)],
            IRType::Bool,
            vec![
                for_(
                    "w",
                    my("_waiters"),
                    vec![if_(
                        not(mcall(v("w"), "cancelled", vec![])),
                        vec![ret_val(boolean(false))],
                    )],
                ),
                ret_val(boolean(true)),
            ],
        ),
        coroutine_method("acquire", c, vec![], "lock_acquire", vec![]),
        method(
            "release",
            vec![me(c)],
            IRType::None,
            vec![if_else(
                my("_locked"),
                vec![
                    set_attr(v("self"), "_locked", boolean(false)),
                    expr_stmt(mcall(v("self"), "_wake_up_first", vec![])),
                ],
                vec![raise("RuntimeError", "Lock is not acquired.")],
            )],
        ),
        method(
            "_wake_up_first",
            vec![me(c)],
            IRType::None,
            vec![
                if_(eq(len(my("_waiters")), int(0)), vec![ret()]),
                assign_typed("fut", idx(my("_waiters"), int(0)), &future_none()),
                if_(
                    not(mcall(v("fut"), "done", vec![])),
                    vec![expr_stmt(mcall(v("fut"), "_set_none", vec![]))],
                ),
            ],
        ),
        remove_method(c),
    ];
    methods.extend(mixin_methods(c, "lock"));
    plain_class(c, None, methods)
}

fn lock_coroutines() -> Vec<IRFunction> {
    let mut out = vec![coroutine(
        "lock_acquire",
        vec![param("lock", &obj(LOCK))],
        IRType::Bool,
        vec![
            if_(
                and(
                    not(attr(v("lock"), "_locked")),
                    mcall(v("lock"), "_free", vec![]),
                ),
                vec![
                    set_attr(v("lock"), "_locked", boolean(true)),
                    ret_val(boolean(true)),
                ],
            ),
            assign_typed(
                "fut",
                call(&future_class(&IRType::None), vec![]),
                &future_none(),
            ),
            expr_stmt(mcall(attr(v("lock"), "_waiters"), "append", vec![v("fut")])),
            IRStatement::TryExcept {
                try_body: body(vec![try_finally(
                    vec![awaited(v("fut"))],
                    vec![expr_stmt(mcall(
                        v("lock"),
                        "_drop",
                        vec![attr(v("lock"), "_waiters"), v("fut")],
                    ))],
                )]),
                except_handlers: vec![handler(
                    "CancelledError",
                    vec![
                        if_(
                            not(attr(v("lock"), "_locked")),
                            vec![expr_stmt(mcall(v("lock"), "_wake_up_first", vec![]))],
                        ),
                        reraise(),
                    ],
                )],
                finally_body: None,
            },
            set_attr(v("lock"), "_locked", boolean(true)),
            ret_val(boolean(true)),
        ],
    )];
    out.extend(mixin_coroutines(LOCK, "lock"));
    out
}

fn event() -> IRClass {
    let c = EVENT;
    plain_class(
        c,
        None,
        vec![
            method(
                "__init__",
                vec![me(c)],
                IRType::None,
                vec![
                    set_my_typed("_waiters", IRExpr::ListLiteral(vec![]), &waiters()),
                    set_my_typed("_value", boolean(false), &IRType::Bool),
                ],
            ),
            method(
                "is_set",
                vec![me(c)],
                IRType::Bool,
                vec![ret_val(my("_value"))],
            ),
            method(
                "set",
                vec![me(c)],
                IRType::None,
                vec![if_(
                    not(my("_value")),
                    vec![
                        set_attr(v("self"), "_value", boolean(true)),
                        for_(
                            "fut",
                            my("_waiters"),
                            vec![if_(
                                not(mcall(v("fut"), "done", vec![])),
                                vec![expr_stmt(mcall(v("fut"), "_set_none", vec![]))],
                            )],
                        ),
                    ],
                )],
            ),
            method(
                "clear",
                vec![me(c)],
                IRType::None,
                vec![set_attr(v("self"), "_value", boolean(false))],
            ),
            coroutine_method("wait", c, vec![], "event_wait", vec![]),
            remove_method(c),
        ],
    )
}

fn event_coroutines() -> Vec<IRFunction> {
    vec![coroutine(
        "event_wait",
        vec![param("ev", &obj(EVENT))],
        IRType::Bool,
        vec![
            if_(attr(v("ev"), "_value"), vec![ret_val(boolean(true))]),
            assign_typed(
                "fut",
                call(&future_class(&IRType::None), vec![]),
                &future_none(),
            ),
            expr_stmt(mcall(attr(v("ev"), "_waiters"), "append", vec![v("fut")])),
            try_finally(
                vec![awaited(v("fut")), ret_val(boolean(true))],
                vec![expr_stmt(mcall(
                    v("ev"),
                    "_drop",
                    vec![attr(v("ev"), "_waiters"), v("fut")],
                ))],
            ),
        ],
    )]
}

fn semaphore() -> IRClass {
    let c = SEMAPHORE;
    let mut methods = vec![
        method(
            "__init__",
            vec![me(c), param("value", &IRType::Int)],
            IRType::None,
            vec![
                if_(
                    cmp(v("value"), IRCompareOp::Lt, int(0)),
                    vec![raise("ValueError", "Semaphore initial value must be >= 0")],
                ),
                set_my_typed("_waiters", IRExpr::ListLiteral(vec![]), &waiters()),
                set_my_typed("_value", v("value"), &IRType::Int),
            ],
        ),
        method(
            "locked",
            vec![me(c)],
            IRType::Bool,
            vec![
                if_(eq(my("_value"), int(0)), vec![ret_val(boolean(true))]),
                for_(
                    "w",
                    my("_waiters"),
                    vec![if_(
                        not(mcall(v("w"), "cancelled", vec![])),
                        vec![ret_val(boolean(true))],
                    )],
                ),
                ret_val(boolean(false)),
            ],
        ),
        coroutine_method("acquire", c, vec![], "sem_acquire", vec![]),
        method(
            "release",
            vec![me(c)],
            IRType::None,
            vec![
                set_attr(v("self"), "_value", bin(my("_value"), IROp::Add, int(1))),
                expr_stmt(mcall(v("self"), "_wake_up_next", vec![])),
            ],
        ),
        method(
            "_wake_up_next",
            vec![me(c)],
            IRType::Bool,
            vec![
                for_(
                    "fut",
                    my("_waiters"),
                    vec![if_(
                        not(mcall(v("fut"), "done", vec![])),
                        vec![
                            set_attr(v("self"), "_value", bin(my("_value"), IROp::Sub, int(1))),
                            expr_stmt(mcall(v("fut"), "_set_none", vec![])),
                            ret_val(boolean(true)),
                        ],
                    )],
                ),
                ret_val(boolean(false)),
            ],
        ),
        remove_method(c),
    ];
    methods.extend(mixin_methods(c, "sem"));
    plain_class(c, None, methods)
}

fn bounded_semaphore() -> IRClass {
    let c = BOUNDED;
    plain_class(
        c,
        Some(SEMAPHORE),
        vec![
            method(
                "__init__",
                vec![me(c), param("value", &IRType::Int)],
                IRType::None,
                vec![
                    set_my_typed("_bound_value", v("value"), &IRType::Int),
                    expr_stmt(qcall(SEMAPHORE, "__init__", vec![v("self"), v("value")])),
                ],
            ),
            method(
                "release",
                vec![me(c)],
                IRType::None,
                vec![
                    if_(
                        cmp(my("_value"), IRCompareOp::GtE, my("_bound_value")),
                        vec![raise(
                            "ValueError",
                            "BoundedSemaphore released too many times",
                        )],
                    ),
                    expr_stmt(qcall(SEMAPHORE, "release", vec![v("self")])),
                ],
            ),
        ],
    )
}

fn semaphore_coroutines() -> Vec<IRFunction> {
    let mut out = vec![coroutine(
        "sem_acquire",
        vec![param("sem", &obj(SEMAPHORE))],
        IRType::Bool,
        vec![
            if_(
                not(mcall(v("sem"), "locked", vec![])),
                vec![
                    set_attr(
                        v("sem"),
                        "_value",
                        bin(attr(v("sem"), "_value"), IROp::Sub, int(1)),
                    ),
                    ret_val(boolean(true)),
                ],
            ),
            assign_typed(
                "fut",
                call(&future_class(&IRType::None), vec![]),
                &future_none(),
            ),
            expr_stmt(mcall(attr(v("sem"), "_waiters"), "append", vec![v("fut")])),
            IRStatement::TryExcept {
                try_body: body(vec![try_finally(
                    vec![awaited(v("fut"))],
                    vec![expr_stmt(mcall(
                        v("sem"),
                        "_drop",
                        vec![attr(v("sem"), "_waiters"), v("fut")],
                    ))],
                )]),
                except_handlers: vec![handler(
                    "CancelledError",
                    vec![
                        if_(
                            and(
                                mcall(v("fut"), "done", vec![]),
                                not(mcall(v("fut"), "cancelled", vec![])),
                            ),
                            vec![set_attr(
                                v("sem"),
                                "_value",
                                bin(attr(v("sem"), "_value"), IROp::Add, int(1)),
                            )],
                        ),
                        reraise(),
                    ],
                )],
                finally_body: Some(body(vec![while_(
                    cmp(attr(v("sem"), "_value"), IRCompareOp::Gt, int(0)),
                    vec![if_(
                        not(mcall(v("sem"), "_wake_up_next", vec![])),
                        vec![IRStatement::Break],
                    )],
                )])),
            },
            ret_val(boolean(true)),
        ],
    )];
    out.extend(mixin_coroutines(SEMAPHORE, "sem"));
    out
}

// ---- queues.py ----------------------------------------------------------------------

fn error_class(name: &str) -> IRClass {
    plain_class(name, Some("Exception"), Vec::new())
}

/// `asyncio.Queue[T]`, the base of the other two kinds.
fn queue(t: &IRType) -> IRClass {
    let c = queue_class("Queue", t);
    let items = || list(t.clone());
    let stem = format!("Queue[{}]", spelled(t));
    let wakeup = |ws: &str| expr_stmt(mcall(v("self"), "_wakeup_next", vec![my(ws)]));
    let mut maxsize = method(
        "maxsize",
        vec![me(&c)],
        IRType::Int,
        vec![ret_val(my("_maxsize"))],
    );
    maxsize.decorators = vec!["property".to_string()];
    let unfinished = || my("_unfinished_tasks");
    plain_class(
        &c,
        None,
        vec![
            method(
                "__init__",
                vec![me(&c), param("maxsize", &IRType::Int)],
                IRType::None,
                vec![
                    set_my_typed("_maxsize", v("maxsize"), &IRType::Int),
                    set_my_typed("_getters", IRExpr::ListLiteral(vec![]), &waiters()),
                    set_my_typed("_putters", IRExpr::ListLiteral(vec![]), &waiters()),
                    set_my_typed("_unfinished_tasks", int(0), &IRType::Int),
                    set_my_typed("_finished", call(EVENT, vec![]), &obj(EVENT)),
                    expr_stmt(mcall(my("_finished"), "set", vec![])),
                    set_my_typed("_queue", IRExpr::ListLiteral(vec![]), &items()),
                    set_my_typed("_is_shutdown", boolean(false), &IRType::Bool),
                ],
            ),
            method(
                "_get",
                vec![me(&c)],
                t.clone(),
                vec![ret_val(mcall(my("_queue"), "pop", vec![int(0)]))],
            ),
            method(
                "_put",
                vec![me(&c), param("item", t)],
                IRType::None,
                vec![expr_stmt(mcall(my("_queue"), "append", vec![v("item")]))],
            ),
            method(
                "_wakeup_next",
                vec![me(&c), param("ws", &waiters())],
                IRType::None,
                vec![while_(
                    cmp(len(v("ws")), IRCompareOp::Gt, int(0)),
                    vec![
                        assign_typed("w", mcall(v("ws"), "pop", vec![int(0)]), &future_none()),
                        if_(
                            not(mcall(v("w"), "done", vec![])),
                            vec![
                                expr_stmt(mcall(v("w"), "_set_none", vec![])),
                                IRStatement::Break,
                            ],
                        ),
                    ],
                )],
            ),
            method(
                "qsize",
                vec![me(&c)],
                IRType::Int,
                vec![ret_val(len(my("_queue")))],
            ),
            maxsize,
            method(
                "empty",
                vec![me(&c)],
                IRType::Bool,
                vec![ret_val(eq(len(my("_queue")), int(0)))],
            ),
            method(
                "full",
                vec![me(&c)],
                IRType::Bool,
                vec![
                    if_(
                        cmp(my("_maxsize"), IRCompareOp::LtE, int(0)),
                        vec![ret_val(boolean(false))],
                    ),
                    ret_val(cmp(len(my("_queue")), IRCompareOp::GtE, my("_maxsize"))),
                ],
            ),
            coroutine_method(
                "put",
                &c,
                vec![param("item", t)],
                &format!("{stem}_put"),
                vec![v("item")],
            ),
            method(
                "put_nowait",
                vec![me(&c), param("item", t)],
                IRType::None,
                vec![
                    if_(my("_is_shutdown"), vec![raise_new(QUEUE_SHUTDOWN)]),
                    if_(
                        mcall(v("self"), "full", vec![]),
                        vec![raise_new(QUEUE_FULL)],
                    ),
                    expr_stmt(mcall(v("self"), "_put", vec![v("item")])),
                    set_attr(
                        v("self"),
                        "_unfinished_tasks",
                        bin(unfinished(), IROp::Add, int(1)),
                    ),
                    expr_stmt(mcall(my("_finished"), "clear", vec![])),
                    wakeup("_getters"),
                ],
            ),
            coroutine_method("get", &c, vec![], &format!("{stem}_get"), vec![]),
            method(
                "get_nowait",
                vec![me(&c)],
                t.clone(),
                vec![
                    if_(
                        mcall(v("self"), "empty", vec![]),
                        vec![
                            if_(my("_is_shutdown"), vec![raise_new(QUEUE_SHUTDOWN)]),
                            raise_new(QUEUE_EMPTY),
                        ],
                    ),
                    assign_typed("item", mcall(v("self"), "_get", vec![]), t),
                    wakeup("_putters"),
                    ret_val(v("item")),
                ],
            ),
            method(
                "task_done",
                vec![me(&c)],
                IRType::None,
                vec![
                    if_(
                        cmp(unfinished(), IRCompareOp::LtE, int(0)),
                        vec![raise("ValueError", "task_done() called too many times")],
                    ),
                    set_attr(
                        v("self"),
                        "_unfinished_tasks",
                        bin(unfinished(), IROp::Sub, int(1)),
                    ),
                    if_(
                        eq(unfinished(), int(0)),
                        vec![expr_stmt(mcall(my("_finished"), "set", vec![]))],
                    ),
                ],
            ),
            coroutine_method("join", &c, vec![], &format!("{stem}_join"), vec![]),
            method(
                "shutdown",
                vec![
                    me(&c),
                    param_default("immediate", &IRType::Bool, boolean(false)),
                ],
                IRType::None,
                vec![
                    set_attr(v("self"), "_is_shutdown", boolean(true)),
                    if_(
                        v("immediate"),
                        vec![
                            while_(
                                not(mcall(v("self"), "empty", vec![])),
                                vec![
                                    expr_stmt(mcall(v("self"), "_get", vec![])),
                                    if_(
                                        cmp(unfinished(), IRCompareOp::Gt, int(0)),
                                        vec![set_attr(
                                            v("self"),
                                            "_unfinished_tasks",
                                            bin(unfinished(), IROp::Sub, int(1)),
                                        )],
                                    ),
                                ],
                            ),
                            if_(
                                eq(unfinished(), int(0)),
                                vec![expr_stmt(mcall(my("_finished"), "set", vec![]))],
                            ),
                        ],
                    ),
                    for_waiters_set("_getters"),
                    for_waiters_set("_putters"),
                ],
            ),
            method(
                "_drop",
                vec![
                    me(&c),
                    param("ws", &waiters()),
                    param("fut", &future_none()),
                ],
                IRType::None,
                remove_method(&c).body.statements,
            ),
        ],
    )
}

/// `while ws: w = ws.popleft(); if not w.done(): w.set_result(None)`.
fn for_waiters_set(field: &str) -> IRStatement {
    while_(
        cmp(len(my(field)), IRCompareOp::Gt, int(0)),
        vec![
            assign_typed("w", mcall(my(field), "pop", vec![int(0)]), &future_none()),
            if_(
                not(mcall(v("w"), "done", vec![])),
                vec![expr_stmt(mcall(v("w"), "_set_none", vec![]))],
            ),
        ],
    )
}

fn lifo_queue(t: &IRType) -> IRClass {
    let c = queue_class("LifoQueue", t);
    plain_class(
        &c,
        Some(&queue_class("Queue", t)),
        vec![method(
            "_get",
            vec![me(&c)],
            t.clone(),
            vec![ret_val(mcall(my("_queue"), "pop", vec![]))],
        )],
    )
}

/// `PriorityQueue[T]`: `heapq`'s `heappush` and `heappop` over the list,
/// written as CPython's are, so items that compare equal come out in its
/// order.
fn priority_queue(t: &IRType) -> IRClass {
    let c = queue_class("PriorityQueue", t);
    let heap = || my("_queue");
    let at = |i: &str| idx(heap(), v(i));
    // heapq._siftdown(heap, startpos, pos)
    let siftdown = method(
        "_siftdown",
        vec![
            me(&c),
            param("startpos", &IRType::Int),
            param("pos", &IRType::Int),
        ],
        IRType::None,
        vec![
            assign_typed("newitem", at("pos"), t),
            while_(
                cmp(v("pos"), IRCompareOp::Gt, v("startpos")),
                vec![
                    assign_typed(
                        "parentpos",
                        bin(bin(v("pos"), IROp::Sub, int(1)), IROp::FloorDiv, int(2)),
                        &IRType::Int,
                    ),
                    assign_typed("parent", at("parentpos"), t),
                    if_(
                        cmp(v("newitem"), IRCompareOp::Lt, v("parent")),
                        vec![
                            set_item(heap(), v("pos"), v("parent")),
                            assign("pos", v("parentpos")),
                            IRStatement::Continue,
                        ],
                    ),
                    IRStatement::Break,
                ],
            ),
            set_item(heap(), v("pos"), v("newitem")),
        ],
    );
    // heapq._siftup(heap, pos)
    let siftup = method(
        "_siftup",
        vec![me(&c), param("pos", &IRType::Int)],
        IRType::None,
        vec![
            assign_typed("endpos", len(heap()), &IRType::Int),
            assign_typed("startpos", v("pos"), &IRType::Int),
            assign_typed("newitem", at("pos"), t),
            assign_typed(
                "childpos",
                bin(bin(int(2), IROp::Mul, v("pos")), IROp::Add, int(1)),
                &IRType::Int,
            ),
            while_(
                cmp(v("childpos"), IRCompareOp::Lt, v("endpos")),
                vec![
                    assign_typed(
                        "rightpos",
                        bin(v("childpos"), IROp::Add, int(1)),
                        &IRType::Int,
                    ),
                    if_(
                        cmp(v("rightpos"), IRCompareOp::Lt, v("endpos")),
                        vec![if_(
                            not(cmp(at("childpos"), IRCompareOp::Lt, at("rightpos"))),
                            vec![assign("childpos", v("rightpos"))],
                        )],
                    ),
                    set_item(heap(), v("pos"), at("childpos")),
                    assign("pos", v("childpos")),
                    assign(
                        "childpos",
                        bin(bin(int(2), IROp::Mul, v("pos")), IROp::Add, int(1)),
                    ),
                ],
            ),
            set_item(heap(), v("pos"), v("newitem")),
            expr_stmt(mcall(v("self"), "_siftdown", vec![v("startpos"), v("pos")])),
        ],
    );
    plain_class(
        &c,
        Some(&queue_class("Queue", t)),
        vec![
            method(
                "_put",
                vec![me(&c), param("item", t)],
                IRType::None,
                vec![
                    expr_stmt(mcall(heap(), "append", vec![v("item")])),
                    expr_stmt(mcall(
                        v("self"),
                        "_siftdown",
                        vec![int(0), bin(len(heap()), IROp::Sub, int(1))],
                    )),
                ],
            ),
            method(
                "_get",
                vec![me(&c)],
                t.clone(),
                vec![
                    assign_typed("lastelt", mcall(heap(), "pop", vec![]), t),
                    if_(
                        cmp(len(heap()), IRCompareOp::Gt, int(0)),
                        vec![
                            assign_typed("returnitem", idx(heap(), int(0)), t),
                            set_item(heap(), int(0), v("lastelt")),
                            expr_stmt(mcall(v("self"), "_siftup", vec![int(0)])),
                            ret_val(v("returnitem")),
                        ],
                    ),
                    ret_val(v("lastelt")),
                ],
            ),
            siftdown,
            siftup,
        ],
    )
}

fn queue_coroutines(t: &IRType) -> Vec<IRFunction> {
    let q = || param("q", &obj(&queue_class("Queue", t)));
    let stem = format!("Queue[{}]", spelled(t));
    // The waiting loop `put()` and `get()` share: wait on a new future in
    // `ws` while `blocked`, and on any failure let go of it, waking the next
    // waiter when there is room (or an item) after all.
    let wait_while = |blocked: IRExpr, ws: &str, shutdown: IRExpr| {
        while_(
            blocked.clone(),
            vec![
                if_(shutdown, vec![raise_new(QUEUE_SHUTDOWN)]),
                assign_typed(
                    "w",
                    call(&future_class(&IRType::None), vec![]),
                    &future_none(),
                ),
                expr_stmt(mcall(attr(v("q"), ws), "append", vec![v("w")])),
                IRStatement::TryExcept {
                    try_body: body(vec![awaited(v("w"))]),
                    except_handlers: vec![handler(
                        "BaseException",
                        vec![
                            expr_stmt(mcall(v("w"), "cancel", vec![])),
                            expr_stmt(mcall(v("q"), "_drop", vec![attr(v("q"), ws), v("w")])),
                            if_(
                                and(not(blocked), not(mcall(v("w"), "cancelled", vec![]))),
                                vec![expr_stmt(mcall(
                                    v("q"),
                                    "_wakeup_next",
                                    vec![attr(v("q"), ws)],
                                ))],
                            ),
                            reraise(),
                        ],
                    )],
                    finally_body: None,
                },
            ],
        )
    };
    vec![
        coroutine(
            &format!("{stem}_put"),
            vec![q(), param("item", t)],
            IRType::None,
            vec![
                wait_while(
                    mcall(v("q"), "full", vec![]),
                    "_putters",
                    attr(v("q"), "_is_shutdown"),
                ),
                expr_stmt(mcall(v("q"), "put_nowait", vec![v("item")])),
            ],
        ),
        coroutine(
            &format!("{stem}_get"),
            vec![q()],
            t.clone(),
            vec![
                wait_while(
                    mcall(v("q"), "empty", vec![]),
                    "_getters",
                    and(attr(v("q"), "_is_shutdown"), mcall(v("q"), "empty", vec![])),
                ),
                ret_val(mcall(v("q"), "get_nowait", vec![])),
            ],
        ),
        coroutine(
            &format!("{stem}_join"),
            vec![q()],
            IRType::None,
            vec![if_(
                cmp(attr(v("q"), "_unfinished_tasks"), IRCompareOp::Gt, int(0)),
                vec![awaited(mcall(attr(v("q"), "_finished"), "wait", vec![]))],
            )],
        ),
    ]
}

// ---- timeouts.py ---------------------------------------------------------------------

/// `Timeout.__aenter__` and `__aexit__`, which `ir::asyncio`'s `_Timeout`
/// (what `wait_for()` runs under) gains when the program uses
/// `asyncio.timeout()`.
fn timeout_methods() -> Vec<IRFunction> {
    let c = super::asyncio::name("_Timeout");
    let opt_exc = || opt(base_exc());
    vec![
        coroutine_method("__aenter__", &c, vec![], "timeout_aenter", vec![]),
        coroutine_method(
            "__aexit__",
            &c,
            vec![
                param("t", &opt_exc()),
                param("e", &opt_exc()),
                param("tb", &opt_exc()),
            ],
            "timeout_aexit",
            vec![v("e")],
        ),
        method(
            "expired",
            vec![me(&c)],
            IRType::Bool,
            vec![ret_val(or(
                eq(my("_state"), int(2)),
                eq(my("_state"), int(3)),
            ))],
        ),
    ]
}

fn timeout_coroutines() -> Vec<IRFunction> {
    let c = super::asyncio::name("_Timeout");
    vec![
        coroutine(
            "timeout_aenter",
            vec![param("t", &obj(&c))],
            obj(&c),
            vec![
                if_(
                    cmp(attr(v("t"), "_state"), IRCompareOp::NotEq, int(0)),
                    vec![raise("RuntimeError", "Timeout has already been entered")],
                ),
                expr_stmt(mcall(v("t"), "_enter", vec![])),
                ret_val(v("t")),
            ],
        ),
        coroutine(
            "timeout_aexit",
            vec![param("t", &obj(&c)), param("e", &opt(base_exc()))],
            IRType::Bool,
            vec![
                if_else(
                    is_none(v("e")),
                    vec![expr_stmt(mcall(v("t"), "_exit_ok", vec![]))],
                    vec![expr_stmt(mcall(
                        v("t"),
                        "_exit_failed",
                        vec![call(crate::ir::SOME_FN, vec![v("e"), text("_exit_failed")])],
                    ))],
                ),
                ret_val(boolean(false)),
            ],
        ),
    ]
}

/// `asyncio.timeout(delay)`: a timeout whose deadline is the delay from
/// now, or none for None.
pub(crate) fn timeout_call(delay: IRExpr) -> IRExpr {
    let c = super::asyncio::name("_Timeout");
    if matches!(delay, IRExpr::Const(crate::ir::IRConstant::None)) {
        return call(&c, vec![none()]);
    }
    call(
        &c,
        vec![bin(
            call(CLOCK_FN, vec![]),
            IROp::Add,
            call(NS_FN, vec![delay]),
        )],
    )
}

// ---- taskgroups.py -------------------------------------------------------------------

fn some(e: IRExpr, why: &str) -> IRExpr {
    call(crate::ir::SOME_FN, vec![e, text(why)])
}
fn isinstance(e: IRExpr, class: &str) -> IRExpr {
    call("isinstance", vec![e, v(class)])
}

/// `BaseExceptionGroup` and `ExceptionGroup`, what a task group raises: the
/// message, the exceptions as a tuple, and `str()` as CPython words it.
fn exception_groups() -> Vec<IRClass> {
    let base = "BaseExceptionGroup";
    let n = || len(my("exceptions"));
    vec![
        plain_class(
            base,
            Some("BaseException"),
            vec![
                method(
                    "__init__",
                    vec![
                        me(base),
                        param("message", &IRType::String),
                        param("exceptions", &list(base_exc())),
                    ],
                    IRType::None,
                    vec![
                        expr_stmt(mcall(call("super", vec![]), "__init__", vec![v("message")])),
                        set_my_typed("message", v("message"), &IRType::String),
                        set_my_typed(
                            "exceptions",
                            as_tuple(v("exceptions")),
                            &IRType::VarTuple(Box::new(base_exc())),
                        ),
                    ],
                ),
                method(
                    "__str__",
                    vec![me(base)],
                    IRType::String,
                    vec![
                        assign_typed("s", text(" sub-exception"), &IRType::String),
                        if_(
                            cmp(n(), IRCompareOp::NotEq, int(1)),
                            vec![assign("s", text(" sub-exceptions"))],
                        ),
                        ret_val(bin(
                            bin(
                                bin(my("message"), IROp::Add, text(" (")),
                                IROp::Add,
                                bin(call("str", vec![n()]), IROp::Add, v("s")),
                            ),
                            IROp::Add,
                            text(")"),
                        )),
                    ],
                ),
            ],
        ),
        IRClass {
            bases: vec![base.to_string(), "Exception".to_string()],
            ..plain_class("ExceptionGroup", None, Vec::new())
        },
    ]
}

/// `asyncio.TaskGroup`: its tasks in the order they were made (CPython's
/// is a set's, whose order is usually this), which is the order a failure
/// cancels the others in.
fn task_group() -> IRClass {
    let c = TASKGROUP;
    let tasks_ty = || list(cls_base());
    let check = |cond: IRExpr, what: &str| {
        if_(
            cond,
            vec![IRStatement::Raise {
                exception: Some(call(
                    "RuntimeError",
                    vec![bin(
                        bin(
                            text("TaskGroup "),
                            IROp::Add,
                            mcall(v("self"), "_repr", vec![]),
                        ),
                        IROp::Add,
                        text(what),
                    )],
                )),
                cause: None,
            }],
        )
    };
    let mut methods = vec![
        method(
            "__init__",
            vec![me(c)],
            IRType::None,
            vec![
                set_my_typed("_entered", boolean(false), &IRType::Bool),
                set_my_typed("_exiting", boolean(false), &IRType::Bool),
                set_my_typed("_aborting", boolean(false), &IRType::Bool),
                set_my_typed("_parent_task", none(), &opt(cls_base())),
                set_my_typed("_parent_cancel_requested", boolean(false), &IRType::Bool),
                set_my_typed("_tasks", IRExpr::ListLiteral(vec![]), &tasks_ty()),
                set_my_typed("_errors", IRExpr::ListLiteral(vec![]), &list(base_exc())),
                set_my_typed("_base_error", none(), &opt(base_exc())),
                set_my_typed("_on_completed_fut", none(), &opt(future_none())),
            ],
        ),
        method(
            "_repr",
            vec![me(c)],
            IRType::String,
            vec![
                assign_typed("info", text(""), &IRType::String),
                if_(
                    cmp(len(my("_tasks")), IRCompareOp::Gt, int(0)),
                    vec![add_to(
                        "info",
                        bin(
                            text(" tasks="),
                            IROp::Add,
                            call("str", vec![len(my("_tasks"))]),
                        ),
                    )],
                ),
                if_(
                    cmp(len(my("_errors")), IRCompareOp::Gt, int(0)),
                    vec![add_to(
                        "info",
                        bin(
                            text(" errors="),
                            IROp::Add,
                            call("str", vec![len(my("_errors"))]),
                        ),
                    )],
                ),
                if_else(
                    my("_aborting"),
                    vec![add_to("info", text(" cancelling"))],
                    vec![if_(my("_entered"), vec![add_to("info", text(" entered"))])],
                ),
                ret_val(bin(
                    bin(text("<TaskGroup"), IROp::Add, v("info")),
                    IROp::Add,
                    text(">"),
                )),
            ],
        ),
        // `create_task()`'s checks, made before the task is.
        method(
            "_check_open",
            vec![me(c)],
            IRType::None,
            vec![
                check(not(my("_entered")), " has not been entered"),
                check(
                    and(my("_exiting"), eq(len(my("_tasks")), int(0))),
                    " is finished",
                ),
                check(my("_aborting"), " is shutting down"),
            ],
        ),
        method(
            "_adopt",
            vec![me(c), param("task", &cls_base())],
            IRType::None,
            vec![
                expr_stmt(mcall(my("_tasks"), "append", vec![v("task")])),
                expr_stmt(mcall(
                    v("task"),
                    "_add_cb",
                    vec![call(GROUP_DONE, vec![v("self")])],
                )),
            ],
        ),
        method(
            "_is_base_error",
            vec![me(c), param("e", &base_exc())],
            IRType::Bool,
            vec![ret_val(or(
                isinstance(v("e"), "SystemExit"),
                isinstance(v("e"), "KeyboardInterrupt"),
            ))],
        ),
        method(
            "_abort",
            vec![me(c)],
            IRType::None,
            vec![
                set_attr(v("self"), "_aborting", boolean(true)),
                for_(
                    "t",
                    my("_tasks"),
                    vec![if_(
                        not(mcall(v("t"), "done", vec![])),
                        vec![expr_stmt(mcall(v("t"), "cancel", vec![]))],
                    )],
                ),
            ],
        ),
        method(
            "_on_task_done",
            vec![me(c), param("done", &opt(cls_base()))],
            IRType::None,
            vec![
                assign_typed("task", some(v("done"), "_on_task_done"), &cls_base()),
                assign_typed("kept", IRExpr::ListLiteral(vec![]), &tasks_ty()),
                for_(
                    "t",
                    my("_tasks"),
                    vec![if_(
                        cmp(v("t"), IRCompareOp::IsNot, v("task")),
                        vec![expr_stmt(mcall(v("kept"), "append", vec![v("t")]))],
                    )],
                ),
                set_attr(v("self"), "_tasks", v("kept")),
                if_(
                    and(
                        not_none(my("_on_completed_fut")),
                        eq(len(my("_tasks")), int(0)),
                    ),
                    vec![
                        assign_typed(
                            "fut",
                            some(my("_on_completed_fut"), "_on_task_done"),
                            &future_none(),
                        ),
                        if_(
                            not(mcall(v("fut"), "done", vec![])),
                            vec![expr_stmt(mcall(v("fut"), "_set_none", vec![]))],
                        ),
                    ],
                ),
                if_(mcall(v("task"), "cancelled", vec![]), vec![ret()]),
                assign_typed(
                    "exc",
                    mcall(v("task"), "exception", vec![]),
                    &opt(base_exc()),
                ),
                if_(is_none(v("exc")), vec![ret()]),
                assign_typed("e", some(v("exc"), "_on_task_done"), &base_exc()),
                expr_stmt(mcall(my("_errors"), "append", vec![v("e")])),
                if_(
                    and(
                        mcall(v("self"), "_is_base_error", vec![v("e")]),
                        is_none(my("_base_error")),
                    ),
                    vec![set_attr(v("self"), "_base_error", v("e"))],
                ),
                assign_typed(
                    "parent",
                    some(my("_parent_task"), "_on_task_done"),
                    &cls_base(),
                ),
                // Unreachable while `__aexit__` waits for every task.
                if_(mcall(v("parent"), "done", vec![]), vec![ret()]),
                if_(
                    and(not(my("_aborting")), not(my("_parent_cancel_requested"))),
                    vec![
                        expr_stmt(mcall(v("self"), "_abort", vec![])),
                        set_attr(v("self"), "_parent_cancel_requested", boolean(true)),
                        expr_stmt(mcall(v("parent"), "cancel", vec![])),
                    ],
                ),
            ],
        ),
    ];
    let opt_exc = || opt(base_exc());
    methods.push(coroutine_method(
        "__aenter__",
        c,
        vec![],
        "tg_aenter",
        vec![],
    ));
    methods.push(coroutine_method(
        "__aexit__",
        c,
        vec![
            param("t", &opt_exc()),
            param("e", &opt_exc()),
            param("tb", &opt_exc()),
        ],
        "tg_aexit",
        vec![v("e")],
    ));
    plain_class(c, None, methods)
}

fn cls_base() -> IRType {
    obj(&super::asyncio::name("_FutureBase"))
}

/// The callback a task group's task runs when it is done.
fn group_done() -> IRClass {
    plain_class(
        GROUP_DONE,
        Some(&super::asyncio::name("_Callback")),
        vec![
            method(
                "__init__",
                vec![me(GROUP_DONE), param("group", &obj(TASKGROUP))],
                IRType::None,
                vec![set_my_typed("group", v("group"), &obj(TASKGROUP))],
            ),
            method(
                "run",
                vec![me(GROUP_DONE), param("fut", &opt(cls_base()))],
                IRType::None,
                vec![expr_stmt(mcall(
                    my("group"),
                    "_on_task_done",
                    vec![v("fut")],
                ))],
            ),
        ],
    )
}

fn task_group_coroutines() -> Vec<IRFunction> {
    let tg = || v("tg");
    let field = |f: &str| attr(tg(), f);
    let set = |f: &str, value: IRExpr| set_attr(tg(), f, value);
    let raise_text = |what: &str| IRStatement::Raise {
        exception: Some(call(
            "RuntimeError",
            vec![bin(
                bin(text("TaskGroup "), IROp::Add, mcall(tg(), "_repr", vec![])),
                IROp::Add,
                text(what),
            )],
        )),
        cause: None,
    };
    let raise_some = |e: IRExpr| IRStatement::Raise {
        exception: Some(some(e, "raise")),
        cause: None,
    };
    let group = |class: &str| IRStatement::Raise {
        exception: Some(call(
            class,
            vec![text("unhandled errors in a TaskGroup"), field("_errors")],
        )),
        cause: None,
    };
    let aexit = vec![
        set("_exiting", boolean(true)),
        assign_typed("propagate", none(), &opt(base_exc())),
        assign_typed("cancelled_exit", boolean(false), &IRType::Bool),
        if_(
            not_none(v("e")),
            vec![
                assign_typed("err", some(v("e"), "__aexit__"), &base_exc()),
                if_(
                    and(
                        mcall(tg(), "_is_base_error", vec![v("err")]),
                        is_none(field("_base_error")),
                    ),
                    vec![set("_base_error", v("err"))],
                ),
                if_(
                    isinstance(v("err"), "CancelledError"),
                    vec![
                        assign("propagate", v("err")),
                        assign("cancelled_exit", boolean(true)),
                    ],
                ),
                if_(
                    not(field("_aborting")),
                    vec![expr_stmt(mcall(tg(), "_abort", vec![]))],
                ),
            ],
        ),
        while_(
            cmp(len(field("_tasks")), IRCompareOp::Gt, int(0)),
            vec![
                if_(
                    is_none(field("_on_completed_fut")),
                    vec![set(
                        "_on_completed_fut",
                        call(&future_class(&IRType::None), vec![]),
                    )],
                ),
                assign_typed(
                    "waiter",
                    some(field("_on_completed_fut"), "__aexit__"),
                    &future_none(),
                ),
                IRStatement::TryExcept {
                    try_body: body(vec![awaited(v("waiter"))]),
                    except_handlers: vec![IRExceptHandler {
                        exception_types: vec!["CancelledError".to_string()],
                        name: Some("ex".to_string()),
                        body: crate::ir::IRBody {
                            statements: vec![if_(
                                not(field("_aborting")),
                                vec![
                                    assign("propagate", v("ex")),
                                    expr_stmt(mcall(tg(), "_abort", vec![])),
                                ],
                            )],
                        },
                    }],
                    finally_body: None,
                },
                set("_on_completed_fut", none()),
            ],
        ),
        if_(
            not_none(field("_base_error")),
            vec![raise_some(field("_base_error"))],
        ),
        if_(
            field("_parent_cancel_requested"),
            vec![if_(
                eq(
                    mcall(some(field("_parent_task"), "__aexit__"), "uncancel", vec![]),
                    int(0),
                ),
                vec![assign("propagate", none())],
            )],
        ),
        if_(
            and(not_none(v("propagate")), eq(len(field("_errors")), int(0))),
            vec![raise_some(v("propagate"))],
        ),
        if_(
            and(not_none(v("e")), not(v("cancelled_exit"))),
            vec![expr_stmt(mcall(
                field("_errors"),
                "append",
                vec![some(v("e"), "__aexit__")],
            ))],
        ),
        if_(
            cmp(len(field("_errors")), IRCompareOp::Gt, int(0)),
            vec![
                assign_typed(
                    "parent",
                    some(field("_parent_task"), "__aexit__"),
                    &cls_base(),
                ),
                if_(
                    cmp(
                        mcall(v("parent"), "cancelling", vec![]),
                        IRCompareOp::Gt,
                        int(0),
                    ),
                    vec![
                        expr_stmt(mcall(v("parent"), "uncancel", vec![])),
                        expr_stmt(mcall(v("parent"), "cancel", vec![])),
                    ],
                ),
                // `BaseExceptionGroup(...)` makes an `ExceptionGroup` when
                // every exception is an `Exception`.
                assign_typed("all_exceptions", boolean(true), &IRType::Bool),
                for_(
                    "x",
                    field("_errors"),
                    vec![if_(
                        not(isinstance(v("x"), "Exception")),
                        vec![assign("all_exceptions", boolean(false))],
                    )],
                ),
                if_else(
                    v("all_exceptions"),
                    vec![group("ExceptionGroup")],
                    vec![group("BaseExceptionGroup")],
                ),
            ],
        ),
        ret_val(boolean(false)),
    ];
    vec![
        coroutine(
            "tg_aenter",
            vec![param("tg", &obj(TASKGROUP))],
            obj(TASKGROUP),
            vec![
                if_(
                    field("_entered"),
                    vec![raise_text(" has already been entered")],
                ),
                if_(
                    not(super::asyncio::loop_var("has_current")),
                    vec![raise_text(" cannot determine the parent task")],
                ),
                set("_parent_task", super::asyncio::loop_var("current")),
                set("_entered", boolean(true)),
                ret_val(tg()),
            ],
        ),
        coroutine(
            "tg_aexit",
            vec![param("tg", &obj(TASKGROUP)), param("e", &opt(base_exc()))],
            IRType::Bool,
            vec![try_finally(
                aexit,
                vec![
                    set("_parent_task", none()),
                    set("_errors", IRExpr::ListLiteral(vec![])),
                    set("_base_error", none()),
                ],
            )],
        ),
    ]
}

/// `_Coro[T]._in_group(tg)`: `tg.create_task(coro)`, its checks made before
/// the task is.
pub(crate) fn in_group_method(t: &IRType) -> IRFunction {
    let c = super::asyncio::coro_class(t);
    let task_ty = IRType::Class(super::asyncio::task_class(t));
    method(
        "_in_group",
        vec![me(&c), param("tg", &obj(TASKGROUP))],
        task_ty.clone(),
        vec![
            expr_stmt(mcall(v("tg"), "_check_open", vec![])),
            assign_typed(
                "task",
                call(&super::asyncio::task_class(t), vec![v("self")]),
                &task_ty,
            ),
            expr_stmt(mcall(v("tg"), "_adopt", vec![v("task")])),
            ret_val(v("task")),
        ],
    )
}

// ---- what the program uses ------------------------------------------------------------

/// What of these modules the program names, so that only that is written.
#[derive(Default)]
pub(crate) struct Wanted {
    lock: bool,
    event: bool,
    semaphore: bool,
    bounded: bool,
    pub(crate) timeout: bool,
    pub(crate) task_group: bool,
    /// `ExceptionGroup`, which a task group raises.
    groups: bool,
    queue_errors: bool,
    /// The queues, each its kind and element type.
    queues: Vec<(String, IRType)>,
}

impl Wanted {
    pub(crate) fn of(module: &mut IRModule, api: &Api) -> Wanted {
        let mut w = Wanted::default();
        if !api.used() {
            return w;
        }
        let mark = |name: &str, w: &mut Wanted| match name {
            "Lock" => w.lock = true,
            "Event" => w.event = true,
            "Semaphore" => w.semaphore = true,
            "BoundedSemaphore" => {
                w.semaphore = true;
                w.bounded = true;
            }
            "timeout" | "Timeout" => w.timeout = true,
            "TaskGroup" => w.task_group = true,
            "QueueEmpty" | "QueueFull" | "QueueShutDown" => w.queue_errors = true,
            _ => {}
        };
        let mut seen: Vec<String> = Vec::new();
        let mut visit = |e: &mut IRExpr| {
            if let Some((n, _)) = api.call_of(e) {
                seen.push(n);
            } else if let Some(n) = api.name_of(e) {
                seen.push(n);
            }
            Ok(())
        };
        for f in &mut module.functions {
            let _ = crate::ir::visit_body(&mut f.body, &mut visit);
        }
        for c in &mut module.classes {
            for m in &mut c.methods {
                let _ = crate::ir::visit_body(&mut m.body, &mut visit);
            }
        }
        for var in &mut module.variables {
            let mut b = crate::ir::IRBody {
                statements: vec![expr_stmt(var.value.clone())],
            };
            let _ = crate::ir::visit_body(&mut b, &mut visit);
        }
        for n in &seen {
            mark(n, &mut w);
        }
        // The classes annotations name, nested at any depth: their spelling.
        let spelled_types = format!(
            "{:?}{:?}{:?}",
            module.functions, module.classes, module.variables
        );
        for (class, name) in [
            (LOCK, "Lock"),
            (EVENT, "Event"),
            (SEMAPHORE, "Semaphore"),
            (BOUNDED, "BoundedSemaphore"),
            (QUEUE_EMPTY, "QueueEmpty"),
            (QUEUE_FULL, "QueueFull"),
            (QUEUE_SHUTDOWN, "QueueShutDown"),
            (TASKGROUP, "TaskGroup"),
        ] {
            if spelled_types.contains(&format!("\"{class}\"")) {
                mark(name, &mut w);
            }
        }
        let mut rest = spelled_types.as_str();
        while let Some(at) = rest.find("\"asyncio.queues.") {
            let tail = &rest[at + 1..];
            let end = tail.find('"').unwrap_or(tail.len());
            let class = &tail[..end];
            rest = &tail[end..];
            for kind in QUEUE_KINDS {
                let Some(inner) = class
                    .strip_prefix(&format!("asyncio.queues.{kind}["))
                    .and_then(|r| r.strip_suffix(']'))
                else {
                    continue;
                };
                if let Some(t) = crate::ir::collections::spec::decode(inner) {
                    if !w.queues.iter().any(|(k, x)| k == kind && *x == t) {
                        w.queues.push((kind.to_string(), t));
                    }
                }
            }
        }
        if !w.queues.is_empty() {
            w.event = true;
            w.queue_errors = true;
        }
        w.groups = w.task_group
            || spelled_types.contains("\"ExceptionGroup\"")
            || spelled_types.contains("\"BaseExceptionGroup\"");
        w
    }

    /// The element types of the queues, each with its base `Queue[T]`.
    fn element_types(&self) -> Vec<IRType> {
        let mut out: Vec<IRType> = Vec::new();
        for (_, t) in &self.queues {
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
        out
    }

    /// The runtime coroutine functions the classes' methods answer.
    pub(crate) fn coroutine_functions(&self) -> Vec<IRFunction> {
        let mut out = Vec::new();
        if self.lock {
            out.extend(lock_coroutines());
        }
        if self.event {
            out.extend(event_coroutines());
        }
        if self.semaphore {
            out.extend(semaphore_coroutines());
        }
        if self.timeout {
            out.extend(timeout_coroutines());
        }
        if self.task_group {
            out.extend(task_group_coroutines());
        }
        for t in self.element_types() {
            out.extend(queue_coroutines(&t));
        }
        out
    }

    /// The classes, after the runtime's.
    pub(crate) fn classes(&self) -> Vec<IRClass> {
        let mut out = Vec::new();
        if self.lock {
            out.push(lock());
        }
        if self.event {
            out.push(event());
        }
        if self.semaphore {
            out.push(semaphore());
        }
        if self.bounded {
            out.push(bounded_semaphore());
        }
        if self.queue_errors {
            for name in [QUEUE_EMPTY, QUEUE_FULL, QUEUE_SHUTDOWN] {
                out.push(error_class(name));
            }
        }
        if self.groups {
            out.extend(exception_groups());
        }
        if self.task_group {
            out.push(task_group());
            out.push(group_done());
        }
        for t in self.element_types() {
            out.push(queue(&t));
        }
        for (kind, t) in &self.queues {
            match kind.as_str() {
                "LifoQueue" => out.push(lifo_queue(t)),
                "PriorityQueue" => out.push(priority_queue(t)),
                _ => {}
            }
        }
        out
    }

    /// The methods the runtime's `_Timeout` gains.
    pub(crate) fn timeout_methods(&self) -> Vec<IRFunction> {
        if self.timeout {
            timeout_methods()
        } else {
            Vec::new()
        }
    }

    /// The messages the code raises.
    pub(crate) fn strings() -> [&'static str; 24] {
        [
            " (",
            ")",
            ">",
            "",
            "TaskGroup ",
            " has not been entered",
            " is finished",
            " is shutting down",
            " has already been entered",
            " cannot determine the parent task",
            "<TaskGroup",
            " tasks=",
            " errors=",
            " cancelling",
            " entered",
            "unhandled errors in a TaskGroup",
            " sub-exception",
            " sub-exceptions",
            "Lock is not acquired.",
            "Semaphore initial value must be >= 0",
            "BoundedSemaphore released too many times",
            "task_done() called too many times",
            "Timeout has already been entered",
            "_exit_failed",
        ]
    }
}

/// The class of the queue an annotated assignment makes
/// (`q: asyncio.Queue[int] = asyncio.Queue()`): the kind the call names,
/// holding the element type the annotation names.
pub(crate) fn typed_queue(kind: &str, declared: &IRType) -> Option<String> {
    let IRType::Class(c) = declared else {
        return None;
    };
    QUEUE_KINDS.iter().find_map(|k| {
        let inner = c
            .strip_prefix(&format!("asyncio.queues.{k}["))?
            .strip_suffix(']')?;
        let t = crate::ir::collections::spec::decode(inner)?;
        Some(queue_class(kind, &t))
    })
}

/// A queue's constructor call, given the class and the call's arguments
/// (`maxsize`, positional or by keyword, 0 when absent).
pub(crate) fn queue_call(class: &str, args: &[IRExpr], kind: &str) -> anyhow::Result<IRExpr> {
    let maxsize = match args {
        [] => int(0),
        [IRExpr::Keyword { name, value }] if name == "maxsize" => (**value).clone(),
        [IRExpr::Keyword { name, .. }] => {
            return Err(refused(format!(
                "asyncio.{kind}() got an unexpected keyword argument '{name}'"
            )))
        }
        [x] => x.clone(),
        _ => {
            return Err(refused(format!(
                "asyncio.{kind}() takes at most 1 argument"
            )))
        }
    };
    Ok(call(class, vec![maxsize]))
}
