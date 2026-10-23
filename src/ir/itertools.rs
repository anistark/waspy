//! `itertools`, lowered to generator functions synthesized in IR.
//!
//! Each call `itertools.f(args)` becomes a call to a generator function
//! `__itertools_f_<n>` built here (a regular function, where CPython checks
//! its arguments as the iterator is made, wrapping one), so iterating it,
//! `next()`, comprehensions, and the builtins that walk an iterator all go
//! through the generator machinery like any generator's. The bodies follow
//! CPython's own: the documented equivalents for the lazy ones and the
//! algorithms of `itertoolsmodule.c` for the combinatoric ones, which give
//! their output in CPython's order.
//!
//! A generator's parameters need types to carry strings, floats, and tuples
//! (an untyped one holds an int's word), and the arguments' types are not
//! known before code generation. So each call gets a generator of its own,
//! typed from its arguments where the IR shows them: literals, annotations,
//! the declared results of the module's functions, and what other generators
//! yield. An argument whose type is not shown stays untyped, which takes
//! ints and refuses anything else where it is used.

use super::build::*;
use super::infer::{bind, Types};
use crate::ir::{
    IRBody, IRCompareOp, IRComprehensionKind, IRConstant, IRExpr, IRFunction, IRGenerator,
    IRModule, IROp, IRParam, IRStatement, IRType,
};
use anyhow::{anyhow, Result};
use std::collections::HashMap;

/// The prefix of the synthesized functions; they are internal.
pub const ITERTOOLS_FN_PREFIX: &str = "__itertools_";

// ---- the pass -----------------------------------------------------------------

pub fn lower_itertools(module: &mut IRModule) -> Result<()> {
    let mut names: HashMap<String, String> = HashMap::new();
    let mut through_module = false;
    for imp in &module.imports {
        if imp.module != "itertools" {
            continue;
        }
        match (&imp.name, imp.is_from_import) {
            (Some(name), true) => {
                names.insert(
                    imp.alias.clone().unwrap_or_else(|| name.clone()),
                    name.clone(),
                );
            }
            (None, false) => through_module = true,
            _ => {}
        }
    }
    if names.is_empty() && !through_module {
        return Ok(());
    }
    let mut types = Types::of_module(module);
    let defined: Vec<String> = module.functions.iter().map(|f| f.name.clone()).collect();
    let mut globals = defined.clone();
    globals.extend(module.classes.iter().map(|c| c.name.clone()));
    globals.extend(module.variables.iter().map(|v| v.name.clone()));
    let mut out: Vec<IRFunction> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    let mut lowerer = Lowerer {
        names: &names,
        through_module,
        defined: &defined,
        globals: &globals,
        types: &mut types,
        out: &mut out,
        strings: &mut strings,
    };
    for func in &mut module.functions {
        let mut env: HashMap<String, IRType> = func
            .params
            .iter()
            .map(|p| (p.name.clone(), p.param_type.clone()))
            .collect();
        lowerer.body(&mut func.body, &mut env)?;
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            let mut env: HashMap<String, IRType> = method
                .params
                .iter()
                .map(|p| (p.name.clone(), p.param_type.clone()))
                .collect();
            lowerer.body(&mut method.body, &mut env)?;
        }
    }
    let empty = HashMap::new();
    for var in &mut module.variables {
        lowerer.expr(&mut var.value, &empty)?;
    }
    for s in strings {
        module.memory_layout.add_string(&s);
    }
    module.functions.extend(out);
    Ok(())
}

struct Lowerer<'a> {
    names: &'a HashMap<String, String>,
    through_module: bool,
    defined: &'a [String],
    /// The module-level names a function body may read (functions, classes,
    /// definitions), so a lambda reading only those captures nothing.
    globals: &'a [String],
    types: &'a mut Types,
    out: &'a mut Vec<IRFunction>,
    strings: &'a mut Vec<String>,
}

impl Lowerer<'_> {
    fn body(&mut self, body: &mut IRBody, env: &mut HashMap<String, IRType>) -> Result<()> {
        for stmt in &mut body.statements {
            self.stmt(stmt, env)?;
            bind(stmt, self.types, env);
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &mut IRStatement, env: &mut HashMap<String, IRType>) -> Result<()> {
        match stmt {
            IRStatement::If {
                condition,
                then_body,
                else_body,
            } => {
                self.expr(condition, env)?;
                self.body(then_body, env)?;
                if let Some(e) = else_body {
                    self.body(e, env)?;
                }
            }
            IRStatement::While { condition, body } => {
                self.expr(condition, env)?;
                self.body(body, env)?;
            }
            IRStatement::For {
                target,
                iterable,
                body,
                else_body,
            } => {
                self.expr(iterable, env)?;
                let elem = self.types.element(&self.types.of(iterable, env));
                env.entry(target.clone()).or_insert(elem);
                self.body(body, env)?;
                if let Some(e) = else_body {
                    self.body(e, env)?;
                }
            }
            IRStatement::TryExcept {
                try_body,
                except_handlers,
                finally_body,
            } => {
                self.body(try_body, env)?;
                for h in except_handlers {
                    self.body(&mut h.body, env)?;
                }
                if let Some(f) = finally_body {
                    self.body(f, env)?;
                }
            }
            IRStatement::With {
                context_expr, body, ..
            } => {
                self.expr(context_expr, env)?;
                self.body(body, env)?;
            }
            IRStatement::Return(Some(e))
            | IRStatement::Yield { value: Some(e) }
            | IRStatement::Expression(e)
            | IRStatement::Assign { value: e, .. }
            | IRStatement::AugAssign { value: e, .. }
            | IRStatement::TupleUnpack { value: e, .. } => self.expr(e, env)?,
            IRStatement::AttributeAssign { object, value, .. }
            | IRStatement::AttributeAugAssign { object, value, .. } => {
                self.expr(object, env)?;
                self.expr(value, env)?;
            }
            IRStatement::IndexAssign {
                container,
                index,
                value,
            } => {
                self.expr(container, env)?;
                self.expr(index, env)?;
                self.expr(value, env)?;
            }
            IRStatement::Raise { exception, cause } => {
                if let Some(e) = exception {
                    self.expr(e, env)?;
                }
                if let Some(c) = cause {
                    self.expr(c, env)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Rewrite the itertools calls in an expression, innermost first.
    fn expr(&mut self, expr: &mut IRExpr, env: &HashMap<String, IRType>) -> Result<()> {
        let mut wrapper = IRBody {
            statements: vec![IRStatement::Expression(std::mem::replace(
                expr,
                IRExpr::Const(IRConstant::None),
            ))],
        };
        let mut result = Ok(());
        let _ = crate::ir::visit_body(&mut wrapper, &mut |e: &mut IRExpr| {
            if result.is_ok() {
                if let Some(name) = self.itertools_name(e) {
                    let arguments = match e {
                        IRExpr::MethodCall { arguments, .. }
                        | IRExpr::FunctionCall { arguments, .. } => arguments.clone(),
                        _ => unreachable!("an itertools name is a call"),
                    };
                    match self.lower(&name, &arguments, env) {
                        Ok(replacement) => *e = replacement,
                        Err(err) => result = Err(err),
                    }
                }
            }
            Ok(())
        });
        if let Some(IRStatement::Expression(e)) = wrapper.statements.pop() {
            *expr = e;
        }
        result
    }

    /// The itertools function a call names (`chain.from_iterable` as
    /// `from_iterable`), if it is one.
    fn itertools_name(&self, expr: &IRExpr) -> Option<String> {
        match expr {
            IRExpr::MethodCall {
                object,
                method_name,
                ..
            } => match object.as_ref() {
                IRExpr::Variable(m) if self.through_module && m == "itertools" => {
                    Some(method_name.clone())
                }
                // `itertools.chain.from_iterable(...)`, `chain.from_iterable(...)`
                IRExpr::Attribute { object, attribute }
                    if attribute == "chain"
                        && method_name == "from_iterable"
                        && self.through_module
                        && matches!(object.as_ref(), IRExpr::Variable(m) if m == "itertools") =>
                {
                    Some("from_iterable".to_string())
                }
                IRExpr::Variable(c)
                    if method_name == "from_iterable"
                        && self.names.get(c).is_some_and(|n| n == "chain") =>
                {
                    Some("from_iterable".to_string())
                }
                _ => None,
            },
            IRExpr::FunctionCall { function_name, .. } if !self.defined.contains(function_name) => {
                self.names.get(function_name).cloned()
            }
            _ => None,
        }
    }

    fn fresh(&self, name: &str) -> String {
        format!("{ITERTOOLS_FN_PREFIX}{name}_{}", self.out.len())
    }

    fn string(&mut self, s: &str) {
        self.strings.push(s.to_string());
    }

    /// Add a synthesized generator, recording what it yields.
    fn generator(
        &mut self,
        name: &str,
        params: Vec<IRParam>,
        statements: Vec<IRStatement>,
        yields: IRType,
    ) -> String {
        self.types.yields.insert(name.to_string(), yields.clone());
        self.out.push(IRFunction {
            name: name.to_string(),
            params,
            body: IRBody { statements },
            return_type: IRType::Generator(Box::new(yields)),
            decorators: Vec::new(),
        });
        name.to_string()
    }

    /// Add a regular function that checks its arguments as CPython's
    /// constructor does, then makes the generator `gen`.
    fn checked(
        &mut self,
        name: &str,
        params: Vec<IRParam>,
        mut checks: Vec<IRStatement>,
        gen: &str,
    ) -> String {
        let args = params.iter().map(|p| v(&p.name)).collect();
        checks.push(IRStatement::Return(Some(call(gen, args))));
        self.types
            .returns
            .insert(name.to_string(), IRType::Class(format!("__Gen_{gen}")));
        let yields = self
            .types
            .yields
            .get(gen)
            .cloned()
            .unwrap_or(IRType::Unknown);
        self.types.yields.insert(name.to_string(), yields);
        self.out.push(IRFunction {
            name: name.to_string(),
            params,
            body: IRBody { statements: checks },
            return_type: IRType::Class(format!("__Gen_{gen}")),
            decorators: Vec::new(),
        });
        name.to_string()
    }

    /// The function a function argument calls by name: a module function, or
    /// a lambda reading only its parameters and module-level names, made a
    /// module function typed by `params`. A generator holds its arguments in
    /// fields, through which nothing can be called, so a function value it
    /// is given has to be one it can name.
    fn named_function(&mut self, f: &IRExpr, params: &[IRType], what: &str) -> Result<String> {
        match f {
            IRExpr::Variable(name) if self.defined.contains(name) => Ok(name.clone()),
            IRExpr::Lambda {
                params: lparams,
                body: lbody,
                ..
            } => {
                let bound: Vec<&str> = lparams.iter().map(|p| p.name.as_str()).collect();
                let mut free: Vec<String> = Vec::new();
                let mut probe = IRBody {
                    statements: vec![IRStatement::Expression((**lbody).clone())],
                };
                let _ = crate::ir::visit_body(&mut probe, &mut |e: &mut IRExpr| {
                    if let IRExpr::Variable(n) = e {
                        if !bound.contains(&n.as_str()) && !self.globals.contains(n) {
                            free.push(n.clone());
                        }
                    }
                    Ok(())
                });
                if let Some(n) = free.first() {
                    return Err(refused(format!(
                        "{what} of a lambda that reads '{n}' from the function around it is not \
                         supported. Hint: pass a 'def' of the module, or a lambda reading only \
                         its parameters"
                    )));
                }
                if lparams.len() != params.len() {
                    return Err(anyhow!(
                        "TypeError: {what}'s function takes {} arguments where it is called with {}",
                        lparams.len(),
                        params.len()
                    ));
                }
                let fname = format!("{ITERTOOLS_FN_PREFIX}fn_{}", self.out.len());
                let fparams: Vec<IRParam> = lparams
                    .iter()
                    .zip(params)
                    .map(|(p, t)| param(&p.name, t))
                    .collect();
                // What it answers, as far as its typed parameters show.
                let scope: HashMap<String, IRType> = fparams
                    .iter()
                    .map(|p| (p.name.clone(), p.param_type.clone()))
                    .collect();
                let returns = self.types.of(lbody, &scope);
                let returns = if known(&returns) {
                    returns
                } else {
                    IRType::Unknown
                };
                if known(&returns) {
                    self.types.returns.insert(fname.clone(), returns.clone());
                }
                self.out.push(IRFunction {
                    name: fname.clone(),
                    params: fparams,
                    body: IRBody {
                        statements: vec![IRStatement::Return(Some((**lbody).clone()))],
                    },
                    return_type: returns,
                    decorators: Vec::new(),
                });
                Ok(fname)
            }
            _ => Err(refused(format!(
                "{what} of a function value is not supported here. Hint: pass a 'def' of the \
                 module by name, or a lambda reading only its parameters"
            ))),
        }
    }

    /// What calling the function a `named_function` names answers, where
    /// that is declared.
    fn returns_of(&self, name: &str) -> IRType {
        self.types
            .returns
            .get(name)
            .cloned()
            .unwrap_or(IRType::Unknown)
    }

    fn lower(
        &mut self,
        name: &str,
        arguments: &[IRExpr],
        env: &HashMap<String, IRType>,
    ) -> Result<IRExpr> {
        let positional: Vec<&IRExpr> = arguments
            .iter()
            .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
            .collect();
        let keyword = |key: &str| {
            arguments.iter().find_map(|a| match a {
                IRExpr::Keyword { name, value } if name == key => Some(value.as_ref()),
                _ => None,
            })
        };
        let keywords: Vec<&str> = arguments
            .iter()
            .filter_map(|a| match a {
                IRExpr::Keyword { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let allow = |ok: &[&str]| -> Result<()> {
            match keywords.iter().find(|k| !ok.contains(k)) {
                Some(k) => Err(anyhow!(
                    "TypeError: {name}() got an unexpected keyword argument '{k}'"
                )),
                None => Ok(()),
            }
        };
        let arity = |min: usize, max: usize| -> Result<()> {
            if positional.len() < min || positional.len() > max {
                return Err(anyhow!(
                    "TypeError: itertools.{name}() takes {} positional arguments, {} given",
                    if min == max {
                        min.to_string()
                    } else {
                        format!("{min} to {max}")
                    },
                    positional.len()
                ));
            }
            Ok(())
        };
        let n = self.out.len();
        match name {
            "count" => {
                allow(&["start", "step"])?;
                arity(0, 2)?;
                let start = positional.first().copied().or(keyword("start")).cloned();
                let step = positional.get(1).copied().or(keyword("step")).cloned();
                let start = start.unwrap_or(int(0));
                let step = step.unwrap_or(int(1));
                let (ts, tp) = (self.types.of_in(env, &start), self.types.of_in(env, &step));
                if !matches!(ts, IRType::Float) && matches!(tp, IRType::Float) {
                    return Err(refused(
                        "itertools.count() of an int start and a float step is not supported: it \
                         yields an int and then floats. Hint: write the start as a float"
                            .to_string(),
                    ));
                }
                let g = self.fresh("count");
                let name = self.generator(
                    &g,
                    vec![param("start", &ts), param("step", &tp)],
                    vec![
                        assign_typed("n", v("start"), &ts),
                        while_(boolean(true), vec![yield_(v("n")), add_to("n", v("step"))]),
                    ],
                    ts,
                );
                Ok(call(&name, vec![start, step]))
            }
            "cycle" => {
                allow(&[])?;
                arity(1, 1)?;
                let it = positional[0].clone();
                let t = self.types.of_in(env, &it);
                let e = self.types.element(&t);
                let g = self.fresh("cycle");
                let name = self.generator(
                    &g,
                    vec![param("it", &t)],
                    vec![
                        assign_typed(
                            "saved",
                            IRExpr::ListLiteral(Vec::new()),
                            &IRType::List(Box::new(e.clone())),
                        ),
                        for_(
                            "x",
                            v("it"),
                            vec![
                                yield_(v("x")),
                                IRStatement::Expression(mcall(v("saved"), "append", vec![v("x")])),
                            ],
                        ),
                        while_(
                            cmp(len(v("saved")), IRCompareOp::Gt, int(0)),
                            vec![for_("y", v("saved"), vec![yield_(v("y"))])],
                        ),
                    ],
                    e,
                );
                Ok(call(&name, vec![it]))
            }
            "repeat" => {
                allow(&["object", "times"])?;
                arity(0, 2)?;
                let object = positional
                    .first()
                    .copied()
                    .or(keyword("object"))
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!("TypeError: repeat() missing required argument 'object'")
                    })?;
                let times = positional.get(1).copied().or(keyword("times")).cloned();
                let t = self.types.of_in(env, &object);
                let g = self.fresh("repeat");
                let (params, statements, args) = match times {
                    None => (
                        vec![param("object", &t)],
                        vec![while_(boolean(true), vec![yield_(v("object"))])],
                        vec![object],
                    ),
                    Some(times) => (
                        vec![param("object", &t), param("times", &IRType::Int)],
                        vec![
                            assign("i", int(0)),
                            while_(
                                cmp(v("i"), IRCompareOp::Lt, v("times")),
                                vec![yield_(v("object")), add_to("i", int(1))],
                            ),
                        ],
                        vec![object, times],
                    ),
                };
                let name = self.generator(&g, params, statements, t);
                Ok(call(&name, args))
            }
            "accumulate" => {
                allow(&["func", "initial"])?;
                arity(1, 2)?;
                let it = positional[0].clone();
                let func = positional.get(1).copied().or(keyword("func")).cloned();
                let initial = keyword("initial").cloned();
                let t_it = self.types.of_in(env, &it);
                let e = self.types.element(&t_it);
                let total_ty = match &initial {
                    Some(i) => self.types.of_in(env, i),
                    None => e.clone(),
                };
                let fname = match &func {
                    Some(f) => Some(self.named_function(
                        f,
                        &[total_ty.clone(), e.clone()],
                        "itertools.accumulate()",
                    )?),
                    None => None,
                };
                let combine = |acc: IRExpr, x: IRExpr| match &fname {
                    Some(f) => call(f, vec![acc, x]),
                    None => bin(acc, IROp::Add, x),
                };
                let mut params = vec![param("it", &t_it)];
                let mut args = vec![it];
                let statements = match initial {
                    Some(i) => {
                        params.push(param("initial", &total_ty));
                        args.push(i);
                        vec![
                            assign_typed("total", v("initial"), &total_ty),
                            yield_(v("total")),
                            for_(
                                "x",
                                v("it"),
                                vec![
                                    assign("total", combine(v("total"), v("x"))),
                                    yield_(v("total")),
                                ],
                            ),
                        ]
                    }
                    None => vec![
                        assign("first", boolean(true)),
                        for_(
                            "x",
                            v("it"),
                            vec![
                                if_else(
                                    v("first"),
                                    vec![
                                        assign_typed("total", v("x"), &total_ty),
                                        assign("first", boolean(false)),
                                    ],
                                    vec![assign("total", combine(v("total"), v("x")))],
                                ),
                                yield_(v("total")),
                            ],
                        ),
                    ],
                };
                let g = self.fresh("accumulate");
                let name = self.generator(&g, params, statements, total_ty);
                Ok(call(&name, args))
            }
            "batched" => {
                allow(&["strict"])?;
                arity(2, 2)?;
                let strict = match keyword("strict") {
                    None | Some(IRExpr::Const(IRConstant::Bool(false))) => false,
                    Some(IRExpr::Const(IRConstant::Bool(true))) => true,
                    Some(_) => {
                        return Err(refused(
                            "itertools.batched()'s strict= must be True or False here".to_string(),
                        ))
                    }
                };
                let it = positional[0].clone();
                let t = self.types.of_in(env, &it);
                let e = self.types.element(&t);
                let list_ty = IRType::List(Box::new(e.clone()));
                self.string("n must be at least one");
                self.string("batched(): incomplete batch");
                let mut tail = vec![];
                if strict {
                    tail.push(raise("ValueError", "batched(): incomplete batch"));
                }
                tail.push(yield_(as_tuple(v("batch"))));
                let g = self.fresh("batched_gen");
                let gen = self.generator(
                    &g,
                    vec![param("it", &t), param("n", &IRType::Int)],
                    vec![
                        assign_typed("batch", IRExpr::ListLiteral(Vec::new()), &list_ty),
                        for_(
                            "x",
                            v("it"),
                            vec![
                                IRStatement::Expression(mcall(v("batch"), "append", vec![v("x")])),
                                if_(
                                    cmp(len(v("batch")), IRCompareOp::Eq, v("n")),
                                    vec![
                                        yield_(as_tuple(v("batch"))),
                                        assign_typed(
                                            "batch",
                                            IRExpr::ListLiteral(Vec::new()),
                                            &list_ty,
                                        ),
                                    ],
                                ),
                            ],
                        ),
                        if_(cmp(len(v("batch")), IRCompareOp::Gt, int(0)), tail),
                    ],
                    IRType::VarTuple(Box::new(e)),
                );
                let w = self.fresh("batched");
                let name = self.checked(
                    &w,
                    vec![param("it", &t), param("n", &IRType::Int)],
                    vec![if_(
                        cmp(v("n"), IRCompareOp::Lt, int(1)),
                        vec![raise("ValueError", "n must be at least one")],
                    )],
                    &gen,
                );
                Ok(call(&name, vec![it, positional[1].clone()]))
            }
            "chain" => {
                allow(&[])?;
                let types: Vec<IRType> = positional.iter().map(|a| self.types.of(a, env)).collect();
                let elems: Vec<IRType> = types.iter().map(|t| self.types.element(t)).collect();
                let e =
                    match elems.first() {
                        Some(first) if elems.iter().all(|x| x == first) => first.clone(),
                        Some(_) => return Err(refused(
                            "itertools.chain() of iterables whose elements differ in type is not \
                             supported: a chain yields one type here"
                                .to_string(),
                        )),
                        None => IRType::Unknown,
                    };
                let params = types
                    .iter()
                    .enumerate()
                    .map(|(k, t)| param(&format!("it{k}"), t))
                    .collect();
                let statements = (0..positional.len())
                    .map(|k| for_("x", v(&format!("it{k}")), vec![yield_(v("x"))]))
                    .collect();
                let g = self.fresh("chain");
                let name = self.generator(&g, params, statements, e);
                Ok(call(&name, positional.into_iter().cloned().collect()))
            }
            "from_iterable" => {
                allow(&[])?;
                arity(1, 1)?;
                let t = self.types.of_in(env, positional[0]);
                let inner = self.types.element(&t);
                let e = self.types.element(&inner);
                let g = self.fresh("from_iterable");
                let name = self.generator(
                    &g,
                    vec![param("its", &t)],
                    vec![for_(
                        "it",
                        v("its"),
                        vec![for_("x", v("it"), vec![yield_(v("x"))])],
                    )],
                    e,
                );
                Ok(call(&name, vec![positional[0].clone()]))
            }
            "compress" => {
                allow(&["data", "selectors"])?;
                arity(0, 2)?;
                let data = positional.first().copied().or(keyword("data")).cloned();
                let selectors = positional.get(1).copied().or(keyword("selectors")).cloned();
                let (Some(data), Some(selectors)) = (data, selectors) else {
                    return Err(anyhow!("TypeError: compress() takes data and selectors"));
                };
                let (td, ts) = (
                    self.types.of_in(env, &data),
                    self.types.of_in(env, &selectors),
                );
                let e = self.types.element(&td);
                let g = self.fresh("compress");
                // The shape the converter lowers `for d, s in zip(data,
                // selectors)` to, which the generator pass pulls through an
                // iterator's `__next__` where `selectors` is one.
                let u = format!("it{n}");
                let seq = format!("{}{u}_1", crate::ir::ZIP_SEQ_PREFIX);
                let counter = format!("__zipi_{u}");
                let first = format!("{}{u}_0", crate::ir::ZIP_VALUE_PREFIX);
                let second = format!("{}{u}_1", crate::ir::ZIP_VALUE_PREFIX);
                let name = self.generator(
                    &g,
                    vec![param("data", &td), param("selectors", &ts)],
                    vec![
                        assign(&seq, v("selectors")),
                        assign(&counter, int(-1)),
                        for_(
                            &first,
                            v("data"),
                            vec![
                                add_to(&counter, int(1)),
                                if_(
                                    cmp(v(&counter), IRCompareOp::GtE, len(v(&seq))),
                                    vec![IRStatement::Break],
                                ),
                                assign(&second, idx(v(&seq), v(&counter))),
                                assign("d", v(&first)),
                                assign("s", v(&second)),
                                if_(v("s"), vec![yield_(v("d"))]),
                            ],
                        ),
                    ],
                    e,
                );
                Ok(call(&name, vec![data, selectors]))
            }
            "dropwhile" | "takewhile" | "filterfalse" => {
                allow(&[])?;
                arity(2, 2)?;
                let (pred, it) = (positional[0].clone(), positional[1].clone());
                let t = self.types.of_in(env, &it);
                let e = self.types.element(&t);
                let none_pred = matches!(pred, IRExpr::Const(IRConstant::None));
                if none_pred && name != "filterfalse" {
                    return Err(anyhow!("TypeError: 'NoneType' object is not callable"));
                }
                let pname = if none_pred {
                    None
                } else {
                    Some(self.named_function(
                        &pred,
                        std::slice::from_ref(&e),
                        &format!("itertools.{name}()"),
                    )?)
                };
                let test = |x: IRExpr| match &pname {
                    Some(p) => call(p, vec![x]),
                    None => x,
                };
                let statements = match name {
                    "dropwhile" => vec![
                        assign("dropping", boolean(true)),
                        for_(
                            "x",
                            v("it"),
                            vec![
                                if_(
                                    v("dropping"),
                                    vec![
                                        if_(test(v("x")), vec![IRStatement::Continue]),
                                        assign("dropping", boolean(false)),
                                    ],
                                ),
                                yield_(v("x")),
                            ],
                        ),
                    ],
                    "takewhile" => vec![for_(
                        "x",
                        v("it"),
                        vec![
                            if_(not(test(v("x"))), vec![IRStatement::Break]),
                            yield_(v("x")),
                        ],
                    )],
                    _ => vec![for_(
                        "x",
                        v("it"),
                        vec![if_(not(test(v("x"))), vec![yield_(v("x"))])],
                    )],
                };
                let params = vec![param("it", &t)];
                let args = vec![it];
                let g = self.fresh(name);
                let gname = self.generator(&g, params, statements, e);
                Ok(call(&gname, args))
            }
            "starmap" => {
                allow(&[])?;
                arity(2, 2)?;
                let (func, it) = (positional[0].clone(), positional[1].clone());
                let t = self.types.of_in(env, &it);
                let IRType::Tuple(members) = self.types.element(&t) else {
                    return Err(refused(
                        "itertools.starmap() needs the iterable's elements to be tuples of known \
                         types. Hint: annotate the iterable, for example List[Tuple[int, int]]"
                            .to_string(),
                    ));
                };
                let targets: Vec<String> = (0..members.len()).map(|k| format!("a{k}")).collect();
                let fname = self.named_function(&func, &members, "itertools.starmap()")?;
                let yields = self.returns_of(&fname);
                let g = self.fresh("starmap");
                let name = self.generator(
                    &g,
                    vec![param("it", &t)],
                    vec![for_(
                        "args",
                        v("it"),
                        vec![
                            IRStatement::TupleUnpack {
                                targets: targets.clone(),
                                value: v("args"),
                                starred: None,
                            },
                            yield_(call(&fname, targets.iter().map(|t| v(t)).collect())),
                        ],
                    )],
                    yields,
                );
                Ok(call(&name, vec![it]))
            }
            "islice" => {
                allow(&[])?;
                arity(2, 4)?;
                let it = positional[0].clone();
                let t = self.types.of_in(env, &it);
                let e = self.types.element(&t);
                let none = |x: &IRExpr| matches!(x, IRExpr::Const(IRConstant::None));
                // islice(it, stop) or islice(it, start, stop[, step]); None
                // is the default of each.
                let (start, stop, step) = match positional.len() {
                    2 => (None, Some(positional[1].clone()), None),
                    _ => (
                        Some(positional[1].clone()),
                        Some(positional[2].clone()),
                        positional.get(3).copied().cloned(),
                    ),
                };
                let start = start.filter(|x| !none(x)).unwrap_or(int(0));
                let step = step.filter(|x| !none(x)).unwrap_or(int(1));
                let bounded = stop.as_ref().is_some_and(|x| !none(x));
                let stop = stop.filter(|x| !none(x)).unwrap_or(int(0));
                for m in [
                    "Stop argument for islice() must be None or an integer: 0 <= x <= sys.maxsize.",
                    "Indices for islice() must be None or an integer: 0 <= x <= sys.maxsize.",
                    "Step for islice() must be a positive integer or None.",
                ] {
                    self.string(m);
                }
                // The documented equivalent: `range(max(start, stop))` paired
                // with the iterable bounds what is consumed.
                let mut statements = vec![];
                if bounded {
                    statements.push(assign("limit", v("stop")));
                    statements.push(if_(
                        cmp(v("start"), IRCompareOp::Gt, v("limit")),
                        vec![assign("limit", v("start"))],
                    ));
                    statements.push(if_(cmp(v("limit"), IRCompareOp::Eq, int(0)), vec![ret()]));
                }
                statements.push(assign("cnt", int(0)));
                statements.push(assign("nxt", v("start")));
                let mut each = vec![
                    if_(
                        cmp(v("cnt"), IRCompareOp::Eq, v("nxt")),
                        vec![yield_(v("x")), add_to("nxt", v("step"))],
                    ),
                    add_to("cnt", int(1)),
                ];
                if bounded {
                    each.push(if_(
                        cmp(v("cnt"), IRCompareOp::GtE, v("limit")),
                        vec![ret()],
                    ));
                }
                statements.push(for_("x", v("it"), each));
                let params = vec![
                    param("it", &t),
                    param("start", &IRType::Int),
                    param("stop", &IRType::Int),
                    param("step", &IRType::Int),
                ];
                let g = self.fresh("islice_gen");
                let gen = self.generator(&g, params.clone(), statements, e);
                let mut checks = vec![];
                if bounded {
                    checks.push(if_(
                        cmp(v("stop"), IRCompareOp::Lt, int(0)),
                        vec![raise(
                            "ValueError",
                            "Stop argument for islice() must be None or an integer: 0 <= x <= sys.maxsize.",
                        )],
                    ));
                }
                checks.push(if_(
                    cmp(v("start"), IRCompareOp::Lt, int(0)),
                    vec![raise(
                        "ValueError",
                        "Indices for islice() must be None or an integer: 0 <= x <= sys.maxsize.",
                    )],
                ));
                checks.push(if_(
                    cmp(v("step"), IRCompareOp::LtE, int(0)),
                    vec![raise(
                        "ValueError",
                        "Step for islice() must be a positive integer or None.",
                    )],
                ));
                let w = self.fresh("islice");
                let name = self.checked(&w, params, checks, &gen);
                Ok(call(&name, vec![it, start, stop, step]))
            }
            "pairwise" => {
                allow(&[])?;
                arity(1, 1)?;
                let it = positional[0].clone();
                let t = self.types.of_in(env, &it);
                let e = self.types.element(&t);
                let g = self.fresh("pairwise");
                let name = self.generator(
                    &g,
                    vec![param("it", &t)],
                    vec![
                        assign("first", boolean(true)),
                        for_(
                            "b",
                            v("it"),
                            vec![
                                if_else(
                                    v("first"),
                                    vec![assign("first", boolean(false))],
                                    vec![yield_(IRExpr::TupleLiteral(vec![v("a"), v("b")]))],
                                ),
                                assign_typed("a", v("b"), &e),
                            ],
                        ),
                    ],
                    IRType::Tuple(vec![e.clone(), e]),
                );
                Ok(call(&name, vec![it]))
            }
            "tee" => {
                allow(&[])?;
                arity(1, 2)?;
                let it = positional[0].clone();
                let t = self.types.of_in(env, &it);
                if self.types.is_iterator(&t) {
                    return Err(refused(
                        "itertools.tee() of a generator or iterator is not supported. Hint: \
                         collect it into a list first, which can be iterated more than once"
                            .to_string(),
                    ));
                }
                let copies = match positional.get(1) {
                    None => 2,
                    Some(IRExpr::Const(IRConstant::Int(k))) if *k >= 0 => *k as usize,
                    Some(IRExpr::Const(IRConstant::Int(_))) => {
                        return Err(anyhow!("ValueError: n must be >= 0"))
                    }
                    Some(_) => {
                        return Err(refused(
                            "itertools.tee()'s n must be a constant here, since it sizes the \
                             tuple it answers"
                                .to_string(),
                        ))
                    }
                };
                // Each copy walks the sequence on its own; a sequence that is
                // not an iterator can be walked again and again.
                let e = self.types.element(&t);
                let g = self.fresh("tee");
                let gen = self.generator(
                    &g,
                    vec![param("it", &t)],
                    vec![for_("x", v("it"), vec![yield_(v("x"))])],
                    e,
                );
                let items = (0..copies).map(|_| call(&gen, vec![it.clone()])).collect();
                Ok(IRExpr::TupleLiteral(items))
            }
            "zip_longest" => {
                allow(&["fillvalue"])?;
                let fill = keyword("fillvalue")
                    .cloned()
                    .unwrap_or(IRExpr::Const(IRConstant::None));
                let types: Vec<IRType> = positional.iter().map(|a| self.types.of(a, env)).collect();
                if types.iter().any(|t| self.types.is_iterator(t)) {
                    return Err(refused(
                        "itertools.zip_longest() of a generator or iterator is not supported. \
                         Hint: collect it into a list first"
                            .to_string(),
                    ));
                }
                let k = positional.len();
                let elems: Vec<IRType> = types.iter().map(|t| self.types.element(t)).collect();
                let mut params: Vec<IRParam> = types
                    .iter()
                    .enumerate()
                    .map(|(j, t)| param(&format!("s{j}"), t))
                    .collect();
                params.push(param("fill", &self.types.of_in(env, &fill)));
                let mut statements = vec![assign("n", int(0))];
                for j in 0..k {
                    let s = format!("s{j}");
                    statements.push(if_(
                        cmp(len(v(&s)), IRCompareOp::Gt, v("n")),
                        vec![assign("n", len(v(&s)))],
                    ));
                }
                statements.push(assign("i", int(0)));
                let members = (0..k)
                    .map(|j| {
                        let s = format!("s{j}");
                        IRExpr::Conditional {
                            condition: Box::new(cmp(v("i"), IRCompareOp::Lt, len(v(&s)))),
                            body: Box::new(idx(v(&s), v("i"))),
                            orelse: Box::new(v("fill")),
                        }
                    })
                    .collect();
                statements.push(while_(
                    cmp(v("i"), IRCompareOp::Lt, v("n")),
                    vec![yield_(IRExpr::TupleLiteral(members)), add_to("i", int(1))],
                ));
                let g = self.fresh("zip_longest");
                let name = self.generator(&g, params, statements, IRType::Tuple(elems));
                let mut args: Vec<IRExpr> = positional.into_iter().cloned().collect();
                args.push(fill);
                Ok(call(&name, args))
            }
            "groupby" => {
                allow(&["key"])?;
                arity(1, 2)?;
                let it = positional[0].clone();
                let key = positional.get(1).copied().or(keyword("key")).cloned();
                let key = key.filter(|k| !matches!(k, IRExpr::Const(IRConstant::None)));
                let t = self.types.of_in(env, &it);
                if self.types.is_iterator(&t) {
                    return Err(refused(
                        "itertools.groupby() of a generator or iterator is not supported. Hint: \
                         collect it into a list first"
                            .to_string(),
                    ));
                }
                let e = self.types.element(&t);
                let kname = match &key {
                    Some(k) => Some(self.named_function(
                        k,
                        std::slice::from_ref(&e),
                        "itertools.groupby()",
                    )?),
                    None => None,
                };
                let key_ty = match &kname {
                    None => e.clone(),
                    Some(k) => self.returns_of(k),
                };
                let keyed = |x: IRExpr| match &kname {
                    Some(k) => call(k, vec![x]),
                    None => x,
                };
                // A group is an iterator over the members collected for it,
                // which stops once the groupby has moved on, as CPython's
                // groups do. The groupby bumps `token[0]` as it moves.
                let list_ty = IRType::List(Box::new(e.clone()));
                let g_group = self.fresh("group");
                let group = self.generator(
                    &g_group,
                    vec![
                        param("items", &list_ty),
                        param("token", &IRType::List(Box::new(IRType::Int))),
                        param("me", &IRType::Int),
                    ],
                    vec![for_(
                        "x",
                        v("items"),
                        vec![
                            if_(
                                cmp(idx(v("token"), int(0)), IRCompareOp::NotEq, v("me")),
                                vec![ret()],
                            ),
                            yield_(v("x")),
                        ],
                    )],
                    e.clone(),
                );
                let group_ty = IRType::Class(format!("__Gen_{group}"));
                let params = vec![param("data", &t)];
                let args = vec![it];
                let statements = vec![
                    assign_typed("items", listed(v("data"), n, 0), &list_ty),
                    assign("count", len(v("items"))),
                    if_(cmp(v("count"), IRCompareOp::Eq, int(0)), vec![ret()]),
                    assign_typed(
                        "token",
                        IRExpr::ListLiteral(vec![int(0)]),
                        &IRType::List(Box::new(IRType::Int)),
                    ),
                    assign_typed("cur", idx(v("items"), int(0)), &e),
                    assign_typed("ck", keyed(v("cur")), &key_ty),
                    assign("i", int(1)),
                    assign("more", boolean(true)),
                    while_(
                        v("more"),
                        vec![
                            assign_typed("group", IRExpr::ListLiteral(vec![v("cur")]), &list_ty),
                            assign("more", boolean(false)),
                            while_(
                                cmp(v("i"), IRCompareOp::Lt, v("count")),
                                vec![
                                    assign_typed("x", idx(v("items"), v("i")), &e),
                                    assign_typed("xk", keyed(v("x")), &key_ty),
                                    add_to("i", int(1)),
                                    if_else(
                                        cmp(v("xk"), IRCompareOp::Eq, v("ck")),
                                        vec![IRStatement::Expression(mcall(
                                            v("group"),
                                            "append",
                                            vec![v("x")],
                                        ))],
                                        vec![
                                            assign_typed("nxt", v("x"), &e),
                                            assign_typed("nk", v("xk"), &key_ty),
                                            assign("more", boolean(true)),
                                            IRStatement::Break,
                                        ],
                                    ),
                                ],
                            ),
                            yield_(IRExpr::TupleLiteral(vec![
                                v("ck"),
                                call(
                                    &group,
                                    vec![v("group"), v("token"), idx(v("token"), int(0))],
                                ),
                            ])),
                            // Every resume moves the groupby on, the last one
                            // (which stops it) included, and ends the group
                            // just given.
                            set_item(
                                v("token"),
                                int(0),
                                bin(idx(v("token"), int(0)), IROp::Add, int(1)),
                            ),
                            if_(
                                v("more"),
                                vec![assign("cur", v("nxt")), assign("ck", v("nk"))],
                            ),
                        ],
                    ),
                ];
                let g = self.fresh("groupby");
                let name = self.generator(
                    &g,
                    params,
                    statements,
                    IRType::Tuple(vec![key_ty, group_ty]),
                );
                Ok(call(&name, args))
            }
            "product" => {
                allow(&["repeat"])?;
                let repeat = match keyword("repeat") {
                    None => 1,
                    Some(IRExpr::Const(IRConstant::Int(r))) if *r >= 0 => *r as usize,
                    Some(IRExpr::Const(IRConstant::Int(_))) => {
                        return Err(anyhow!("ValueError: repeat argument cannot be negative"))
                    }
                    Some(_) => return Err(refused(
                        "itertools.product()'s repeat= must be a constant here, since it sizes \
                             the tuples it yields"
                            .to_string(),
                    )),
                };
                // The pools are taken when product() is called, as CPython
                // takes them.
                let pools: Vec<IRExpr> = positional
                    .iter()
                    .enumerate()
                    .map(|(k, a)| listed((*a).clone(), n, k))
                    .collect();
                let elems: Vec<IRType> = positional
                    .iter()
                    .map(|a| self.types.element(&self.types.of(a, env)))
                    .collect();
                let params: Vec<IRParam> = elems
                    .iter()
                    .enumerate()
                    .map(|(k, e)| param(&format!("p{k}"), &IRType::List(Box::new(e.clone()))))
                    .collect();
                let order: Vec<usize> = (0..repeat).flat_map(|_| 0..positional.len()).collect();
                let members: Vec<IRType> = order.iter().map(|&k| elems[k].clone()).collect();
                let mut inner = vec![yield_(IRExpr::TupleLiteral(
                    (0..order.len()).map(|d| v(&format!("a{d}"))).collect(),
                ))];
                for (d, &k) in order.iter().enumerate().rev() {
                    inner = vec![for_(&format!("a{d}"), v(&format!("p{k}")), inner)];
                }
                let g = self.fresh("product");
                let name = self.generator(&g, params, inner, IRType::Tuple(members));
                Ok(call(&name, pools))
            }
            "permutations" | "combinations" | "combinations_with_replacement" => {
                allow(&["r"])?;
                let required = name != "permutations";
                arity(1, 2)?;
                let it = positional[0].clone();
                let r = positional.get(1).copied().or(keyword("r")).cloned();
                if required && r.is_none() {
                    return Err(anyhow!("TypeError: {name}() missing required argument 'r'"));
                }
                let r = r.filter(|x| !matches!(x, IRExpr::Const(IRConstant::None)));
                let e = self.types.element(&self.types.of_in(env, &it));
                let pool_ty = IRType::List(Box::new(e.clone()));
                let ints = IRType::List(Box::new(IRType::Int));
                let tuple = |n: usize| picked("pool", "indices", v("r"), n);
                let statements = match name {
                    "permutations" => vec![
                        assign("n", len(v("pool"))),
                        if_(
                            cmp(v("r"), IRCompareOp::Lt, int(0)),
                            vec![assign("r", v("n"))],
                        ),
                        if_(cmp(v("r"), IRCompareOp::Gt, v("n")), vec![ret()]),
                        assign_typed("indices", listed(range(v("n")), n, 1), &ints),
                        assign_typed(
                            "cycles",
                            IRExpr::Comprehension {
                                kind: IRComprehensionKind::List,
                                element: Box::new(bin(v("n"), IROp::Sub, v(&format!("__c{n}")))),
                                value: None,
                                generators: vec![IRGenerator {
                                    targets: vec![format!("__c{n}")],
                                    iterable: range(v("r")),
                                    conditions: Vec::new(),
                                }],
                            },
                            &ints,
                        ),
                        yield_(tuple(n)),
                        while_(
                            cmp(v("n"), IRCompareOp::Gt, int(0)),
                            vec![
                                assign("i", bin(v("r"), IROp::Sub, int(1))),
                                assign("found", boolean(false)),
                                while_(
                                    cmp(v("i"), IRCompareOp::GtE, int(0)),
                                    vec![
                                        set_item(
                                            v("cycles"),
                                            v("i"),
                                            bin(idx(v("cycles"), v("i")), IROp::Sub, int(1)),
                                        ),
                                        if_else(
                                            cmp(idx(v("cycles"), v("i")), IRCompareOp::Eq, int(0)),
                                            vec![
                                                // indices[i:] = indices[i+1:] + indices[i:i+1]
                                                assign("t", idx(v("indices"), v("i"))),
                                                assign("j", v("i")),
                                                while_(
                                                    cmp(
                                                        v("j"),
                                                        IRCompareOp::Lt,
                                                        bin(v("n"), IROp::Sub, int(1)),
                                                    ),
                                                    vec![
                                                        set_item(
                                                            v("indices"),
                                                            v("j"),
                                                            idx(
                                                                v("indices"),
                                                                bin(v("j"), IROp::Add, int(1)),
                                                            ),
                                                        ),
                                                        add_to("j", int(1)),
                                                    ],
                                                ),
                                                set_item(
                                                    v("indices"),
                                                    bin(v("n"), IROp::Sub, int(1)),
                                                    v("t"),
                                                ),
                                                set_item(
                                                    v("cycles"),
                                                    v("i"),
                                                    bin(v("n"), IROp::Sub, v("i")),
                                                ),
                                            ],
                                            vec![
                                                assign("j", idx(v("cycles"), v("i"))),
                                                assign("t", idx(v("indices"), v("i"))),
                                                set_item(
                                                    v("indices"),
                                                    v("i"),
                                                    idx(
                                                        v("indices"),
                                                        bin(v("n"), IROp::Sub, v("j")),
                                                    ),
                                                ),
                                                set_item(
                                                    v("indices"),
                                                    bin(v("n"), IROp::Sub, v("j")),
                                                    v("t"),
                                                ),
                                                yield_(tuple(n + 1)),
                                                assign("found", boolean(true)),
                                                IRStatement::Break,
                                            ],
                                        ),
                                        assign("i", bin(v("i"), IROp::Sub, int(1))),
                                    ],
                                ),
                                if_(not(v("found")), vec![ret()]),
                            ],
                        ),
                    ],
                    "combinations" => vec![
                        assign("n", len(v("pool"))),
                        if_(cmp(v("r"), IRCompareOp::Gt, v("n")), vec![ret()]),
                        assign_typed("indices", listed(range(v("r")), n, 1), &ints),
                        yield_(tuple(n)),
                        while_(
                            boolean(true),
                            vec![
                                assign("i", bin(v("r"), IROp::Sub, int(1))),
                                while_(
                                    and(
                                        cmp(v("i"), IRCompareOp::GtE, int(0)),
                                        cmp(
                                            idx(v("indices"), v("i")),
                                            IRCompareOp::Eq,
                                            bin(bin(v("i"), IROp::Add, v("n")), IROp::Sub, v("r")),
                                        ),
                                    ),
                                    vec![assign("i", bin(v("i"), IROp::Sub, int(1)))],
                                ),
                                if_(cmp(v("i"), IRCompareOp::Lt, int(0)), vec![ret()]),
                                set_item(
                                    v("indices"),
                                    v("i"),
                                    bin(idx(v("indices"), v("i")), IROp::Add, int(1)),
                                ),
                                assign("j", bin(v("i"), IROp::Add, int(1))),
                                while_(
                                    cmp(v("j"), IRCompareOp::Lt, v("r")),
                                    vec![
                                        set_item(
                                            v("indices"),
                                            v("j"),
                                            bin(
                                                idx(v("indices"), bin(v("j"), IROp::Sub, int(1))),
                                                IROp::Add,
                                                int(1),
                                            ),
                                        ),
                                        add_to("j", int(1)),
                                    ],
                                ),
                                yield_(tuple(n + 1)),
                            ],
                        ),
                    ],
                    _ => vec![
                        assign("n", len(v("pool"))),
                        if_(
                            and(
                                cmp(v("n"), IRCompareOp::Eq, int(0)),
                                cmp(v("r"), IRCompareOp::Gt, int(0)),
                            ),
                            vec![ret()],
                        ),
                        assign_typed(
                            "indices",
                            IRExpr::Comprehension {
                                kind: IRComprehensionKind::List,
                                element: Box::new(int(0)),
                                value: None,
                                generators: vec![IRGenerator {
                                    targets: vec![format!("__z{n}")],
                                    iterable: range(v("r")),
                                    conditions: Vec::new(),
                                }],
                            },
                            &ints,
                        ),
                        yield_(tuple(n)),
                        while_(
                            boolean(true),
                            vec![
                                assign("i", bin(v("r"), IROp::Sub, int(1))),
                                while_(
                                    and(
                                        cmp(v("i"), IRCompareOp::GtE, int(0)),
                                        cmp(
                                            idx(v("indices"), v("i")),
                                            IRCompareOp::Eq,
                                            bin(v("n"), IROp::Sub, int(1)),
                                        ),
                                    ),
                                    vec![assign("i", bin(v("i"), IROp::Sub, int(1)))],
                                ),
                                if_(cmp(v("i"), IRCompareOp::Lt, int(0)), vec![ret()]),
                                assign("w", bin(idx(v("indices"), v("i")), IROp::Add, int(1))),
                                assign("j", v("i")),
                                while_(
                                    cmp(v("j"), IRCompareOp::Lt, v("r")),
                                    vec![
                                        set_item(v("indices"), v("j"), v("w")),
                                        add_to("j", int(1)),
                                    ],
                                ),
                                yield_(tuple(n + 1)),
                            ],
                        ),
                    ],
                };
                self.string("r must be non-negative");
                let params = vec![param("pool", &pool_ty), param("r", &IRType::Int)];
                let g = self.fresh(&format!("{name}_gen"));
                let gen = self.generator(
                    &g,
                    params.clone(),
                    statements,
                    IRType::VarTuple(Box::new(e)),
                );
                let w = self.fresh(name);
                let check = if r.is_some() {
                    vec![if_(
                        cmp(v("r"), IRCompareOp::Lt, int(0)),
                        vec![raise("ValueError", "r must be non-negative")],
                    )]
                } else {
                    vec![]
                };
                let wrapper = self.checked(&w, params, check, &gen);
                // The pool is taken when the call is made, as CPython takes
                // it; permutations() without r passes -1 for "all of it".
                Ok(call(&wrapper, vec![listed(it, n, 9), r.unwrap_or(int(-1))]))
            }
            other => Err(refused(format!("'itertools.{other}()' is not supported"))),
        }
    }
}
