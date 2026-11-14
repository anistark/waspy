//! `pyodide.http`: `pyfetch()` and its `FetchResponse`, Pyodide 0.29's
//! `pyodide/http/_pyfetch.py` written out in IR over the browser's `fetch`
//! (through the JavaScript bridge), with its exceptions (`HttpStatusError`,
//! `BodyUsedError`, `AbortError`, from `_exceptions.py`).
//!
//! `await pyfetch(url, method="POST", headers={...}, body=...)` becomes
//! `await __pyodide_pyfetch(url, <init>)`, where the keywords are the
//! `RequestInit` object JavaScript's `fetch` takes (a dict or a list among
//! them converted with `to_js`, as Pyodide's `to_js(kwargs)` converts it).
//! A response's `text()` awaits the body as a str, and `json()` parses it
//! into the type the place it is stored in declares
//! (`data: dict[str, int] = await resp.json()`), through `.to_py()`, since a
//! value of no declared type has no counterpart here. A failure of the
//! request itself is `AbortError`, as Pyodide's, and `raise_for_status()`
//! raises `HttpStatusError` for a 4xx or 5xx status.
//!
//! Runs before the coroutine pass, which lowers these coroutines with the
//! program's own.

use super::build::*;
use crate::ir::{IRClass, IRCompareOp, IRExpr, IRFunction, IRModule, IROp, IRStatement, IRType};
use anyhow::Result;
use std::collections::HashMap;

pub const FETCH_RESPONSE: &str = "pyodide.http.FetchResponse";
pub const HTTP_STATUS_ERROR: &str = "pyodide.http._exceptions.HttpStatusError";
pub const BODY_USED_ERROR: &str = "pyodide.http._exceptions.BodyUsedError";
pub const ABORT_ERROR: &str = "pyodide.http._exceptions.AbortError";
/// The coroutine `pyfetch()` is.
pub const PYFETCH_FN: &str = "__pyodide_pyfetch";

/// The class a name `pyodide.http` gives is.
fn class_of(attr: &str) -> Option<&'static str> {
    Some(match attr {
        "FetchResponse" => FETCH_RESPONSE,
        "HttpStatusError" => HTTP_STATUS_ERROR,
        "BodyUsedError" => BODY_USED_ERROR,
        "AbortError" => ABORT_ERROR,
        _ => return None,
    })
}

fn response() -> IRType {
    IRType::Class(FETCH_RESPONSE.to_string())
}
fn js(op: &str, args: Vec<IRExpr>) -> IRExpr {
    call(&format!("__js_{op}"), args)
}
fn resp_attr(name: &str) -> IRExpr {
    attr(my("js_response"), name)
}
fn async_method(
    name: &str,
    class: &str,
    result: IRType,
    statements: Vec<IRStatement>,
) -> IRFunction {
    method(
        name,
        vec![param("self", &IRType::Class(class.to_string()))],
        IRType::Coroutine(Box::new(result)),
        statements,
    )
}
fn awaited(e: IRExpr) -> IRExpr {
    call(crate::ir::AWAIT_FN, vec![e])
}
fn property(name: &str, ty: IRType, read: &str) -> IRFunction {
    let mut m = method(
        name,
        vec![param("self", &response())],
        ty.clone(),
        vec![assign_typed("v", resp_attr(read), &ty), ret_val(v("v"))],
    );
    m.decorators = vec!["property".to_string()];
    m
}

/// An `OSError` subclass of `pyodide.http`'s, made with a message.
fn os_error(name: &str) -> IRClass {
    IRClass {
        name: name.to_string(),
        bases: vec!["OSError".to_string()],
        methods: Vec::new(),
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// `HttpStatusError(status, status_text, url)`, its message as Pyodide words
/// it.
fn http_status_error() -> IRClass {
    let c = HTTP_STATUS_ERROR;
    let message = |kind: &str| {
        bin(
            bin(
                bin(call("str", vec![v("status")]), IROp::Add, text(kind)),
                IROp::Add,
                v("status_text"),
            ),
            IROp::Add,
            bin(text(" for url: "), IROp::Add, v("url")),
        )
    };
    let init = |m: IRExpr| expr_stmt(mcall(call("super", vec![]), "__init__", vec![m]));
    IRClass {
        methods: vec![method(
            "__init__",
            vec![
                param("self", &IRType::Class(c.to_string())),
                param("status", &IRType::Int),
                param("status_text", &IRType::String),
                param("url", &IRType::String),
            ],
            IRType::None,
            vec![
                set_my_typed("status", v("status"), &IRType::Int),
                set_my_typed("status_text", v("status_text"), &IRType::String),
                set_my_typed("url", v("url"), &IRType::String),
                if_else(
                    and(
                        cmp(int(400), IRCompareOp::LtE, v("status")),
                        cmp(v("status"), IRCompareOp::Lt, int(500)),
                    ),
                    vec![init(message(" Client Error: "))],
                    vec![if_else(
                        and(
                            cmp(int(500), IRCompareOp::LtE, v("status")),
                            cmp(v("status"), IRCompareOp::Lt, int(600)),
                        ),
                        vec![init(message(" Server Error: "))],
                        vec![init(message(
                            " Invalid error code not between 400 and 599: ",
                        ))],
                    )],
                ),
            ],
        )],
        ..os_error(c)
    }
}

/// `FetchResponse`, over the JavaScript `Response`.
fn fetch_response() -> IRClass {
    let c = FETCH_RESPONSE;
    let me = || IRType::Class(c.to_string());
    let check = expr_stmt(mcall(v("self"), "_raise_if_failed", vec![]));
    let mut headers = method(
        "headers",
        vec![param("self", &me())],
        IRType::Dict(Box::new(IRType::String), Box::new(IRType::String)),
        vec![
            assign_typed(
                "entries",
                mcall(
                    js("global", vec![text("Object")]),
                    "fromEntries",
                    vec![mcall(resp_attr("headers"), "entries", vec![])],
                ),
                &IRType::JsProxy,
            ),
            assign_typed(
                "d",
                mcall(v("entries"), "to_py", vec![]),
                &IRType::Dict(Box::new(IRType::String), Box::new(IRType::String)),
            ),
            ret_val(v("d")),
        ],
    );
    headers.decorators = vec!["property".to_string()];
    IRClass {
        name: c.to_string(),
        bases: Vec::new(),
        methods: vec![
            method(
                "__init__",
                vec![
                    param("self", &me()),
                    param("url", &IRType::String),
                    param("js_response", &IRType::JsProxy),
                ],
                IRType::None,
                vec![
                    set_my_typed("_url", v("url"), &IRType::String),
                    set_my_typed("js_response", v("js_response"), &IRType::JsProxy),
                ],
            ),
            property("status", IRType::Int, "status"),
            property("ok", IRType::Bool, "ok"),
            property("status_text", IRType::String, "statusText"),
            property("url", IRType::String, "url"),
            property("redirected", IRType::Bool, "redirected"),
            property("type", IRType::String, "type"),
            property("body_used", IRType::Bool, "bodyUsed"),
            headers,
            method(
                "_raise_if_failed",
                vec![param("self", &me())],
                IRType::None,
                vec![if_(
                    js("bool", vec![resp_attr("bodyUsed")]),
                    vec![IRStatement::Raise {
                        exception: Some(call(
                            BODY_USED_ERROR,
                            vec![text("Response body is already used")],
                        )),
                        cause: None,
                    }],
                )],
            ),
            method(
                "raise_for_status",
                vec![param("self", &me())],
                IRType::None,
                vec![
                    assign_typed("status", attr(v("self"), "status"), &IRType::Int),
                    if_(
                        and(
                            cmp(int(400), IRCompareOp::LtE, v("status")),
                            cmp(v("status"), IRCompareOp::Lt, int(600)),
                        ),
                        vec![IRStatement::Raise {
                            exception: Some(call(
                                HTTP_STATUS_ERROR,
                                vec![
                                    v("status"),
                                    attr(v("self"), "status_text"),
                                    attr(v("self"), "url"),
                                ],
                            )),
                            cause: None,
                        }],
                    ),
                ],
            ),
            method(
                "clone",
                vec![param("self", &me())],
                me(),
                vec![
                    check.clone(),
                    ret_val(call(
                        c,
                        vec![my("_url"), mcall(my("js_response"), "clone", vec![])],
                    )),
                ],
            ),
            async_method(
                "text",
                c,
                IRType::String,
                vec![
                    check.clone(),
                    assign_typed(
                        "t",
                        awaited(mcall(my("js_response"), "text", vec![])),
                        &IRType::String,
                    ),
                    ret_val(v("t")),
                ],
            ),
            async_method(
                "string",
                c,
                IRType::String,
                vec![
                    assign_typed(
                        "t",
                        awaited(mcall(v("self"), "text", vec![])),
                        &IRType::String,
                    ),
                    ret_val(v("t")),
                ],
            ),
            // What `json()` parses: a JavaScript value its store converts
            // with `.to_py()` (`json_into`).
            async_method(
                "_json",
                c,
                IRType::JsProxy,
                vec![
                    check,
                    assign_typed(
                        "t",
                        awaited(mcall(my("js_response"), "text", vec![])),
                        &IRType::String,
                    ),
                    ret_val(mcall(
                        js("global", vec![text("JSON")]),
                        "parse",
                        vec![v("t")],
                    )),
                ],
            ),
        ],
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// `pyfetch(url, init)`: the request, and its response once it arrives; a
/// failure of the request is `AbortError` with JavaScript's message.
fn pyfetch() -> IRFunction {
    let message = vec![
        assign_typed("s", call("str", vec![v("e")]), &IRType::String),
        assign_typed("at", mcall(v("s"), "find", vec![text(": ")]), &IRType::Int),
        if_(
            cmp(v("at"), IRCompareOp::GtE, int(0)),
            vec![assign(
                "s",
                slice(v("s"), Some(bin(v("at"), IROp::Add, int(2))), None),
            )],
        ),
        IRStatement::Raise {
            exception: Some(call(ABORT_ERROR, vec![v("s")])),
            cause: None,
        },
    ];
    method(
        PYFETCH_FN,
        vec![
            param("url", &IRType::String),
            param("init", &IRType::JsProxy),
        ],
        IRType::Coroutine(Box::new(response())),
        vec![
            IRStatement::TryExcept {
                try_body: body(vec![assign_typed(
                    "r",
                    awaited(js(
                        "call",
                        vec![js("global", vec![text("fetch")]), v("url"), v("init")],
                    )),
                    &IRType::JsProxy,
                )]),
                except_handlers: vec![crate::ir::IRExceptHandler {
                    exception_types: vec!["JsException".to_string()],
                    name: Some("e".to_string()),
                    body: crate::ir::IRBody {
                        statements: message,
                    },
                }],
                finally_body: None,
            },
            ret_val(call(FETCH_RESPONSE, vec![v("url"), v("r")])),
        ],
    )
}

/// The names `pyodide.http`'s are imported as (`from pyodide.http import
/// pyfetch as f`), and whether the module is imported whole.
struct Names {
    from: HashMap<String, String>,
    module: bool,
}

impl Names {
    fn of(module: &IRModule) -> Names {
        let mut from = HashMap::new();
        let mut whole = false;
        for imp in &module.imports {
            if imp.module == "pyodide.http" && imp.is_from_import {
                if let Some(n) = &imp.name {
                    from.insert(imp.alias.clone().unwrap_or_else(|| n.clone()), n.clone());
                }
            } else if !imp.is_from_import && imp.module.starts_with("pyodide") {
                whole = true;
            }
        }
        Names {
            from,
            module: whole,
        }
    }

    /// `pyfetch(...)` or `pyodide.http.pyfetch(...)`: its arguments.
    fn pyfetch_call<'e>(&self, e: &'e IRExpr) -> Option<&'e [IRExpr]> {
        match e {
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if self.from.get(function_name).map(String::as_str) == Some("pyfetch") => {
                Some(arguments)
            }
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } if self.module && method_name == "pyfetch" => match object.as_ref() {
                IRExpr::Attribute {
                    object: m,
                    attribute,
                } if attribute == "http"
                    && matches!(m.as_ref(), IRExpr::Variable(p) if p == "pyodide") =>
                {
                    Some(arguments)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// A class of the module read by name.
    fn class_named(&self, e: &IRExpr) -> Option<&'static str> {
        match e {
            IRExpr::Variable(n) => self.from.get(n).and_then(|a| class_of(a)),
            IRExpr::Attribute { object, attribute } if self.module => match object.as_ref() {
                IRExpr::Attribute {
                    object: m,
                    attribute: sub,
                } if sub == "http"
                    && matches!(m.as_ref(), IRExpr::Variable(p) if p == "pyodide") =>
                {
                    class_of(attribute)
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// The `RequestInit` a `pyfetch()` call's keywords make, as a JavaScript
/// object: `Object.assign({}, {method: ..., headers: to_js(...)})`, through
/// the bridge's keyword arguments.
fn init_object(keywords: Vec<(String, IRExpr)>) -> IRExpr {
    let object = js("global", vec![text("Object")]);
    let mut args = vec![mcall(object.clone(), "new", vec![])];
    for (name, value) in keywords {
        // A dict or a list written out crosses through `to_js`; a scalar is
        // itself, and a dict held in a variable is refused by the bridge
        // with its hint (`to_js(headers)`).
        let value = match value {
            e @ (IRExpr::DictLiteral(_) | IRExpr::ListLiteral(_) | IRExpr::TupleLiteral(_)) => {
                js("to_js", vec![e])
            }
            e => e,
        };
        args.push(IRExpr::Keyword {
            name,
            value: Box::new(value),
        });
    }
    mcall(object, "assign", args)
}

pub fn lower_pyodide_http(module: &mut IRModule) -> Result<()> {
    let names = Names::of(module);
    if names.from.is_empty() && !names.module {
        return Ok(());
    }
    let mut used = false;
    let mut err = None;
    let mut rewrite = |e: &mut IRExpr| -> Result<()> {
        if let Some(class) = names.class_named(e) {
            *e = v(class);
            used = true;
            return Ok(());
        }
        let Some(args) = names.pyfetch_call(e) else {
            return Ok(());
        };
        let mut url = None;
        let mut keywords = Vec::new();
        for a in args {
            match a {
                IRExpr::Keyword { name, .. } if name == crate::ir::STARRED_ARG => {
                    err.get_or_insert(refused(
                        "pyfetch() with '*args' is not supported".to_string(),
                    ));
                }
                IRExpr::Keyword { name, .. } if name == "signal" || name == "fetcher" => {
                    err.get_or_insert(refused(format!("pyfetch()'s '{name}' is not supported")));
                }
                IRExpr::Keyword { name, value } => keywords.push((name.clone(), (**value).clone())),
                other if url.is_none() => url = Some(other.clone()),
                _ => {
                    err.get_or_insert(refused(
                        "pyfetch() takes the URL and keyword arguments".to_string(),
                    ));
                }
            }
        }
        let Some(url) = url else {
            err.get_or_insert(refused("pyfetch() needs a URL".to_string()));
            return Ok(());
        };
        *e = call(PYFETCH_FN, vec![url, init_object(keywords)]);
        used = true;
        Ok(())
    };
    for f in &mut module.functions {
        crate::ir::visit_body(&mut f.body, &mut rewrite)?;
        rename_handlers(&mut f.body, &names);
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            crate::ir::visit_body(&mut m.body, &mut rewrite)?;
            rename_handlers(&mut m.body, &names);
        }
    }
    if let Some(e) = err {
        return Err(e);
    }
    if !used && names.from.is_empty() {
        return Ok(());
    }
    module.classes.extend([
        fetch_response(),
        http_status_error(),
        os_error(BODY_USED_ERROR),
        os_error(ABORT_ERROR),
    ]);
    module.functions.push(pyfetch());
    for s in [
        "Object",
        "fetch",
        "JSON",
        "Response body is already used",
        ": ",
        " for url: ",
        " Client Error: ",
        " Server Error: ",
        " Invalid error code not between 400 and 599: ",
    ] {
        module.memory_layout.add_string(s);
    }
    Ok(())
}

/// `except HttpStatusError:` names the class by its full name.
fn rename_handlers(body: &mut crate::ir::IRBody, names: &Names) {
    for stmt in &mut body.statements {
        if let IRStatement::TryExcept {
            except_handlers, ..
        } = stmt
        {
            for h in except_handlers {
                for t in &mut h.exception_types {
                    if let Some(c) = names.from.get(t.as_str()).and_then(|a| class_of(a)) {
                        *t = c.to_string();
                    }
                }
            }
        }
        for nested in nested_bodies_mut(stmt) {
            rename_handlers(nested, names);
        }
    }
}

fn nested_bodies_mut(stmt: &mut IRStatement) -> Vec<&mut crate::ir::IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => std::iter::once(then_body.as_mut())
            .chain(else_body.as_deref_mut())
            .collect(),
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body],
        IRStatement::For {
            body, else_body, ..
        } => std::iter::once(body.as_mut())
            .chain(else_body.as_deref_mut())
            .collect(),
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => std::iter::once(try_body.as_mut())
            .chain(except_handlers.iter_mut().map(|h| &mut h.body))
            .chain(finally_body.as_deref_mut())
            .collect(),
        _ => Vec::new(),
    }
}
