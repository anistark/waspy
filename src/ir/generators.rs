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

/// The zip sequence a statement checks the length of, when it is one round's
/// `if counter >= len(seq): break` (see the zip lowering in `ir::converter`).
fn zip_length_check(stmt: &IRStatement) -> Option<&str> {
    let IRStatement::If {
        condition:
            IRExpr::CompareOp {
                right,
                op: IRCompareOp::GtE,
                ..
            },
        then_body,
        else_body: None,
    } = stmt
    else {
        return None;
    };
    if !matches!(then_body.statements.as_slice(), [IRStatement::Break]) {
        return None;
    }
    match &**right {
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } if function_name == "len" => match arguments.as_slice() {
            [IRExpr::Variable(seq)] if seq.starts_with(crate::ir::ZIP_SEQ_PREFIX) => Some(seq),
            _ => None,
        },
        _ => None,
    }
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
        cause: None,
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
pub(crate) fn body_has_yield(body: &IRBody) -> bool {
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
    let gen_methods = lift_generator_methods(module)?;
    let class_bases: HashMap<String, Vec<String>> = module
        .classes
        .iter()
        .map(|c| (c.name.clone(), c.bases.clone()))
        .collect();

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
    module.memory_layout.add_string(IGNORED_GENERATOR_EXIT);

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
            gen_methods: &gen_methods,
            class_bases: &class_bases,
            instances: HashMap::new(),
        };
        for func in &mut module.functions {
            desugarer.current = func.name.clone();
            desugarer.instances = certain_instances(&func.params, &func.body, None, &class_bases);
            let mut vars = desugarer.seed_params(&func.params);
            desugarer.rewrite_body(&mut func.body, &mut vars);
        }
        for class in &mut module.classes {
            for method in &mut class.methods {
                desugarer.current = format!("{}.{}", class.name, method.name);
                desugarer.instances = certain_instances(
                    &method.params,
                    &method.body,
                    Some(&class.name),
                    &class_bases,
                );
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
            keyword_only: false,
            star: crate::ir::ParamStar::None,
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
    /// `(class, method)` of each generator method, to its generator function.
    gen_methods: &'a HashMap<(String, String), String>,
    class_bases: &'a HashMap<String, Vec<String>>,
    /// The locals of the function being rewritten whose class is certain.
    instances: HashMap<String, String>,
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
            // A generator method called on a receiver whose class is certain.
            IRExpr::MethodCall {
                object,
                method_name,
                ..
            } => {
                let receiver = match &**object {
                    IRExpr::Variable(n) | IRExpr::Param(n) => self.instances.get(n)?.clone(),
                    IRExpr::FunctionCall { function_name, .. }
                        if self.class_bases.contains_key(function_name) =>
                    {
                        function_name.clone()
                    }
                    _ => return None,
                };
                let function = self.method_generator(&receiver, method_name)?;
                self.gen_fns.get(&function).cloned()
            }
            _ => None,
        }
    }

    /// The generator function behind `class.method`, found on the class or
    /// a base. A generator method is defined by one class of its hierarchy
    /// only (`lift_generator_methods`), so the first found is the one.
    fn method_generator(&self, class: &str, method: &str) -> Option<String> {
        let mut pending = vec![class.to_string()];
        let mut seen = HashSet::new();
        while let Some(c) = pending.pop() {
            if !seen.insert(c.clone()) {
                continue;
            }
            if let Some(f) = self.gen_methods.get(&(c.clone(), method.to_string())) {
                return Some(f.clone());
            }
            if let Some(bases) = self.class_bases.get(&c) {
                pending.extend(bases.iter().rev().cloned());
            }
        }
        None
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
            // One round of a `for ... in zip(...)` whose later argument is an
            // iterator: its length check goes, and its indexing becomes a
            // pull, ending the loop when the iterator is exhausted.
            if let Some(seq) = zip_length_check(&stmt) {
                if vars.contains_key(seq) {
                    continue;
                }
            }
            if let IRStatement::Assign {
                target,
                value: IRExpr::Indexing { container, .. },
                ..
            } = &stmt
            {
                if let IRExpr::Variable(seq) = &**container {
                    if seq.starts_with(crate::ir::ZIP_SEQ_PREFIX) {
                        if let Some(class) = vars.get(seq) {
                            out.push(IRStatement::Expression(stop_check()));
                            out.push(IRStatement::Assign {
                                target: target.clone(),
                                value: IRExpr::FunctionCall {
                                    function_name: format!("{class}::__next__"),
                                    arguments: vec![IRExpr::Variable(seq.clone())],
                                },
                                var_type: None,
                            });
                            out.push(IRStatement::If {
                                condition: stop_check(),
                                then_body: body_of(vec![IRStatement::Break]),
                                else_body: None,
                            });
                            continue;
                        }
                    }
                }
            }
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
                        // A later zip() argument that is an iterator is pulled
                        // one value per round (above), as zip pulls it; the
                        // iterator `__iter__` gives is what is pulled.
                        Some(class) if target.starts_with(crate::ir::ZIP_SEQ_PREFIX) => {
                            let (it_expr, it_class) = self.iteration_source(value, class);
                            if !self.facts.get(&it_class).is_some_and(|f| f.has_next) {
                                self.errors.push(format!(
                                    "'{}' passes zip() an iterable whose __iter__ returns no \
                                     iterator this module defines",
                                    self.current
                                ));
                            }
                            vars.insert(target.clone(), it_class);
                            out.push(IRStatement::Assign {
                                target,
                                value: it_expr,
                                var_type,
                            });
                            continue;
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
            | IRStatement::Raise {
                exception: Some(e), ..
            }
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
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => {
            f(condition);
            f(body);
            f(orelse);
        }
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
    let mut body = std::mem::replace(
        &mut func.body,
        IRBody {
            statements: Vec::new(),
        },
    );
    desugar_suspending_fors(&mut body, counter);
    split_literal_unpacks(&mut body, counter);
    unique_handler_names(&mut body, counter);

    // Resolved before the lift below rewrites parameter references into
    // `self.<name>` attribute reads.
    let elem_type = yielded_type(&body, &func.params);

    // Every parameter and assigned local moves into a state-class field so it
    // survives suspension; the targets of tuple unpacking and of yield-free
    // `for` loops stay WASM locals (their loop machinery binds locals). One
    // of those read after a `yield` is bound through a temporary instead and
    // copied into an ordinary variable, which is a field.
    let flatten = |body: IRBody| -> Result<(Flattener, BTreeSet<String>)> {
        let mut body = body;
        let mut lift: HashSet<String> = func.params.iter().map(|p| p.name.clone()).collect();
        lift.insert(GEN_SENT_FIELD.to_string());
        collect_assigned_names(&body, &mut lift);
        collect_handler_names(&body, &mut lift);
        let mut exempt = HashSet::new();
        collect_exempt_names(&body, &mut exempt);
        for name in &exempt {
            lift.remove(name);
        }
        lift_body(&mut body, &lift);
        let mut flattener = Flattener::new();
        flattener.flatten_body(body.statements, &func.name)?;
        flattener.jump(EXHAUSTED);
        let stale = locals_read_across_yields(&flattener.blocks, &exempt);
        Ok((flattener, stale))
    };
    let (mut flattener, stale) = flatten(body.clone())?;
    if !stale.is_empty() {
        bind_through_temporaries(&mut body, &stale, counter);
        let (retried, still) = flatten(body)?;
        if let Some(name) = still.iter().next() {
            return Err(crate::core::errors::unsupported_feature(
                format!(
                    "generator '{}' reads '{name}' after a 'yield', but '{name}' is bound by a \
                     loop or by unpacking, which does not survive the 'yield'. Hint: copy it \
                     into an ordinary variable first ('x = {name}')",
                    func.name
                ),
                None,
            )
            .into());
        }
        flattener = retried;
    }
    let throw_points = std::mem::take(&mut flattener.throw_points);

    // __step: `while True` trampoline dispatching on the stored block id.
    // Every block ends in a `return` (yield) or a `continue` after storing
    // the next id, so the sequential `if`s never fall through into each
    // other; an unmatched id (-1 included) raises StopIteration.
    let blocks = std::mem::take(&mut flattener.blocks);
    let mut dispatch: Vec<IRStatement> = blocks
        .into_iter()
        .enumerate()
        .map(|(id, statements)| IRStatement::If {
            condition: IRExpr::CompareOp {
                left: Box::new(self_attr(GEN_PC_FIELD)),
                right: Box::new(int_const(id as i32)),
                op: IRCompareOp::Eq,
            },
            then_body: body_of(flattener.wrap_block(id, statements)),
            else_body: None,
        })
        .collect();
    dispatch.push(set_pc(EXHAUSTED));
    dispatch.push(raise_stop_iteration());

    let self_param = || IRParam {
        name: "self".to_string(),
        param_type: IRType::Class(class_name.to_string()),
        default_value: None,
        keyword_only: false,
        star: crate::ir::ParamStar::None,
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
        IRType::String => IRExpr::Const(IRConstant::String(String::new())),
        _ => int_const(0),
    };

    let mut init_statements = vec![
        set_pc(0),
        set_sent(sent_zero()),
        set_field(GEN_THROW_FIELD, int_const(0)),
    ];
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
                    keyword_only: false,
                    star: crate::ir::ParamStar::None,
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
            close_body(class_name, &throw_points),
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
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    })
}

/// The prefix of the generator functions generator methods are lifted to.
pub const GEN_METHOD_PREFIX: &str = "__genmethod_";

/// A method containing `yield` becomes a generator function taking the
/// receiver as its first argument, so the generator it makes holds the
/// receiver like any parameter, and the method returns that generator.
/// Iteration finds the generator's class from the receiver's, statically, so
/// a generator method another class of its hierarchy also defines (which
/// would make the answer depend on the instance) is refused, as are the
/// static, class, and property kinds and `super()` inside one.
fn lift_generator_methods(module: &mut IRModule) -> Result<HashMap<(String, String), String>> {
    let related = |a: &str, b: &str, classes: &[IRClass]| {
        let ancestors = |start: &str| {
            let mut out = HashSet::new();
            let mut pending = vec![start.to_string()];
            while let Some(c) = pending.pop() {
                if out.insert(c.clone()) {
                    if let Some(class) = classes.iter().find(|k| k.name == c) {
                        pending.extend(class.bases.iter().cloned());
                    }
                }
            }
            out
        };
        ancestors(a).contains(b) || ancestors(b).contains(a)
    };
    let mut lifted = HashMap::new();
    let mut functions = Vec::new();
    for c in 0..module.classes.len() {
        let class_name = module.classes[c].name.clone();
        for m in 0..module.classes[c].methods.len() {
            if !body_has_yield(&module.classes[c].methods[m].body) {
                continue;
            }
            let method = module.classes[c].methods[m].clone();
            let refuse = |why: &str| -> anyhow::Error {
                crate::core::errors::unsupported_feature(
                    format!("the generator method '{class_name}.{}' {why}", method.name),
                    None,
                )
                .into()
            };
            if !method.decorators.is_empty() {
                return Err(refuse(
                    "has a decorator, which a generator method cannot have here",
                ));
            }
            if let Some(other) = module.classes.iter().find(|k| {
                k.name != class_name
                    && k.methods.iter().any(|x| x.name == method.name)
                    && related(&k.name, &class_name, &module.classes)
            }) {
                return Err(refuse(&format!(
                    "is also defined by '{}' in the same hierarchy, so which generator a call \
                     makes would depend on the instance",
                    other.name
                )));
            }
            let mut uses_super = false;
            for stmt in &method.body.statements {
                any_sub_body(stmt, &mut |s| {
                    let mut probe = IRBody {
                        statements: vec![s.clone()],
                    };
                    let _ = crate::ir::visit_body(&mut probe, &mut |e: &mut IRExpr| {
                        if matches!(e, IRExpr::FunctionCall { function_name, .. } if function_name == "super")
                            || matches!(e, IRExpr::MethodCall { object, .. } if matches!(&**object, IRExpr::FunctionCall { function_name, .. } if function_name == "super"))
                        {
                            uses_super = true;
                        }
                        Ok(())
                    });
                    false
                });
            }
            if uses_super {
                return Err(refuse(
                    "calls super(), which is not supported in a generator method",
                ));
            }
            let Some(receiver) = method.params.first().map(|p| p.name.clone()) else {
                return Err(refuse("takes no parameters, so it has no instance"));
            };
            let function = format!("{GEN_METHOD_PREFIX}{class_name}_{}", method.name);
            let renamed = "__gen_self".to_string();
            let mut body = method.body.clone();
            crate::ir::rename_body(
                &mut body,
                &HashMap::from([(receiver.clone(), renamed.clone())]),
            );
            let mut params = method.params.clone();
            params[0].name = renamed;
            params[0].param_type = IRType::Class(class_name.clone());
            let arguments = method
                .params
                .iter()
                .map(|p| IRExpr::Variable(p.name.clone()))
                .collect();
            functions.push(IRFunction {
                name: function.clone(),
                params,
                body,
                return_type: IRType::Unknown,
                decorators: Vec::new(),
            });
            let target = &mut module.classes[c].methods[m];
            target.body = IRBody {
                statements: vec![IRStatement::Return(Some(IRExpr::FunctionCall {
                    function_name: function.clone(),
                    arguments,
                }))],
            };
            target.return_type = IRType::Class(format!("__Gen_{function}"));
            lifted.insert((class_name.clone(), method.name.clone()), function);
        }
    }
    module.functions.extend(functions);
    Ok(lifted)
}

/// The locals whose class is certain throughout a body: a parameter
/// annotated with a class (the receiver of a method, `owner`'s), or a
/// variable every assignment of which constructs the same class.
fn certain_instances(
    params: &[IRParam],
    body: &IRBody,
    owner: Option<&str>,
    classes: &HashMap<String, Vec<String>>,
) -> HashMap<String, String> {
    let mut seen: HashMap<String, Option<String>> = HashMap::new();
    for (k, p) in params.iter().enumerate() {
        match (&p.param_type, owner) {
            (IRType::Class(c), _) => {
                seen.insert(p.name.clone(), Some(c.clone()));
            }
            (_, Some(owner)) if k == 0 => {
                seen.insert(p.name.clone(), Some(owner.to_string()));
            }
            _ => {}
        }
    }
    fn record(seen: &mut HashMap<String, Option<String>>, name: &str, class: Option<String>) {
        let entry = seen
            .entry(name.to_string())
            .or_insert_with(|| class.clone());
        if *entry != class {
            *entry = None;
        }
    }
    fn walk(
        body: &IRBody,
        seen: &mut HashMap<String, Option<String>>,
        classes: &HashMap<String, Vec<String>>,
    ) {
        for stmt in &body.statements {
            match stmt {
                IRStatement::Assign { target, value, .. } => {
                    let class = match value {
                        IRExpr::FunctionCall { function_name, .. }
                            if classes.contains_key(function_name) =>
                        {
                            Some(function_name.clone())
                        }
                        _ => None,
                    };
                    record(seen, target, class);
                }
                IRStatement::AugAssign { target, .. } | IRStatement::For { target, .. } => {
                    record(seen, target, None)
                }
                IRStatement::TupleUnpack { targets, .. } => {
                    for t in targets {
                        record(seen, t, None);
                    }
                }
                IRStatement::With {
                    optional_vars: Some(v),
                    ..
                } => record(seen, v, None),
                IRStatement::TryExcept {
                    except_handlers, ..
                } => {
                    for h in except_handlers {
                        if let Some(n) = &h.name {
                            record(seen, n, None);
                        }
                    }
                }
                _ => {}
            }
            for sub in sub_bodies(stmt) {
                walk(sub, seen, classes);
            }
        }
    }
    walk(body, &mut seen, classes);
    seen.into_iter()
        .filter_map(|(name, class)| class.map(|c| (name, c)))
        .collect()
}

/// `close()`: suspended at a `yield` inside a `try`, the generator resumes
/// with GeneratorExit raised there, so its handlers and `finally` blocks run;
/// it then has to stop, by letting GeneratorExit (or StopIteration) out, and
/// yielding instead is CPython's RuntimeError. Anywhere else nothing would
/// run, so it is only marked exhausted.
fn close_body(class_name: &str, throw_points: &[i32]) -> Vec<IRStatement> {
    let finish = vec![set_pc(EXHAUSTED), IRStatement::Return(Some(int_const(0)))];
    let Some(suspended) = throw_points
        .iter()
        .map(|&block| IRExpr::CompareOp {
            left: Box::new(self_attr(GEN_PC_FIELD)),
            right: Box::new(int_const(block)),
            op: IRCompareOp::Eq,
        })
        .reduce(|left, right| IRExpr::BoolOp {
            left: Box::new(left),
            right: Box::new(right),
            op: IRBoolOp::Or,
        })
    else {
        return finish;
    };
    let mut statements = vec![IRStatement::If {
        condition: suspended,
        then_body: body_of(vec![
            set_field(GEN_THROW_FIELD, int_const(1)),
            IRStatement::Expression(stop_check()),
            IRStatement::TryExcept {
                try_body: body_of(vec![IRStatement::Expression(IRExpr::FunctionCall {
                    function_name: format!("{class_name}::__step"),
                    arguments: vec![self_var()],
                })]),
                except_handlers: vec![crate::ir::IRExceptHandler {
                    exception_types: vec!["GeneratorExit".to_string()],
                    name: None,
                    body: IRBody {
                        statements: finish.clone(),
                    },
                }],
                finally_body: None,
            },
            IRStatement::If {
                condition: stop_check(),
                then_body: body_of(finish.clone()),
                else_body: None,
            },
            IRStatement::Raise {
                exception: Some(IRExpr::FunctionCall {
                    function_name: "RuntimeError".to_string(),
                    arguments: vec![IRExpr::Const(IRConstant::String(
                        IGNORED_GENERATOR_EXIT.to_string(),
                    ))],
                }),
                cause: None,
            },
        ]),
        else_body: None,
    }];
    statements.extend(finish);
    statements
}

/// CPython's message when a generator yields from `close()`.
pub const IGNORED_GENERATOR_EXIT: &str = "generator ignored GeneratorExit";

/// Give each handler's `as` name a name of its own. The names become fields,
/// typed once per class, so two handlers both binding `e` to different
/// exception classes would share a field of their common base and lose what
/// each class adds; Python unbinds the name at the handler's end, so nothing
/// outside the handler reads it.
fn unique_handler_names(body: &mut IRBody, counter: &mut u32) {
    for stmt in &mut body.statements {
        match stmt {
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                unique_handler_names(try_body, counter);
                for handler in except_handlers {
                    unique_handler_names(&mut handler.body, counter);
                    if let Some(name) = handler.name.clone() {
                        *counter += 1;
                        let unique = format!("{name}__exc{counter}");
                        let map = HashMap::from([(name, unique.clone())]);
                        crate::ir::rename_body(&mut handler.body, &map);
                        handler.name = Some(unique);
                    }
                }
                if let Some(finally_body) = finally_body {
                    unique_handler_names(finally_body, counter);
                }
            }
            other => {
                for sub in sub_bodies_mut(other) {
                    unique_handler_names(sub, counter);
                }
            }
        }
    }
}

/// The `as` names of every handler in the body.
fn collect_handler_names(body: &IRBody, out: &mut HashSet<String>) {
    for stmt in &body.statements {
        if let IRStatement::TryExcept {
            except_handlers, ..
        } = stmt
        {
            out.extend(except_handlers.iter().filter_map(|h| h.name.clone()));
        }
        for sub in sub_bodies(stmt) {
            collect_handler_names(sub, out);
        }
    }
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
        | IRStatement::Raise {
            exception: Some(e), ..
        }
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

/// The unpacking and loop targets (WASM locals of `__step`) read in a
/// different resumption from the one that set them: the local does not
/// survive the `yield` between them.
fn locals_read_across_yields(
    blocks: &[Vec<IRStatement>],
    exempt: &HashSet<String>,
) -> BTreeSet<String> {
    let mut stale = BTreeSet::new();
    for block in blocks {
        let mut reads = HashSet::new();
        let mut writes = HashSet::new();
        for stmt in block {
            collect_stmt_names(stmt, &mut reads, &mut writes);
        }
        stale.extend(
            reads
                .into_iter()
                .filter(|n| exempt.contains(n) && !writes.contains(n)),
        );
    }
    stale
}

/// Bind each of `names` where a loop or an unpacking binds it through a
/// temporary, copied into the name right after, so the name is an ordinary
/// assigned variable (a field) and the temporary is the loop's local.
fn bind_through_temporaries(body: &mut IRBody, names: &BTreeSet<String>, counter: &mut u32) {
    let statements = std::mem::take(&mut body.statements);
    for mut stmt in statements {
        for sub in sub_bodies_mut(&mut stmt) {
            bind_through_temporaries(sub, names, counter);
        }
        let mut after = Vec::new();
        match &mut stmt {
            IRStatement::For { target, body, .. } if names.contains(target) => {
                *counter += 1;
                let temp = format!("__glt_{counter}");
                let name = std::mem::replace(target, temp.clone());
                body.statements
                    .insert(0, assign(&name, IRExpr::Variable(temp)));
            }
            IRStatement::TupleUnpack { targets, .. } => {
                for target in targets.iter_mut().filter(|t| names.contains(*t)) {
                    *counter += 1;
                    let temp = format!("__glt_{counter}");
                    let name = std::mem::replace(target, temp.clone());
                    after.push(assign(&name, IRExpr::Variable(temp)));
                }
            }
            _ => {}
        }
        body.statements.push(stmt);
        body.statements.append(&mut after);
    }
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
            var_type,
        } if lift.contains(target) => {
            lift_expr(value, lift);
            let replacement = IRStatement::AttributeAssign {
                object: self_var(),
                attribute: target.clone(),
                value: value.clone(),
                annotation: var_type.clone(),
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
        | IRStatement::Raise {
            exception: Some(e), ..
        } => lift_expr(e, lift),
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
                // The handler binds a local; the body reads the field.
                if let Some(name) = handler.name.as_ref().filter(|n| lift.contains(*n)) {
                    handler.body.statements.insert(
                        0,
                        IRStatement::AttributeAssign {
                            object: self_var(),
                            attribute: name.clone(),
                            value: IRExpr::Variable(name.clone()),
                            annotation: None,
                        },
                    );
                }
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
/// a float constant or a float-typed parameter, str when one is text, i32
/// otherwise (the general slot type). Best-effort, through plain locals; a
/// generator yielding both text and numbers is refused where its value is
/// returned, since one slot cannot hold both.
fn yielded_type(body: &IRBody, params: &[IRParam]) -> IRType {
    #[derive(Clone, Copy, PartialEq)]
    enum Kind {
        Float,
        Text,
    }
    let mut kinds: HashMap<String, Kind> = params
        .iter()
        .filter_map(|p| match p.param_type {
            IRType::Float => Some((p.name.clone(), Kind::Float)),
            IRType::String => Some((p.name.clone(), Kind::Text)),
            _ => None,
        })
        .collect();
    fn kind_of(expr: &IRExpr, kinds: &HashMap<String, Kind>) -> Option<Kind> {
        match expr {
            IRExpr::Const(IRConstant::Float(_)) => Some(Kind::Float),
            IRExpr::Const(IRConstant::String(_)) => Some(Kind::Text),
            IRExpr::Variable(name) | IRExpr::Param(name) => kinds.get(name).copied(),
            // True division is a float whatever its operands are; `yield i / 2`
            // was inferred as an int and truncated every value.
            IRExpr::BinaryOp { op: IROp::Div, .. } => Some(Kind::Float),
            IRExpr::BinaryOp { left, right, .. } => {
                let (l, r) = (kind_of(left, kinds), kind_of(right, kinds));
                if l == Some(Kind::Text) || r == Some(Kind::Text) {
                    Some(Kind::Text)
                } else {
                    l.or(r)
                }
            }
            IRExpr::UnaryOp { operand, .. } => kind_of(operand, kinds),
            IRExpr::Conditional { body, orelse, .. } => {
                kind_of(body, kinds).or_else(|| kind_of(orelse, kinds))
            }
            IRExpr::Attribute { object, .. } => kind_of(object, kinds),
            IRExpr::FunctionCall { function_name, .. } => match function_name.as_str() {
                "float" => Some(Kind::Float),
                "str" | "repr" | "format" | "ascii" | "chr" | "input" => Some(Kind::Text),
                _ => None,
            },
            IRExpr::MethodCall { method_name, .. } => match method_name.as_str() {
                "upper" | "lower" | "strip" | "lstrip" | "rstrip" | "join" | "replace"
                | "title" | "capitalize" | "format" | "casefold" | "swapcase" | "center"
                | "ljust" | "rjust" | "zfill" => Some(Kind::Text),
                _ => None,
            },
            _ => None,
        }
    }
    // A local assigned a float or a string is one too (`x = i / 4; yield x`),
    // which may in turn make another one, so this runs to a fixpoint.
    loop {
        let mut found = Vec::new();
        for stmt in &body.statements {
            any_sub_body(stmt, &mut |s: &IRStatement| {
                if let IRStatement::Assign { target, value, .. } = s {
                    if !kinds.contains_key(target.as_str()) {
                        if let Some(kind) = kind_of(value, &kinds) {
                            found.push((target.clone(), kind));
                        }
                    }
                }
                false
            });
        }
        if found.is_empty() {
            break;
        }
        kinds.extend(found);
    }
    let mut yielded = None;
    for stmt in &body.statements {
        any_sub_body(stmt, &mut |s: &IRStatement| {
            if let IRStatement::Yield { value: Some(value) } = s {
                yielded = yielded.or(kind_of(value, &kinds));
            }
            false
        });
    }
    match yielded {
        Some(Kind::Float) => IRType::Float,
        Some(Kind::Text) => IRType::String,
        None => IRType::Int,
    }
}

/// Flattens a generator body into basic blocks for the `__gen_pc` trampoline.
/// Statements that neither suspend nor transfer control out of a flattened
/// frame are kept whole (nested control flow included); everything else is
/// split at the suspension/branch points.
///
/// A `try` whose region suspends or jumps is flattened too, which is how a
/// `yield` inside one works: each block records the `try` regions around it,
/// and `wrap_block` puts the block's statements back inside those regions'
/// handlers when the dispatch is built. A handler there only stores the
/// exception and jumps to the region's flattened handler body (or its
/// `finally`). A `finally` is a block of its own entered with a continuation
/// in a field: the block to go on to, [`RERAISE`] after an exception, or
/// [`EXHAUSTED`] after a `return`. Every jump out of a region (`break`,
/// `continue`, `return`) goes through each `finally` it leaves.
struct Flattener {
    blocks: Vec<Vec<IRStatement>>,
    /// Per block, the regions around it, outermost first.
    block_regions: Vec<Vec<usize>>,
    current: usize,
    /// Innermost-last (head, exit, open regions) of flattened loops,
    /// targeted by flattened `continue`/`break`.
    loops: Vec<(i32, i32, usize)>,
    regions: Vec<Region>,
    /// The regions open where the flattener is, outermost first.
    open: Vec<usize>,
    /// Resume points inside a region, where `close()` raises GeneratorExit.
    throw_points: Vec<i32>,
}

/// A flattened `try`'s protected region.
enum Region {
    /// `try ... except`: each handler's types, `as` name, and block.
    Except {
        handlers: Vec<(Vec<String>, Option<String>, i32)>,
    },
    /// `try ... finally`: the `finally` block.
    Finally { entry: i32 },
}

/// Field holding the exception a region caught.
fn region_exc_field(region: usize) -> String {
    format!("__gen_exc_{region}")
}

/// Field holding where a region's `finally` goes on to.
fn region_fin_field(region: usize) -> String {
    format!("__gen_fin_{region}")
}

/// Field `close()` sets to have the resumed generator raise GeneratorExit.
const GEN_THROW_FIELD: &str = "__gen_throw";

/// A `finally` continuation: re-raise the exception its region caught.
const RERAISE: i32 = -2;

fn set_field(name: &str, value: IRExpr) -> IRStatement {
    IRStatement::AttributeAssign {
        object: self_var(),
        attribute: name.to_string(),
        value,
        annotation: None,
    }
}

/// Bare `raise` statements in a handler body, re-raising what `field` holds.
/// A nested handler's body re-raises its own exception, so it is left alone.
fn reraise_from(body: &mut [IRStatement], field: &str) {
    for stmt in body {
        match stmt {
            IRStatement::Raise {
                exception: exception @ None,
                ..
            } => *exception = Some(self_attr(field)),
            IRStatement::TryExcept {
                try_body,
                finally_body,
                ..
            } => {
                reraise_from(&mut try_body.statements, field);
                if let Some(b) = finally_body {
                    reraise_from(&mut b.statements, field);
                }
            }
            other => {
                for sub in sub_bodies_mut(other) {
                    reraise_from(&mut sub.statements, field);
                }
            }
        }
    }
}

impl Flattener {
    fn new() -> Self {
        Flattener {
            blocks: vec![Vec::new()],
            block_regions: vec![Vec::new()],
            current: 0,
            loops: Vec::new(),
            regions: Vec::new(),
            open: Vec::new(),
            throw_points: Vec::new(),
        }
    }

    fn new_block(&mut self) -> i32 {
        self.blocks.push(Vec::new());
        self.block_regions.push(self.open.clone());
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

    /// Jump to `target`, which lies outside every region past the first
    /// `depth`, through the `finally` of each region left on the way.
    fn route(&mut self, target: i32, depth: usize) {
        let mut destination = target;
        let left: Vec<usize> = self.open[depth..].to_vec();
        for region in left {
            if let Region::Finally { entry } = self.regions[region] {
                self.push(set_field(&region_fin_field(region), int_const(destination)));
                destination = entry;
            }
        }
        self.jump(destination);
    }

    /// Continue emitting into a fresh block that nothing jumps to; used after
    /// an unconditional transfer so trailing unreachable statements stay
    /// syntactically valid without executing.
    fn dead_block(&mut self) {
        let dead = self.new_block();
        self.current = dead as usize;
    }

    /// Block `id`'s statements inside the handlers of the regions around it,
    /// innermost first, each handler storing what it caught and jumping.
    fn wrap_block(&self, id: usize, statements: Vec<IRStatement>) -> Vec<IRStatement> {
        let mut statements = statements;
        for &region in self.block_regions[id].iter().rev() {
            let caught = |k: usize| format!("__gen_caught_{region}_{k}");
            let handlers = match &self.regions[region] {
                Region::Except { handlers } => handlers
                    .iter()
                    .enumerate()
                    .map(|(k, (types, name, block))| {
                        let mut body = vec![set_field(
                            &region_exc_field(region),
                            IRExpr::Variable(caught(k)),
                        )];
                        if let Some(name) = name {
                            body.push(set_field(name, IRExpr::Variable(caught(k))));
                        }
                        body.push(set_pc(*block));
                        body.push(IRStatement::Continue);
                        crate::ir::IRExceptHandler {
                            exception_types: if types.is_empty() {
                                vec!["BaseException".to_string()]
                            } else {
                                types.clone()
                            },
                            name: Some(caught(k)),
                            body: IRBody { statements: body },
                        }
                    })
                    .collect(),
                Region::Finally { entry } => vec![crate::ir::IRExceptHandler {
                    exception_types: vec!["BaseException".to_string()],
                    name: Some(caught(0)),
                    body: IRBody {
                        statements: vec![
                            set_field(&region_exc_field(region), IRExpr::Variable(caught(0))),
                            set_field(&region_fin_field(region), int_const(RERAISE)),
                            set_pc(*entry),
                            IRStatement::Continue,
                        ],
                    },
                }],
            };
            statements = vec![IRStatement::TryExcept {
                try_body: body_of(statements),
                except_handlers: handlers,
                finally_body: None,
            }];
        }
        statements
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
                // `close()` raises GeneratorExit here, inside the regions,
                // so their handlers and `finally` blocks see it.
                if !self.open.is_empty() {
                    self.throw_points.push(resume);
                    self.push(IRStatement::If {
                        condition: self_attr(GEN_THROW_FIELD),
                        then_body: body_of(vec![
                            set_field(GEN_THROW_FIELD, int_const(0)),
                            IRStatement::Raise {
                                exception: Some(IRExpr::Variable("GeneratorExit".to_string())),
                                cause: None,
                            },
                        ]),
                        else_body: None,
                    });
                }
            }
            // `return` in a generator ends iteration (the value is ignored in
            // this subset), after the `finally` blocks it leaves.
            IRStatement::Return(_) => {
                self.route(EXHAUSTED, 0);
                self.dead_block();
            }
            IRStatement::Break => {
                let (_, exit, depth) = *self
                    .loops
                    .last()
                    .ok_or_else(|| anyhow!("generator '{func_name}': break outside a loop"))?;
                self.route(exit, depth);
                self.dead_block();
            }
            IRStatement::Continue => {
                let (head, _, depth) = *self
                    .loops
                    .last()
                    .ok_or_else(|| anyhow!("generator '{func_name}': continue outside a loop"))?;
                self.route(head, depth);
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
                self.loops.push((head, exit, self.open.len()));
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
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body: Some(finally_body),
            } => {
                // `try/except/finally` is `try: (try/except) finally`.
                let after = self.new_block();
                let region = self.regions.len();
                let entry = self.new_block();
                self.regions.push(Region::Finally { entry });
                self.open.push(region);
                let start = self.new_block();
                self.jump(start);
                self.current = start as usize;
                if except_handlers.is_empty() {
                    self.flatten_body(try_body.statements, func_name)?;
                } else {
                    self.flatten_stmt(
                        IRStatement::TryExcept {
                            try_body,
                            except_handlers,
                            finally_body: None,
                        },
                        func_name,
                    )?;
                }
                self.push(set_field(&region_fin_field(region), int_const(after)));
                self.jump(entry);
                self.open.pop();
                self.current = entry as usize;
                self.flatten_body(finally_body.statements, func_name)?;
                self.push(IRStatement::If {
                    condition: IRExpr::CompareOp {
                        left: Box::new(self_attr(&region_fin_field(region))),
                        right: Box::new(int_const(RERAISE)),
                        op: IRCompareOp::Eq,
                    },
                    then_body: body_of(vec![IRStatement::Raise {
                        exception: Some(self_attr(&region_exc_field(region))),
                        cause: None,
                    }]),
                    else_body: None,
                });
                self.push(set_field(
                    GEN_PC_FIELD,
                    self_attr(&region_fin_field(region)),
                ));
                self.push(IRStatement::Continue);
                self.current = after as usize;
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body: None,
            } => {
                let after = self.new_block();
                let region = self.regions.len();
                let handler_blocks: Vec<i32> =
                    except_handlers.iter().map(|_| self.new_block()).collect();
                self.regions.push(Region::Except {
                    handlers: except_handlers
                        .iter()
                        .zip(&handler_blocks)
                        .map(|(h, b)| (h.exception_types.clone(), h.name.clone(), *b))
                        .collect(),
                });
                self.open.push(region);
                let start = self.new_block();
                self.jump(start);
                self.current = start as usize;
                self.flatten_body(try_body.statements, func_name)?;
                self.jump(after);
                self.open.pop();
                for (handler, block) in except_handlers.into_iter().zip(handler_blocks) {
                    self.current = block as usize;
                    let mut statements = handler.body.statements;
                    // The `as` name is stored where the exception is caught.
                    if let (Some(name), Some(IRStatement::AttributeAssign { attribute, .. })) =
                        (&handler.name, statements.first())
                    {
                        if attribute == name {
                            statements.remove(0);
                        }
                    }
                    reraise_from(&mut statements, &region_exc_field(region));
                    self.flatten_body(statements, func_name)?;
                    self.jump(after);
                }
                self.current = after as usize;
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
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => std::iter::once(try_body.as_ref())
            .chain(except_handlers.iter().map(|h| &h.body))
            .chain(finally_body.as_deref())
            .any(|b| b.statements.iter().any(needs_flatten)),
        _ => false,
    }
}
