//! Whole-module pass that lowers `with` statements onto the context-manager
//! protocol (#5).
//!
//! `with expr as name:` becomes a flat statement sequence that the rest of the
//! pipeline already compiles:
//!
//! ```text
//! __with_cm_0 = expr            # the context manager itself
//! name        = __with_cm_0.__enter__()
//! <body>
//! __with_cm_0.__exit__(None, None, None)
//! ```
//!
//! Doing it here rather than during statement lowering is what makes it
//! correct: the class table is complete by now, so the pass can type the
//! synthesized locals from the real `__enter__` return type (an f64-returning
//! `__enter__` needs an f64 local) and reject a class that does not implement
//! the protocol. The previous direct codegen for `IRStatement::With` never
//! called either method and allocated locals after the function's local vector
//! was fixed, which produced a module that failed WASM validation while the
//! compiler reported success.
//!
//! The body runs inside a `try`: an exception leaving it is handed to
//! `__exit__(exc, exc, None)`, which suppresses it by answering true, and
//! otherwise it goes on; a `return`, `break`, or `continue` leaving the body
//! runs `__exit__(None, None, None)` first (the returned expression is
//! evaluated into a temporary before it), and so does falling off the end.
//! Classes are not values here, so `__exit__`'s `exc_type` is given the
//! exception object too and `tb` None, and either may only be compared with
//! None. Nested `with` statements compose because the inner one is rewritten
//! first.
//!
//! Known limits, rejected rather than miscompiled: the context expression's
//! class must be resolvable at compile time (an instantiation, a call whose
//! return type is annotated, or a variable of known class type).

use crate::ir::{IRBody, IRExpr, IRFunction, IRModule, IRStatement, IRType};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

/// Process-wide counter so the synthesized locals stay unique even when several
/// modules are lowered and merged into one binary.
static WITH_COUNTER: AtomicU32 = AtomicU32::new(0);

/// What a class contributes to the context-manager protocol, resolved through
/// its inheritance chain.
struct ProtocolMethods {
    /// Return type of `__enter__`, used to type the local bound by `as`.
    enter_returns: Option<IRType>,
    has_exit: bool,
}

/// Rewrite every `with` statement in the module into explicit
/// `__enter__`/`__exit__` calls.
pub fn desugar_with_statements(module: &mut IRModule) -> Result<()> {
    let protocols = resolve_protocols(module);
    let class_returns: HashMap<String, String> = module
        .functions
        .iter()
        .filter_map(|f| match &f.return_type {
            IRType::Class(name) => Some((f.name.clone(), name.clone())),
            _ => None,
        })
        .collect();
    let classes: Vec<String> = module.classes.iter().map(|c| c.name.clone()).collect();

    let resolver = Resolver {
        protocols,
        class_returns,
        classes,
        in_generator: std::cell::Cell::new(false),
    };

    // `__exit__(self, exc_type, exc, tb)` is called with the exception
    // object (or None) as `exc`. Classes are not values here, so `exc_type`
    // is given the same object, and `tb` None: either may only be compared
    // with None (or tested for truth), which reads the same of both.
    for class in &mut module.classes {
        for method in &mut class.methods {
            if method.name != "__exit__" || method.params.len() != 4 {
                continue;
            }
            for (position, param) in method.params.iter_mut().enumerate().skip(1) {
                let exception = IRType::Class("BaseException".to_string());
                match &param.param_type {
                    IRType::Class(_) => {}
                    IRType::Optional(inner) if matches!(inner.as_ref(), IRType::Class(_)) => {}
                    _ => param.param_type = exception,
                }
                if position != 2 {
                    let name = param.name.clone();
                    if uses_beyond_none_checks(&mut method.body, &name) {
                        return Err(crate::core::errors::unsupported_feature(
                            format!(
                                "'{name}' in {}.__exit__ can only be compared with None: \
                                 exception classes and tracebacks are not values here. Hint: \
                                 use the exception itself, the second parameter",
                                class.name
                            ),
                            None,
                        )
                        .into());
                    }
                }
            }
        }
    }

    for func in &mut module.functions {
        rewrite_function(func, &resolver, None)?;
    }
    for class in &mut module.classes {
        let owner = class.name.clone();
        for method in &mut class.methods {
            rewrite_function(method, &resolver, Some(&owner))?;
        }
    }
    Ok(())
}

/// Static knowledge the rewrite needs about the module's classes and functions.
struct Resolver {
    protocols: HashMap<String, ProtocolMethods>,
    /// Module functions whose declared return type is a class instance.
    class_returns: HashMap<String, String>,
    classes: Vec<String>,
    /// Whether the function being rewritten is a generator, whose `finally`
    /// blocks the generator pass runs on every exit itself.
    in_generator: std::cell::Cell<bool>,
}

impl Resolver {
    /// Class of a context-manager expression, when it is knowable statically.
    /// `env` carries the classes of the enclosing function's locals.
    fn class_of(&self, expr: &IRExpr, env: &HashMap<String, String>) -> Option<String> {
        match expr {
            IRExpr::FunctionCall { function_name, .. } => {
                if self.classes.iter().any(|c| c == function_name) {
                    Some(function_name.clone())
                } else {
                    self.class_returns.get(function_name).cloned()
                }
            }
            IRExpr::Variable(name) => env.get(name).cloned(),
            _ => None,
        }
    }
}

/// Collect each class's context-manager methods, walking single-inheritance
/// bases so a subclass of a context manager is one too.
fn resolve_protocols(module: &IRModule) -> HashMap<String, ProtocolMethods> {
    let by_name: HashMap<&str, &crate::ir::IRClass> = module
        .classes
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();

    let mut out = HashMap::new();
    for class in &module.classes {
        let mut enter_returns = None;
        let mut has_exit = false;
        let mut current = Some(class.name.as_str());
        // Bounded by the class count: a cycle in the base links would otherwise
        // loop forever, and lowering does not guarantee an acyclic chain.
        for _ in 0..=module.classes.len() {
            let Some(node) = current.and_then(|name| by_name.get(name)) else {
                break;
            };
            for method in &node.methods {
                if method.name == "__enter__" && enter_returns.is_none() {
                    enter_returns = Some(method.return_type.clone());
                } else if method.name == "__exit__" {
                    has_exit = true;
                }
            }
            current = node.bases.first().map(|b| b.as_str());
        }
        out.insert(
            class.name.clone(),
            ProtocolMethods {
                enter_returns,
                has_exit,
            },
        );
    }
    out
}

/// Rewrite one function or method body. `owner` names the enclosing class when
/// rewriting a method, so `self` resolves to a class type.
fn rewrite_function(func: &mut IRFunction, resolver: &Resolver, owner: Option<&str>) -> Result<()> {
    let mut env: HashMap<String, String> = HashMap::new();
    for param in &func.params {
        if let IRType::Class(name) = &param.param_type {
            env.insert(param.name.clone(), name.clone());
        }
    }
    if let (Some(owner), Some(first)) = (owner, func.params.first()) {
        if first.name == "self" {
            env.insert("self".to_string(), owner.to_string());
        }
    }

    let return_type = func.return_type.clone();
    resolver
        .in_generator
        .set(crate::ir::generators::body_has_yield(&func.body));
    let body = std::mem::replace(
        &mut func.body,
        IRBody {
            statements: Vec::new(),
        },
    );
    func.body = rewrite_body(body, resolver, &mut env, &return_type)?;
    Ok(())
}

/// Rewrite a body, threading the local-class environment through the statements
/// in order so a `with` can name a manager bound earlier in the same block.
fn rewrite_body(
    body: IRBody,
    resolver: &Resolver,
    env: &mut HashMap<String, String>,
    return_type: &IRType,
) -> Result<IRBody> {
    let mut out = Vec::with_capacity(body.statements.len());

    for stmt in body.statements {
        match stmt {
            IRStatement::With {
                context_expr,
                optional_vars,
                body,
            } => {
                let expanded = expand_with(
                    context_expr,
                    optional_vars,
                    *body,
                    resolver,
                    env,
                    return_type,
                )?;
                out.extend(expanded);
            }
            other => out.push(rewrite_nested(other, resolver, env, return_type)?),
        }
    }

    Ok(IRBody { statements: out })
}

/// Recurse into a non-`with` statement's nested bodies, and record the class of
/// anything assigned to a local so a later `with` over that local resolves.
fn rewrite_nested(
    stmt: IRStatement,
    resolver: &Resolver,
    env: &mut HashMap<String, String>,
    return_type: &IRType,
) -> Result<IRStatement> {
    Ok(match stmt {
        IRStatement::Assign {
            target,
            value,
            var_type,
        } => {
            match (&var_type, resolver.class_of(&value, env)) {
                (Some(IRType::Class(name)), _) => {
                    env.insert(target.clone(), name.clone());
                }
                (_, Some(name)) => {
                    env.insert(target.clone(), name);
                }
                _ => {
                    env.remove(&target);
                }
            }
            IRStatement::Assign {
                target,
                value,
                var_type,
            }
        }
        IRStatement::If {
            condition,
            then_body,
            else_body,
        } => IRStatement::If {
            condition,
            then_body: Box::new(rewrite_body(
                *then_body,
                resolver,
                &mut env.clone(),
                return_type,
            )?),
            else_body: match else_body {
                Some(b) => Some(Box::new(rewrite_body(
                    *b,
                    resolver,
                    &mut env.clone(),
                    return_type,
                )?)),
                None => None,
            },
        },
        IRStatement::While { condition, body } => IRStatement::While {
            condition,
            body: Box::new(rewrite_body(
                *body,
                resolver,
                &mut env.clone(),
                return_type,
            )?),
        },
        IRStatement::For {
            target,
            iterable,
            body,
            else_body,
        } => IRStatement::For {
            target,
            iterable,
            body: Box::new(rewrite_body(
                *body,
                resolver,
                &mut env.clone(),
                return_type,
            )?),
            else_body: match else_body {
                Some(b) => Some(Box::new(rewrite_body(
                    *b,
                    resolver,
                    &mut env.clone(),
                    return_type,
                )?)),
                None => None,
            },
        },
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            let try_body = Box::new(rewrite_body(
                *try_body,
                resolver,
                &mut env.clone(),
                return_type,
            )?);
            let mut handlers = Vec::with_capacity(except_handlers.len());
            for handler in except_handlers {
                handlers.push(crate::ir::IRExceptHandler {
                    exception_types: handler.exception_types,
                    name: handler.name,
                    body: rewrite_body(handler.body, resolver, &mut env.clone(), return_type)?,
                });
            }
            let finally_body = match finally_body {
                Some(b) => Some(Box::new(rewrite_body(
                    *b,
                    resolver,
                    &mut env.clone(),
                    return_type,
                )?)),
                None => None,
            };

            // `finally` on the non-local exit paths. Code generation emits the
            // finally body after the handlers, which the fall-through path
            // reaches but a `return`, `break`, or `continue` inside the block
            // jumps straight past, so those used to skip it silently. Each such
            // exit gets its own copy of the body ahead of it (there are no
            // landing pads to jump to here), and the trailing copy still covers
            // the ordinary path, so the body runs exactly once either way.
            // A generator's are run on every exit by the generator pass.
            let (mut try_body, mut handlers) = (try_body, handlers);
            if let Some(finally) = finally_body
                .as_ref()
                .filter(|_| !resolver.in_generator.get())
            {
                run_cleanup_before_exits(&mut try_body, &finally.statements, return_type, false);
                for handler in &mut handlers {
                    run_cleanup_before_exits(
                        &mut handler.body,
                        &finally.statements,
                        return_type,
                        false,
                    );
                }
            }

            IRStatement::TryExcept {
                try_body,
                except_handlers: handlers,
                finally_body,
            }
        }
        other => other,
    })
}

/// Expand one `with` statement into its protocol call sequence.
fn expand_with(
    context_expr: IRExpr,
    optional_vars: Option<String>,
    body: IRBody,
    resolver: &Resolver,
    env: &mut HashMap<String, String>,
    return_type: &IRType,
) -> Result<Vec<IRStatement>> {
    let Some(class_name) = resolver.class_of(&context_expr, env) else {
        return Err(crate::core::errors::unsupported_feature(
            "a 'with' statement needs a context manager whose class is known at compile \
             time: use 'with ClassName(...)', a call annotated with a class return type, \
             or a variable holding one",
            None,
        )
        .into());
    };

    let protocol = resolver.protocols.get(&class_name).ok_or_else(|| {
        crate::core::errors::type_error(format!("Unknown class '{class_name}'"), None)
    })?;

    let Some(enter_returns) = protocol.enter_returns.clone() else {
        return Err(crate::core::errors::type_error(
            format!("Class '{class_name}' has no '__enter__' method, so it cannot be used in a 'with' statement"),
            None,
        )
        .into());
    };
    if !protocol.has_exit {
        return Err(crate::core::errors::type_error(
            format!("Class '{class_name}' has no '__exit__' method, so it cannot be used in a 'with' statement"),
            None,
        )
        .into());
    }

    let seq = WITH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let manager = format!("__with_cm_{seq}");
    let mut out = Vec::new();

    // Hold the manager in its own local: the `as` name is bound to whatever
    // `__enter__` returns, which in general is not the manager itself, and
    // `__exit__` must still reach the manager afterwards.
    out.push(IRStatement::Assign {
        target: manager.clone(),
        value: context_expr,
        var_type: Some(IRType::Class(class_name.clone())),
    });

    // `__enter__` runs even without an `as` clause; its result is bound to a
    // throwaway local so the value is consumed exactly once.
    let bound = optional_vars.unwrap_or_else(|| format!("__with_val_{seq}"));
    if let IRType::Class(name) = &enter_returns {
        env.insert(bound.clone(), name.clone());
    } else {
        env.remove(&bound);
    }
    out.push(IRStatement::Assign {
        target: bound,
        value: IRExpr::MethodCall {
            object: Box::new(IRExpr::Variable(manager.clone())),
            method_name: "__enter__".to_string(),
            arguments: Vec::new(),
        },
        var_type: Some(enter_returns),
    });

    // The body goes inside a `try`. An exception leaving it is handed to
    // `__exit__`, which suppresses it by answering true and otherwise lets
    // it go on; leaving it any other way calls `__exit__(None, None, None)`:
    // falling off the end (after the `try`, under a flag, so an exception
    // `__exit__` raises itself is not handed back to it), and a
    // `return`/`break`/`continue` (which `run_cleanup_before_exits` puts a
    // copy ahead of, since those jump past the end).
    let mut inner = rewrite_body(body, resolver, &mut env.clone(), return_type)?;
    let normal_exit = vec![exit_call(&manager)];
    run_cleanup_before_exits(&mut inner, &normal_exit, return_type, false);
    let ok = format!("__with_ok_{seq}");
    let caught = format!("__with_exc_{seq}");
    out.push(IRStatement::Assign {
        target: ok.clone(),
        value: IRExpr::Const(crate::ir::IRConstant::Bool(true)),
        var_type: Some(IRType::Bool),
    });
    let exc = || IRExpr::Variable(caught.clone());
    out.push(IRStatement::TryExcept {
        try_body: Box::new(inner),
        except_handlers: vec![crate::ir::IRExceptHandler {
            exception_types: vec!["BaseException".to_string()],
            name: Some(caught.clone()),
            body: IRBody {
                statements: vec![
                    IRStatement::Assign {
                        target: ok.clone(),
                        value: IRExpr::Const(crate::ir::IRConstant::Bool(false)),
                        var_type: Some(IRType::Bool),
                    },
                    IRStatement::If {
                        condition: IRExpr::UnaryOp {
                            operand: Box::new(IRExpr::MethodCall {
                                object: Box::new(IRExpr::Variable(manager.clone())),
                                method_name: "__exit__".to_string(),
                                arguments: vec![
                                    exc(),
                                    exc(),
                                    IRExpr::Const(crate::ir::IRConstant::None),
                                ],
                            }),
                            op: crate::ir::IRUnaryOp::Not,
                        },
                        then_body: Box::new(IRBody {
                            statements: vec![IRStatement::Raise {
                                exception: None,
                                cause: None,
                            }],
                        }),
                        else_body: None,
                    },
                ],
            },
        }],
        finally_body: None,
    });
    out.push(IRStatement::If {
        condition: IRExpr::Variable(ok),
        then_body: Box::new(IRBody {
            statements: normal_exit,
        }),
        else_body: None,
    });

    Ok(out)
}

/// Whether `name` is used in `body` other than in `name is None`,
/// `name is not None`, `not name`, or as the condition of an `if` or a
/// `while`.
fn uses_beyond_none_checks(body: &mut IRBody, name: &str) -> bool {
    use crate::ir::{IRCompareOp, IRConstant, IRUnaryOp};
    let is_name = |e: &IRExpr| matches!(e, IRExpr::Variable(n) | IRExpr::Param(n) if n == name);
    let is_none = |e: &IRExpr| matches!(e, IRExpr::Const(IRConstant::None));
    let mut total = 0;
    let mut allowed = 0;
    let _ = crate::ir::visit_body(body, &mut |e: &mut IRExpr| {
        match e {
            _ if is_name(e) => total += 1,
            IRExpr::CompareOp { left, right, op }
                if matches!(op, IRCompareOp::Is | IRCompareOp::IsNot)
                    && ((is_name(left) && is_none(right)) || (is_none(left) && is_name(right))) =>
            {
                allowed += 1
            }
            IRExpr::UnaryOp {
                operand,
                op: IRUnaryOp::Not,
            } if is_name(operand) => allowed += 1,
            _ => {}
        }
        Ok(())
    });
    fn conditions(body: &IRBody, is_name: &dyn Fn(&IRExpr) -> bool) -> usize {
        body.statements
            .iter()
            .map(|stmt| match stmt {
                IRStatement::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    usize::from(is_name(condition))
                        + conditions(then_body, is_name)
                        + else_body.as_ref().map_or(0, |b| conditions(b, is_name))
                }
                IRStatement::While { condition, body } => {
                    usize::from(is_name(condition)) + conditions(body, is_name)
                }
                IRStatement::For { body, .. } => conditions(body, is_name),
                IRStatement::TryExcept {
                    try_body,
                    except_handlers,
                    finally_body,
                } => {
                    conditions(try_body, is_name)
                        + except_handlers
                            .iter()
                            .map(|h| conditions(&h.body, is_name))
                            .sum::<usize>()
                        + finally_body.as_ref().map_or(0, |b| conditions(b, is_name))
                }
                _ => 0,
            })
            .sum()
    }
    allowed += conditions(body, &is_name);
    total > allowed
}

/// The `__exit__(None, None, None)` call. The normal exit path has no exception
/// to report, and this subset never suppresses one through the return value, so
/// the result is discarded.
fn exit_call(manager: &str) -> IRStatement {
    IRStatement::Expression(IRExpr::MethodCall {
        object: Box::new(IRExpr::Variable(manager.to_string())),
        method_name: "__exit__".to_string(),
        arguments: vec![
            IRExpr::Const(crate::ir::IRConstant::None),
            IRExpr::Const(crate::ir::IRConstant::None),
            IRExpr::Const(crate::ir::IRConstant::None),
        ],
    })
}

/// Run `cleanup` before every statement that leaves `body` early: a `return`
/// anywhere inside it, and a `break`/`continue` that leaves it rather than an
/// enclosing loop written inside it. The returned expression is evaluated into
/// a temporary first, so it still sees the state from inside the block,
/// matching Python's order.
///
/// `in_nested_loop` says whether the walk has descended into a loop that lives
/// inside the block. A `break` down there belongs to that loop and never leaves
/// the block, so it gets no cleanup; a `return` always leaves, at any depth.
///
/// Because both `with` and `try`/`finally` are lowered by the same bottom-up
/// walk, cleanups compose in Python's order without any extra bookkeeping: the
/// inner construct inserts its cleanup immediately before the exit statement
/// first, and the outer one then inserts its own before that same statement,
/// which lands after the inner cleanup.
fn run_cleanup_before_exits(
    body: &mut IRBody,
    cleanup: &[IRStatement],
    return_type: &IRType,
    in_nested_loop: bool,
) {
    let mut out = Vec::with_capacity(body.statements.len());
    for stmt in std::mem::take(&mut body.statements) {
        match stmt {
            IRStatement::Return(None) => {
                out.extend(cleanup.iter().cloned());
                out.push(IRStatement::Return(None));
            }
            IRStatement::Return(Some(IRExpr::Const(c))) => {
                // A constant cannot observe anything the cleanup does, so it
                // needs no temporary.
                out.extend(cleanup.iter().cloned());
                out.push(IRStatement::Return(Some(IRExpr::Const(c))));
            }
            IRStatement::Return(Some(expr)) => {
                let seq = WITH_COUNTER.fetch_add(1, Ordering::Relaxed);
                let temp = format!("__with_ret_{seq}");
                out.push(IRStatement::Assign {
                    target: temp.clone(),
                    value: expr,
                    var_type: match return_type {
                        IRType::Unknown => None,
                        other => Some(other.clone()),
                    },
                });
                out.extend(cleanup.iter().cloned());
                out.push(IRStatement::Return(Some(IRExpr::Variable(temp))));
            }
            stmt @ (IRStatement::Break | IRStatement::Continue) if !in_nested_loop => {
                out.extend(cleanup.iter().cloned());
                out.push(stmt);
            }
            mut other => {
                let loops_here = in_nested_loop || opens_a_loop(&other);
                for nested in nested_bodies(&mut other) {
                    run_cleanup_before_exits(nested, cleanup, return_type, loops_here);
                }
                out.push(other);
            }
        }
    }
    body.statements = out;
}

/// True for statements that own a loop a `break`/`continue` inside them would
/// bind to, rather than the block being walked.
fn opens_a_loop(stmt: &IRStatement) -> bool {
    matches!(stmt, IRStatement::While { .. } | IRStatement::For { .. })
}

/// Every nested body a statement owns, for the `return` walk.
fn nested_bodies(stmt: &mut IRStatement) -> Vec<&mut IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            let mut bodies = vec![&mut **then_body];
            if let Some(b) = else_body {
                bodies.push(&mut **b);
            }
            bodies
        }
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![&mut **body],
        IRStatement::For {
            body, else_body, ..
        } => {
            let mut bodies = vec![&mut **body];
            if let Some(b) = else_body {
                bodies.push(&mut **b);
            }
            bodies
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            let mut bodies = vec![&mut **try_body];
            for handler in except_handlers {
                bodies.push(&mut handler.body);
            }
            if let Some(b) = finally_body {
                bodies.push(&mut **b);
            }
            bodies
        }
        _ => Vec::new(),
    }
}
