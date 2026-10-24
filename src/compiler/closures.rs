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

use crate::compiler::context::CompilationContext;
use crate::compiler::function::{infer_value_type, resolve_return_type, scan_and_allocate_locals};
use crate::ir::{IRConstant, IRExpr, IRFunction, IRModule, IRType};
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
        let mut next = CaptureTypes::new();
        // Every creator before the closures it makes.
        let mut bodies: Vec<&IRFunction> = typed
            .functions
            .iter()
            .filter(|f| !f.name.starts_with("__lambda_"))
            .collect();
        bodies.extend(typed.classes.iter().flat_map(|c| c.methods.iter()));
        bodies.extend(closures.iter().map(|&i| &typed.functions[i]));
        let classes: Vec<String> = typed.classes.iter().map(|c| c.name.clone()).collect();
        for f in bodies {
            observe(f, &classes, &params, &returns, &mut next, &mut seen);
        }

        let mut changed = next != captures;
        for &i in &closures {
            let f = &mut typed.functions[i];
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

/// Read `func`'s body for what it tells about closures: the type of each cell
/// it stores, the cells of each closure it makes, and the arguments of each
/// call to one.
fn observe(
    func: &IRFunction,
    classes: &[String],
    params: &HashMap<String, Vec<IRType>>,
    returns: &HashMap<String, IRType>,
    captures: &mut CaptureTypes,
    seen: &mut HashMap<String, Vec<Seen>>,
) {
    let mut ctx = CompilationContext::new();
    // The classes, so a constructor call makes an instance of its class (and
    // a list of them a list of instances), which a key is called with. Only
    // the names: nothing is laid out yet.
    for class in classes {
        ctx.add_class(crate::compiler::context::ClassInfo {
            name: class.clone(),
            base: None,
            class_id: 0,
            mro: vec![class.clone()],
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
