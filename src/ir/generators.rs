//! Generator functions and the iterator protocol (#6, #40, #45).
//!
//! Runs over the lowered IR (before `finalize`) and rewrites every generator
//! function — one whose body contains `yield` — into a resumable state
//! machine, then desugars iteration over generators and over user classes
//! implementing `__iter__`/`__next__`.
//!
//! A generator function `def gen(a): ...` becomes:
//!
//! - A synthesized state class `__Gen_gen` whose instance holds the resume
//!   point (`__gen_pc`, -1 once exhausted), the value passed by `send()`
//!   (`__gen_sent`), and every parameter and local of the original body as a
//!   field, so all live state survives suspension in linear memory.
//! - A `__step` method holding the original body flattened into basic blocks
//!   dispatched on `__gen_pc` inside a `while True` trampoline. `yield v`
//!   stores the next block id and returns `v`; the next call re-dispatches to
//!   the stored block. Falling off the end (or `return`) marks the generator
//!   exhausted and raises `StopIteration`.
//! - `__next__` / `send` / `close` / `__iter__` methods riding on the
//!   ordinary class machinery, so a generator object is a plain heap instance.
//! - The original function keeps its name and signature but simply returns a
//!   fresh state instance, so `gen(3)` builds a suspended generator.
//!
//! Iteration is desugared at the IR level: `for x in it` over a generator (or
//! a user iterator class) becomes `while True: x = Cls::__next__(it);
//! if <stop-flag> break`, where the stop flag is a module-wide WASM global
//! set by `raise StopIteration` and read-and-cleared by the
//! `__waspy_stop_check` codegen intrinsic. Static dispatch through qualified
//! `Class::method` calls keeps the desugared code independent of local type
//! inference.
//!
//! Accepted deviations of this subset: generator methods (yield inside a
//! class method) are rejected; `yield`/`return` inside `try`/`with` in a
//! generator is rejected; a lambda inside a generator cannot capture the
//! generator's locals; `for`-loop `else` clauses are ignored (matching the
//! existing `for` codegen); tuple-unpacking targets and the targets of
//! yield-free `for` loops stay in WASM locals, so they do not survive across
//! a `yield`.

use crate::ir::{
    IRBody, IRBoolOp, IRClass, IRCompareOp, IRConstant, IRExpr, IRFunction, IRGenerator, IRModule,
    IROp, IRParam, IRStatement, IRType, IRUnaryOp,
};
use anyhow::{anyhow, Result};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Codegen intrinsic recognized by name in expression codegen: leaves the
/// current value of the StopIteration flag (global 1) on the stack and clears
/// the flag.
pub const STOP_CHECK_FN: &str = "__waspy_stop_check";

/// Prefix of the synthesized helpers that drain an iterator into a list for a
/// comprehension (see `drain_function`). They are internal: not exported, and
/// left out of function metadata.
pub const DRAIN_FN_PREFIX: &str = "__drain_";

/// State-class field holding the sent value; `x = yield v` resumes by reading
/// this name, and the converter lowers the assignment through it.
pub const GEN_SENT_FIELD: &str = "__gen_sent";

/// State-class field holding the block id to resume at; -1 once exhausted.
const GEN_PC_FIELD: &str = "__gen_pc";

/// `__gen_pc` value marking an exhausted generator.
const EXHAUSTED: i32 = -1;

/// What a class contributes to the iterator protocol. Synthesized generator
/// state classes support everything; user classes support what they define.
struct IterFacts {
    has_iter: bool,
    has_next: bool,
    has_send: bool,
    has_close: bool,
    /// Class of the object `__iter__` returns (declared return annotation,
    /// falling back to the class itself for the common `return self`).
    iter_returns: Option<String>,
    /// True for a synthesized generator state class, whose `__iter__` is the
    /// identity — iterating it skips the extra call.
    synthetic: bool,
}

fn int_const(v: i32) -> IRExpr {
    IRExpr::Const(IRConstant::Int(v))
}

fn self_var() -> IRExpr {
    IRExpr::Variable("self".to_string())
}

fn self_attr(name: &str) -> IRExpr {
    IRExpr::Attribute {
        object: Box::new(self_var()),
        attribute: name.to_string(),
    }
}

fn set_pc(block: i32) -> IRStatement {
    IRStatement::AttributeAssign {
        object: self_var(),
        attribute: GEN_PC_FIELD.to_string(),
        value: int_const(block),
        annotation: None,
    }
}

fn raise_stop_iteration() -> IRStatement {
    IRStatement::Raise {
        exception: Some(IRExpr::Variable("StopIteration".to_string())),
    }
}

fn stop_check() -> IRExpr {
    IRExpr::FunctionCall {
        function_name: STOP_CHECK_FN.to_string(),
        arguments: Vec::new(),
    }
}

fn while_true(statements: Vec<IRStatement>) -> IRStatement {
    IRStatement::While {
        condition: IRExpr::Const(IRConstant::Bool(true)),
        body: Box::new(IRBody { statements }),
    }
}

fn body_of(statements: Vec<IRStatement>) -> Box<IRBody> {
    Box::new(IRBody { statements })
}

/// Does this body contain a `yield` at any statement depth?
fn body_has_yield(body: &IRBody) -> bool {
    body.statements.iter().any(stmt_has_yield)
}

fn stmt_has_yield(stmt: &IRStatement) -> bool {
    any_sub_body(stmt, &mut |s| matches!(s, IRStatement::Yield { .. }))
}

/// Does this body suspend (yield) or leave the generator (return) anywhere?
/// Both force the enclosing control flow to be flattened.
fn body_suspends(body: &IRBody) -> bool {
    body.statements.iter().any(|s| {
        any_sub_body(s, &mut |s| {
            matches!(s, IRStatement::Yield { .. } | IRStatement::Return(_))
        })
    })
}

/// Apply `pred` to `stmt` and every statement nested under it.
fn any_sub_body(stmt: &IRStatement, pred: &mut impl FnMut(&IRStatement) -> bool) -> bool {
    if pred(stmt) {
        return true;
    }
    let bodies: Vec<&IRBody> = match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => std::iter::once(then_body.as_ref())
            .chain(else_body.as_deref())
            .collect(),
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body],
        IRStatement::For {
            body, else_body, ..
        } => std::iter::once(body.as_ref())
            .chain(else_body.as_deref())
            .collect(),
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => std::iter::once(try_body.as_ref())
            .chain(except_handlers.iter().map(|h| &h.body))
            .chain(finally_body.as_deref())
            .collect(),
        _ => Vec::new(),
    };
    bodies
        .into_iter()
        .any(|b| b.statements.iter().any(|s| any_sub_body(s, pred)))
}

/// Entry point: rewrite generators and desugar iterator-protocol consumption
/// across the whole module.
pub fn transform_generators(module: &mut IRModule) -> Result<()> {
    for class in &module.classes {
        for method in &class.methods {
            if body_has_yield(&method.body) {
                return Err(anyhow!(
                    "generator methods are not supported: '{}.{}' contains yield",
                    class.name,
                    method.name
                ));
            }
        }
    }

    // Generator functions and the state class each will get.
    let gen_classes: HashMap<String, String> = module
        .functions
        .iter()
        .filter(|f| body_has_yield(&f.body))
        .map(|f| (f.name.clone(), format!("__Gen_{}", f.name)))
        .collect();

    // Iterator facts per class: user classes as declared, plus the (not yet
    // built) generator state classes.
    let mut iter_facts: HashMap<String, IterFacts> = module
        .classes
        .iter()
        .map(|c| {
            let has = |n: &str| c.methods.iter().any(|m| m.name == n);
            let iter_returns =
                c.methods
                    .iter()
                    .find(|m| m.name == "__iter__")
                    .map(|m| match &m.return_type {
                        IRType::Class(name) => name.clone(),
                        _ => c.name.clone(),
                    });
            (
                c.name.clone(),
                IterFacts {
                    has_iter: has("__iter__"),
                    has_next: has("__next__"),
                    has_send: has("send"),
                    has_close: has("close"),
                    iter_returns,
                    synthetic: false,
                },
            )
        })
        .collect();
    for class_name in gen_classes.values() {
        iter_facts.insert(
            class_name.clone(),
            IterFacts {
                has_iter: true,
                has_next: true,
                has_send: true,
                has_close: true,
                iter_returns: Some(class_name.clone()),
                synthetic: true,
            },
        );
    }

    if gen_classes.is_empty() && !iter_facts.values().any(|f| f.has_next) {
        return Ok(());
    }

    // Desugar iteration and iterator method calls in every body (generator
    // bodies included, so a generator can consume another generator).
    let mut counter = 0u32;
    let mut drains = BTreeSet::new();
    let mut errors = Vec::new();
    {
        let mut desugarer = Desugarer {
            gen_fns: &gen_classes,
            facts: &iter_facts,
            counter: &mut counter,
            drains: &mut drains,
            errors: &mut errors,
            current: String::new(),
        };
        for func in &mut module.functions {
            desugarer.current = func.name.clone();
            let mut vars = desugarer.seed_params(&func.params);
            desugarer.rewrite_body(&mut func.body, &mut vars);
        }
        for class in &mut module.classes {
            for method in &mut class.methods {
                desugarer.current = format!("{}.{}", class.name, method.name);
                let mut vars = desugarer.seed_params(&method.params);
                desugarer.rewrite_body(&mut method.body, &mut vars);
            }
        }
    }
    if !errors.is_empty() {
        return Err(anyhow!(errors.join("\n")));
    }

    // Rewrite each generator function into a constructor plus a state class.
    let mut state_classes = Vec::new();
    for func in &mut module.functions {
        let Some(class_name) = gen_classes.get(&func.name) else {
            continue;
        };
        state_classes.push(build_state_class(func, class_name, &mut counter)?);

        let arguments = func
            .params
            .iter()
            .map(|p| IRExpr::Variable(p.name.clone()))
            .collect();
        func.body = IRBody {
            statements: vec![IRStatement::Return(Some(IRExpr::FunctionCall {
                function_name: class_name.clone(),
                arguments,
            }))],
        };
        func.return_type = IRType::Class(class_name.clone());
    }
    module.classes.extend(state_classes);

    // A drain helper's list holds what the iterator's `__next__` returns, which
    // for a generator is only known once its state class is built.
    for class in drains {
        let element = module
            .classes
            .iter()
            .find(|c| c.name == class)
            .and_then(|c| c.methods.iter().find(|m| m.name == "__next__"))
            .map_or(IRType::Unknown, |m| m.return_type.clone());
        module.functions.push(drain_function(&class, element));
    }

    Ok(())
}

fn drain_fn_name(class: &str) -> String {
    format!("{DRAIN_FN_PREFIX}{class}")
}

/// `def __drain_<C>(it) -> List[T]`: every value an iterator of class `C`
/// yields, collected into a fresh list for a comprehension to iterate.
fn drain_function(class: &str, element: IRType) -> IRFunction {
    let var = |name: &str| IRExpr::Variable(name.to_string());
    let list = IRType::List(Box::new(element));
    IRFunction {
        name: drain_fn_name(class),
        params: vec![IRParam {
            name: "it".to_string(),
            param_type: IRType::Class(class.to_string()),
            default_value: None,
        }],
        body: IRBody {
            statements: vec![
                IRStatement::Assign {
                    target: "items".to_string(),
                    value: IRExpr::ListLiteral(Vec::new()),
                    var_type: Some(list.clone()),
                },
                while_true(vec![
                    IRStatement::Expression(stop_check()),
                    IRStatement::Assign {
                        target: "item".to_string(),
                        value: IRExpr::FunctionCall {
                            function_name: format!("{class}::__next__"),
                            arguments: vec![var("it")],
                        },
                        var_type: None,
                    },
                    IRStatement::If {
                        condition: stop_check(),
                        then_body: body_of(vec![IRStatement::Break]),
                        else_body: None,
                    },
                    IRStatement::Expression(IRExpr::MethodCall {
                        object: Box::new(var("items")),
                        method_name: "append".to_string(),
                        arguments: vec![var("item")],
                    }),
                ]),
                IRStatement::Return(Some(var("items"))),
            ],
        },
        return_type: list,
        decorators: Vec::new(),
    }
}

/// Rewrites iterator-protocol consumption: `for` over a generator or user
/// iterator becomes an explicit `__next__` drive loop, and
/// `__next__`/`send`/`close` method calls on statically known iterator values
/// become qualified `Class::method` calls (static dispatch, independent of
/// codegen-time type inference). Iterator-ness flows per function from
/// generator constructor calls, class instantiations, and parameter
/// annotations through local assignments.
struct Desugarer<'a> {
    gen_fns: &'a HashMap<String, String>,
    facts: &'a HashMap<String, IterFacts>,
    counter: &'a mut u32,
    /// Iterator classes a comprehension drains, each needing a helper.
    drains: &'a mut BTreeSet<String>,
    errors: &'a mut Vec<String>,
    /// The function or method being rewritten, for error messages.
    current: String,
}

impl Desugarer<'_> {
    fn seed_params(&self, params: &[IRParam]) -> HashMap<String, String> {
        params
            .iter()
            .filter_map(|p| match &p.param_type {
                IRType::Class(c) if self.facts.get(c).is_some_and(|f| f.has_next || f.has_iter) => {
                    Some((p.name.clone(), c.clone()))
                }
                _ => None,
            })
            .collect()
    }

    /// Class of the iterator/generator `expr` evaluates to, if statically known.
    fn iterator_class(&self, expr: &IRExpr, vars: &HashMap<String, String>) -> Option<String> {
        match expr {
            IRExpr::FunctionCall { function_name, .. } => {
                if let Some(class) = self.gen_fns.get(function_name) {
                    return Some(class.clone());
                }
                self.facts
                    .get(function_name)
                    .filter(|f| f.has_next || f.has_iter)
                    .map(|_| function_name.clone())
            }
            IRExpr::Variable(name) | IRExpr::Param(name) => vars.get(name).cloned(),
            IRExpr::MethodCall {
                object,
                method_name,
                ..
            } if method_name == "__iter__" => {
                let class = self.iterator_class(object, vars)?;
                self.facts.get(&class)?.iter_returns.clone()
            }
            _ => None,
        }
    }

    /// What iterating `iterable`, an iterator of class `class`, drives, and its
    /// class. Python calls `__iter__` first; a synthesized generator's is the
    /// identity, and a `__next__`-only class iterates itself.
    fn iteration_source(&self, iterable: IRExpr, class: String) -> (IRExpr, String) {
        let facts = &self.facts[&class];
        if facts.synthetic || !facts.has_iter {
            return (iterable, class);
        }
        let returns = facts.iter_returns.clone().unwrap_or_else(|| class.clone());
        let call = IRExpr::FunctionCall {
            function_name: format!("{class}::__iter__"),
            arguments: vec![iterable],
        };
        (call, returns)
    }

    /// A comprehension sizes its result from its iterables before filling it,
    /// so an iterator there (a generator, or a class with `__next__`) is first
    /// drained into a list by a synthesized helper. The comprehension used to
    /// read the iterator object itself as a list (#129). Only the first `for`
    /// may iterate one: an inner iterable is evaluated once to size the result
    /// and again to fill it, which would drain an iterator twice.
    fn drain_comprehension_iterables(
        &mut self,
        generators: &mut [IRGenerator],
        vars: &HashMap<String, String>,
    ) {
        let mut scope = vars.clone();
        for (g, generator) in generators.iter_mut().enumerate() {
            if let Some(class) = self.iterator_class(&generator.iterable, &scope) {
                if g > 0 {
                    self.errors.push(format!(
                        "a comprehension in '{}' iterates a generator or iterator in its \
                         'for' clause {}, but only the first 'for' of a comprehension can. \
                         Hint: collect it into a list first, or write the loops out",
                        self.current,
                        g + 1
                    ));
                } else {
                    let iterable =
                        std::mem::replace(&mut generator.iterable, IRExpr::Const(IRConstant::None));
                    let (it_expr, it_class) = self.iteration_source(iterable, class);
                    generator.iterable = if self.facts.get(&it_class).is_some_and(|f| f.has_next) {
                        self.drains.insert(it_class.clone());
                        IRExpr::FunctionCall {
                            function_name: drain_fn_name(&it_class),
                            arguments: vec![it_expr],
                        }
                    } else {
                        it_expr
                    };
                }
            }
            for target in &generator.targets {
                scope.remove(target);
            }
        }
    }

    fn rewrite_body(&mut self, body: &mut IRBody, vars: &mut HashMap<String, String>) {
        let stmts = std::mem::take(&mut body.statements);
        let mut out = Vec::with_capacity(stmts.len());
        for mut stmt in stmts {
            self.rewrite_stmt_exprs(&mut stmt, vars);
            match stmt {
                IRStatement::For {
                    target,
                    iterable,
                    body: mut for_body,
                    else_body,
                } => {
                    let Some(class) = self.iterator_class(&iterable, vars) else {
                        vars.remove(&target);
                        self.rewrite_body(&mut for_body, vars);
                        out.push(IRStatement::For {
                            target,
                            iterable,
                            body: for_body,
                            else_body,
                        });
                        continue;
                    };
                    let (it_expr, it_class) = self.iteration_source(iterable, class);
                    if !self.facts.get(&it_class).is_some_and(|f| f.has_next) {
                        vars.remove(&target);
                        self.rewrite_body(&mut for_body, vars);
                        out.push(IRStatement::For {
                            target,
                            iterable: it_expr,
                            body: for_body,
                            else_body,
                        });
                        continue;
                    }

                    let n = *self.counter;
                    *self.counter += 1;
                    let it_var = format!("__giter_{n}");
                    out.push(IRStatement::Assign {
                        target: it_var.clone(),
                        value: it_expr,
                        var_type: None,
                    });
                    vars.insert(it_var.clone(), it_class.clone());
                    vars.remove(&target);
                    self.rewrite_body(&mut for_body, vars);

                    // Clear any stale stop flag, pull the next value, and
                    // break once `__next__` raised StopIteration.
                    let mut loop_body = vec![
                        IRStatement::Expression(stop_check()),
                        IRStatement::Assign {
                            target,
                            value: IRExpr::FunctionCall {
                                function_name: format!("{it_class}::__next__"),
                                arguments: vec![IRExpr::Variable(it_var)],
                            },
                            var_type: None,
                        },
                        IRStatement::If {
                            condition: stop_check(),
                            then_body: body_of(vec![IRStatement::Break]),
                            else_body: None,
                        },
                    ];
                    loop_body.extend(for_body.statements);
                    out.push(while_true(loop_body));
                }
                IRStatement::Assign {
                    target,
                    value,
                    var_type,
                } => {
                    match self.iterator_class(&value, vars) {
                        // A later zip() argument is read with len() and an
                        // index (see the zip lowering in `ir::converter`),
                        // which an iterator has neither of (#133). Draining
                        // it first would pull every value up front, where
                        // zip pulls one per round.
                        Some(_) if target.starts_with(crate::ir::ZIP_SEQ_PREFIX) => {
                            self.errors.push(format!(
                                "'{}' passes a generator or iterator to zip() after its first \
                                 argument, which is not supported yet. Hint: pass it as the \
                                 first argument, or collect it into a list with a \
                                 comprehension first",
                                self.current
                            ));
                        }
                        Some(class) => {
                            vars.insert(target.clone(), class);
                        }
                        None => {
                            vars.remove(&target);
                        }
                    }
                    out.push(IRStatement::Assign {
                        target,
                        value,
                        var_type,
                    });
                }
                IRStatement::If {
                    condition,
                    mut then_body,
                    mut else_body,
                } => {
                    self.rewrite_body(&mut then_body, vars);
                    if let Some(else_body) = else_body.as_mut() {
                        self.rewrite_body(else_body, vars);
                    }
                    out.push(IRStatement::If {
                        condition,
                        then_body,
                        else_body,
                    });
                }
                IRStatement::While {
                    condition,
                    mut body,
                } => {
                    self.rewrite_body(&mut body, vars);
                    out.push(IRStatement::While { condition, body });
                }
                IRStatement::With {
                    context_expr,
                    optional_vars,
                    mut body,
                } => {
                    self.rewrite_body(&mut body, vars);
                    out.push(IRStatement::With {
                        context_expr,
                        optional_vars,
                        body,
                    });
                }
                IRStatement::TryExcept {
                    mut try_body,
                    mut except_handlers,
                    mut finally_body,
                } => {
                    self.rewrite_body(&mut try_body, vars);
                    for handler in &mut except_handlers {
                        self.rewrite_body(&mut handler.body, vars);
                    }
                    if let Some(finally_body) = finally_body.as_mut() {
                        self.rewrite_body(finally_body, vars);
                    }
                    out.push(IRStatement::TryExcept {
                        try_body,
                        except_handlers,
                        finally_body,
                    });
                }
                other => out.push(other),
            }
        }
        body.statements = out;
    }

    /// Rewrite every expression embedded in `stmt` (not its nested bodies —
    /// `rewrite_body` recurses into those with scope updates).
    fn rewrite_stmt_exprs(&mut self, stmt: &mut IRStatement, vars: &HashMap<String, String>) {
        match stmt {
            IRStatement::Return(Some(e))
            | IRStatement::Yield { value: Some(e) }
            | IRStatement::Raise { exception: Some(e) }
            | IRStatement::Expression(e)
            | IRStatement::Assign { value: e, .. }
            | IRStatement::AugAssign { value: e, .. }
            | IRStatement::TupleUnpack { value: e, .. }
            | IRStatement::DynamicImport { module_name: e, .. }
            | IRStatement::While { condition: e, .. }
            | IRStatement::If { condition: e, .. }
            | IRStatement::For { iterable: e, .. }
            | IRStatement::With {
                context_expr: e, ..
            } => self.rewrite_expr(e, vars),
            IRStatement::AttributeAssign { object, value, .. }
            | IRStatement::AttributeAugAssign { object, value, .. } => {
                self.rewrite_expr(object, vars);
                self.rewrite_expr(value, vars);
            }
            IRStatement::IndexAssign {
                container,
                index,
                value,
            } => {
                self.rewrite_expr(container, vars);
                self.rewrite_expr(index, vars);
                self.rewrite_expr(value, vars);
            }
            _ => {}
        }
    }

    fn rewrite_expr(&mut self, expr: &mut IRExpr, vars: &HashMap<String, String>) {
        for_each_child(expr, &mut |child| self.rewrite_expr(child, vars));

        if let IRExpr::Comprehension { generators, .. } = expr {
            self.drain_comprehension_iterables(generators, vars);
            return;
        }

        // `len()` of an iterator is a TypeError in Python; it read the
        // iterator object's first word as a length (#133).
        if let IRExpr::FunctionCall {
            function_name,
            arguments,
        } = expr
        {
            if function_name == "len" && arguments.len() == 1 {
                if let Some(class) = self.iterator_class(&arguments[0], vars) {
                    let kind = if self.facts.get(&class).is_some_and(|f| f.synthetic) {
                        "generator".to_string()
                    } else {
                        class
                    };
                    self.errors.push(format!(
                        "object of type '{kind}' has no len() (in function '{}')",
                        self.current
                    ));
                }
            }
            return;
        }

        let IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } = expr
        else {
            return;
        };
        let supported = |facts: &IterFacts| match method_name.as_str() {
            "__next__" => facts.has_next,
            "send" => facts.has_send,
            "close" => facts.has_close,
            "__iter__" => facts.has_iter && !facts.synthetic,
            _ => false,
        };
        let Some(class) = self.iterator_class(object, vars) else {
            return;
        };
        if !self.facts.get(&class).is_some_and(supported) {
            return;
        }
        let mut call_args = vec![(**object).clone()];
        call_args.append(arguments);
        let replacement = IRExpr::FunctionCall {
            function_name: format!("{class}::{method_name}"),
            arguments: call_args,
        };
        *expr = replacement;
    }
}

/// Visit each direct child expression of `expr`. Lambda bodies are skipped —
/// their parameters shadow enclosing names and their capture analysis runs in
/// the later lambda-lifting pass.
fn for_each_child(expr: &mut IRExpr, f: &mut impl FnMut(&mut IRExpr)) {
    match expr {
        // No child expressions: the slot is a constant and the environment is a
        // parameter.
        IRExpr::EnvRead { .. } | IRExpr::CellNew | IRExpr::CellLoad { .. } => {}
        IRExpr::CellStore { value, .. } | IRExpr::Keyword { value, .. } => f(value),
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            f(left);
            f(right);
        }
        IRExpr::UnaryOp { operand, .. } => f(operand),
        IRExpr::FunctionCall { arguments, .. } => arguments.iter_mut().for_each(f),
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            items.iter_mut().for_each(f)
        }
        IRExpr::DictLiteral(entries) => {
            for (k, v) in entries {
                f(k);
                f(v);
            }
        }
        IRExpr::Indexing { container, index } => {
            f(container);
            f(index);
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            f(container);
            for part in [start, end, step].into_iter().flatten() {
                f(part);
            }
        }
        IRExpr::Attribute { object, .. } => f(object),
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            f(element);
            if let Some(value) = value {
                f(value);
            }
            for generator in generators {
                f(&mut generator.iterable);
                generator.conditions.iter_mut().for_each(&mut *f);
            }
        }
        IRExpr::MethodCall {
            object, arguments, ..
        } => {
            f(object);
            arguments.iter_mut().for_each(f);
        }
        IRExpr::DynamicImportExpr { module_name } => f(module_name),
        IRExpr::RangeCall { start, stop, step } => {
            for part in [start, step].into_iter().flatten() {
                f(part);
            }
            f(stop);
        }
        IRExpr::Const(_)
        | IRExpr::Param(_)
        | IRExpr::Variable(_)
        | IRExpr::Lambda { .. }
        | IRExpr::ClosureMake { .. } => {}
    }
}

/// Build the state class for one generator function and leave `func`'s body
/// untouched (the caller replaces it with the constructor call).
fn build_state_class(
    func: &mut IRFunction,
    class_name: &str,
    counter: &mut u32,
) -> Result<IRClass> {
    reject_unsupported(&func.body, false, &func.name)?;

    let mut body = std::mem::replace(
        &mut func.body,
        IRBody {
            statements: Vec::new(),
        },
    );
    desugar_suspending_fors(&mut body, counter);
    split_literal_unpacks(&mut body, counter);

    // Resolved before the lift below rewrites parameter references into
    // `self.<name>` attribute reads.
    let elem_type = yielded_type(&body, &func.params);

    // Every parameter and assigned local moves into a state-class field so it
    // survives suspension; the targets of tuple unpacking and of yield-free
    // `for` loops stay WASM locals (their loop machinery binds locals).
    let mut lift: HashSet<String> = func.params.iter().map(|p| p.name.clone()).collect();
    lift.insert(GEN_SENT_FIELD.to_string());
    collect_assigned_names(&body, &mut lift);
    let mut exempt = HashSet::new();
    collect_exempt_names(&body, &mut exempt);
    for name in &exempt {
        lift.remove(name);
    }
    lift_body(&mut body, &lift);

    let mut flattener = Flattener {
        blocks: vec![Vec::new()],
        current: 0,
        loops: Vec::new(),
    };
    flattener.flatten_body(body.statements, &func.name)?;
    flattener.jump(EXHAUSTED);
    check_locals_across_yields(&flattener.blocks, &exempt, &func.name)?;

    // __step: `while True` trampoline dispatching on the stored block id.
    // Every block ends in a `return` (yield) or a `continue` after storing
    // the next id, so the sequential `if`s never fall through into each
    // other; an unmatched id (-1 included) raises StopIteration.
    let mut dispatch: Vec<IRStatement> = flattener
        .blocks
        .into_iter()
        .enumerate()
        .map(|(id, statements)| IRStatement::If {
            condition: IRExpr::CompareOp {
                left: Box::new(self_attr(GEN_PC_FIELD)),
                right: Box::new(int_const(id as i32)),
                op: IRCompareOp::Eq,
            },
            then_body: body_of(statements),
            else_body: None,
        })
        .collect();
    dispatch.push(set_pc(EXHAUSTED));
    dispatch.push(raise_stop_iteration());

    let self_param = || IRParam {
        name: "self".to_string(),
        param_type: IRType::Class(class_name.to_string()),
        default_value: None,
    };
    let method =
        |name: &str, params: Vec<IRParam>, statements: Vec<IRStatement>, ret: IRType| IRFunction {
            name: name.to_string(),
            params,
            body: IRBody { statements },
            return_type: ret,
            decorators: Vec::new(),
        };
    let step_call = IRExpr::FunctionCall {
        function_name: format!("{class_name}::__step"),
        arguments: vec![self_var()],
    };
    let set_sent = |value: IRExpr| IRStatement::AttributeAssign {
        object: self_var(),
        attribute: GEN_SENT_FIELD.to_string(),
        value,
        annotation: None,
    };
    // The sent-value slot shares the element type so a float `send()` stores
    // at the right width (the first concrete assignment fixes a field's type).
    let sent_zero = || match elem_type {
        IRType::Float => IRExpr::Const(IRConstant::Float(0.0)),
        _ => int_const(0),
    };

    let mut init_statements = vec![set_pc(0), set_sent(sent_zero())];
    for param in &func.params {
        init_statements.push(IRStatement::AttributeAssign {
            object: self_var(),
            attribute: param.name.clone(),
            value: IRExpr::Variable(param.name.clone()),
            annotation: None,
        });
    }
    let mut init_params = vec![self_param()];
    init_params.extend(func.params.iter().cloned());

    let methods = vec![
        method("__init__", init_params, init_statements, IRType::Unknown),
        method(
            "__step",
            vec![self_param()],
            vec![while_true(dispatch)],
            elem_type.clone(),
        ),
        // A plain `__next__` resumes with no sent value (None reads as 0).
        method(
            "__next__",
            vec![self_param()],
            vec![
                set_sent(sent_zero()),
                IRStatement::Return(Some(step_call.clone())),
            ],
            elem_type.clone(),
        ),
        method(
            "send",
            vec![
                self_param(),
                IRParam {
                    name: "value".to_string(),
                    param_type: elem_type.clone(),
                    default_value: None,
                },
            ],
            vec![
                set_sent(IRExpr::Variable("value".to_string())),
                IRStatement::Return(Some(step_call)),
            ],
            elem_type,
        ),
        method(
            "close",
            vec![self_param()],
            vec![set_pc(EXHAUSTED), IRStatement::Return(Some(int_const(0)))],
            IRType::Int,
        ),
        method(
            "__iter__",
            vec![self_param()],
            vec![IRStatement::Return(Some(self_var()))],
            IRType::Class(class_name.to_string()),
        ),
    ];

    Ok(IRClass {
        name: class_name.to_string(),
        bases: Vec::new(),
        methods,
        class_vars: Vec::new(),
    })
}

/// Suspension cannot cross a `try` or `with` frame — the trampoline would
/// have to re-enter the middle of the protected region.
fn reject_unsupported(body: &IRBody, protected: bool, func_name: &str) -> Result<()> {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Yield { .. } | IRStatement::Return(_) if protected => {
                return Err(anyhow!(
                    "generator '{func_name}': yield/return inside try/with is not supported"
                ));
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                reject_unsupported(try_body, true, func_name)?;
                for handler in except_handlers {
                    reject_unsupported(&handler.body, true, func_name)?;
                }
                if let Some(finally_body) = finally_body {
                    reject_unsupported(finally_body, true, func_name)?;
                }
            }
            IRStatement::With { body, .. } => reject_unsupported(body, true, func_name)?,
            IRStatement::If {
                then_body,
                else_body,
                ..
            } => {
                reject_unsupported(then_body, protected, func_name)?;
                if let Some(else_body) = else_body {
                    reject_unsupported(else_body, protected, func_name)?;
                }
            }
            IRStatement::While { body, .. } => reject_unsupported(body, protected, func_name)?,
            IRStatement::For {
                body, else_body, ..
            } => {
                reject_unsupported(body, protected, func_name)?;
                if let Some(else_body) = else_body {
                    reject_unsupported(else_body, protected, func_name)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Rewrite each `for` loop whose body suspends into an equivalent `while`
/// with explicit iteration state, so the flattener only has to handle
/// `while`. Range loops step arithmetically; everything else drives
/// `len()` + indexing (lists and tuples). The increment leads the body so a
/// flattened `continue` (which jumps to the loop head) still advances.
fn desugar_suspending_fors(body: &mut IRBody, counter: &mut u32) {
    let stmts = std::mem::take(&mut body.statements);
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            IRStatement::For {
                target,
                iterable,
                body: mut for_body,
                else_body: _,
            } if body_suspends(&for_body) => {
                desugar_suspending_fors(&mut for_body, counter);
                let n = *counter;
                *counter += 1;
                let var = |prefix: &str| IRExpr::Variable(format!("{prefix}_{n}"));
                let assign = |prefix: &str, value: IRExpr| IRStatement::Assign {
                    target: format!("{prefix}_{n}"),
                    value,
                    var_type: None,
                };
                let binop = |left: IRExpr, op: IROp, right: IRExpr| IRExpr::BinaryOp {
                    left: Box::new(left),
                    right: Box::new(right),
                    op,
                };
                let cmp = |left: IRExpr, op: IRCompareOp, right: IRExpr| IRExpr::CompareOp {
                    left: Box::new(left),
                    right: Box::new(right),
                    op,
                };
                let break_unless = |condition: IRExpr| IRStatement::If {
                    condition: IRExpr::UnaryOp {
                        operand: Box::new(condition),
                        op: IRUnaryOp::Not,
                    },
                    then_body: body_of(vec![IRStatement::Break]),
                    else_body: None,
                };
                match iterable {
                    IRExpr::RangeCall { start, stop, step } => {
                        out.push(assign(
                            "__grs",
                            step.map(|s| *s).unwrap_or_else(|| int_const(1)),
                        ));
                        out.push(assign("__grt", *stop));
                        out.push(IRStatement::Assign {
                            target: target.clone(),
                            value: binop(
                                start.map(|s| *s).unwrap_or_else(|| int_const(0)),
                                IROp::Sub,
                                var("__grs"),
                            ),
                            var_type: None,
                        });
                        // Keep iterating while (step > 0 and t < stop) or
                        // (step <= 0 and t > stop), matching the sign-aware
                        // range codegen.
                        let ascending = IRExpr::BoolOp {
                            left: Box::new(cmp(var("__grs"), IRCompareOp::Gt, int_const(0))),
                            right: Box::new(cmp(
                                IRExpr::Variable(target.clone()),
                                IRCompareOp::Lt,
                                var("__grt"),
                            )),
                            op: IRBoolOp::And,
                        };
                        let descending = IRExpr::BoolOp {
                            left: Box::new(cmp(var("__grs"), IRCompareOp::LtE, int_const(0))),
                            right: Box::new(cmp(
                                IRExpr::Variable(target.clone()),
                                IRCompareOp::Gt,
                                var("__grt"),
                            )),
                            op: IRBoolOp::And,
                        };
                        let mut loop_body = vec![
                            IRStatement::Assign {
                                target: target.clone(),
                                value: binop(
                                    IRExpr::Variable(target.clone()),
                                    IROp::Add,
                                    var("__grs"),
                                ),
                                var_type: None,
                            },
                            break_unless(IRExpr::BoolOp {
                                left: Box::new(ascending),
                                right: Box::new(descending),
                                op: IRBoolOp::Or,
                            }),
                        ];
                        loop_body.extend(for_body.statements);
                        out.push(while_true(loop_body));
                    }
                    other => {
                        out.push(assign("__gsq", other));
                        out.push(assign("__gix", int_const(-1)));
                        let mut loop_body = vec![
                            IRStatement::AugAssign {
                                target: format!("__gix_{n}"),
                                value: int_const(1),
                                op: IROp::Add,
                            },
                            break_unless(cmp(
                                var("__gix"),
                                IRCompareOp::Lt,
                                IRExpr::FunctionCall {
                                    function_name: "len".to_string(),
                                    arguments: vec![var("__gsq")],
                                },
                            )),
                            IRStatement::Assign {
                                target: target.clone(),
                                value: IRExpr::Indexing {
                                    container: Box::new(var("__gsq")),
                                    index: Box::new(var("__gix")),
                                },
                                var_type: None,
                            },
                        ];
                        loop_body.extend(for_body.statements);
                        out.push(while_true(loop_body));
                    }
                }
            }
            mut other => {
                for sub in sub_bodies_mut(&mut other) {
                    desugar_suspending_fors(sub, counter);
                }
                out.push(other);
            }
        }
    }
    body.statements = out;
}

/// Mutable references to every body directly nested under `stmt`.
fn sub_bodies_mut(stmt: &mut IRStatement) -> Vec<&mut IRBody> {
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

/// Names assigned anywhere in the body (plain and augmented assignment).
fn collect_assigned_names(body: &IRBody, out: &mut HashSet<String>) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Assign { target, .. } | IRStatement::AugAssign { target, .. } => {
                out.insert(target.clone());
            }
            _ => {}
        }
        for sub in sub_bodies(stmt) {
            collect_assigned_names(sub, out);
        }
    }
}

/// `a, b = x, y` in a generator body becomes one assignment per target, so
/// the targets are lifted into fields like any assigned local. Unpacking
/// targets stay WASM locals of `__step`, which do not survive a `yield`, so
/// `a, b = b, a + b` in a `while True: yield a` loop restarted from zero on
/// every resume. When the right side reads a target, it is evaluated into
/// temporaries first, keeping Python's evaluate-then-assign order.
fn split_literal_unpacks(body: &mut IRBody, counter: &mut u32) {
    let statements = std::mem::take(&mut body.statements);
    for mut stmt in statements {
        for sub in sub_bodies_mut(&mut stmt) {
            split_literal_unpacks(sub, counter);
        }
        match stmt {
            IRStatement::TupleUnpack {
                targets,
                value: IRExpr::TupleLiteral(items),
                starred: None,
            } if items.len() == targets.len() => {
                let mut read = HashSet::new();
                for item in &items {
                    collect_read_names(item, &mut read);
                }
                if targets.iter().any(|t| read.contains(t)) {
                    let temps: Vec<String> = targets
                        .iter()
                        .map(|_| {
                            *counter += 1;
                            format!("__tu_{counter}")
                        })
                        .collect();
                    for (temp, item) in temps.iter().zip(items) {
                        body.statements.push(assign(temp, item));
                    }
                    for (target, temp) in targets.iter().zip(&temps) {
                        body.statements
                            .push(assign(target, IRExpr::Variable(temp.clone())));
                    }
                } else {
                    for (target, item) in targets.iter().zip(items) {
                        body.statements.push(assign(target, item));
                    }
                }
            }
            other => body.statements.push(other),
        }
    }
}

fn assign(target: &str, value: IRExpr) -> IRStatement {
    IRStatement::Assign {
        target: target.to_string(),
        value,
        var_type: None,
    }
}

/// Every name `expr` reads.
fn collect_read_names(expr: &IRExpr, out: &mut HashSet<String>) {
    let mut expr = expr.clone();
    fn walk(e: &mut IRExpr, out: &mut HashSet<String>) {
        if let IRExpr::Variable(n) | IRExpr::Param(n) = e {
            out.insert(n.clone());
        }
        for_each_child(e, &mut |c| walk(c, out));
    }
    walk(&mut expr, out);
}

/// Every name a statement reads and writes, nested bodies included.
fn collect_stmt_names(
    stmt: &IRStatement,
    reads: &mut HashSet<String>,
    writes: &mut HashSet<String>,
) {
    let mut stmt = stmt.clone();
    let mut exprs: Vec<IRExpr> = Vec::new();
    match &mut stmt {
        IRStatement::Assign { target, value, .. } => {
            writes.insert(target.clone());
            exprs.push(value.clone());
        }
        IRStatement::AugAssign { target, value, .. } => {
            reads.insert(target.clone());
            writes.insert(target.clone());
            exprs.push(value.clone());
        }
        IRStatement::TupleUnpack { targets, value, .. } => {
            writes.extend(targets.iter().cloned());
            exprs.push(value.clone());
        }
        IRStatement::For {
            target, iterable, ..
        } => {
            writes.insert(target.clone());
            exprs.push(iterable.clone());
        }
        IRStatement::Return(Some(e))
        | IRStatement::Expression(e)
        | IRStatement::Yield { value: Some(e) }
        | IRStatement::Raise { exception: Some(e) }
        | IRStatement::If { condition: e, .. }
        | IRStatement::While { condition: e, .. } => exprs.push(e.clone()),
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => {
            exprs.push(object.clone());
            exprs.push(value.clone());
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            exprs.extend([container.clone(), index.clone(), value.clone()]);
        }
        _ => {}
    }
    for e in &exprs {
        collect_read_names(e, reads);
    }
    for sub in sub_bodies(&stmt) {
        for inner in &sub.statements {
            collect_stmt_names(inner, reads, writes);
        }
    }
}

/// Refuse a generator whose unpacking or loop target, a WASM local of
/// `__step`, is read in a different resumption from the one that set it: the
/// local does not survive the `yield` between them.
fn check_locals_across_yields(
    blocks: &[Vec<IRStatement>],
    exempt: &HashSet<String>,
    func_name: &str,
) -> Result<()> {
    for block in blocks {
        let mut reads = HashSet::new();
        let mut writes = HashSet::new();
        for stmt in block {
            collect_stmt_names(stmt, &mut reads, &mut writes);
        }
        if let Some(name) = reads
            .iter()
            .filter(|n| exempt.contains(*n) && !writes.contains(*n))
            .min()
        {
            return Err(crate::core::errors::unsupported_feature(
                format!(
                    "generator '{func_name}' reads '{name}' after a 'yield', but '{name}' is \
                     bound by a loop or by unpacking, which does not survive the 'yield'. Hint: \
                     copy it into an ordinary variable first ('x = {name}')"
                ),
                None,
            )
            .into());
        }
    }
    Ok(())
}

/// Names that must stay WASM locals: yield-free `for` targets (the loop
/// machinery binds a local) and tuple-unpacking targets.
fn collect_exempt_names(body: &IRBody, out: &mut HashSet<String>) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::For { target, .. } => {
                out.insert(target.clone());
            }
            IRStatement::TupleUnpack { targets, .. } => {
                out.extend(targets.iter().cloned());
            }
            _ => {}
        }
        for sub in sub_bodies(stmt) {
            collect_exempt_names(sub, out);
        }
    }
}

fn sub_bodies(stmt: &IRStatement) -> Vec<&IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => std::iter::once(then_body.as_ref())
            .chain(else_body.as_deref())
            .collect(),
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body],
        IRStatement::For {
            body, else_body, ..
        } => std::iter::once(body.as_ref())
            .chain(else_body.as_deref())
            .collect(),
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => std::iter::once(try_body.as_ref())
            .chain(except_handlers.iter().map(|h| &h.body))
            .chain(finally_body.as_deref())
            .collect(),
        _ => Vec::new(),
    }
}

/// Rewrite every reference to a lifted name into a `self.<name>` field access
/// and every assignment to one into a field store.
fn lift_body(body: &mut IRBody, lift: &HashSet<String>) {
    for stmt in &mut body.statements {
        lift_stmt(stmt, lift);
    }
}

fn lift_stmt(stmt: &mut IRStatement, lift: &HashSet<String>) {
    match stmt {
        IRStatement::Assign {
            target,
            value,
            var_type: _,
        } if lift.contains(target) => {
            lift_expr(value, lift);
            let replacement = IRStatement::AttributeAssign {
                object: self_var(),
                attribute: target.clone(),
                value: value.clone(),
                annotation: None,
            };
            *stmt = replacement;
        }
        IRStatement::AugAssign { target, value, op } if lift.contains(target) => {
            lift_expr(value, lift);
            let replacement = IRStatement::AttributeAugAssign {
                object: self_var(),
                attribute: target.clone(),
                value: value.clone(),
                op: op.clone(),
            };
            *stmt = replacement;
        }
        IRStatement::Return(Some(e))
        | IRStatement::Yield { value: Some(e) }
        | IRStatement::Expression(e)
        | IRStatement::Assign { value: e, .. }
        | IRStatement::AugAssign { value: e, .. }
        | IRStatement::TupleUnpack { value: e, .. }
        | IRStatement::DynamicImport { module_name: e, .. }
        | IRStatement::Raise { exception: Some(e) } => lift_expr(e, lift),
        IRStatement::If {
            condition,
            then_body,
            else_body,
        } => {
            lift_expr(condition, lift);
            lift_body(then_body, lift);
            if let Some(else_body) = else_body {
                lift_body(else_body, lift);
            }
        }
        IRStatement::While { condition, body } => {
            lift_expr(condition, lift);
            lift_body(body, lift);
        }
        IRStatement::For {
            target,
            iterable,
            body,
            else_body,
        } => {
            lift_expr(iterable, lift);
            // The loop target shadows any same-named field within the loop.
            let mut scoped = lift.clone();
            scoped.remove(target);
            lift_body(body, &scoped);
            if let Some(else_body) = else_body {
                lift_body(else_body, &scoped);
            }
        }
        IRStatement::With {
            context_expr, body, ..
        } => {
            lift_expr(context_expr, lift);
            lift_body(body, lift);
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            lift_body(try_body, lift);
            for handler in except_handlers {
                lift_body(&mut handler.body, lift);
            }
            if let Some(finally_body) = finally_body {
                lift_body(finally_body, lift);
            }
        }
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => {
            lift_expr(object, lift);
            lift_expr(value, lift);
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            lift_expr(container, lift);
            lift_expr(index, lift);
            lift_expr(value, lift);
        }
        _ => {}
    }
}

fn lift_expr(expr: &mut IRExpr, lift: &HashSet<String>) {
    match expr {
        IRExpr::Variable(name) | IRExpr::Param(name) if lift.contains(name) => {
            *expr = self_attr(name);
        }
        _ => for_each_child(expr, &mut |child| lift_expr(child, lift)),
    }
}

/// Element type the generator yields: f64 when any yield expression involves
/// a float constant or a float-typed parameter, i32 otherwise (the general
/// slot type). Best-effort — deeper float inference (e.g. through plain
/// locals) is a follow-up.
fn yielded_type(body: &IRBody, params: &[IRParam]) -> IRType {
    let mut float_params: HashSet<String> = params
        .iter()
        .filter(|p| p.param_type == IRType::Float)
        .map(|p| p.name.clone())
        .collect();
    // A local assigned a float value is a float too (`x = i / 4; yield x`),
    // which may in turn make another one, so this runs to a fixpoint.
    loop {
        let mut found = Vec::new();
        for stmt in &body.statements {
            any_sub_body(stmt, &mut |s: &IRStatement| {
                if let IRStatement::Assign { target, value, .. } = s {
                    if !float_params.contains(target.as_str())
                        && expr_has_float(value, &float_params)
                    {
                        found.push(target.clone());
                    }
                }
                false
            });
        }
        if found.is_empty() {
            break;
        }
        float_params.extend(found);
    }
    fn expr_has_float(expr: &IRExpr, float_params: &HashSet<String>) -> bool {
        match expr {
            IRExpr::Const(IRConstant::Float(_)) => true,
            IRExpr::Variable(name) | IRExpr::Param(name) => float_params.contains(name),
            // True division is a float whatever its operands are; `yield i / 2`
            // was inferred as an int and truncated every value.
            IRExpr::BinaryOp { op: IROp::Div, .. } => true,
            IRExpr::BinaryOp { left, right, .. } => {
                expr_has_float(left, float_params) || expr_has_float(right, float_params)
            }
            IRExpr::UnaryOp { operand, .. } => expr_has_float(operand, float_params),
            IRExpr::Attribute { object, .. } => expr_has_float(object, float_params),
            IRExpr::FunctionCall { function_name, .. } if function_name == "float" => true,
            _ => false,
        }
    }
    let mut float = false;
    for stmt in &body.statements {
        let mut check = |s: &IRStatement| {
            if let IRStatement::Yield { value: Some(value) } = s {
                if expr_has_float(value, &float_params) {
                    float = true;
                }
            }
            false
        };
        any_sub_body(stmt, &mut check);
    }
    if float {
        IRType::Float
    } else {
        IRType::Int
    }
}

/// Flattens a generator body into basic blocks for the `__gen_pc` trampoline.
/// Statements that neither suspend nor transfer control out of a flattened
/// frame are kept whole (nested control flow included); everything else is
/// split at the suspension/branch points.
struct Flattener {
    blocks: Vec<Vec<IRStatement>>,
    current: usize,
    /// Innermost-last (head, exit) block ids of flattened loops, targeted by
    /// flattened `continue`/`break`.
    loops: Vec<(i32, i32)>,
}

impl Flattener {
    fn new_block(&mut self) -> i32 {
        self.blocks.push(Vec::new());
        (self.blocks.len() - 1) as i32
    }

    fn push(&mut self, stmt: IRStatement) {
        self.blocks[self.current].push(stmt);
    }

    /// Store the next block id and re-enter the trampoline.
    fn jump(&mut self, block: i32) {
        self.push(set_pc(block));
        self.push(IRStatement::Continue);
    }

    /// Continue emitting into a fresh block that nothing jumps to; used after
    /// an unconditional transfer so trailing unreachable statements stay
    /// syntactically valid without executing.
    fn dead_block(&mut self) {
        let dead = self.new_block();
        self.current = dead as usize;
    }

    fn flatten_body(&mut self, stmts: Vec<IRStatement>, func_name: &str) -> Result<()> {
        for stmt in stmts {
            self.flatten_stmt(stmt, func_name)?;
        }
        Ok(())
    }

    fn flatten_stmt(&mut self, stmt: IRStatement, func_name: &str) -> Result<()> {
        if !needs_flatten(&stmt) {
            self.push(stmt);
            return Ok(());
        }
        match stmt {
            IRStatement::Yield { value } => {
                let resume = self.new_block();
                self.push(set_pc(resume));
                self.push(IRStatement::Return(Some(
                    value.unwrap_or_else(|| int_const(0)),
                )));
                self.current = resume as usize;
            }
            // `return` in a generator ends iteration (the value is ignored in
            // this subset).
            IRStatement::Return(_) => {
                self.push(set_pc(EXHAUSTED));
                self.push(raise_stop_iteration());
                self.dead_block();
            }
            IRStatement::Break => {
                let (_, exit) = *self
                    .loops
                    .last()
                    .ok_or_else(|| anyhow!("generator '{func_name}': break outside a loop"))?;
                self.jump(exit);
                self.dead_block();
            }
            IRStatement::Continue => {
                let (head, _) = *self
                    .loops
                    .last()
                    .ok_or_else(|| anyhow!("generator '{func_name}': continue outside a loop"))?;
                self.jump(head);
                self.dead_block();
            }
            IRStatement::While { condition, body } => {
                let head = self.new_block();
                let exit = self.new_block();
                self.jump(head);
                self.current = head as usize;
                self.push(IRStatement::If {
                    condition: IRExpr::UnaryOp {
                        operand: Box::new(condition),
                        op: IRUnaryOp::Not,
                    },
                    then_body: body_of(vec![set_pc(exit), IRStatement::Continue]),
                    else_body: None,
                });
                self.loops.push((head, exit));
                self.flatten_body(body.statements, func_name)?;
                self.loops.pop();
                self.jump(head);
                self.current = exit as usize;
            }
            IRStatement::If {
                condition,
                then_body,
                else_body,
            } => {
                let then_block = self.new_block();
                let join = self.new_block();
                let else_target = match &else_body {
                    Some(_) => self.new_block(),
                    None => join,
                };
                self.push(IRStatement::If {
                    condition,
                    then_body: body_of(vec![set_pc(then_block), IRStatement::Continue]),
                    else_body: Some(body_of(vec![set_pc(else_target), IRStatement::Continue])),
                });
                self.current = then_block as usize;
                self.flatten_body(then_body.statements, func_name)?;
                self.jump(join);
                if let Some(else_body) = else_body {
                    self.current = else_target as usize;
                    self.flatten_body(else_body.statements, func_name)?;
                    self.jump(join);
                }
                self.current = join as usize;
            }
            other => {
                return Err(anyhow!(
                    "generator '{func_name}': unsupported control flow around yield: {other:?}"
                ));
            }
        }
        Ok(())
    }
}

/// Must this statement be split into trampoline blocks? True when it
/// suspends, leaves the generator, or transfers control out of a frame the
/// flattener owns. `break`/`continue` enclosed in a nested yield-free loop
/// stay opaque with that loop; at flattening level they belong to a
/// flattened loop.
fn needs_flatten(stmt: &IRStatement) -> bool {
    match stmt {
        IRStatement::Yield { .. }
        | IRStatement::Return(_)
        | IRStatement::Break
        | IRStatement::Continue => true,
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            then_body.statements.iter().any(needs_flatten)
                || else_body
                    .as_ref()
                    .is_some_and(|b| b.statements.iter().any(needs_flatten))
        }
        IRStatement::While { body, .. } | IRStatement::For { body, .. } => body_suspends(body),
        _ => false,
    }
}
