//! `for` loops over `x.items()`, `x.keys()`, and `x.values()`.
//!
//! The converter lowers these as it lowers any loop over a method call, since
//! it cannot tell a dict from an instance whose class defines the method. Here,
//! with the module's classes known, a loop over an instance of a class that
//! defines the method (`collections.Counter`'s included) is left as it is, so
//! the method is called
//! as CPython calls it (and a generator method is consumed by
//! `ir::generators`, which runs next). Any other receiver is taken to be a
//! dict (a `Counter`, `defaultdict`, `OrderedDict`, or `ChainMap` as the dict
//! it reads as), and the loop walks its entry slots in place through the
//! `__dict_key_at` / `__dict_val_at` intrinsics, allocating nothing. A loop
//! whose shape the walk does not cover (`for kv in d.items()`, a star target)
//! is left as a loop over the method's result, which code generation builds as
//! a list.

use super::build::{assign, call, cmp, int, mcall, v};
use super::collections::spec::{self, Kind};
use super::infer::{bind, Types};
use crate::ir::{
    IRBody, IRCompareOp, IRExpr, IRModule, IROp, IRStatement, IRType, DATA_METHOD, DICT_KEY_AT_FN,
    DICT_VAL_AT_FN, VIEW_METHOD,
};
use std::collections::{HashMap, HashSet};

const VIEWS: [&str; 3] = ["items", "keys", "values"];

/// The prefix of the local a dict-view loop parks the dict it walks in. It
/// starts `__dict_`, which `len()` reads as a compiler-made collection.
pub const DICT_VIEW_PREFIX: &str = "__dict_view_";

/// Rewrite every dict-view loop of the module's functions and methods.
pub fn lower_dict_views(module: &mut IRModule) {
    let types = Types::of_module(module);
    let classes = Classes::of(module, &types);
    let mut pass = Pass {
        types: &types,
        classes: &classes,
        next: 0,
    };
    for func in &mut module.functions {
        let mut env = params_env(&func.params, None);
        pass.body(&mut func.body, &mut env);
    }
    for class in &mut module.classes {
        for method in &mut class.methods {
            let mut env = params_env(&method.params, Some(&class.name));
            pass.body(&mut method.body, &mut env);
        }
    }
}

/// What the loops need to know of the module's classes, the ones the
/// standard-library passes write (`collections.Counter`, ...) included.
struct Classes {
    bases: HashMap<String, Vec<String>>,
    /// The view methods each class defines itself.
    views: HashMap<String, HashSet<String>>,
    /// The instance each class's methods store in a field, where every
    /// store agrees (`self.bag: Bag = Bag()`).
    fields: HashMap<String, HashMap<String, IRType>>,
}

impl Classes {
    fn of(module: &IRModule, types: &Types) -> Classes {
        let mut classes = Classes {
            bases: HashMap::new(),
            views: HashMap::new(),
            fields: HashMap::new(),
        };
        for class in &module.classes {
            classes
                .bases
                .insert(class.name.clone(), class.bases.clone());
            let views = class
                .methods
                .iter()
                .filter(|m| VIEWS.contains(&m.name.as_str()))
                .map(|m| m.name.clone())
                .collect();
            classes.views.insert(class.name.clone(), views);
            let mut fields = HashMap::new();
            for method in &class.methods {
                let env = params_env(&method.params, Some(&class.name));
                record_fields(&method.body, types, &env, &mut fields);
            }
            classes.fields.insert(class.name.clone(), fields);
        }
        classes
    }

    /// The class and its bases, depth first, each once.
    fn lineage(&self, class: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut stack = vec![class.to_string()];
        while let Some(c) = stack.pop() {
            if out.contains(&c) {
                continue;
            }
            if let Some(bases) = self.bases.get(&c) {
                stack.extend(bases.iter().rev().cloned());
            }
            out.push(c);
        }
        out
    }

    fn defines(&self, class: &str, method: &str) -> bool {
        self.lineage(class)
            .iter()
            .any(|c| self.views.get(c).is_some_and(|v| v.contains(method)))
    }

    fn field(&self, class: &str, field: &str) -> IRType {
        self.lineage(class)
            .iter()
            .find_map(|c| self.fields.get(c).and_then(|f| f.get(field)).cloned())
            .unwrap_or(IRType::Unknown)
    }
}

/// The instances a method's `self.<field> = value` stores, by annotation or
/// by the value's type; a field given two different ones is `Unknown`.
fn record_fields(
    body: &IRBody,
    types: &Types,
    env: &HashMap<String, IRType>,
    fields: &mut HashMap<String, IRType>,
) {
    for stmt in &body.statements {
        if let IRStatement::AttributeAssign {
            object: IRExpr::Variable(name) | IRExpr::Param(name),
            attribute,
            value,
            annotation,
        } = stmt
        {
            if name == "self" {
                let ty = match annotation {
                    Some(ty @ IRType::Class(_)) => ty.clone(),
                    _ => types.of(value, env),
                };
                if matches!(ty, IRType::Class(_)) {
                    let slot = fields.entry(attribute.clone()).or_insert(ty.clone());
                    if *slot != ty {
                        *slot = IRType::Unknown;
                    }
                }
            }
        }
        for nested in super::infer::nested_bodies(stmt) {
            record_fields(nested, types, env, fields);
        }
    }
}

/// The parameters' declared types, with an untyped `self` of a method taken
/// to be an instance of its class.
fn params_env(params: &[crate::ir::IRParam], class: Option<&str>) -> HashMap<String, IRType> {
    let mut env: HashMap<String, IRType> = params
        .iter()
        .map(|p| (p.name.clone(), p.param_type.clone()))
        .collect();
    if let (Some(class), Some(first)) = (class, params.first()) {
        if first.name == "self" && matches!(first.param_type, IRType::Unknown) {
            env.insert(first.name.clone(), IRType::Class(class.to_string()));
        }
    }
    env
}

struct Pass<'a> {
    types: &'a Types,
    classes: &'a Classes,
    next: usize,
}

impl Pass<'_> {
    fn body(&mut self, body: &mut IRBody, env: &mut HashMap<String, IRType>) {
        let statements = std::mem::take(&mut body.statements);
        for stmt in statements {
            let mut produced = self.walk(stmt, env);
            for stmt in &mut produced {
                for nested in super::finalize::nested_bodies_mut(stmt) {
                    self.body(nested, env);
                }
                bind(stmt, self.types, env);
            }
            body.statements.extend(produced);
        }
    }

    /// What the receiver is: a field of an instance, by what its class's
    /// methods store in it, or anything the IR shows.
    fn receiver(&self, object: &IRExpr, env: &HashMap<String, IRType>) -> IRType {
        if let IRExpr::Attribute { object, attribute } = object {
            if let IRType::Class(class) = self.types.of(object, env) {
                return self.classes.field(&class, attribute);
            }
        }
        self.types.of(object, env)
    }

    /// The dict walk a loop becomes, or the statement back as it was.
    fn walk(&mut self, stmt: IRStatement, env: &HashMap<String, IRType>) -> Vec<IRStatement> {
        let IRStatement::For {
            target,
            iterable:
                IRExpr::MethodCall {
                    object,
                    method_name,
                    arguments,
                },
            body,
            else_body: None,
        } = stmt
        else {
            return vec![stmt];
        };
        let keep = |object: Box<IRExpr>,
                    method_name: String,
                    arguments: Vec<IRExpr>,
                    body: Box<IRBody>| IRStatement::For {
            target: target.clone(),
            iterable: IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            },
            body,
            else_body: None,
        };
        if !VIEWS.contains(&method_name.as_str()) || !arguments.is_empty() {
            return vec![keep(object, method_name, arguments, body)];
        }
        let mut object = object;
        if let IRType::Class(class) = self.receiver(&object, env) {
            if self.classes.defines(&class, &method_name) {
                return vec![keep(object, method_name, arguments, body)];
            }
            // A `Counter`, `defaultdict`, or `OrderedDict` reads as the dict
            // it holds, and a `ChainMap` as its maps merged: walk that.
            let held = match spec::kind_of_class(&class) {
                Some(Kind::Counter | Kind::DefaultDict | Kind::OrderedDict) => Some(DATA_METHOD),
                Some(Kind::ChainMap) => Some(VIEW_METHOD),
                _ => None,
            };
            if let Some(held) = held {
                object = Box::new(mcall(*object, held, Vec::new()));
            }
        }
        // `for k, v in x.items()` reaches here as a loop binding a hidden
        // element that its first statement unpacks into the two targets.
        let mut statements = body.statements;
        let bindings: Vec<(String, &str)> = match method_name.as_str() {
            "items" => match statements.first() {
                Some(IRStatement::TupleUnpack {
                    targets,
                    value: IRExpr::Variable(element),
                    starred: None,
                }) if element == &target && targets.len() == 2 => vec![
                    (targets[0].clone(), DICT_KEY_AT_FN),
                    (targets[1].clone(), DICT_VAL_AT_FN),
                ],
                _ => Vec::new(),
            },
            "keys" => vec![(target.clone(), DICT_KEY_AT_FN)],
            _ => vec![(target.clone(), DICT_VAL_AT_FN)],
        };
        if bindings.is_empty() {
            let body = Box::new(IRBody { statements });
            return vec![keep(object, method_name, arguments, body)];
        }
        if method_name == "items" {
            statements.remove(0);
        }

        let n = self.next;
        self.next += 1;
        let dict = format!("{DICT_VIEW_PREFIX}{n}");
        let counter = format!("__dview_at_{n}");
        let mut inner = vec![
            IRStatement::AugAssign {
                target: counter.clone(),
                value: int(1),
                op: IROp::Add,
            },
            IRStatement::If {
                condition: cmp(v(&counter), IRCompareOp::GtE, call("len", vec![v(&dict)])),
                then_body: Box::new(IRBody {
                    statements: vec![IRStatement::Break],
                }),
                else_body: None,
            },
        ];
        for (name, intrinsic) in bindings {
            inner.push(assign(&name, call(intrinsic, vec![v(&dict), v(&counter)])));
        }
        inner.extend(statements);
        vec![
            assign(&dict, *object),
            assign(&counter, int(-1)),
            IRStatement::While {
                condition: IRExpr::Const(crate::ir::IRConstant::Bool(true)),
                body: Box::new(IRBody { statements: inner }),
            },
        ]
    }
}
