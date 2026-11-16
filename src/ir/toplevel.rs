//! A module's top-level statements: what CPython runs as it imports a module,
//! besides the definitions.
//!
//! A loop, a call, an `if`, a `try`, or an augmented assignment written at
//! module level runs where it stands, between the definitions around it. Each
//! run of such statements is lowered to a function named with
//! [`TOPLEVEL_FN_PREFIX`] and listed in the module's top level as an
//! [`IRInitItem::Run`](crate::ir::IRInitItem::Run), so every module pass
//! treats it as code (keywords placed, `with` desugared, a loop over a
//! generator consumed through `__next__`). `compiler::start` then writes its
//! body into the module's start at its place, so the statements run in
//! CPython's order, interleaved with the definitions, when the module is
//! first imported (the program's own file when it starts).
//!
//! Every name a statement binds is a module global, as CPython's are: the
//! function declares them `global`, so a function of the module reads what
//! the statements left there. A loop, unpacking, or `with` target binds a
//! temporary that is assigned to the name, since code generation keeps those
//! targets in locals.

use crate::ir::{IRBody, IRExpr, IRFunction, IRStatement, IRType};

/// The prefix of the functions that hold a module's top-level statements,
/// kept out of exports and metadata, and written into the module's start.
pub const TOPLEVEL_FN_PREFIX: &str = "__toplevel_";

/// The prefix of the temporaries a loop, unpacking, or `with` target at
/// module level binds before the name is assigned.
const TARGET_PREFIX: &str = "__toplevel_target_";

/// The names the statements of `body` bind, in first-binding order: every
/// assignment, augmented assignment, loop, unpacking, and `with` target, at
/// any depth. A handler's `except E as e` is left out, as CPython deletes the
/// name when the handler ends.
pub(crate) fn bound_names(body: &IRBody) -> Vec<String> {
    fn add(name: &str, out: &mut Vec<String>) {
        if !out.iter().any(|o| o == name) {
            out.push(name.to_string());
        }
    }
    fn walk(body: &IRBody, out: &mut Vec<String>) {
        for stmt in &body.statements {
            match stmt {
                IRStatement::Assign { target, .. } | IRStatement::AugAssign { target, .. } => {
                    add(target, out)
                }
                IRStatement::TupleUnpack { targets, .. } => {
                    targets.iter().for_each(|t| add(t, out));
                }
                IRStatement::For {
                    target,
                    body,
                    else_body,
                    ..
                } => {
                    add(target, out);
                    walk(body, out);
                    if let Some(e) = else_body {
                        walk(e, out);
                    }
                }
                IRStatement::With {
                    optional_vars,
                    body,
                    ..
                } => {
                    if let Some(name) = optional_vars {
                        add(name, out);
                    }
                    walk(body, out);
                }
                IRStatement::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    walk(then_body, out);
                    if let Some(e) = else_body {
                        walk(e, out);
                    }
                }
                IRStatement::While { body, .. } => walk(body, out),
                IRStatement::TryExcept {
                    try_body,
                    except_handlers,
                    finally_body,
                } => {
                    walk(try_body, out);
                    for h in except_handlers {
                        walk(&h.body, out);
                    }
                    if let Some(f) = finally_body {
                        walk(f, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(body, &mut out);
    out
}

/// Bind each loop, unpacking, and `with` target of `body` that is one of
/// `globals` through a temporary, assigned to the name right after: `for i in
/// xs:` becomes `for t in xs: i = t`, so `i` is stored as a module global is.
pub(crate) fn assign_targets_through_temps(
    body: &mut IRBody,
    globals: &[String],
    counter: &mut usize,
) {
    fn fresh(name: &str, counter: &mut usize) -> String {
        *counter += 1;
        format!("{TARGET_PREFIX}{name}_{counter}")
    }
    let assign = |name: &str, temp: &str| IRStatement::Assign {
        target: name.to_string(),
        value: IRExpr::Variable(temp.to_string()),
        var_type: None,
    };
    let mut out = Vec::with_capacity(body.statements.len());
    for mut stmt in std::mem::take(&mut body.statements) {
        let mut after = Vec::new();
        match &mut stmt {
            IRStatement::For {
                target,
                body,
                else_body,
                ..
            } => {
                assign_targets_through_temps(body, globals, counter);
                if let Some(e) = else_body {
                    assign_targets_through_temps(e, globals, counter);
                }
                if globals.contains(target) {
                    let temp = fresh(target, counter);
                    body.statements.insert(0, assign(target, &temp));
                    *target = temp;
                }
            }
            IRStatement::With {
                optional_vars,
                body,
                ..
            } => {
                assign_targets_through_temps(body, globals, counter);
                if let Some(name) = optional_vars {
                    if globals.contains(name) {
                        let temp = fresh(name, counter);
                        body.statements.insert(0, assign(name, &temp));
                        *name = temp;
                    }
                }
            }
            IRStatement::TupleUnpack { targets, .. } => {
                for target in targets.iter_mut() {
                    if globals.contains(target) {
                        let temp = fresh(target, counter);
                        after.push(assign(target, &temp));
                        *target = temp;
                    }
                }
            }
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                assign_targets_through_temps(then_body, globals, counter);
                if let Some(e) = else_body {
                    assign_targets_through_temps(e, globals, counter);
                }
            }
            IRStatement::While { body, .. } => assign_targets_through_temps(body, globals, counter),
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                assign_targets_through_temps(try_body, globals, counter);
                for h in except_handlers {
                    assign_targets_through_temps(&mut h.body, globals, counter);
                }
                if let Some(f) = finally_body {
                    assign_targets_through_temps(f, globals, counter);
                }
            }
            _ => {}
        }
        out.push(stmt);
        out.extend(after);
    }
    body.statements = out;
}

/// The function holding one run of top-level statements, which declares
/// `globals` (the names they bind) its module's.
pub(crate) fn run_function(name: String, mut body: IRBody, globals: Vec<String>) -> IRFunction {
    // A `global` written at module level changes nothing there.
    body.statements
        .retain(|s| !matches!(s, IRStatement::Global(_)));
    if !globals.is_empty() {
        body.statements.insert(0, IRStatement::Global(globals));
    }
    IRFunction {
        name,
        params: Vec::new(),
        body,
        return_type: IRType::None,
        decorators: Vec::new(),
    }
}

/// The statements of a run function as the module's start runs them: its
/// body without the `global` declaration, which the start does not need,
/// since every assignment there to a module global stores into it.
pub(crate) fn start_statements(function: &IRFunction) -> Vec<IRStatement> {
    function
        .body
        .statements
        .iter()
        .filter(|s| !matches!(s, IRStatement::Global(_)))
        .cloned()
        .collect()
}
