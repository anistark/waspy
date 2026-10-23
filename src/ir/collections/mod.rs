//! `collections` and `typing.NamedTuple`, as classes synthesized in IR.
//!
//! A named tuple (`namedtuple("P", "x y")`, or `class P(NamedTuple)`) is a
//! class whose instances hold the fields, with a hidden `<view>` method
//! that answers the fields as a plain tuple. That method is what marks the
//! class, and code generation reads an instance through it wherever CPython
//! reads it as the tuple it is: indexing and slicing, unpacking, iteration,
//! `in`, `count` / `index`, comparison with a tuple, `+` and `*`, and hashing.
//! The converter builds the class; [`lower_collections`] types the fields
//! `namedtuple()` leaves unannotated from the module's calls of the class, and
//! adds the methods whose signatures depend on those types.

mod classes;
mod ctor;
pub(crate) mod spec;

use super::build::*;
use super::infer::{walk_body, Types};
use crate::ir::{
    IRBody, IRCompareOp, IRExpr, IRFunction, IRModule, IROp, IRParam, IRStatement, IRType,
    IRVariable, MemoryLayout, ParamStar,
};
use anyhow::Result;
use std::collections::HashMap;

/// The hidden method of a named tuple that answers its fields as a tuple (and
/// of a ChainMap, its maps merged): the builtin value an instance reads as,
/// made from it. No Python identifier spells it.
pub const VIEW_METHOD: &str = "<view>";

/// The hidden method of a `collections` class that answers the dict or list
/// it holds, which reading or changing the instance as that builtin value
/// reads or changes.
pub const DATA_METHOD: &str = "<data>";

/// The hidden method that fills a new instance from a constructor's
/// argument and answers it, one variant per argument type
/// (`<from>&lt;list[str]&gt;`).
pub const FROM_METHOD: &str = "<from>";

/// The hidden class variable naming the methods of the builtin value an
/// instance reads as that it answers for the instance.
pub const DELEGATES_VAR: &str = "<delegates>";

/// The hidden class variable that marks a class whose instances cannot be
/// changed once made.
pub const FROZEN_VAR: &str = "<frozen>";

/// The prefix of the functions the pass makes (a defaultdict's lambda
/// factory); they are internal.
pub const COLLECTIONS_FN_PREFIX: &str = "__collections_";

/// A named tuple's field: its name, its type (`Unknown` until inferred when
/// `namedtuple()` names it), and its default.
pub(crate) struct TupleField {
    pub(crate) name: String,
    pub(crate) ty: IRType,
    pub(crate) default: Option<IRExpr>,
}

fn self_param(class: &str) -> IRParam {
    IRParam {
        name: "self".to_string(),
        param_type: IRType::Class(class.to_string()),
        default_value: None,
        keyword_only: false,
        star: ParamStar::None,
    }
}

fn field_of(object: &str, field: &str) -> IRExpr {
    IRExpr::Attribute {
        object: Box::new(v(object)),
        attribute: field.to_string(),
    }
}

/// `"a" + repr(x) + ...`, the constants interned.
fn concat(parts: Vec<IRExpr>) -> IRExpr {
    parts
        .into_iter()
        .reduce(|acc, part| bin(acc, IROp::Add, part))
        .unwrap_or_else(|| text(""))
}

/// Check a named tuple's field names as CPython's `namedtuple()` does, which
/// raises ValueError for any of these.
pub(crate) fn check_field_names(typename: &str, names: &[String]) -> Result<()> {
    let refuse = |message: String| -> Result<()> {
        Err(crate::core::errors::unsupported_feature(message, None).into())
    };
    for (i, name) in names.iter().enumerate() {
        let identifier = name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_alphanumeric() || c == '_');
        if !identifier {
            return refuse(format!(
                "namedtuple '{typename}': type names and field names must be valid identifiers: \
                 '{name}'"
            ));
        }
        if is_keyword(name) {
            return refuse(format!(
                "namedtuple '{typename}': type names and field names cannot be a keyword: \
                 '{name}'"
            ));
        }
        if name.starts_with('_') {
            return refuse(format!(
                "namedtuple '{typename}': field names cannot start with an underscore: '{name}'"
            ));
        }
        if names[..i].contains(name) {
            return refuse(format!(
                "namedtuple '{typename}': encountered duplicate field name: '{name}'"
            ));
        }
    }
    Ok(())
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
    )
}

/// The methods every named tuple has whatever its field types: `__init__`,
/// `<view>`, `__repr__`, the comparisons (as its tuple's), `__len__`, and
/// the `_fields` class variable. A method the class body defines wins, as in
/// CPython; `__init__` and `__new__` cannot be redefined.
pub(crate) fn namedtuple_class(
    name: &str,
    typename: &str,
    fields: Vec<TupleField>,
    mut methods: Vec<IRFunction>,
    mut class_vars: Vec<IRVariable>,
    memory_layout: &mut MemoryLayout,
) -> Result<crate::ir::IRClass> {
    for reserved in ["__init__", "__new__"] {
        if methods.iter().any(|m| m.name == reserved) {
            return Err(crate::core::errors::unsupported_feature(
                format!("named tuple '{name}' defines {reserved}, which CPython does not let a NamedTuple override"),
                None,
            )
            .into());
        }
    }
    let has = |methods: &[IRFunction], m: &str| methods.iter().any(|f| f.name == m);
    // A field's class variable would be its default; the instance's field
    // is what is read.
    class_vars.retain(|v| !fields.iter().any(|f| f.name == v.name));

    let mut init_params = vec![self_param(name)];
    let mut init_body = Vec::new();
    for f in &fields {
        init_params.push(IRParam {
            name: f.name.clone(),
            param_type: f.ty.clone(),
            default_value: f.default.clone(),
            keyword_only: false,
            star: ParamStar::None,
        });
        init_body.push(IRStatement::AttributeAssign {
            object: v("self"),
            attribute: f.name.clone(),
            value: v(&f.name),
            annotation: Some(f.ty.clone()).filter(known),
        });
    }
    methods.push(method("__init__", init_params, IRType::Unknown, init_body));

    let as_tuple_type = IRType::Tuple(fields.iter().map(|f| f.ty.clone()).collect());
    methods.push(method(
        VIEW_METHOD,
        vec![self_param(name)],
        as_tuple_type,
        vec![IRStatement::Return(Some(IRExpr::TupleLiteral(
            fields.iter().map(|f| field_of("self", &f.name)).collect(),
        )))],
    ));

    if !has(&methods, "__repr__") {
        let mut parts = Vec::new();
        let mut label = format!("{typename}(");
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                label.push_str(", ");
            }
            label.push_str(&f.name);
            label.push('=');
            memory_layout.add_string(&label);
            parts.push(text(&std::mem::take(&mut label)));
            parts.push(call("repr", vec![field_of("self", &f.name)]));
        }
        label.push(')');
        memory_layout.add_string(&label);
        parts.push(text(&label));
        methods.push(method(
            "__repr__",
            vec![self_param(name)],
            IRType::String,
            vec![IRStatement::Return(Some(concat(parts)))],
        ));
    }

    let as_tuple = |object: &str| mcall(v(object), VIEW_METHOD, Vec::new());
    for (m, op) in [
        ("__eq__", IRCompareOp::Eq),
        ("__lt__", IRCompareOp::Lt),
        ("__le__", IRCompareOp::LtE),
        ("__gt__", IRCompareOp::Gt),
        ("__ge__", IRCompareOp::GtE),
    ] {
        if has(&methods, m) {
            continue;
        }
        let mut other = self_param(name);
        other.name = "other".to_string();
        methods.push(method(
            m,
            vec![self_param(name), other],
            IRType::Bool,
            vec![IRStatement::Return(Some(cmp(
                as_tuple("self"),
                op,
                as_tuple("other"),
            )))],
        ));
    }
    if !has(&methods, "__len__") {
        methods.push(method(
            "__len__",
            vec![self_param(name)],
            IRType::Int,
            vec![IRStatement::Return(Some(int(fields.len() as i32)))],
        ));
    }

    for d in ["count", "index"] {
        memory_layout.add_string(d);
    }
    class_vars.push(IRVariable {
        name: DELEGATES_VAR.to_string(),
        value: IRExpr::TupleLiteral(vec![text("count"), text("index")]),
        var_type: None,
    });
    if !fields.is_empty() {
        for f in &fields {
            memory_layout.add_string(&f.name);
        }
        class_vars.push(IRVariable {
            name: "_fields".to_string(),
            value: IRExpr::TupleLiteral(fields.iter().map(|f| text(&f.name)).collect()),
            var_type: None,
        });
    }

    Ok(crate::ir::IRClass {
        name: name.to_string(),
        bases: Vec::new(),
        methods,
        class_vars,
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    })
}

/// Whether `class` is a named tuple.
pub(crate) fn is_namedtuple(class: &crate::ir::IRClass) -> bool {
    class.methods.iter().any(|m| m.name == VIEW_METHOD)
}

/// Type each named tuple's untyped fields from the module's calls of it, and
/// add the methods whose signatures need the field types: `_asdict()` and
/// `_make()`, which exist when every field has one type.
pub fn lower_collections(module: &mut IRModule) -> Result<()> {
    type_named_tuples(module)?;
    let before = module.classes.len();
    ctor::lower(module)?;
    // The synthesized classes' strings are interned like the program's.
    let mut strings = Vec::new();
    for class in &mut module.classes[before..] {
        for m in &mut class.methods {
            let _ = crate::ir::visit_body(&mut m.body, &mut |e: &mut IRExpr| {
                if let IRExpr::Const(crate::ir::IRConstant::String(s)) = e {
                    strings.push(s.clone());
                }
                Ok(())
            });
        }
        for var in &class.class_vars {
            if let IRExpr::TupleLiteral(items) = &var.value {
                for i in items {
                    if let IRExpr::Const(crate::ir::IRConstant::String(s)) = i {
                        strings.push(s.clone());
                    }
                }
            }
        }
    }
    for s in strings {
        module.memory_layout.add_string(&s);
    }
    Ok(())
}

/// Type each named tuple's untyped fields, and add `_asdict` and `_make`.
fn type_named_tuples(module: &mut IRModule) -> Result<()> {
    let tuples: Vec<String> = module
        .classes
        .iter()
        .filter(|c| is_namedtuple(c))
        .map(|c| c.name.clone())
        .collect();
    if tuples.is_empty() {
        return Ok(());
    }
    let types = Types::of_module(module);

    // What each call passes for each field, by class.
    let fields_of = |module: &IRModule, class: &str| -> Vec<IRParam> {
        module
            .classes
            .iter()
            .find(|c| c.name == class)
            .and_then(|c| c.methods.iter().find(|m| m.name == "__init__"))
            .map(|m| m.params[1..].to_vec())
            .unwrap_or_default()
    };
    let mut seen: HashMap<String, Vec<Vec<IRType>>> = tuples
        .iter()
        .map(|t| (t.clone(), vec![Vec::new(); fields_of(module, t).len()]))
        .collect();
    let params: HashMap<String, Vec<IRParam>> = tuples
        .iter()
        .map(|t| (t.clone(), fields_of(module, t)))
        .collect();
    {
        let mut record = |expr: &IRExpr, env: &HashMap<String, IRType>| {
            let IRExpr::FunctionCall {
                function_name,
                arguments,
            } = expr
            else {
                return;
            };
            let (Some(slots), Some(ps)) = (seen.get_mut(function_name), params.get(function_name))
            else {
                return;
            };
            let mut position = 0;
            for arg in arguments {
                let (slot, value) = match arg {
                    IRExpr::Keyword { name, value } => {
                        match ps.iter().position(|p| &p.name == name) {
                            Some(i) => (i, value.as_ref()),
                            None => continue,
                        }
                    }
                    other => {
                        position += 1;
                        (position - 1, other)
                    }
                };
                if let Some(s) = slots.get_mut(slot) {
                    s.push(types.of(value, env));
                }
            }
        };
        for func in &module.functions {
            let mut env: HashMap<String, IRType> = func
                .params
                .iter()
                .map(|p| (p.name.clone(), p.param_type.clone()))
                .collect();
            walk_body(&func.body, &types, &mut env, &mut record);
        }
        for class in &module.classes {
            for m in &class.methods {
                let mut env: HashMap<String, IRType> = m
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), p.param_type.clone()))
                    .collect();
                walk_body(&m.body, &types, &mut env, &mut record);
            }
        }
        let empty = HashMap::new();
        let statements: Vec<IRStatement> = module
            .variables
            .iter()
            .map(|var| IRStatement::Expression(var.value.clone()))
            .collect();
        let mut env = empty.clone();
        walk_body(&IRBody { statements }, &types, &mut env, &mut record);
    }

    for class in module
        .classes
        .iter_mut()
        .filter(|c| tuples.contains(&c.name))
    {
        let name = class.name.clone();
        let slots = &seen[&name];
        let mut field_types: Vec<IRType> = Vec::new();
        {
            let init = class
                .methods
                .iter_mut()
                .find(|m| m.name == "__init__")
                .expect("a named tuple has an __init__");
            for (i, p) in init.params.iter_mut().skip(1).enumerate() {
                if !known(&p.param_type) {
                    let mut found: Vec<IRType> = slots[i].clone();
                    if let Some(d) = &p.default_value {
                        found.push(types.of(d, &HashMap::new()));
                    }
                    // One type across every call; a mix (an int here, a
                    // string there) cannot share the field's slot, and an
                    // untyped field takes only ints.
                    if let Some(first) = found.first().filter(|t| known(t)) {
                        if found.iter().all(|t| t == first) {
                            p.param_type = first.clone();
                        }
                    }
                }
                field_types.push(p.param_type.clone());
            }
            for stmt in &mut init.body.statements {
                if let IRStatement::AttributeAssign {
                    attribute,
                    annotation,
                    ..
                } = stmt
                {
                    if let Some(p) = init.params.iter().find(|p| &p.name == attribute) {
                        *annotation = Some(p.param_type.clone()).filter(known);
                    }
                }
            }
        }
        if let Some(m) = class.methods.iter_mut().find(|m| m.name == VIEW_METHOD) {
            m.return_type = IRType::Tuple(field_types.clone());
        }
        let names: Vec<String> = class
            .methods
            .iter()
            .find(|m| m.name == "__init__")
            .map(|m| m.params[1..].iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        let one = field_types
            .first()
            .filter(|t| known(t) && field_types.iter().all(|u| u == *t))
            .cloned();
        // `_make` takes a list when the fields share a type, and a tuple of
        // the fields' types when they do not.
        let iterable_type = match &one {
            Some(elem) => IRType::List(Box::new(elem.clone())),
            None if !field_types.is_empty() && field_types.iter().all(known) => {
                IRType::Tuple(field_types.clone())
            }
            None => continue,
        };
        let has_asdict = class.methods.iter().any(|f| f.name == "_asdict");
        let has_make = class.methods.iter().any(|f| f.name == "_make");
        if let (false, Some(elem)) = (has_asdict, &one) {
            class.methods.push(method(
                "_asdict",
                vec![self_param(&name)],
                IRType::Dict(Box::new(IRType::String), Box::new(elem.clone())),
                vec![IRStatement::Return(Some(IRExpr::DictLiteral(
                    names
                        .iter()
                        .map(|f| (text(f), field_of("self", f)))
                        .collect(),
                )))],
            ));
        }
        if !has_make {
            // `_make(iterable)`: CPython's message for the wrong length.
            let n = names.len() as i32;
            let message = format!("Expected {n} arguments, got ");
            module.memory_layout.add_string(&message);
            let mut cls = self_param(&name);
            cls.name = "cls".to_string();
            let it = IRParam {
                name: "iterable".to_string(),
                param_type: iterable_type,
                default_value: None,
                keyword_only: false,
                star: ParamStar::None,
            };
            let mut make = method(
                "_make",
                vec![cls, it],
                IRType::Class(name.clone()),
                vec![
                    if_(
                        cmp(len(v("iterable")), IRCompareOp::NotEq, int(n)),
                        vec![IRStatement::Raise {
                            exception: Some(call(
                                "TypeError",
                                vec![bin(
                                    text(&message),
                                    IROp::Add,
                                    call("str", vec![len(v("iterable"))]),
                                )],
                            )),
                            cause: None,
                        }],
                    ),
                    IRStatement::Return(Some(call(
                        &name,
                        (0..n).map(|i| idx(v("iterable"), int(i))).collect(),
                    ))),
                ],
            );
            make.decorators = vec!["classmethod".to_string()];
            class.methods.push(make);
        }
    }
    Ok(())
}
