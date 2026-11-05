//! Closure typing, run once over the module before code generation.
//!
//! A lambda's parameters carry no annotation, and neither do a nested
//! function's where its author left them off, so each lifted closure would take
//! untyped words: `(lambda kv: kv[1])((1, 5))` could not index its argument,
//! and a lambda over a captured float could not say it returns one. But a
//! lifted closure is called from places the module can see: the direct calls
//! the scope that defines it makes, and the `key=` of a `sorted()`. This pass
//! types each unannotated parameter from those calls, when they agree, and
//! records what each captured cell holds, so the return type of a closure
//! computed from one is known when the type section is built.
//!
//! A parameter whose calls disagree, or that is never called where this can
//! see, stays untyped, which is what it was: an untyped place holds only ints,
//! bools, and None, and every call through a value checks what it passes.
//!
//! A closure passed where a parameter is declared `Callable[[...], R]` (the
//! `repl` of `re.sub`, or a function of the program's own) takes the declared
//! parameter types for its open parameters and `R` for a result it leaves
//! unannotated, so `lambda m: m.group() + "!"` returns a str. The result is
//! then checked against the body's returns as any annotated one is, so a body
//! that returns something else is refused rather than read as an `R`.

use crate::compiler::context::CompilationContext;
use crate::compiler::function::{infer_value_type, resolve_return_type, scan_and_allocate_locals};
use crate::ir::{
    IRClass, IRConstant, IRExpr, IRFunction, IRModule, IRStatement, IRType, IRVariable,
};
use std::collections::HashMap;

/// What each environment cell of a lifted closure holds, keyed by the closure
/// and then by the local that holds the cell's pointer.
pub(crate) type CaptureTypes = HashMap<String, HashMap<String, IRType>>;

/// What the calls a parameter has been seen at pass it.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Nothing,
    One(IRType),
    Conflict,
}

impl Seen {
    fn record(&mut self, ty: IRType) {
        let ty = match ty {
            IRType::Any | IRType::None => IRType::Unknown,
            other => other,
        };
        *self = match std::mem::replace(self, Seen::Nothing) {
            Seen::Nothing => Seen::One(ty),
            Seen::One(seen) if seen == ty => Seen::One(seen),
            // An untyped word is an int as far as anything reading it goes.
            Seen::One(IRType::Unknown) if matches!(ty, IRType::Int) => Seen::One(ty),
            Seen::One(IRType::Int) if matches!(ty, IRType::Unknown) => Seen::One(IRType::Int),
            _ => Seen::Conflict,
        };
    }

    fn resolved(&self) -> IRType {
        match self {
            Seen::One(ty) => ty.clone(),
            _ => IRType::Unknown,
        }
    }
}

/// Type the unannotated parameters of the module's lifted closures from their
/// calls, and the cells they capture from what their creators store. Returns
/// the module with those parameter types filled in.
pub(crate) fn type_closures(module: &IRModule) -> (IRModule, CaptureTypes) {
    let mut typed = module.clone();
    let closures = crate::compiler::module::closures_outermost_first(&typed);
    if closures.is_empty() {
        return (typed, CaptureTypes::new());
    }
    // Which parameters carry no annotation, and so are typed here.
    let open: HashMap<String, Vec<bool>> = closures
        .iter()
        .map(|&i| {
            let f = &module.functions[i];
            let open = f
                .params
                .iter()
                .map(|p| p.name != "__env" && matches!(p.param_type, IRType::Unknown))
                .collect();
            (f.name.clone(), open)
        })
        .collect();

    // Which closures leave their result unannotated, and so take a declared
    // `Callable`'s.
    let open_returns: Vec<bool> = closures
        .iter()
        .map(|&i| matches!(module.functions[i].return_type, IRType::Unknown))
        .collect();

    let mut captures = CaptureTypes::new();
    // Each round can type a value a later closure is called with; three
    // settle any nesting a real program has.
    for _ in 0..3 {
        let params: HashMap<String, Vec<IRType>> = typed
            .functions
            .iter()
            .map(|f| {
                (
                    f.name.clone(),
                    f.params.iter().map(|p| p.param_type.clone()).collect(),
                )
            })
            .collect();
        let mut returns: HashMap<String, IRType> = HashMap::new();
        for _ in 0..2 {
            for f in &typed.functions {
                let rt = resolve_return_type(f, &returns, &params, &captures);
                returns.insert(f.name.clone(), rt);
            }
        }

        let mut seen: HashMap<String, Vec<Seen>> = open
            .iter()
            .map(|(name, open)| (name.clone(), vec![Seen::Nothing; open.len()]))
            .collect();
        let mut results: HashMap<String, Seen> = HashMap::new();
        let mut next = CaptureTypes::new();
        // Every creator before the closures it makes.
        let mut bodies: Vec<&IRFunction> = typed
            .functions
            .iter()
            .filter(|f| !f.name.starts_with("__lambda_"))
            .collect();
        bodies.extend(typed.classes.iter().flat_map(|c| c.methods.iter()));
        bodies.extend(closures.iter().map(|&i| &typed.functions[i]));
        let known = Known {
            functions: &typed.functions,
            classes: &typed.classes,
            variables: &typed.variables,
            params: &params,
            returns: &returns,
        };
        for f in bodies {
            observe(f, &known, &mut next, &mut seen, &mut results);
        }

        let mut changed = next != captures;
        for (&i, &open_return) in closures.iter().zip(&open_returns) {
            let f = &mut typed.functions[i];
            if open_return {
                let ty = results
                    .get(&f.name)
                    .map(Seen::resolved)
                    .unwrap_or(IRType::Unknown);
                if f.return_type != ty {
                    f.return_type = ty;
                    changed = true;
                }
            }
            let (Some(open), Some(seen)) = (open.get(&f.name), seen.get(&f.name)) else {
                continue;
            };
            for (j, param) in f.params.iter_mut().enumerate() {
                if !open[j] {
                    continue;
                }
                let ty = seen[j].resolved();
                if param.param_type != ty {
                    param.param_type = ty;
                    changed = true;
                }
            }
        }
        captures = next;
        if !changed {
            break;
        }
    }
    (typed, captures)
}

/// What the module declares that a body's calls are read against.
struct Known<'a> {
    functions: &'a [IRFunction],
    classes: &'a [IRClass],
    variables: &'a [IRVariable],
    params: &'a HashMap<String, Vec<IRType>>,
    returns: &'a HashMap<String, IRType>,
}

/// Read `func`'s body for what it tells about closures: the type of each cell
/// it stores, the cells of each closure it makes, the arguments of each call
/// to one, and the `Callable` each is passed as.
fn observe(
    func: &IRFunction,
    known: &Known,
    captures: &mut CaptureTypes,
    seen: &mut HashMap<String, Vec<Seen>>,
    results: &mut HashMap<String, Seen>,
) {
    let params = known.params;
    let returns = known.returns;
    let mut ctx = CompilationContext::new();
    // The classes, so a constructor call makes an instance of its class (and
    // a list of them a list of instances), which a key is called with. Only
    // the names: nothing is laid out yet.
    for class in known.classes {
        ctx.add_class(crate::compiler::context::ClassInfo {
            name: class.name.clone(),
            base: None,
            class_id: 0,
            mro: vec![class.name.clone()],
            methods: HashMap::new(),
            method_owner: HashMap::new(),
            method_kinds: HashMap::new(),
            property_setters: HashMap::new(),
            field_offsets: HashMap::new(),
            field_types: HashMap::new(),
            class_var_values: HashMap::new(),
            class_var_owner: HashMap::new(),
            instance_size: 0,
        });
    }
    for (name, ret) in returns {
        let params = params.get(name).cloned().unwrap_or_default();
        ctx.add_function(name, 0, params, ret.clone());
    }
    for param in &func.params {
        ctx.add_local(&param.name, param.param_type.clone());
    }
    if let Some(cells) = captures.get(&func.name) {
        *ctx.cell_types.borrow_mut() = cells.clone();
    }
    ctx.for_loop_seq = 0;
    scan_and_allocate_locals(&func.body, &mut ctx);

    let mut made: Vec<(String, Vec<String>)> = Vec::new();
    let mut body = func.body.clone();
    let _ = crate::ir::visit_body(&mut body, &mut |expr: &mut IRExpr| {
        match expr {
            IRExpr::CellStore { cell, value } => {
                let ty = argument_type(value, &ctx);
                let mut cells = ctx.cell_types.borrow_mut();
                if !matches!(ty, IRType::Unknown) && !cells.contains_key(cell.as_str()) {
                    cells.insert(cell.clone(), ty);
                }
            }
            IRExpr::ClosureMake {
                lambda_name,
                captured,
            } => made.push((lambda_name.clone(), captured.clone())),
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if seen.contains_key(function_name.as_str()) => {
                let slots = seen.get_mut(function_name.as_str()).expect("checked");
                // The last argument of a direct call is the environment.
                let user = arguments.len().saturating_sub(1);
                for (slot, arg) in slots.iter_mut().zip(arguments.iter().take(user)) {
                    slot.record(argument_type(arg, &ctx));
                }
            }
            // A closure passed to a function of the module, where its
            // parameter is declared `Callable`.
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if module_function(known, function_name).is_some() => {
                let callee = module_function(known, function_name).expect("checked");
                observe_callables(&callee.params, arguments, seen, results);
            }
            // A closure passed to a method, found from the receiver's class.
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } if arguments
                .iter()
                .any(|a| matches!(closure_of(a), Some(IRExpr::ClosureMake { .. }))) =>
            {
                if let Some(method) = receiver_class(object, func, &ctx, known)
                    .and_then(|class| find_method(known.classes, &class, method_name))
                {
                    let declared = method.params.get(1..).unwrap_or_default();
                    observe_callables(declared, arguments, seen, results);
                }
            }
            // `min(xs, key=lambda x: ...)` calls the key with each element.
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if matches!(function_name.as_str(), "min" | "max") => {
                let key = arguments.iter().find_map(|a| match a {
                    IRExpr::Keyword { name, value } if name == "key" => Some(value.as_ref()),
                    _ => None,
                });
                let positional: Vec<&IRExpr> = arguments
                    .iter()
                    .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
                    .collect();
                if let Some(IRExpr::ClosureMake { lambda_name, .. }) = key {
                    let elem = match positional.as_slice() {
                        [iterable] => element_type(&argument_type(iterable, &ctx)),
                        several => {
                            let types: Vec<IRType> =
                                several.iter().map(|a| argument_type(a, &ctx)).collect();
                            element_type(&IRType::Tuple(types))
                        }
                    };
                    if let Some(slot) = seen
                        .get_mut(lambda_name.as_str())
                        .and_then(|slots| slots.first_mut())
                    {
                        slot.record(elem);
                    }
                }
            }
            // `functools.reduce(lambda acc, x: ..., xs[, initial])` calls the
            // function with the accumulator (the initial value's type, or an
            // element's) and each element.
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } if method_name == "reduce"
                && matches!(object.as_ref(), IRExpr::Variable(m) if m == "functools") =>
            {
                observe_reduce(arguments, &ctx, seen);
            }
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if function_name == "reduce" && !params.contains_key("reduce") => {
                observe_reduce(arguments, &ctx, seen);
            }
            // `sorted(xs, key=lambda x: ...)` calls the key with each element.
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if matches!(function_name.as_str(), "sorted" | "__sorted_desc") => {
                if let (Some(iterable), Some(IRExpr::ClosureMake { lambda_name, .. })) =
                    (arguments.first(), arguments.get(1))
                {
                    if let Some(slot) = seen
                        .get_mut(lambda_name.as_str())
                        .and_then(|slots| slots.first_mut())
                    {
                        slot.record(element_type(&argument_type(iterable, &ctx)));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    });

    let cells = ctx.cell_types.borrow();
    for (lambda, captured) in made {
        let entry = captures.entry(lambda).or_default();
        for cell in captured {
            if let Some(ty) = cells.get(&cell) {
                entry.insert(cell, ty.clone());
            }
        }
    }
}

/// Record what each closure passed among `arguments` is declared to be, where
/// its parameter in `declared` is a `Callable`: its parameters' types, and the
/// result.
fn observe_callables(
    declared: &[crate::ir::IRParam],
    arguments: &[IRExpr],
    seen: &mut HashMap<String, Vec<Seen>>,
    results: &mut HashMap<String, Seen>,
) {
    let mut position = 0;
    for arg in arguments {
        let param = match arg {
            IRExpr::Keyword { name, .. } => declared.iter().find(|p| p.name == *name),
            _ => {
                position += 1;
                declared.get(position - 1)
            }
        };
        let (
            Some(IRExpr::ClosureMake { lambda_name, .. }),
            Some(IRType::Callable {
                params,
                return_type,
            }),
        ) = (closure_of(arg), param.map(|p| &p.param_type))
        else {
            continue;
        };
        if let Some(slots) = seen.get_mut(lambda_name.as_str()) {
            for (slot, ty) in slots.iter_mut().zip(params) {
                slot.record(ty.clone());
            }
        }
        if !matches!(return_type.as_ref(), IRType::Unknown | IRType::Any) {
            results
                .entry(lambda_name.clone())
                .or_insert(Seen::Nothing)
                .record(return_type.as_ref().clone());
        }
    }
}

/// The closure an argument passes, by position or by name.
fn closure_of(arg: &IRExpr) -> Option<&IRExpr> {
    match arg {
        IRExpr::Keyword { value, .. } => Some(value.as_ref()),
        other => Some(other),
    }
}

/// The module function `name`, outside the closures themselves (whose calls
/// are typed from their arguments instead).
fn module_function<'a>(known: &Known<'a>, name: &str) -> Option<&'a IRFunction> {
    known
        .functions
        .iter()
        .find(|f| f.name == name && !f.name.starts_with("__lambda_"))
}

/// The class of the instance a method is called on: what its type is known to
/// be, or what the module declares for the variable or class variable it is.
fn receiver_class(
    object: &IRExpr,
    func: &IRFunction,
    ctx: &CompilationContext,
    known: &Known,
) -> Option<String> {
    match argument_type(object, ctx) {
        IRType::Class(class) => return Some(class),
        IRType::Unknown => {}
        _ => return None,
    }
    // A local the function assigns at its top level, every time with an
    // instance of one class.
    if let IRExpr::Variable(name) = object {
        let mut assigned = func.body.statements.iter().filter_map(|stmt| match stmt {
            IRStatement::Assign { target, value, .. } if target == name => {
                Some(declared_class(value, known, 0))
            }
            _ => None,
        });
        if let Some(first) = assigned.next() {
            return first.filter(|class| assigned.all(|c| c.as_ref() == Some(class)));
        }
    }
    declared_class(object, known, 0)
}

/// The class of the instance `expr` makes or names, read from the module's
/// declarations: a constructor call, or a module variable or class variable
/// annotated with a class or holding such a value.
fn declared_class(expr: &IRExpr, known: &Known, depth: u32) -> Option<String> {
    let var = |vars: &[IRVariable], name: &str| -> Option<String> {
        let var = vars.iter().find(|v| v.name == name)?;
        match &var.var_type {
            Some(IRType::Class(class)) => Some(class.clone()),
            _ if depth < 4 => declared_class(&var.value, known, depth + 1),
            _ => None,
        }
    };
    match expr {
        IRExpr::FunctionCall { function_name, .. }
            if known.classes.iter().any(|c| c.name == *function_name) =>
        {
            Some(function_name.clone())
        }
        IRExpr::Variable(name) => var(known.variables, name),
        IRExpr::Attribute { object, attribute } => match object.as_ref() {
            IRExpr::Variable(class) => {
                let class = known.classes.iter().find(|c| c.name == *class)?;
                var(&class.class_vars, attribute)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The method `name` a class has, its own or the first of its bases'.
fn find_method<'a>(classes: &'a [IRClass], class: &str, name: &str) -> Option<&'a IRFunction> {
    let class = classes.iter().find(|c| c.name == class)?;
    class.methods.iter().find(|m| m.name == name).or_else(|| {
        class
            .bases
            .iter()
            .find_map(|base| find_method(classes, base, name))
    })
}

/// Type a lambda passed to `reduce` from the sequence and initial value.
fn observe_reduce(
    arguments: &[IRExpr],
    ctx: &CompilationContext,
    seen: &mut HashMap<String, Vec<Seen>>,
) {
    let (Some(IRExpr::ClosureMake { lambda_name, .. }), Some(iterable)) =
        (arguments.first(), arguments.get(1))
    else {
        return;
    };
    let elem = element_type(&argument_type(iterable, ctx));
    let acc = match arguments.get(2) {
        Some(initial) => argument_type(initial, ctx),
        None => elem.clone(),
    };
    if let Some(slots) = seen.get_mut(lambda_name.as_str()) {
        if let [first, second, ..] = slots.as_mut_slice() {
            first.record(acc);
            second.record(elem);
        }
    }
}

/// The type an argument expression is known to have before code generation:
/// what [`infer_value_type`] knows, plus the int and bool literals and the
/// tuple and list literals built of them, which it leaves to code generation.
fn argument_type(expr: &IRExpr, ctx: &CompilationContext) -> IRType {
    match expr {
        IRExpr::Const(IRConstant::Int(_)) => IRType::Int,
        IRExpr::Const(IRConstant::Bool(_)) => IRType::Bool,
        IRExpr::TupleLiteral(items) => {
            IRType::Tuple(items.iter().map(|i| argument_type(i, ctx)).collect())
        }
        IRExpr::ListLiteral(items) => {
            let types: Vec<IRType> = items.iter().map(|i| argument_type(i, ctx)).collect();
            match types.first() {
                Some(first) if types.iter().all(|t| t == first) => {
                    IRType::List(Box::new(first.clone()))
                }
                _ => IRType::List(Box::new(IRType::Unknown)),
            }
        }
        IRExpr::UnaryOp { operand, .. } => match argument_type(operand, ctx) {
            t @ (IRType::Int | IRType::Float) => t,
            _ => infer_value_type(expr, ctx),
        },
        other => infer_value_type(other, ctx),
    }
}

/// What iterating a value of `ty` yields, where that is one type.
fn element_type(ty: &IRType) -> IRType {
    match ty {
        IRType::List(e) | IRType::Set(e) | IRType::VarTuple(e) => e.as_ref().clone(),
        IRType::Tuple(members) => match members.first() {
            Some(first) if members.iter().all(|m| m == first) => first.clone(),
            _ => IRType::Unknown,
        },
        IRType::Dict(k, _) => k.as_ref().clone(),
        IRType::String => IRType::String,
        _ => IRType::Unknown,
    }
}
