//! `pyodide.ffi.create_proxy(f)` and `create_once_callable(f)`: a Python
//! function JavaScript can call, as an event listener, a timer's callback,
//! or anything else that takes a function.
//!
//! JavaScript calls a proxy with JavaScript values, so the function's
//! parameters are JavaScript values (`JsProxy`): an unannotated one is typed
//! so, as Pyodide hands it a proxy. What it answers goes back as Pyodide
//! converts it. To give every proxied function one shape per number of
//! parameters, the function is wrapped in a lambda that takes the values and
//! answers the handle of its result (`__js_export`), and `create_proxy(f)`
//! becomes `__jsbridge_proxy_<n>(wrapper, once)`, which keeps the wrapper
//! alive in a registry (the collector would otherwise free what only
//! JavaScript holds) and asks the loader for the JavaScript function (the
//! `function` import). The loader calls it back through the export
//! `__waspy_call_<n>(wrapper, handles...)`, which turns a Python exception
//! into the `PythonError` JavaScript sees, and the proxy's `destroy()` lets
//! go of it through `__waspy_forget(id)`, as Pyodide's does.
//!
//! Runs before nested functions and lambdas are lifted, so the wrapper is
//! lifted with the program's own.

use super::build::*;
use crate::ir::{IRExpr, IRFunction, IRModule, IRParam, IRStatement, IRType, IRVariable};
use anyhow::Result;
use std::collections::{BTreeSet, HashMap};

/// The registry of the functions of `n` parameters JavaScript holds, by
/// id, and the last id given.
fn kept(n: usize) -> String {
    format!("__jsbridge_kept_{n}")
}
const COUNT: &str = "__jsbridge_count";
/// The function that makes a proxy of `n` parameters.
fn proxy_fn(n: usize) -> String {
    format!("__jsbridge_proxy_{n}")
}
/// The export JavaScript calls a proxy of `n` parameters through.
pub const CALL_PREFIX: &str = "__waspy_call_";
/// The export a proxy's `destroy()` lets go of it through.
pub const FORGET_FN: &str = "__waspy_forget";
const LINE_FN: &str = "__jsbridge_proxy_line";

fn refuse(message: impl Into<String>) -> anyhow::Error {
    refused(message.into())
}

fn wrapper_type(n: usize) -> IRType {
    IRType::Callable {
        params: vec![IRType::JsProxy; n],
        return_type: Box::new(IRType::Int),
    }
}

/// Whether a call is `create_proxy(f)` (false) or `create_once_callable(f)`
/// (true), by the names the program imports them as or through
/// `pyodide.ffi`.
fn proxy_call<'e>(e: &'e IRExpr, names: &HashMap<String, bool>) -> Option<(bool, &'e [IRExpr])> {
    match e {
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => names
            .get(function_name)
            .map(|once| (*once, arguments.as_slice())),
        IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } => {
            let IRExpr::Attribute {
                object: m,
                attribute,
            } = object.as_ref()
            else {
                return None;
            };
            if attribute != "ffi" || !matches!(m.as_ref(), IRExpr::Variable(p) if p == "pyodide") {
                return None;
            }
            match method_name.as_str() {
                "create_proxy" => Some((false, arguments.as_slice())),
                "create_once_callable" => Some((true, arguments.as_slice())),
                _ => None,
            }
        }
        _ => None,
    }
}

pub fn lower_js_proxies(module: &mut IRModule) -> Result<()> {
    let mut names = HashMap::new();
    for imp in &module.imports {
        if imp.module != "pyodide.ffi" || !imp.is_from_import {
            continue;
        }
        let once = match imp.name.as_deref() {
            Some("create_proxy") => false,
            Some("create_once_callable") => true,
            _ => continue,
        };
        let bound = imp
            .alias
            .clone()
            .or_else(|| imp.name.clone())
            .expect("named");
        names.insert(bound, once);
    }
    let through_module = module
        .imports
        .iter()
        .any(|i| !i.is_from_import && i.module.starts_with("pyodide"));
    if names.is_empty() && !through_module {
        return Ok(());
    }
    // Whether each module function answers a value: one declaring no
    // result that returns none is called for its effect.
    let answers: HashMap<String, Option<bool>> = module
        .functions
        .iter()
        .map(|f| {
            let answers = match &f.return_type {
                IRType::None => Some(false),
                IRType::Unknown | IRType::Any if returns_value(&f.body) => None,
                IRType::Unknown | IRType::Any => Some(false),
                _ => Some(true),
            };
            (f.name.clone(), answers)
        })
        .collect();
    // Each module function's positional parameters without a default: the
    // ones JavaScript passes.
    let functions: HashMap<String, Vec<IRParam>> = module
        .functions
        .iter()
        .filter(|f| {
            f.params
                .iter()
                .all(|p| p.star == crate::ir::ParamStar::None)
        })
        .map(|f| {
            let passed = f
                .params
                .iter()
                .filter(|p| p.default_value.is_none() && !p.keyword_only)
                .cloned()
                .collect();
            (f.name.clone(), passed)
        })
        .collect();

    let mut arities = BTreeSet::new();
    let mut typed: BTreeSet<String> = BTreeSet::new();
    let mut err = None;
    let mut rewrite = |e: &mut IRExpr| -> Result<()> {
        let Some((once, args)) = proxy_call(e, &names) else {
            return Ok(());
        };
        let wrapper = match args {
            [IRExpr::Variable(f)] if functions.contains_key(f) => {
                let params = &functions[f];
                typed.insert(f.clone());
                let mine: Vec<IRParam> = (0..params.len())
                    .map(|k| param(&format!("__jsp{k}"), &IRType::JsProxy))
                    .collect();
                let passed = mine.iter().map(|p| v(&p.name)).collect();
                // What a function answering nothing returns is undefined.
                let Some(answers) = answers[f] else {
                    err.get_or_insert(refuse(format!(
                        "the function '{f}' JavaScript calls returns a value but declares no \
                         return type. Hint: annotate it, for example '-> int'"
                    )));
                    return Ok(());
                };
                let mut exported = vec![call(f, passed)];
                if !answers {
                    exported.push(boolean(true));
                }
                IRExpr::Lambda {
                    params: mine,
                    body: Box::new(call("__js_export", exported)),
                }
            }
            [IRExpr::Lambda { params, body }] => {
                let params = params
                    .iter()
                    .map(|p| IRParam {
                        param_type: match p.param_type {
                            IRType::Unknown | IRType::Any => IRType::JsProxy,
                            ref t => t.clone(),
                        },
                        ..p.clone()
                    })
                    .collect();
                IRExpr::Lambda {
                    params,
                    body: Box::new(call("__js_export", vec![(**body).clone()])),
                }
            }
            _ => {
                err.get_or_insert(refuse(
                    "create_proxy() and create_once_callable() take a function defined with \
                     'def' at module level, or a lambda, here; their other options are not \
                     supported",
                ));
                return Ok(());
            }
        };
        let n = match &wrapper {
            IRExpr::Lambda { params, .. } => params.len(),
            _ => unreachable!("a lambda"),
        };
        arities.insert(n);
        *e = call(&proxy_fn(n), vec![wrapper, boolean(once)]);
        Ok(())
    };
    for f in &mut module.functions {
        crate::ir::visit_body(&mut f.body, &mut rewrite)?;
    }
    for c in &mut module.classes {
        for m in &mut c.methods {
            crate::ir::visit_body(&mut m.body, &mut rewrite)?;
        }
    }
    if let Some(e) = err {
        return Err(e);
    }
    if arities.is_empty() {
        return Ok(());
    }
    // A function JavaScript calls takes JavaScript values.
    for f in &mut module.functions {
        if typed.contains(&f.name) {
            for p in &mut f.params {
                if matches!(p.param_type, IRType::Unknown | IRType::Any)
                    && p.default_value.is_none()
                {
                    p.param_type = IRType::JsProxy;
                }
            }
        }
    }

    for n in &arities {
        module.variables.push(IRVariable {
            name: kept(*n),
            value: IRExpr::DictLiteral(vec![]),
            var_type: Some(IRType::Dict(
                Box::new(IRType::Int),
                Box::new(wrapper_type(*n)),
            )),
        });
    }
    module.variables.push(IRVariable {
        name: COUNT.to_string(),
        value: int(0),
        var_type: Some(IRType::Int),
    });
    for n in &arities {
        module.functions.push(make_proxy(*n));
        module.functions.push(dispatcher(*n));
    }
    module.functions.push(forget(&arities));
    module
        .functions
        .push(crate::compiler::js::exception_line(LINE_FN));
    for s in [": ", "__main__."] {
        module.memory_layout.add_string(s);
    }
    Ok(())
}

/// Whether a body returns a value anywhere.
fn returns_value(body: &crate::ir::IRBody) -> bool {
    body.statements.iter().any(|s| match s {
        IRStatement::Return(Some(e)) => !matches!(e, IRExpr::Const(crate::ir::IRConstant::None)),
        other => crate::ir::infer::nested_bodies(other)
            .into_iter()
            .any(returns_value),
    })
}

/// `__jsbridge_proxy_<n>(fn, once)`: keep `fn` and make its JavaScript
/// function.
fn make_proxy(n: usize) -> IRFunction {
    method(
        &proxy_fn(n),
        vec![param("fn", &wrapper_type(n)), param("once", &IRType::Bool)],
        IRType::JsProxy,
        vec![
            IRStatement::Global(vec![COUNT.to_string()]),
            assign(COUNT, bin(v(COUNT), crate::ir::IROp::Add, int(1))),
            set_item(v(&kept(n)), v(COUNT), v("fn")),
            ret_val(call(
                "__js_function",
                vec![v("fn"), int(n as i32), v("once"), v(COUNT)],
            )),
        ],
    )
}

/// `__waspy_call_<n>(fn, h0, ...)`: JavaScript's call of a proxy, the
/// handle of what it answers, or -2 with the exception's last line handed
/// over for the loader to throw.
fn dispatcher(n: usize) -> IRFunction {
    let mut params = vec![param("fn", &wrapper_type(n))];
    let mut passed = Vec::new();
    for k in 0..n {
        let h = format!("h{k}");
        params.push(param(&h, &IRType::Int));
        passed.push(call("__js_adopt", vec![v(&h)]));
    }
    method(
        &format!("{CALL_PREFIX}{n}"),
        params,
        IRType::Int,
        vec![IRStatement::TryExcept {
            try_body: body(vec![ret_val(call("fn", passed))]),
            except_handlers: vec![crate::ir::IRExceptHandler {
                exception_types: vec!["BaseException".to_string()],
                name: Some("e".to_string()),
                body: crate::ir::IRBody {
                    statements: vec![ret_val(call(
                        "__js_fail",
                        vec![call(LINE_FN, vec![v("e")])],
                    ))],
                },
            }],
            finally_body: None,
        }],
    )
}

/// `__waspy_forget(id)`: a proxy's `destroy()`, which lets go of its function.
fn forget(arities: &BTreeSet<usize>) -> IRFunction {
    let mut statements: Vec<IRStatement> = arities
        .iter()
        .map(|n| {
            if_(
                cmp(v("id"), crate::ir::IRCompareOp::In, v(&kept(*n))),
                vec![del_item(v(&kept(*n)), v("id"))],
            )
        })
        .collect();
    statements.push(ret_val(int(0)));
    method(
        FORGET_FN,
        vec![param("id", &IRType::Int)],
        IRType::Int,
        statements,
    )
}
