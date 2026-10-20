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
    IRBody, IRClass, IRExpr, IRFunction, IRModule, IRParam, IRStatement, IRType, ParamStar,
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

    name_function_values(module);
    lift_lambdas(module)?;

    Ok(())
}

/// Builtins that can be passed as a value (`sorted(words, key=len)`), each a
/// function of one argument. The type names (`int`, `str`, ...) are not among
/// them: as a value they are a type, read by `isinstance` and its kin.
const VALUE_BUILTINS: [&str; 2] = ["len", "abs"];

/// A module function or one of [`VALUE_BUILTINS`] named as a value rather
/// than called (`apply(double, 3)`, `key=len`) becomes the lambda that calls
/// it, `lambda x: double(x)`, which lifts to a closure like any other. A
/// function's lambda takes its annotated parameters; a builtin's takes one,
/// typed from where the value is called. Such a name was refused as undefined.
fn name_function_values(module: &mut IRModule) {
    let functions: HashMap<String, Vec<IRParam>> = module
        .functions
        .iter()
        .filter(|f| !f.name.starts_with("__") && f.params.iter().all(|p| p.star == ParamStar::None))
        .map(|f| (f.name.clone(), f.params.clone()))
        .collect();
    let shadowing: HashSet<String> = module
        .variables
        .iter()
        .map(|v| v.name.clone())
        .chain(module.classes.iter().map(|c| c.name.clone()))
        .collect();
    let as_value = |name: &str, bound: &HashSet<String>| -> Option<IRExpr> {
        if bound.contains(name) || shadowing.contains(name) {
            return None;
        }
        let params: Vec<IRParam> = match functions.get(name) {
            Some(params) => params
                .iter()
                .map(|p| IRParam {
                    default_value: None,
                    keyword_only: false,
                    ..p.clone()
                })
                .collect(),
            None if VALUE_BUILTINS.contains(&name) => vec![IRParam {
                name: "__value".to_string(),
                param_type: IRType::Unknown,
                default_value: None,
                keyword_only: false,
                star: ParamStar::None,
            }],
            None => return None,
        };
        let arguments = params
            .iter()
            .map(|p| IRExpr::Variable(p.name.clone()))
            .collect();
        Some(IRExpr::Lambda {
            params,
            body: Box::new(IRExpr::FunctionCall {
                function_name: name.to_string(),
                arguments,
            }),
            captured_vars: Vec::new(),
        })
    };
    let rewrite_body = |params: &[IRParam], body: &mut IRBody| {
        let mut bound: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
        collect_bound_names(body, &mut bound);
        let _ = visit_body(body, &mut |expr: &mut IRExpr| {
            if let IRExpr::Variable(name) = expr {
                if let Some(lambda) = as_value(name, &bound) {
                    *expr = lambda;
                }
            }
            Ok(())
        });
    };
    for func in &mut module.functions {
        rewrite_body(&func.params, &mut func.body);
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            rewrite_body(&method.params, &mut method.body);
        }
    }
    let empty = HashSet::new();
    for var in &mut module.variables {
        let _ = visit_expr(&mut var.value, &mut |expr: &mut IRExpr| {
            if let IRExpr::Variable(name) = expr {
                if let Some(lambda) = as_value(name, &empty) {
                    *expr = lambda;
                }
            }
            Ok(())
        });
    }
}

/// Move every keyword argument of a call to a function or class of this module
/// into its parameter's position, filling any parameter skipped in between with
/// its default.
///
/// Keywords on a plain call used to be dropped, so `f(1, b=5)` ran `f(1)` with
/// `b` at its default and `enumerate(xs, start=1)` counted from 0, each
/// reporting success. A keyword this pass does not place (a builtin, a closure,
/// a function from another file) stays in the call, and code generation
/// refuses it.
///
/// Runs before the other module passes, which walk expressions with wildcard
/// arms that would not descend into a keyword's value.
pub fn resolve_keyword_arguments(module: &mut IRModule) -> Result<()> {
    // A method call never places keywords, so a keyword-only parameter of a
    // method could not be passed at all.
    for class in &module.classes {
        for method in &class.methods {
            if method.name == "__init__" {
                continue;
            }
            if method.params.iter().any(|p| p.keyword_only) {
                return Err(crate::core::errors::unsupported_feature(
                    format!(
                        "keyword-only parameters are supported on functions and __init__, \
                         not on the method '{}.{}'. Hint: make them positional",
                        class.name, method.name
                    ),
                    None,
                )
                .into());
            }
            if let Some(p) = method.params.iter().find(|p| p.star != ParamStar::None) {
                let star = if p.star == ParamStar::Args { "*" } else { "**" };
                return Err(crate::core::errors::unsupported_feature(
                    format!(
                        "'{star}{}' is supported on functions and __init__, not on the method \
                         '{}.{}'. Hint: take a list or a dict parameter",
                        p.name, class.name, method.name
                    ),
                    None,
                )
                .into());
            }
        }
    }
    let facts = resolve_class_facts(&module.classes);
    // A lifted nested function is only ever called from the direct calls
    // `lift_nested_functions` already bound, environment included.
    let mut signatures: HashMap<String, Vec<IRParam>> = module
        .functions
        .iter()
        .filter(|f| !f.name.starts_with("__lambda_"))
        .map(|f| (f.name.clone(), f.params.clone()))
        .collect();
    for (name, class_facts) in &facts {
        // Instantiation supplies `self`. With no `__init__` along the chain the
        // signature is not this module's (an exception class takes its
        // base's arguments).
        if class_facts.init_params.is_empty() {
            continue;
        }
        let params = class_facts.init_params.iter().skip(1).cloned().collect();
        signatures.insert(name.clone(), params);
    }

    let mut rewrite = |expr: &mut IRExpr| -> Result<()> {
        if let IRExpr::FunctionCall {
            function_name,
            arguments,
        } = expr
        {
            if let Some(params) = signatures.get(function_name.as_str()) {
                place_keywords(function_name, arguments, params)?;
            }
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
    for var in &mut module.variables {
        visit_expr(&mut var.value, &mut rewrite)?;
    }
    Ok(())
}

/// Whether evaluating `expr` can have an effect another argument could
/// observe, so moving it changes the program.
fn may_have_effects(expr: &IRExpr) -> bool {
    !matches!(
        expr,
        IRExpr::Const(_) | IRExpr::Variable(_) | IRExpr::Param(_) | IRExpr::Lambda { .. }
    )
}

/// Bind a call's arguments to the parameters of a function or constructor of
/// this module, leaving one argument per parameter in parameter order.
///
/// Positional arguments fill the positional parameters, and any beyond them go
/// to `*args` as a tuple; keywords fill the parameter they name, and any no
/// parameter takes go to `**kwargs` as a dict. `f(*xs)` passes a copy of `xs`
/// as the `*args` tuple. A skipped parameter takes its default.
///
/// The signature is this module's own, so it is authoritative: a surplus or
/// missing argument is CPython's `TypeError`, at compile time. The values are
/// evaluated in parameter order once bound, which is the order they were
/// written in unless the call reorders two of them; that is refused when one
/// of those has an effect.
fn place_keywords(callee: &str, arguments: &mut Vec<IRExpr>, params: &[IRParam]) -> Result<()> {
    let type_error = |message: String| -> anyhow::Error {
        crate::core::errors::type_error(message, None).into()
    };
    let refuse = |message: String| -> anyhow::Error {
        crate::core::errors::unsupported_feature(message, None).into()
    };

    let mut positional = Vec::new();
    let mut keywords: Vec<(String, IRExpr)> = Vec::new();
    let mut spread: Option<IRExpr> = None;
    for arg in arguments.drain(..) {
        match arg {
            IRExpr::Keyword { name, value } if name == crate::ir::STARRED_ARG => {
                if spread.is_some() || !keywords.is_empty() {
                    return Err(refuse(format!(
                        "a call to {callee}() unpacks an argument after another or after a \
                         keyword, which is not supported. Hint: unpack one sequence, last \
                         among the positional arguments"
                    )));
                }
                spread = Some(*value);
            }
            IRExpr::Keyword { name, value } if name.starts_with('*') => {
                let _ = value;
                return Err(refuse(format!(
                    "'**' unpacking in a call to {callee}() is not supported. Hint: pass the \
                     keywords by name"
                )));
            }
            IRExpr::Keyword { name, value } => keywords.push((name, *value)),
            other => {
                if !keywords.is_empty() {
                    return Err(type_error(format!(
                        "a positional argument follows a keyword argument in a call to {callee}()"
                    )));
                }
                if spread.is_some() {
                    return Err(refuse(format!(
                        "a positional argument after '*' unpacking in a call to {callee}() is \
                         not supported. Hint: unpack last"
                    )));
                }
                positional.push(other);
            }
        }
    }

    let fixed: Vec<usize> = (0..params.len())
        .filter(|&i| params[i].star == ParamStar::None && !params[i].keyword_only)
        .collect();
    let var_positional = params.iter().position(|p| p.star == ParamStar::Args);
    let var_keyword = params.iter().position(|p| p.star == ParamStar::Kwargs);

    let mut slots: Vec<Option<IRExpr>> = vec![None; params.len()];
    // Where each argument lands, in the order it was written: the parameter,
    // the position inside `*args` or `**kwargs`, and whether it has an effect.
    let mut order: Vec<(usize, usize, bool)> = Vec::new();
    let mut surplus = Vec::new();
    let given = positional.len();
    for (i, value) in positional.into_iter().enumerate() {
        let effects = may_have_effects(&value);
        if let Some(&p) = fixed.get(i) {
            order.push((p, 0, effects));
            slots[p] = Some(value);
        } else if let Some(p) = var_positional {
            order.push((p, surplus.len(), effects));
            surplus.push(value);
        } else {
            return Err(type_error(format!(
                "{callee}() takes {} positional argument(s) but {given} were given",
                fixed.len()
            )));
        }
    }
    if let Some(value) = spread {
        let Some(p) = var_positional else {
            return Err(refuse(format!(
                "'*' unpacking in a call to {callee}(), which has no *args parameter, is not \
                 supported. Hint: pass the arguments one by one"
            )));
        };
        if given != fixed.len() {
            return Err(refuse(format!(
                "'*' unpacking in a call to {callee}() has to follow exactly one argument for \
                 each of its {} positional parameters",
                fixed.len()
            )));
        }
        order.push((p, 0, may_have_effects(&value)));
        // The callee gets a tuple of its own, as in CPython, rather than the
        // caller's list.
        slots[p] = Some(IRExpr::Slicing {
            container: Box::new(value),
            start: None,
            end: None,
            step: None,
        });
    }

    let mut extra: Vec<(IRExpr, IRExpr)> = Vec::new();
    for (name, value) in keywords {
        let effects = may_have_effects(&value);
        match params
            .iter()
            .position(|p| p.name == name && p.star == ParamStar::None)
        {
            Some(index) => {
                if slots[index].is_some() {
                    return Err(type_error(format!(
                        "{callee}() got multiple values for argument '{name}'"
                    )));
                }
                order.push((index, 0, effects));
                slots[index] = Some(value);
            }
            None => {
                let Some(p) = var_keyword else {
                    return Err(type_error(format!(
                        "{callee}() got an unexpected keyword argument '{name}'"
                    )));
                };
                let key = IRExpr::Const(crate::ir::IRConstant::String(name.clone()));
                if extra.iter().any(|(k, _)| matches!(k, IRExpr::Const(crate::ir::IRConstant::String(n)) if *n == name)) {
                    return Err(type_error(format!(
                        "{callee}() got multiple values for keyword argument '{name}'"
                    )));
                }
                order.push((p, extra.len(), effects));
                extra.push((key, value));
            }
        }
    }

    let reordered = order
        .windows(2)
        .any(|w| (w[0].0, w[0].1) > (w[1].0, w[1].1));
    if reordered && order.iter().any(|&(_, _, effects)| effects) {
        return Err(refuse(format!(
            "keyword arguments to {callee}() written out of parameter order, where one of them \
             is computed, would be evaluated in a different order than Python's. Hint: pass \
             them in parameter order, or compute the values into variables first"
        )));
    }

    let mut surplus = Some(surplus);
    let mut extra = Some(extra);
    for (index, param) in params.iter().enumerate() {
        if slots[index].is_some() {
            continue;
        }
        slots[index] = Some(match param.star {
            ParamStar::Args => IRExpr::TupleLiteral(surplus.take().unwrap_or_default()),
            ParamStar::Kwargs => IRExpr::DictLiteral(extra.take().unwrap_or_default()),
            ParamStar::None => match &param.default_value {
                Some(default) => default.clone(),
                None => {
                    return Err(type_error(format!(
                        "{callee}() missing required argument: '{}'",
                        param.name
                    )))
                }
            },
        });
    }
    *arguments = slots.into_iter().flatten().collect();
    Ok(())
}

/// A nested function visible from a scope: the module function it was lifted
/// to and the parameters a direct call is placed against.
#[derive(Clone)]
struct NestedDef {
    lifted: String,
    params: Vec<IRParam>,
    /// The scope that defines it, so a call that runs before the `def` can be
    /// told apart from one inside a later function body.
    owner: String,
}

/// Lift every nested `def` into a module function (#141).
///
/// Runs after the decorator and metaclass passes (which read a decorator's
/// nested wrapper as written) and before every other module pass, so the
/// lifted functions take part in keyword placement, the generator and `with`
/// passes, and lambda lifting like any function written at module level.
///
/// A nested `def name(params)` becomes a module function `__lambda_{n}_name`
/// with the same parameters, annotations, and return type, plus a trailing
/// `__env` parameter: the environment that carries the cell of every variable
/// it closes over, exactly the shape a lifted lambda has. The `def` statement
/// itself becomes `name = <closure>`, since that is what it is in Python: a
/// binding, at that point in the body, of a function value over the enclosing
/// variables. A call by name in the scope that can see it is compiled as a
/// direct call to the lifted function with the closure passed as `__env`, so
/// it is fully typed; any other use of the name is the closure value, called
/// through the function table.
///
/// `nonlocal x` makes `x` a variable of the enclosing function, which the
/// closure reaches through its cell, so an assignment rebinds it for every
/// reader. A nested function inside a generator, a `def` inside a block, and a
/// call that runs before the `def` is reached are refused: each would need a
/// binding that does not exist yet at run time.
pub fn lift_nested_functions(module: &mut IRModule) -> Result<()> {
    let mut lifted: Vec<IRFunction> = Vec::new();
    for func in &mut module.functions {
        lift_scope(
            &func.name,
            &func.params,
            &mut func.body,
            &HashMap::new(),
            &[],
            &mut lifted,
        )?;
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            let owner = format!("{}.{}", class.name, method.name);
            lift_scope(
                &owner,
                &method.params,
                &mut method.body,
                &HashMap::new(),
                &[],
                &mut lifted,
            )?;
        }
    }
    module.functions.extend(lifted);
    Ok(())
}

fn nested_error(message: String) -> anyhow::Error {
    crate::core::errors::unsupported_feature(message, None).into()
}

/// Lift the nested functions of one function body, `owner`, whose parameters
/// are `params`. `visible` holds the nested functions of the enclosing scopes
/// that this body can call by name, and `enclosing` the names each enclosing
/// function binds, innermost last.
fn lift_scope(
    owner: &str,
    params: &[IRParam],
    body: &mut IRBody,
    visible: &HashMap<String, NestedDef>,
    enclosing: &[HashSet<String>],
    lifted: &mut Vec<IRFunction>,
) -> Result<()> {
    let nonlocals = declared_names(body, true);
    let globals = declared_names(body, false);
    for name in &nonlocals {
        if !enclosing.iter().any(|scope| scope.contains(name)) {
            return Err(crate::core::errors::parse_error(
                format!("no binding for nonlocal '{name}' found (in function '{owner}')"),
                None,
                None,
            )
            .into());
        }
        if visible.contains_key(name) {
            return Err(nested_error(format!(
                "'nonlocal {name}' in '{owner}' names a nested function, which cannot be \
                 rebound. Hint: give the variable another name"
            )));
        }
        if globals.contains(name) {
            return Err(crate::core::errors::parse_error(
                format!("name '{name}' is nonlocal and global (in function '{owner}')"),
                None,
                None,
            )
            .into());
        }
    }
    strip_nonlocal(body);
    if !body_yields(body) {
        name_lambdas(body, params, &nonlocals, &globals);
    }

    // The nested functions of this body, which must sit at its top level.
    let mut defs: Vec<String> = Vec::new();
    for stmt in &body.statements {
        if let IRStatement::FunctionDef(def) = stmt {
            if defs.contains(&def.name) {
                return Err(nested_error(format!(
                    "the nested function '{}' is defined twice in '{owner}'. Hint: give the \
                     second one another name",
                    def.name
                )));
            }
            defs.push(def.name.clone());
        }
        for nested in nested_bodies_ref(stmt) {
            if contains_def(nested) {
                return Err(nested_error(format!(
                    "a 'def' inside an if, loop, 'try', or 'with' block of '{owner}' is not \
                     supported. Hint: define the nested function at the top level of the \
                     enclosing function's body"
                )));
            }
        }
    }
    if defs.is_empty() {
        let visible = narrowed(visible, params, body, &nonlocals);
        return rewrite_nested_calls(owner, body, &visible, &[]);
    }
    if body_yields(body) {
        return Err(nested_error(format!(
            "a nested function inside the generator '{owner}' is not supported. Hint: define \
             it at module level"
        )));
    }

    let mut bound: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    collect_bound_names(body, &mut bound);
    for def in &defs {
        if bound.contains(def) || nonlocals.contains(def) || globals.contains(def) {
            return Err(nested_error(format!(
                "the nested function '{def}' is also bound another way in '{owner}'. Hint: \
                 give the function or the variable another name"
            )));
        }
    }
    bound.extend(defs.iter().cloned());
    for name in &nonlocals {
        bound.remove(name);
    }

    let mut visible = narrowed(visible, params, body, &nonlocals);
    for name in &defs {
        let id = LAMBDA_COUNTER.fetch_add(1, Ordering::Relaxed);
        visible.insert(
            name.clone(),
            NestedDef {
                // The qualified name follows the counter, so an error inside
                // it names the function as Python would ('outer.inner').
                lifted: format!("__lambda_{id}_{owner}.{name}"),
                params: Vec::new(),
                owner: owner.to_string(),
            },
        );
    }
    for stmt in &body.statements {
        if let IRStatement::FunctionDef(def) = stmt {
            visible.get_mut(&def.name).expect("registered above").params = def.params.clone();
        }
    }

    let mut inner_enclosing = enclosing.to_vec();
    inner_enclosing.push(bound);
    for stmt in &mut body.statements {
        let IRStatement::FunctionDef(def) = stmt else {
            continue;
        };
        let mut def = std::mem::replace(def, Box::new(placeholder_function()));
        let qualified = format!("{owner}.{}", def.name);
        if body_yields(&def.body) {
            return Err(nested_error(format!(
                "the nested generator '{qualified}' is not supported. Hint: define it at \
                 module level"
            )));
        }
        for param in &def.params {
            if let Some(default) = &param.default_value {
                if !is_constant_default(default) {
                    return Err(nested_error(format!(
                        "the default of parameter '{}' of the nested function '{qualified}' \
                         is computed, and would be evaluated at each call rather than once at \
                         the 'def'. Hint: use a constant default",
                        param.name
                    )));
                }
            }
        }
        let def_nonlocals = declared_names(&def.body, true);
        lift_scope(
            &qualified,
            &def.params,
            &mut def.body,
            &visible,
            &inner_enclosing,
            lifted,
        )?;

        // What the function closes over: every name it reads that it does not
        // bind itself and that an enclosing function does, plus its nonlocals.
        let mut own: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
        let mut own_set: HashSet<String> = HashSet::new();
        collect_bound_names(&def.body, &mut own_set);
        own.extend(own_set.into_iter().filter(|n| !def_nonlocals.contains(n)));
        let mut free = Vec::new();
        collect_body_free_names(&def.body, &mut own, &mut free);
        // A nonlocal the function only assigns is never read, but it is still
        // the enclosing variable.
        let mut def_nonlocals: Vec<String> = def_nonlocals.into_iter().collect();
        def_nonlocals.sort();
        for name in def_nonlocals {
            if !free.contains(&name) {
                free.push(name);
            }
        }
        let captured: Vec<String> = free
            .into_iter()
            .filter(|name| inner_enclosing.iter().any(|scope| scope.contains(name)))
            .collect();

        let mut fn_params = def.params.clone();
        fn_params.push(IRParam {
            name: "__env".to_string(),
            param_type: IRType::Int,
            default_value: None,
            keyword_only: false,
            star: crate::ir::ParamStar::None,
        });
        let mut statements: Vec<IRStatement> = captured
            .iter()
            .enumerate()
            .map(|(k, cap)| IRStatement::Assign {
                target: cell_local(cap),
                value: IRExpr::EnvRead {
                    env: "__env".to_string(),
                    slot: k as u32,
                },
                var_type: Some(IRType::Int),
            })
            .collect();
        statements.append(&mut def.body.statements);
        let lifted_name = visible[&def.name].lifted.clone();
        lifted.push(IRFunction {
            name: lifted_name.clone(),
            params: fn_params,
            body: IRBody { statements },
            return_type: def.return_type.clone(),
            decorators: Vec::new(),
        });
        *stmt = IRStatement::Assign {
            target: def.name.clone(),
            value: IRExpr::ClosureMake {
                lambda_name: lifted_name,
                captured,
            },
            var_type: None,
        };
    }

    rewrite_nested_calls(owner, body, &visible, &defs)
}

/// `name = lambda params: body` at the top level of a function, where `name`
/// is bound nowhere else, is `def name(params): return body`: Python gives
/// both the same meaning but for the function's `__name__`. As a nested
/// function it is called directly, with keywords and defaults, and its
/// parameters are typed from those calls.
fn name_lambdas(
    body: &mut IRBody,
    params: &[IRParam],
    nonlocals: &HashSet<String>,
    globals: &HashSet<String>,
) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    count_bindings(body, &mut counts);
    for stmt in &mut body.statements {
        let IRStatement::Assign {
            target,
            value:
                IRExpr::Lambda {
                    params: lambda_params,
                    body: lambda_body,
                    ..
                },
            var_type: None,
        } = stmt
        else {
            continue;
        };
        let once = counts.get(target.as_str()) == Some(&1)
            && !params.iter().any(|p| &p.name == target)
            && !nonlocals.contains(target)
            && !globals.contains(target);
        let constant_defaults = lambda_params
            .iter()
            .all(|p| p.default_value.as_ref().map_or(true, is_constant_default));
        if !once || !constant_defaults {
            continue;
        }
        let function = IRFunction {
            name: target.clone(),
            params: lambda_params.clone(),
            body: IRBody {
                statements: vec![IRStatement::Return(Some((**lambda_body).clone()))],
            },
            return_type: IRType::Unknown,
            decorators: Vec::new(),
        };
        *stmt = IRStatement::FunctionDef(Box::new(function));
    }
}

/// How many times each name is bound in a body, nested blocks included.
fn count_bindings(body: &IRBody, counts: &mut HashMap<String, usize>) {
    for stmt in &body.statements {
        let mut bind = |name: &String| *counts.entry(name.clone()).or_default() += 1;
        match stmt {
            IRStatement::Assign { target, .. }
            | IRStatement::AugAssign { target, .. }
            | IRStatement::For { target, .. }
            | IRStatement::DynamicImport { target, .. } => bind(target),
            IRStatement::With {
                optional_vars: Some(target),
                ..
            } => bind(target),
            IRStatement::TupleUnpack { targets, .. } => targets.iter().for_each(bind),
            IRStatement::FunctionDef(def) => bind(&def.name),
            IRStatement::TryExcept {
                except_handlers, ..
            } => except_handlers
                .iter()
                .filter_map(|h| h.name.as_ref())
                .for_each(bind),
            _ => {}
        }
        for nested in nested_bodies_ref(stmt) {
            count_bindings(nested, counts);
        }
    }
}

/// `visible` without the names this body binds itself, which shadow them.
fn narrowed(
    visible: &HashMap<String, NestedDef>,
    params: &[IRParam],
    body: &IRBody,
    nonlocals: &HashSet<String>,
) -> HashMap<String, NestedDef> {
    let mut shadowing: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    collect_bound_names(body, &mut shadowing);
    visible
        .iter()
        .filter(|(name, _)| !shadowing.contains(*name) || nonlocals.contains(*name))
        .map(|(name, def)| (name.clone(), def.clone()))
        .collect()
}

/// Rewrite every call by name to a visible nested function into a direct call
/// of the lifted function, with the closure as its environment argument.
/// `defs` are the functions this body defines itself, in order: a call that
/// runs before its `def` statement would see no binding.
fn rewrite_nested_calls(
    owner: &str,
    body: &mut IRBody,
    visible: &HashMap<String, NestedDef>,
    defs: &[String],
) -> Result<()> {
    if visible.is_empty() {
        return Ok(());
    }
    let mut defined: HashSet<String> = HashSet::new();
    for stmt in &mut body.statements {
        let mut rewrite = |expr: &mut IRExpr| -> Result<()> {
            if let IRExpr::Lambda { params, .. } = expr {
                if let Some(p) = params.iter().find(|p| visible.contains_key(&p.name)) {
                    return Err(nested_error(format!(
                        "the lambda parameter '{}' in '{owner}' shadows a nested function. \
                         Hint: rename the parameter",
                        p.name
                    )));
                }
                return Ok(());
            }
            let IRExpr::FunctionCall {
                function_name,
                arguments,
            } = expr
            else {
                return Ok(());
            };
            let Some(def) = visible.get(function_name.as_str()) else {
                return Ok(());
            };
            if def.owner == owner
                && defs.contains(function_name)
                && !defined.contains(function_name)
            {
                return Err(nested_error(format!(
                    "'{function_name}' is called in '{owner}' before its 'def' runs, where \
                     Python raises UnboundLocalError. Hint: move the 'def' above the call"
                )));
            }
            place_keywords(function_name, arguments, &def.params)?;
            fill_defaults(function_name, arguments, &def.params, true)?;
            arguments.push(IRExpr::Variable(function_name.clone()));
            *function_name = def.lifted.clone();
            Ok(())
        };
        visit_stmt(stmt, &mut rewrite)?;
        if let IRStatement::Assign {
            target,
            value: IRExpr::ClosureMake { .. },
            ..
        } = stmt
        {
            defined.insert(target.clone());
        }
    }
    Ok(())
}

/// The names a body declares `nonlocal` (or, with `nonlocal` false, `global`),
/// anywhere in it but inside a nested function.
fn declared_names(body: &IRBody, nonlocal: bool) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in &body.statements {
        match stmt {
            IRStatement::Nonlocal(names) if nonlocal => out.extend(names.iter().cloned()),
            IRStatement::Global(names) if !nonlocal => out.extend(names.iter().cloned()),
            _ => {}
        }
        for nested in nested_bodies_ref(stmt) {
            out.extend(declared_names(nested, nonlocal));
        }
    }
    out
}

fn strip_nonlocal(body: &mut IRBody) {
    body.statements
        .retain(|stmt| !matches!(stmt, IRStatement::Nonlocal(_)));
    for stmt in &mut body.statements {
        for nested in nested_bodies_mut(stmt) {
            strip_nonlocal(nested);
        }
    }
}

fn contains_def(body: &IRBody) -> bool {
    body.statements.iter().any(|stmt| {
        matches!(stmt, IRStatement::FunctionDef(_))
            || nested_bodies_ref(stmt).into_iter().any(contains_def)
    })
}

fn body_yields(body: &IRBody) -> bool {
    body.statements.iter().any(|stmt| {
        matches!(stmt, IRStatement::Yield { .. })
            || nested_bodies_ref(stmt).into_iter().any(body_yields)
    })
}

/// A default value that evaluates to the same thing wherever it is evaluated.
fn is_constant_default(expr: &IRExpr) -> bool {
    match expr {
        IRExpr::Const(_) => true,
        IRExpr::UnaryOp { operand, .. } => is_constant_default(operand),
        IRExpr::TupleLiteral(items) => items.iter().all(is_constant_default),
        _ => false,
    }
}

fn placeholder_function() -> IRFunction {
    IRFunction {
        name: String::new(),
        params: Vec::new(),
        body: IRBody {
            statements: Vec::new(),
        },
        return_type: IRType::Unknown,
        decorators: Vec::new(),
    }
}

/// The names a function body reads without binding them, in first-use order.
/// Unlike a lambda's, the check against module names is left to the caller,
/// which knows which enclosing function binds what: a local of the enclosing
/// function that shadows a module name is still captured.
fn collect_body_free_names(body: &IRBody, bound: &mut Vec<String>, out: &mut Vec<String>) {
    let none = HashSet::new();
    for stmt in &body.statements {
        let mut exprs: Vec<&IRExpr> = Vec::new();
        match stmt {
            IRStatement::Return(Some(e))
            | IRStatement::Yield { value: Some(e) }
            | IRStatement::Raise {
                exception: Some(e), ..
            }
            | IRStatement::Expression(e)
            | IRStatement::Assign { value: e, .. }
            | IRStatement::TupleUnpack { value: e, .. }
            | IRStatement::If { condition: e, .. }
            | IRStatement::While { condition: e, .. }
            | IRStatement::For { iterable: e, .. }
            | IRStatement::With {
                context_expr: e, ..
            }
            | IRStatement::DynamicImport { module_name: e, .. } => exprs.push(e),
            IRStatement::AugAssign { target, value, .. } => {
                if !bound.contains(target) && !out.contains(target) {
                    out.push(target.clone());
                }
                exprs.push(value);
            }
            IRStatement::AttributeAssign { object, value, .. }
            | IRStatement::AttributeAugAssign { object, value, .. } => {
                exprs.push(object);
                exprs.push(value);
            }
            IRStatement::IndexAssign {
                container,
                index,
                value,
            } => {
                exprs.push(container);
                exprs.push(index);
                exprs.push(value);
            }
            _ => {}
        }
        for expr in exprs {
            collect_free_names(expr, bound, &none, out);
        }
        for nested in nested_bodies_ref(stmt) {
            collect_body_free_names(nested, bound, out);
        }
    }
}

/// The variables a lifted function reads out of its environment: the targets
/// of the `__cell_x = __env[k]` prelude its lifting put at the top.
fn env_cells(body: &IRBody) -> HashSet<String> {
    body.statements
        .iter()
        .map_while(|stmt| match stmt {
            IRStatement::Assign {
                target,
                value: IRExpr::EnvRead { .. },
                ..
            } => target.strip_prefix("__cell_").map(str::to_string),
            _ => None,
        })
        .collect()
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

        let mut fn_params = params.clone();
        fn_params.push(IRParam {
            name: "__env".to_string(),
            param_type: IRType::List(Box::new(IRType::Unknown)),
            default_value: None,
            keyword_only: false,
            star: crate::ir::ParamStar::None,
        });

        // __cell_k = __env[k] preludes, then return the lambda body. The
        // environment carries cell *pointers*, so the body reads through them
        // and sees the current value rather than a copy taken at creation.
        let mut statements: Vec<IRStatement> = captured
            .iter()
            .enumerate()
            .map(|(k, cap)| IRStatement::Assign {
                target: cell_local(cap),
                value: IRExpr::EnvRead {
                    env: "__env".to_string(),
                    slot: k as u32,
                },
                var_type: Some(IRType::Int),
            })
            .collect();
        statements.push(IRStatement::Return(Some((**body).clone())));
        let mut lifted_body = IRBody { statements };
        // The lambda's own captures come from the environment, and anything a
        // *nested* closure captures from this lambda (its parameters, above
        // all) needs a cell of its own, exactly as in an ordinary function.
        let own_cells = cells_of(&lifted_body, &fn_params, &global_names);
        let mut all_cells: HashSet<String> = captured.iter().cloned().collect();
        all_cells.extend(own_cells.iter().cloned());
        cellify(&mut lifted_body, &all_cells);
        add_cell_setup(&mut lifted_body, &own_cells, &fn_params);

        lifted.push(IRFunction {
            name: lambda_name.clone(),
            params: fn_params,
            body: lifted_body,
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
        // A lifted nested function reads the variables it closes over out of
        // its environment; it does not make cells of its own for them.
        let from_env = env_cells(&func.body);
        let cells: HashSet<String> = cells_of(&func.body, &func.params, &global_names)
            .into_iter()
            .filter(|name| !from_env.contains(name))
            .collect();
        visit_body(&mut func.body, &mut rewrite)?;
        let mut all_cells = cells.clone();
        all_cells.extend(from_env);
        cellify(&mut func.body, &all_cells);
        add_cell_setup(&mut func.body, &cells, &func.params);
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            let cells = cells_of(&method.body, &method.params, &global_names);
            visit_body(&mut method.body, &mut rewrite)?;
            cellify(&mut method.body, &cells);
            add_cell_setup(&mut method.body, &cells, &method.params);
        }
    }
    for var in &mut module.variables {
        visit_expr(&mut var.value, &mut rewrite)?;
    }

    module.functions.extend(lifted);
    Ok(())
}

/// Which of this function's own variables a closure inside it captures.
///
/// Collected before any lifting, from every lambda in the body, and then
/// narrowed to names this function actually binds: an inner lambda's free names
/// include the *outer lambda's* parameters, which are not variables of this
/// function and get their cells from the environment instead.
fn cells_of(body: &IRBody, params: &[IRParam], globals: &HashSet<String>) -> HashSet<String> {
    let mut captured: Vec<String> = Vec::new();
    let mut collect = |expr: &mut IRExpr| -> Result<()> {
        match expr {
            IRExpr::Lambda { params, body, .. } => {
                let mut bound: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                collect_free_names(body, &mut bound, globals, &mut captured);
            }
            // An inner lambda lifted before this one: what it captures is read
            // here, so those names need cells here too.
            IRExpr::ClosureMake {
                captured: inner, ..
            } => {
                for name in inner {
                    if !captured.iter().any(|c| c == name) {
                        captured.push(name.clone());
                    }
                }
            }
            _ => {}
        }
        Ok(())
    };
    let mut probe = body.clone();
    let _ = visit_body(&mut probe, &mut collect);

    let mut mine: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
    collect_bound_names(body, &mut mine);
    captured.into_iter().filter(|c| mine.contains(c)).collect()
}

/// Every name this body binds: assignment targets, loop variables, and the
/// targets of an unpacking assignment.
fn collect_bound_names(body: &IRBody, out: &mut HashSet<String>) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Assign { target, .. } | IRStatement::AugAssign { target, .. } => {
                out.insert(target.clone());
            }
            IRStatement::For { target, .. } => {
                out.insert(target.clone());
            }
            IRStatement::TupleUnpack { targets, .. } => {
                for target in targets {
                    out.insert(target.clone());
                }
            }
            IRStatement::With {
                optional_vars: Some(target),
                ..
            }
            | IRStatement::DynamicImport { target, .. } => {
                out.insert(target.clone());
            }
            IRStatement::TryExcept {
                except_handlers, ..
            } => {
                out.extend(except_handlers.iter().filter_map(|h| h.name.clone()));
            }
            _ => {}
        }
        for nested in nested_bodies_ref(stmt) {
            collect_bound_names(nested, out);
        }
    }
}

/// Create each cell at the top of the function, and seed the ones that start
/// life as a parameter with the argument they were passed.
fn add_cell_setup(body: &mut IRBody, cells: &HashSet<String>, params: &[IRParam]) {
    if cells.is_empty() {
        return;
    }
    let mut names: Vec<&String> = cells.iter().collect();
    names.sort();
    let mut setup = Vec::with_capacity(names.len());
    for name in names {
        setup.push(IRStatement::Assign {
            target: cell_local(name),
            value: IRExpr::CellNew,
            var_type: Some(IRType::Int),
        });
        if params.iter().any(|p| &p.name == name) {
            setup.push(IRStatement::Expression(IRExpr::CellStore {
                cell: cell_local(name),
                value: Box::new(IRExpr::Param(name.clone())),
            }));
        }
    }
    setup.append(&mut body.statements);
    body.statements = setup;
}

/// Prefix for the local that holds a captured variable's cell pointer.
pub fn cell_local(name: &str) -> String {
    format!("__cell_{name}")
}

/// Turn every variable a closure captures into a shared heap cell.
///
/// Python closures capture the *variable*, not its value: a closure reading `v`
/// sees whatever `v` holds when the closure runs, including changes the
/// enclosing function made after creating it, and closures made in a loop all
/// share the one loop variable. Copying the value into the environment at
/// creation time, which is what this used to do, gets that wrong silently.
///
/// So a captured variable lives in a one-slot heap cell instead. The enclosing
/// function keeps the cell's pointer in a local, writes go through the cell,
/// reads come back out of it, and the closure environment carries the *pointer*,
/// so both sides are looking at the same slot. Inside a lifted lambda the same
/// shape holds: its prologue binds the pointer out of the environment, and its
/// body reads through it, which is what makes nested closures share a cell all
/// the way down.
fn cellify(body: &mut IRBody, cells: &HashSet<String>) {
    if cells.is_empty() {
        return;
    }
    rewrite_cell_uses(body, cells);
}

/// Rewrite reads and writes of every name in `cells` to go through its cell.
fn rewrite_cell_uses(body: &mut IRBody, cells: &HashSet<String>) {
    let mut out = Vec::with_capacity(body.statements.len());
    for mut stmt in std::mem::take(&mut body.statements) {
        // Reads first, so a value expression that mentions the target reads the
        // cell's current contents rather than the local.
        visit_stmt_exprs(&mut stmt, &mut |expr| cell_reads(expr, cells));

        match stmt {
            IRStatement::Assign {
                ref target,
                ref value,
                ..
            } if cells.contains(target) => {
                out.push(IRStatement::Expression(IRExpr::CellStore {
                    cell: cell_local(target),
                    value: Box::new(value.clone()),
                }));
            }
            IRStatement::AugAssign {
                ref target,
                ref value,
                op,
            } if cells.contains(target) => {
                out.push(IRStatement::Expression(IRExpr::CellStore {
                    cell: cell_local(target),
                    value: Box::new(IRExpr::BinaryOp {
                        left: Box::new(IRExpr::CellLoad {
                            cell: cell_local(target),
                        }),
                        right: Box::new(value.clone()),
                        op,
                    }),
                }));
            }
            mut other => {
                for nested in nested_bodies_mut(&mut other) {
                    rewrite_cell_uses(nested, cells);
                }
                // A loop variable is bound by the loop itself, so mirror each
                // binding into the cell: closures made in the loop share it and
                // see the last value, as Python's do. Inserted after the walk
                // above, so the read it makes of the loop local stays a read of
                // the local rather than becoming a read of the cell.
                if let IRStatement::For { target, body, .. } = &mut other {
                    if cells.contains(target) {
                        let mirror = IRStatement::Expression(IRExpr::CellStore {
                            cell: cell_local(target),
                            value: Box::new(IRExpr::Variable(target.clone())),
                        });
                        body.statements.insert(0, mirror);
                    }
                }
                out.push(other);
            }
        }
    }
    body.statements = out;
}

/// Replace reads of a celled name with a read through its cell.
fn cell_reads(expr: &mut IRExpr, cells: &HashSet<String>) {
    match expr {
        IRExpr::Variable(name) | IRExpr::Param(name) if cells.contains(name) => {
            *expr = IRExpr::CellLoad {
                cell: cell_local(name),
            };
        }
        // A closure creation captures the cell pointer, not the value, which is
        // the whole point: the local named here is the cell local.
        IRExpr::ClosureMake { captured, .. } => {
            for name in captured.iter_mut() {
                if cells.contains(name) {
                    *name = cell_local(name);
                }
            }
        }
        _ => {}
    }
}

/// The bodies a statement owns, for the read-only name scan.
fn nested_bodies_ref(stmt: &IRStatement) -> Vec<&IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            let mut bodies = vec![then_body.as_ref()];
            bodies.extend(else_body.as_deref());
            bodies
        }
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body.as_ref()],
        IRStatement::For {
            body, else_body, ..
        } => {
            let mut bodies = vec![body.as_ref()];
            bodies.extend(else_body.as_deref());
            bodies
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            let mut bodies = vec![try_body.as_ref()];
            bodies.extend(except_handlers.iter().map(|h| &h.body));
            bodies.extend(finally_body.as_deref());
            bodies
        }
        _ => Vec::new(),
    }
}

/// Every body a statement owns, for the cell rewrite.
fn nested_bodies_mut(stmt: &mut IRStatement) -> Vec<&mut IRBody> {
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

/// Apply `f` to every expression a statement evaluates directly, recursing into
/// the expression tree (nested bodies are walked separately).
fn visit_stmt_exprs(stmt: &mut IRStatement, f: &mut impl FnMut(&mut IRExpr)) {
    let mut walk = |expr: &mut IRExpr| {
        walk_expr(expr, f);
    };
    match stmt {
        IRStatement::Return(Some(e)) | IRStatement::Yield { value: Some(e) } => walk(e),
        IRStatement::Raise {
            exception: Some(e), ..
        } => walk(e),
        IRStatement::Assign { value, .. }
        | IRStatement::AugAssign { value, .. }
        | IRStatement::Expression(value)
        | IRStatement::TupleUnpack { value, .. } => walk(value),
        IRStatement::If { condition, .. } | IRStatement::While { condition, .. } => walk(condition),
        IRStatement::For { iterable, .. } => walk(iterable),
        IRStatement::With { context_expr, .. } => walk(context_expr),
        IRStatement::AttributeAssign { object, value, .. }
        | IRStatement::AttributeAugAssign { object, value, .. } => {
            walk(object);
            walk(value);
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            walk(container);
            walk(index);
            walk(value);
        }
        IRStatement::DynamicImport { module_name, .. } => walk(module_name),
        _ => {}
    }
}

/// Apply `f` to `expr` and, post-order, to every expression inside it.
fn walk_expr(expr: &mut IRExpr, f: &mut impl FnMut(&mut IRExpr)) {
    let _ = visit_expr(expr, &mut |e: &mut IRExpr| {
        f(e);
        Ok(())
    });
    f(expr);
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
        // The environment is a parameter of the lifted function, never a free
        // variable of it.
        IRExpr::EnvRead { .. } | IRExpr::CellNew => {}
        // A cell is reached through the local holding its pointer, so that
        // local is what an enclosing closure has to capture.
        IRExpr::CellLoad { cell } => consider(cell, bound, out),
        IRExpr::CellStore { cell, value } => {
            consider(cell, bound, out);
            collect_free_names(value, bound, globals, out);
        }
        IRExpr::Keyword { value, .. } => collect_free_names(value, bound, globals, out),
        IRExpr::Const(_) => {}
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            collect_free_names(left, bound, globals, out);
            collect_free_names(right, bound, globals, out);
        }
        IRExpr::UnaryOp { operand, .. } => collect_free_names(operand, bound, globals, out),
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => {
            collect_free_names(condition, bound, globals, out);
            collect_free_names(body, bound, globals, out);
            collect_free_names(orelse, bound, globals, out);
        }
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
pub(crate) fn visit_body(
    body: &mut IRBody,
    f: &mut impl FnMut(&mut IRExpr) -> Result<()>,
) -> Result<()> {
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
        IRStatement::Raise { exception, cause } => {
            if let Some(exception) = exception {
                visit_expr(exception, f)?;
            }
            if let Some(cause) = cause {
                visit_expr(cause, f)?;
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
        IRStatement::FunctionDef(def) => visit_body(&mut def.body, f)?,
        IRStatement::ImportModule { .. }
        | IRStatement::Break
        | IRStatement::Continue
        | IRStatement::Global(_)
        | IRStatement::Nonlocal(_) => {}
    }
    Ok(())
}

fn visit_expr(expr: &mut IRExpr, f: &mut impl FnMut(&mut IRExpr) -> Result<()>) -> Result<()> {
    match expr {
        IRExpr::Const(_)
        | IRExpr::Param(_)
        | IRExpr::Variable(_)
        | IRExpr::EnvRead { .. }
        | IRExpr::CellNew
        | IRExpr::CellLoad { .. } => {}
        IRExpr::CellStore { value, .. } | IRExpr::Keyword { value, .. } => visit_expr(value, f)?,
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            visit_expr(left, f)?;
            visit_expr(right, f)?;
        }
        IRExpr::UnaryOp { operand, .. } => visit_expr(operand, f)?,
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => {
            visit_expr(condition, f)?;
            visit_expr(body, f)?;
            visit_expr(orelse, f)?;
        }
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
