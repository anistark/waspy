//! `functools.partial`, lowered to a closure over the bound arguments.
//!
//! `partial(f, 1, c=5)` evaluates its arguments once, where it is called,
//! and makes a callable that calls `f` with them and whatever it is called
//! with. Each `partial(...)` of a function this module defines becomes a
//! call to a synthesized maker, `__partial_<n>(bound...)`, whose parameters
//! hold the bound values (typed as `f`'s parameters they bind) and which
//! returns a lambda over the parameters left free:
//!
//! ```python
//! def __partial_0(__b0: int, __k_c: int) -> Callable[[int], int]:
//!     return lambda b: f(__b0, b, c=__k_c)
//! ```
//!
//! A call of the maker binds fresh values each time, as each `partial` does,
//! so partials made in a loop keep their own. The free parameters are
//! positional: one bound by keyword has to follow every free one, since
//! CPython would otherwise pass a positional argument to it.
//!
//! `functools.cmp_to_key(cmp)` is lowered here too, to what CPython's is: a
//! class wrapping a value whose `__lt__` asks `cmp`, made per comparator, and
//! a key function making one per element (`lambda x: __CmpKey_0(x)`).

use crate::ir::{
    IRBody, IRClass, IRCompareOp, IRConstant, IRExpr, IRFunction, IRModule, IRParam, IRStatement,
    IRType, ParamStar,
};
use anyhow::{anyhow, Result};

/// The prefix of the synthesized makers; they are internal.
pub const PARTIAL_FN_PREFIX: &str = "__partial_";

/// The prefix of `cmp_to_key`'s key classes.
pub const CMP_KEY_PREFIX: &str = "__CmpKey_";

pub fn lower_partials(module: &mut IRModule) -> Result<()> {
    // What `partial` and `cmp_to_key` are called as: through the module, or
    // imported by name.
    let mut names: Vec<(String, &'static str)> = Vec::new();
    let mut through_module = false;
    for imp in &module.imports {
        if imp.module != "functools" {
            continue;
        }
        match (&imp.name, imp.is_from_import) {
            (Some(name), true) if name == "partial" || name == "cmp_to_key" => {
                let which = if name == "partial" {
                    "partial"
                } else {
                    "cmp_to_key"
                };
                names.push((imp.alias.clone().unwrap_or_else(|| name.clone()), which));
            }
            (None, false) => through_module = true,
            _ => {}
        }
    }
    if names.is_empty() && !through_module {
        return Ok(());
    }
    let defined: Vec<IRFunction> = module.functions.clone();
    let mut makers: Vec<IRFunction> = Vec::new();
    let mut key_classes: Vec<IRClass> = Vec::new();
    let mut rewrite = |expr: &mut IRExpr| -> Result<()> {
        let (which, arguments) = match expr {
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } if through_module
                && (method_name == "partial" || method_name == "cmp_to_key")
                && matches!(object.as_ref(), IRExpr::Variable(m) if m == "functools") =>
            {
                (
                    if method_name == "partial" {
                        "partial"
                    } else {
                        "cmp_to_key"
                    },
                    arguments.clone(),
                )
            }
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } if !defined.iter().any(|f| &f.name == function_name) => {
                match names.iter().find(|(n, _)| n == function_name) {
                    Some((_, which)) => (*which, arguments.clone()),
                    None => return Ok(()),
                }
            }
            _ => return Ok(()),
        };
        if which == "cmp_to_key" {
            let (make, class, key) = cmp_to_key(&arguments, &defined, key_classes.len())?;
            makers.push(make);
            key_classes.push(class);
            *expr = key;
            return Ok(());
        }
        let (maker, call) = lower_one(&arguments, &defined, makers.len())?;
        makers.push(maker);
        *expr = call;
        Ok(())
    };
    for func in &mut module.functions {
        crate::ir::visit_body(&mut func.body, &mut rewrite)?;
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            crate::ir::visit_body(&mut method.body, &mut rewrite)?;
        }
    }
    for var in &mut module.variables {
        let mut body = IRBody {
            statements: vec![IRStatement::Expression(var.value.clone())],
        };
        crate::ir::visit_body(&mut body, &mut rewrite)?;
        if let Some(IRStatement::Expression(value)) = body.statements.pop() {
            var.value = value;
        }
    }
    module.functions.extend(makers);
    module.classes.extend(key_classes);
    Ok(())
}

/// `cmp_to_key(cmp)`: the key class and the key function making its
/// instances. `cmp` is a function this module defines, named directly.
fn cmp_to_key(
    arguments: &[IRExpr],
    defined: &[IRFunction],
    n: usize,
) -> Result<(IRFunction, IRClass, IRExpr)> {
    let [IRExpr::Variable(cmp)] = arguments else {
        return Err(refused(
            "functools.cmp_to_key() takes one function this module defines, named directly"
                .to_string(),
        ));
    };
    let Some(f) = defined.iter().find(|f| &f.name == cmp) else {
        return Err(refused(format!(
            "functools.cmp_to_key() of '{cmp}', which is not a function this module defines, \
             is not supported. Hint: write the comparison as a 'def'"
        )));
    };
    if f.params.len() != 2 {
        return Err(anyhow!(
            "TypeError: cmp_to_key()'s comparison '{cmp}' must take two arguments"
        ));
    }
    let class = format!("{CMP_KEY_PREFIX}{n}_{cmp}");
    let obj_type = f.params[0].param_type.clone();
    let self_param = |class: &str| IRParam {
        name: "self".to_string(),
        param_type: IRType::Class(class.to_string()),
        default_value: None,
        keyword_only: false,
        star: ParamStar::None,
    };
    let field = |of: &str| IRExpr::Attribute {
        object: Box::new(IRExpr::Variable(of.to_string())),
        attribute: "obj".to_string(),
    };
    let init = IRFunction {
        name: "__init__".to_string(),
        params: vec![
            self_param(&class),
            IRParam {
                name: "obj".to_string(),
                param_type: obj_type,
                default_value: None,
                keyword_only: false,
                star: ParamStar::None,
            },
        ],
        body: IRBody {
            statements: vec![IRStatement::AttributeAssign {
                object: IRExpr::Variable("self".to_string()),
                attribute: "obj".to_string(),
                value: IRExpr::Variable("obj".to_string()),
                annotation: None,
            }],
        },
        return_type: IRType::None,
        decorators: Vec::new(),
    };
    // __lt__(self, other) = cmp(self.obj, other.obj) < 0
    let lt = IRFunction {
        name: "__lt__".to_string(),
        params: vec![
            self_param(&class),
            IRParam {
                name: "other".to_string(),
                param_type: IRType::Class(class.clone()),
                default_value: None,
                keyword_only: false,
                star: ParamStar::None,
            },
        ],
        body: IRBody {
            statements: vec![IRStatement::Return(Some(IRExpr::CompareOp {
                left: Box::new(IRExpr::FunctionCall {
                    function_name: cmp.clone(),
                    arguments: vec![field("self"), field("other")],
                }),
                right: Box::new(IRExpr::Const(IRConstant::Int(0))),
                op: IRCompareOp::Lt,
            }))],
        },
        return_type: IRType::Bool,
        decorators: Vec::new(),
    };
    // The key function: `def __CmpKey_n_make(x: T) -> __CmpKey_n`.
    let make = IRFunction {
        name: format!("{class}_make"),
        params: vec![IRParam {
            name: "x".to_string(),
            param_type: f.params[0].param_type.clone(),
            default_value: None,
            keyword_only: false,
            star: ParamStar::None,
        }],
        body: IRBody {
            statements: vec![IRStatement::Return(Some(IRExpr::FunctionCall {
                function_name: class.clone(),
                arguments: vec![IRExpr::Variable("x".to_string())],
            }))],
        },
        return_type: IRType::Class(class.clone()),
        decorators: Vec::new(),
    };
    // As a value, the lambda that calls it (what naming a function as a
    // value lowers to).
    let key = IRExpr::Lambda {
        params: make.params.clone(),
        body: Box::new(IRExpr::FunctionCall {
            function_name: make.name.clone(),
            arguments: vec![IRExpr::Variable("x".to_string())],
        }),
        captured_vars: Vec::new(),
    };
    Ok((
        make,
        IRClass {
            name: class,
            bases: Vec::new(),
            methods: vec![init, lt],
            class_vars: Vec::new(),
            metaclass: None,
            position: 0,
            deferred_vars: Vec::new(),
        },
        key,
    ))
}

fn refused(message: String) -> anyhow::Error {
    crate::core::errors::unsupported_feature(message, None).into()
}

fn lower_one(
    arguments: &[IRExpr],
    defined: &[IRFunction],
    n: usize,
) -> Result<(IRFunction, IRExpr)> {
    let Some(IRExpr::Variable(target)) = arguments.first() else {
        return Err(refused(
            "functools.partial() of anything but a function this module defines, named \
             directly, is not supported"
                .to_string(),
        ));
    };
    let Some(f) = defined.iter().find(|f| &f.name == target) else {
        return Err(refused(format!(
            "functools.partial() of '{target}', which is not a function this module defines, is \
             not supported"
        )));
    };
    if f.params.iter().any(|p| p.star != ParamStar::None) {
        return Err(refused(format!(
            "functools.partial() of '{target}', which takes *args or **kwargs, is not supported"
        )));
    }
    let positional: Vec<&IRExpr> = arguments[1..]
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect();
    let keywords: Vec<(&String, &IRExpr)> = arguments[1..]
        .iter()
        .filter_map(|a| match a {
            IRExpr::Keyword { name, value } => Some((name, value.as_ref())),
            _ => None,
        })
        .collect();
    let takes_positionally = f.params.iter().filter(|p| !p.keyword_only).count();
    if positional.len() > takes_positionally {
        return Err(anyhow!(
            "TypeError: functools.partial() binds {} positional arguments to '{target}', which \
             takes {takes_positionally}",
            positional.len()
        ));
    }
    for (name, _) in &keywords {
        if !f.params.iter().any(|p| &p.name == *name) {
            return Err(anyhow!(
                "TypeError: {target}() got an unexpected keyword argument '{name}'"
            ));
        }
        if f.params[..positional.len()]
            .iter()
            .any(|p| &p.name == *name)
        {
            return Err(anyhow!(
                "TypeError: {target}() got multiple values for argument '{name}'"
            ));
        }
    }
    let bound_by_name = |p: &IRParam| keywords.iter().any(|(name, _)| **name == p.name);
    let rest = &f.params[positional.len()..];
    let free: Vec<&IRParam> = rest.iter().filter(|p| !bound_by_name(p)).collect();
    // A free parameter after one bound by keyword would take a positional
    // argument CPython passes to the bound one.
    if let Some(first_bound) = rest.iter().position(bound_by_name) {
        if rest[first_bound..]
            .iter()
            .any(|p| !bound_by_name(p) && !p.keyword_only)
        {
            return Err(refused(format!(
                "functools.partial() binding a parameter of '{target}' by keyword before one \
                 left free is not supported: CPython passes the call's positional arguments to \
                 the bound one first"
            )));
        }
    }
    if free.iter().any(|p| p.keyword_only) {
        return Err(refused(format!(
            "functools.partial() leaving a keyword-only parameter of '{target}' free is not \
             supported"
        )));
    }

    let mut maker_params = Vec::new();
    let mut call_args = Vec::new();
    for (i, p) in f.params[..positional.len()].iter().enumerate() {
        let name = format!("__b{i}");
        maker_params.push(IRParam {
            name: name.clone(),
            param_type: p.param_type.clone(),
            default_value: None,
            keyword_only: false,
            star: ParamStar::None,
        });
        call_args.push(IRExpr::Variable(name));
    }
    for p in &free {
        call_args.push(IRExpr::Variable(p.name.clone()));
    }
    for (name, _) in &keywords {
        let p = f.params.iter().find(|p| &p.name == *name).expect("checked");
        let local = format!("__k_{name}");
        maker_params.push(IRParam {
            name: local.clone(),
            param_type: p.param_type.clone(),
            default_value: None,
            keyword_only: false,
            star: ParamStar::None,
        });
        call_args.push(IRExpr::Keyword {
            name: (*name).clone(),
            value: Box::new(IRExpr::Variable(local)),
        });
    }
    let lambda_params: Vec<IRParam> = free
        .iter()
        .map(|p| IRParam {
            name: p.name.clone(),
            param_type: p.param_type.clone(),
            default_value: None,
            keyword_only: false,
            star: ParamStar::None,
        })
        .collect();
    let maker_name = format!("{PARTIAL_FN_PREFIX}{n}_{target}");
    let maker = IRFunction {
        name: maker_name.clone(),
        params: maker_params,
        body: IRBody {
            statements: vec![IRStatement::Return(Some(IRExpr::Lambda {
                params: lambda_params.clone(),
                body: Box::new(IRExpr::FunctionCall {
                    function_name: target.clone(),
                    arguments: call_args,
                }),
                captured_vars: Vec::new(),
            }))],
        },
        return_type: IRType::Callable {
            params: lambda_params.iter().map(|p| p.param_type.clone()).collect(),
            return_type: Box::new(f.return_type.clone()),
        },
        decorators: Vec::new(),
    };
    let mut bound: Vec<IRExpr> = positional.into_iter().cloned().collect();
    bound.extend(keywords.into_iter().map(|(_, v)| v.clone()));
    Ok((
        maker,
        IRExpr::FunctionCall {
            function_name: maker_name,
            arguments: bound,
        },
    ))
}
