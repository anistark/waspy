//! Whole-module post-pass over the lowered IR.
//!
//! Runs after every class and function of a module is converted, so checks and
//! rewrites that need cross-declaration knowledge live here rather than in the
//! statement-by-statement lowering:
//!
//! - **Abstract classes**: instantiating a class that inherits `abc.ABC` and
//!   still has unimplemented `@abstractmethod` methods is rejected at compile
//!   time, mirroring Python's runtime `TypeError`.
//! - **Call-site defaults**: a call that omits trailing arguments gets the
//!   callee's parameter defaults spliced in (constructors and module-level
//!   functions). Codegen emits exactly one value per parameter, so without
//!   this rewrite an omitted default underflowed the stack into invalid WASM.

use crate::ir::{
    IRBody, IRClass, IRConstant, IRExpr, IRFunction, IRModule, IRParam, IRStatement, IRType,
};
use anyhow::Result;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

/// Base-list markers that make a class hierarchy "ABC-enabled". Python only
/// enforces abstractness for classes whose metaclass is `ABCMeta`, i.e. those
/// deriving from `abc.ABC`; a stray `@abstractmethod` without it is inert.
const ABC_MARKERS: [&str; 2] = ["ABC", "abc.ABC"];

/// Per-class facts resolved across the inheritance chain.
struct ClassFacts {
    /// Abstract methods with no concrete override at this class, in sorted
    /// order so error messages are deterministic. Non-empty only for
    /// ABC-enabled classes, which therefore reject instantiation.
    unimplemented: BTreeSet<String>,
    /// The `__init__` parameters governing instantiation (own or inherited),
    /// including `self`. Empty when no `__init__` exists along the chain.
    init_params: Vec<IRParam>,
}

/// Run the post-pass: reject abstract instantiations and splice parameter
/// defaults into calls that omit trailing arguments.
pub fn finalize_module(module: &mut IRModule) -> Result<()> {
    let facts = resolve_class_facts(&module.classes);
    let function_params: HashMap<String, Vec<IRParam>> = module
        .functions
        .iter()
        .map(|f| (f.name.clone(), f.params.clone()))
        .collect();

    let mut rewrite = |expr: &mut IRExpr| -> Result<()> {
        let IRExpr::FunctionCall {
            function_name,
            arguments,
        } = expr
        else {
            return Ok(());
        };
        if let Some(class_facts) = facts.get(function_name) {
            // `ClassName(...)` is always instantiation.
            if let Some(method) = class_facts.unimplemented.iter().next() {
                return Err(crate::core::errors::type_error(
                    format!(
                        "Can't instantiate abstract class '{function_name}' with abstract \
                         method '{method}'"
                    ),
                    None,
                )
                .into());
            }
            if !class_facts.init_params.is_empty() {
                // Skip the implicit `self`; instantiation provides it.
                fill_defaults(
                    function_name,
                    arguments,
                    &class_facts.init_params[1..],
                    true,
                )?;
            }
        } else if let Some(params) = function_params.get(function_name) {
            fill_defaults(function_name, arguments, params, false)?;
        }
        Ok(())
    };

    for func in &mut module.functions {
        visit_body(&mut func.body, &mut rewrite)?;
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            visit_body(&mut method.body, &mut rewrite)?;
        }
    }

    lift_lambdas(module)?;

    Ok(())
}

/// Names that resolve as built-in calls at codegen; a reference to one inside
/// a lambda body is never a captured variable.
const BUILTIN_NAMES: [&str; 16] = [
    "print",
    "len",
    "int",
    "float",
    "str",
    "bool",
    "min",
    "max",
    "sum",
    "abs",
    "range",
    "isinstance",
    "issubclass",
    "super",
    "namedtuple",
    "type",
];

/// Process-wide counter so lifted lambda names stay unique even when several
/// modules are lowered and merged into one binary (the multi-file path drops
/// duplicate function names).
static LAMBDA_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Closure variable capture (#43): hoist every `IRExpr::Lambda` into a real
/// module function and replace the expression with `IRExpr::ClosureMake`.
///
/// The lifted function keeps the lambda's parameters and gains a trailing
/// `__env` parameter — a pointer to the environment built at the creation
/// site. Its body first rebinds each captured variable from the environment
/// (`cap = __env[k]`, which reuses the list-indexing codegen since the
/// environment shares the `[word][slot0][slot1]...` layout) and then returns
/// the lambda's body expression unchanged.
///
/// A name is captured when it is free in the lambda body (not one of its
/// parameters, nor bound by a nested lambda or a comprehension target) and
/// does not resolve globally (module functions, classes, module variables,
/// stdlib modules, built-ins). Capture is by value at creation time — Python's
/// late-binding cell semantics are only observable when a captured variable
/// mutates after the closure is created, which this subset accepts as a
/// documented deviation.
///
/// Nested lambdas compose because rewriting is post-order: an inner lambda is
/// already a `ClosureMake` when the outer one is lifted, so the inner capture
/// list references names that are parameters (or env-rebound captures) of the
/// outer lifted function.
fn lift_lambdas(module: &mut IRModule) -> Result<()> {
    let mut global_names: HashSet<String> = HashSet::new();
    global_names.extend(module.functions.iter().map(|f| f.name.clone()));
    global_names.extend(module.classes.iter().map(|c| c.name.clone()));
    global_names.extend(module.variables.iter().map(|v| v.name.clone()));

    let mut lifted: Vec<IRFunction> = Vec::new();

    let mut rewrite = |expr: &mut IRExpr| -> Result<()> {
        let IRExpr::Lambda { params, body, .. } = expr else {
            return Ok(());
        };

        let mut bound: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
        let mut captured: Vec<String> = Vec::new();
        collect_free_names(body, &mut bound, &global_names, &mut captured);

        let id = LAMBDA_COUNTER.fetch_add(1, Ordering::Relaxed);
        let lambda_name = format!("__lambda_{id}");

        // cap_k = __env[k] preludes, then return the lambda body.
        let mut statements: Vec<IRStatement> = captured
            .iter()
            .enumerate()
            .map(|(k, cap)| IRStatement::Assign {
                target: cap.clone(),
                value: IRExpr::Indexing {
                    container: Box::new(IRExpr::Variable("__env".to_string())),
                    index: Box::new(IRExpr::Const(IRConstant::Int(k as i32))),
                },
                var_type: None,
            })
            .collect();
        statements.push(IRStatement::Return(Some((**body).clone())));

        let mut fn_params = params.clone();
        fn_params.push(IRParam {
            name: "__env".to_string(),
            param_type: IRType::List(Box::new(IRType::Unknown)),
            default_value: None,
        });

        lifted.push(IRFunction {
            name: lambda_name.clone(),
            params: fn_params,
            body: IRBody { statements },
            return_type: IRType::Unknown,
            decorators: Vec::new(),
        });

        *expr = IRExpr::ClosureMake {
            lambda_name,
            captured,
        };
        Ok(())
    };

    for func in &mut module.functions {
        visit_body(&mut func.body, &mut rewrite)?;
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            visit_body(&mut method.body, &mut rewrite)?;
        }
    }
    for var in &mut module.variables {
        visit_expr(&mut var.value, &mut rewrite)?;
    }

    module.functions.extend(lifted);
    Ok(())
}

/// Collect the free variable names of `expr` (in first-use order, deduped)
/// into `out`: references that are not locally `bound`, not globally
/// resolvable, not stdlib modules, and not built-ins. Call targets count as
/// references too — calling an enclosing closure-valued local must capture
/// it — while known function/class names resolve statically and don't.
fn collect_free_names(
    expr: &IRExpr,
    bound: &mut Vec<String>,
    globals: &HashSet<String>,
    out: &mut Vec<String>,
) {
    let consider = |name: &str, bound: &Vec<String>, out: &mut Vec<String>| {
        if !bound.iter().any(|b| b == name)
            && !globals.contains(name)
            && !crate::stdlib::is_stdlib_module(name)
            && !BUILTIN_NAMES.contains(&name)
            && !out.iter().any(|c| c == name)
        {
            out.push(name.to_string());
        }
    };

    match expr {
        IRExpr::Variable(name) | IRExpr::Param(name) => consider(name, bound, out),
        IRExpr::Const(_) => {}
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            collect_free_names(left, bound, globals, out);
            collect_free_names(right, bound, globals, out);
        }
        IRExpr::UnaryOp { operand, .. } => collect_free_names(operand, bound, globals, out),
        IRExpr::FunctionCall {
            function_name,
            arguments,
        } => {
            consider(function_name, bound, out);
            for arg in arguments {
                collect_free_names(arg, bound, globals, out);
            }
        }
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            for item in items {
                collect_free_names(item, bound, globals, out);
            }
        }
        IRExpr::DictLiteral(entries) => {
            for (key, value) in entries {
                collect_free_names(key, bound, globals, out);
                collect_free_names(value, bound, globals, out);
            }
        }
        IRExpr::Indexing { container, index } => {
            collect_free_names(container, bound, globals, out);
            collect_free_names(index, bound, globals, out);
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            collect_free_names(container, bound, globals, out);
            for b in [start, end, step].into_iter().flatten() {
                collect_free_names(b, bound, globals, out);
            }
        }
        IRExpr::Attribute { object, .. } => collect_free_names(object, bound, globals, out),
        IRExpr::MethodCall {
            object, arguments, ..
        } => {
            collect_free_names(object, bound, globals, out);
            for arg in arguments {
                collect_free_names(arg, bound, globals, out);
            }
        }
        IRExpr::DynamicImportExpr { module_name } => {
            collect_free_names(module_name, bound, globals, out)
        }
        IRExpr::RangeCall { start, stop, step } => {
            for b in [start, step].into_iter().flatten() {
                collect_free_names(b, bound, globals, out);
            }
            collect_free_names(stop, bound, globals, out);
        }
        IRExpr::Lambda { params, body, .. } => {
            // A nested (not yet lifted) lambda binds its own parameters.
            let added = params.len();
            bound.extend(params.iter().map(|p| p.name.clone()));
            collect_free_names(body, bound, globals, out);
            bound.truncate(bound.len() - added);
        }
        IRExpr::ClosureMake { captured, .. } => {
            // An already-lifted inner lambda: its captures are read here, at
            // the outer scope, when the closure is created.
            for name in captured {
                consider(name, bound, out);
            }
        }
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            // Generator targets are bound within the comprehension. Scoping
            // between generators is already enforced by the converter's
            // renaming, so binding them all up front is safe here.
            let added: usize = generators.iter().map(|g| g.targets.len()).sum();
            for generator in generators {
                bound.extend(generator.targets.iter().cloned());
            }
            for generator in generators {
                collect_free_names(&generator.iterable, bound, globals, out);
                for condition in &generator.conditions {
                    collect_free_names(condition, bound, globals, out);
                }
            }
            collect_free_names(element, bound, globals, out);
            if let Some(value) = value {
                collect_free_names(value, bound, globals, out);
            }
            bound.truncate(bound.len() - added);
        }
    }
}

/// Splice trailing parameter defaults into `arguments`. In `strict` mode
/// (instantiation, where the signature is authoritative) a still-missing or
/// surplus argument is a compile error, mirroring Python's `TypeError`;
/// otherwise the call is left as written.
fn fill_defaults(
    callee: &str,
    arguments: &mut Vec<IRExpr>,
    params: &[IRParam],
    strict: bool,
) -> Result<()> {
    for param in params.iter().skip(arguments.len()) {
        match &param.default_value {
            Some(default) => arguments.push(default.clone()),
            None if strict => {
                return Err(crate::core::errors::type_error(
                    format!("{callee}() missing required argument: '{}'", param.name),
                    None,
                )
                .into());
            }
            None => break,
        }
    }
    if strict && arguments.len() > params.len() {
        return Err(crate::core::errors::type_error(
            format!(
                "{callee}() takes {} argument(s) but {} were given",
                params.len(),
                arguments.len()
            ),
            None,
        )
        .into());
    }
    Ok(())
}

/// Resolve every class's abstract-method set and effective `__init__`
/// signature by walking its single-inheritance chain within this module.
fn resolve_class_facts(classes: &[IRClass]) -> HashMap<String, ClassFacts> {
    let by_name: HashMap<&str, &IRClass> = classes.iter().map(|c| (c.name.as_str(), c)).collect();

    // Root-to-leaf chain of a class, following the (at most one) base that is
    // a class of this module. Bounded by the class count to survive cycles.
    fn chain_of<'a>(
        class: &'a IRClass,
        by_name: &HashMap<&str, &'a IRClass>,
        max_len: usize,
    ) -> Vec<&'a IRClass> {
        let mut chain = vec![class];
        let mut current = class;
        while let Some(base) = current
            .bases
            .iter()
            .find_map(|b| by_name.get(b.as_str()).copied())
        {
            if chain.len() > max_len {
                break;
            }
            chain.push(base);
            current = base;
        }
        chain.reverse();
        chain
    }

    let is_abstract_method = |decorators: &[String]| {
        decorators
            .iter()
            .any(|d| d == "abstractmethod" || d == "abc.abstractmethod")
    };

    classes
        .iter()
        .map(|class| {
            let chain = chain_of(class, &by_name, classes.len());
            let abc_enabled = chain
                .iter()
                .any(|c| c.bases.iter().any(|b| ABC_MARKERS.contains(&b.as_str())));

            // Walk root to leaf: an `@abstractmethod` declaration marks the
            // name abstract; a later concrete definition of the same name
            // fulfills it.
            let mut unimplemented = BTreeSet::new();
            if abc_enabled {
                for c in &chain {
                    for method in &c.methods {
                        if is_abstract_method(&method.decorators) {
                            unimplemented.insert(method.name.clone());
                        } else {
                            unimplemented.remove(&method.name);
                        }
                    }
                }
            }

            // Nearest `__init__` walking leaf to root.
            let init_params = chain
                .iter()
                .rev()
                .find_map(|c| c.methods.iter().find(|m| m.name == "__init__"))
                .map(|init| init.params.clone())
                .unwrap_or_default();

            (
                class.name.clone(),
                ClassFacts {
                    unimplemented,
                    init_params,
                },
            )
        })
        .collect()
}

/// Apply `f` to every expression in a body, recursing through nested
/// statements and expressions (including call arguments and lambda bodies).
fn visit_body(body: &mut IRBody, f: &mut impl FnMut(&mut IRExpr) -> Result<()>) -> Result<()> {
    for stmt in &mut body.statements {
        visit_stmt(stmt, f)?;
    }
    Ok(())
}

fn visit_stmt(stmt: &mut IRStatement, f: &mut impl FnMut(&mut IRExpr) -> Result<()>) -> Result<()> {
    match stmt {
        IRStatement::Return(expr) | IRStatement::Yield { value: expr } => {
            if let Some(expr) = expr {
                visit_expr(expr, f)?;
            }
        }
        IRStatement::Assign { value, .. }
        | IRStatement::AugAssign { value, .. }
        | IRStatement::TupleUnpack { value, .. }
        | IRStatement::Expression(value)
        | IRStatement::DynamicImport {
            module_name: value, ..
        } => visit_expr(value, f)?,
        IRStatement::Raise { exception } => {
            if let Some(exception) = exception {
                visit_expr(exception, f)?;
            }
        }
        IRStatement::If {
            condition,
            then_body,
            else_body,
        } => {
            visit_expr(condition, f)?;
            visit_body(then_body, f)?;
            if let Some(else_body) = else_body {
                visit_body(else_body, f)?;
            }
        }
        IRStatement::While { condition, body } => {
            visit_expr(condition, f)?;
            visit_body(body, f)?;
        }
        IRStatement::For {
            iterable,
            body,
            else_body,
            ..
        } => {
            visit_expr(iterable, f)?;
            visit_body(body, f)?;
            if let Some(else_body) = else_body {
                visit_body(else_body, f)?;
            }
        }
        IRStatement::With {
            context_expr, body, ..
        } => {
            visit_expr(context_expr, f)?;
            visit_body(body, f)?;
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            visit_body(try_body, f)?;
            for handler in except_handlers {
                visit_body(&mut handler.body, f)?;
            }
            if let Some(finally_body) = finally_body {
                visit_body(finally_body, f)?;
            }
        }
        IRStatement::AttributeAssign { object, value, .. } => {
            visit_expr(object, f)?;
            visit_expr(value, f)?;
        }
        IRStatement::AttributeAugAssign { object, value, .. } => {
            visit_expr(object, f)?;
            visit_expr(value, f)?;
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            visit_expr(container, f)?;
            visit_expr(index, f)?;
            visit_expr(value, f)?;
        }
        IRStatement::ImportModule { .. } | IRStatement::Break | IRStatement::Continue => {}
    }
    Ok(())
}

fn visit_expr(expr: &mut IRExpr, f: &mut impl FnMut(&mut IRExpr) -> Result<()>) -> Result<()> {
    match expr {
        IRExpr::Const(_) | IRExpr::Param(_) | IRExpr::Variable(_) => {}
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            visit_expr(left, f)?;
            visit_expr(right, f)?;
        }
        IRExpr::UnaryOp { operand, .. } => visit_expr(operand, f)?,
        IRExpr::FunctionCall { arguments, .. } => {
            for arg in arguments {
                visit_expr(arg, f)?;
            }
        }
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            for item in items {
                visit_expr(item, f)?;
            }
        }
        IRExpr::DictLiteral(entries) => {
            for (key, value) in entries {
                visit_expr(key, f)?;
                visit_expr(value, f)?;
            }
        }
        IRExpr::Indexing { container, index } => {
            visit_expr(container, f)?;
            visit_expr(index, f)?;
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            visit_expr(container, f)?;
            for bound in [start, end, step].into_iter().flatten() {
                visit_expr(bound, f)?;
            }
        }
        IRExpr::Attribute { object, .. } => visit_expr(object, f)?,
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            visit_expr(element, f)?;
            if let Some(value) = value {
                visit_expr(value, f)?;
            }
            for generator in generators {
                visit_expr(&mut generator.iterable, f)?;
                for condition in &mut generator.conditions {
                    visit_expr(condition, f)?;
                }
            }
        }
        IRExpr::MethodCall {
            object, arguments, ..
        } => {
            visit_expr(object, f)?;
            for arg in arguments {
                visit_expr(arg, f)?;
            }
        }
        IRExpr::DynamicImportExpr { module_name } => visit_expr(module_name, f)?,
        IRExpr::RangeCall {
            start, stop, step, ..
        } => {
            for bound in [start, step].into_iter().flatten() {
                visit_expr(bound, f)?;
            }
            visit_expr(stop, f)?;
        }
        IRExpr::Lambda { body, .. } => visit_expr(body, f)?,
        IRExpr::ClosureMake { .. } => {}
    }
    // Visit the node itself after its children so a rewrite sees fully
    // processed arguments.
    f(expr)
}
