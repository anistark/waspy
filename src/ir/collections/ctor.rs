//! Calls of the `collections` classes, as calls of their specializations.
//!
//! `Counter(words)` becomes `Counter[str]().<from>(words)`: a call of the
//! class specialized by the types the call needs, then the `<from>` variant
//! code generation picks for the argument's type. The types come, in this
//! order, from an annotation the value meets (the variable, field, parameter,
//! or return it is assigned to), the arguments, and what the function does
//! with the variable afterwards (`c[w] += 1` keys a Counter by `w`'s type,
//! `d[k].append(x)` makes a `defaultdict(list)`'s values lists of `x`'s type).
//! A call none of these types is refused with a hint to annotate it.

use super::classes::Factory;
use super::spec::{self, Kind};
use crate::ir::build::*;
use crate::ir::infer::{walk_body, Types};
use crate::ir::{
    IRBody, IRCompareOp, IRConstant, IRExpr, IRFunction, IRModule, IRStatement, IRType,
};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};

fn refused(message: String) -> anyhow::Error {
    crate::core::errors::unsupported_feature(message, None).into()
}

/// What the module calls the classes by.
struct Bindings {
    /// `from collections import Counter [as C]`: the name, and the kind.
    names: HashMap<String, Kind>,
    /// `import collections [as c]`.
    modules: HashSet<String>,
}

impl Bindings {
    fn of(module: &IRModule) -> Bindings {
        let mut names = HashMap::new();
        let mut modules = HashSet::new();
        for imp in &module.imports {
            if imp.module != "collections" {
                continue;
            }
            match (&imp.name, imp.is_from_import) {
                (Some(name), true) => {
                    if let Some(kind) = Kind::named(name) {
                        names.insert(imp.alias.clone().unwrap_or_else(|| name.clone()), kind);
                    }
                }
                (None, false) => {
                    modules.insert(
                        imp.alias
                            .clone()
                            .unwrap_or_else(|| "collections".to_string()),
                    );
                }
                _ => {}
            }
        }
        Bindings { names, modules }
    }

    /// The kind and arguments of a constructor call, if `expr` is one.
    fn constructor<'e>(&self, expr: &'e IRExpr) -> Option<(Kind, &'e [IRExpr])> {
        match expr {
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } => Some((*self.names.get(function_name)?, arguments.as_slice())),
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } => match object.as_ref() {
                IRExpr::Variable(m) if self.modules.contains(m) => {
                    Some((Kind::named(method_name)?, arguments.as_slice()))
                }
                _ => None,
            },
            _ => None,
        }
    }
}

/// What a function does with a variable (or a class with a field) holding
/// one of the classes, from which its types can be read.
#[derive(Default, Debug)]
struct Usage {
    /// Whether anything at all was seen.
    any: bool,
    /// What it is indexed by, searched for, or given as a key.
    keys: Vec<IRType>,
    /// What is stored at a key.
    values: Vec<IRType>,
    /// What is appended, or extended or updated with (element-wise).
    elems: Vec<IRType>,
    /// What is appended or added to the value at a key (`d[k].append(x)`).
    value_elems: Vec<IRType>,
    /// The keys and values stored into the value at a key (`d[k][j] = x`).
    value_keys: Vec<IRType>,
    value_values: Vec<IRType>,
}

impl Usage {
    fn is_empty(&self) -> bool {
        !self.any
            && self.keys.is_empty()
            && self.values.is_empty()
            && self.elems.is_empty()
            && self.value_elems.is_empty()
            && self.value_keys.is_empty()
            && self.value_values.is_empty()
    }
}

fn first_known(types: &[IRType]) -> Option<IRType> {
    types.iter().find(|t| known(t)).cloned()
}

/// Read every use of the place `is_place` matches in `body`.
fn observe(
    body: &IRBody,
    types: &Types,
    env: &mut HashMap<String, IRType>,
    is_place: &dyn Fn(&IRExpr) -> bool,
    usage: &mut Usage,
) {
    walk_body(body, types, env, &mut |expr, env| {
        let of = |e: &IRExpr| types.of(e, env);
        match expr {
            IRExpr::Indexing { container, index } if is_place(container) => {
                usage.keys.push(of(index));
            }
            IRExpr::CompareOp {
                left,
                right,
                op: IRCompareOp::In | IRCompareOp::NotIn,
            } if is_place(right) => {
                usage.keys.push(of(left));
                usage.elems.push(of(left));
            }
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } => {
                let first = arguments.first();
                if is_place(object) {
                    match (method_name.as_str(), first) {
                        ("append" | "appendleft", Some(x)) => usage.elems.push(of(x)),
                        ("extend" | "extendleft" | "update" | "subtract", Some(xs)) => {
                            let t = of(xs);
                            usage.elems.push(types.element(&t));
                            if let IRType::Dict(k, val) = t {
                                usage.keys.push(*k);
                                usage.values.push(*val);
                            }
                        }
                        (
                            "get"
                            | "pop"
                            | "setdefault"
                            | "move_to_end"
                            | crate::ir::DEL_ITEM_METHOD,
                            Some(k),
                        ) => usage.keys.push(of(k)),
                        _ => {}
                    }
                }
                if let IRExpr::Indexing { container, index } = object.as_ref() {
                    if is_place(container) {
                        usage.keys.push(of(index));
                        match (method_name.as_str(), first) {
                            ("append" | "add" | "appendleft", Some(x)) => {
                                usage.value_elems.push(of(x))
                            }
                            ("extend" | "update", Some(xs)) => {
                                usage.value_elems.push(types.element(&of(xs)))
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    });
    // Stores: `p[k] = x` and `p[k][j] = x`, which the expression walk does
    // not see as statements.
    fn stores(
        body: &IRBody,
        types: &Types,
        env: &mut HashMap<String, IRType>,
        is_place: &dyn Fn(&IRExpr) -> bool,
        usage: &mut Usage,
    ) {
        for stmt in &body.statements {
            if let IRStatement::IndexAssign {
                container,
                index,
                value,
            } = stmt
            {
                if is_place(container) {
                    usage.keys.push(types.of(index, env));
                    usage.values.push(types.of(value, env));
                }
                if let IRExpr::Indexing {
                    container: inner,
                    index: outer,
                } = container
                {
                    if is_place(inner) {
                        usage.keys.push(types.of(outer, env));
                        usage.value_keys.push(types.of(index, env));
                        usage.value_values.push(types.of(value, env));
                    }
                }
            }
            crate::ir::infer::bind(stmt, types, env);
            for nested in crate::ir::infer::nested_bodies(stmt) {
                stores(nested, types, env, is_place, usage);
            }
        }
    }
    let mut env2 = env.clone();
    stores(body, types, &mut env2, is_place, usage);
    // `t = p[k]` (how `p[k][j] += 1` is lowered, and a common hand-written
    // shape): what is done with `t` is done to the value at a key.
    fn aliases(body: &IRBody, is_place: &dyn Fn(&IRExpr) -> bool, out: &mut Vec<String>) {
        for stmt in &body.statements {
            if let IRStatement::Assign {
                target,
                value: IRExpr::Indexing { container, .. },
                ..
            } = stmt
            {
                if is_place(container) {
                    out.push(target.clone());
                }
            }
            for nested in crate::ir::infer::nested_bodies(stmt) {
                aliases(nested, is_place, out);
            }
        }
    }
    let mut names = Vec::new();
    aliases(body, is_place, &mut names);
    for name in names {
        let mut inner = Usage::default();
        let is_alias = |e: &IRExpr| matches!(e, IRExpr::Variable(n) if *n == name);
        let mut env3 = env.clone();
        observe(body, types, &mut env3, &is_alias, &mut inner);
        usage.value_keys.extend(inner.keys);
        usage.value_values.extend(inner.values);
        usage.value_elems.extend(inner.elems);
    }
}

/// A factory argument of `defaultdict`, and the value type it makes when
/// that is plain.
fn factory_of(
    arg: Option<&IRExpr>,
    st: &mut State,
) -> Result<(Factory, Option<IRType>, Option<String>)> {
    let Some(arg) = arg else {
        return Ok((Factory::Nothing, None, None));
    };
    Ok(match arg {
        IRExpr::Const(IRConstant::None) => (Factory::Nothing, None, None),
        IRExpr::Variable(name) => {
            let plain = |t: IRType| Some(t);
            match name.as_str() {
                "int" => (Factory::Type("int".into()), plain(IRType::Int), None),
                "float" => (Factory::Type("float".into()), plain(IRType::Float), None),
                "str" => (Factory::Type("str".into()), plain(IRType::String), None),
                "bool" => (Factory::Type("bool".into()), plain(IRType::Bool), None),
                "list" => (Factory::Type("list".into()), None, Some("list".into())),
                "set" => (Factory::Type("set".into()), None, Some("set".into())),
                "dict" => (Factory::Type("dict".into()), None, Some("dict".into())),
                n if st.bindings.names.contains_key(n) => {
                    (Factory::Type(n.to_string()), None, Some(n.to_string()))
                }
                n if st.types.classes.contains(n) => (
                    Factory::Type(n.to_string()),
                    plain(IRType::Class(n.to_string())),
                    None,
                ),
                n if st.types.returns.contains_key(n) => (
                    Factory::Function(n.to_string()),
                    st.types.returns.get(n).cloned(),
                    None,
                ),
                other => {
                    return Err(refused(format!(
                        "defaultdict()'s factory '{other}' is not supported: it takes a type \
                         (int, list, a class), a function of this module that takes no \
                         arguments, or a lambda"
                    )))
                }
            }
        }
        IRExpr::Lambda { params, body, .. } => {
            if !params.is_empty() {
                return Err(refused(
                    "defaultdict()'s factory is called with no arguments, so a lambda for it \
                     takes none"
                        .to_string(),
                ));
            }
            // The lambda becomes a function of its own, which it can only be
            // when it reads nothing of the scope it is written in.
            let mut free = Vec::new();
            free_names(body, &mut free);
            if let Some(name) = free.iter().find(|n| !st.globals.contains(*n)) {
                return Err(refused(format!(
                    "a defaultdict() factory lambda that reads '{name}' from where it is \
                     written is not supported. Hint: write a function of the module"
                )));
            }
            st.counter += 1;
            let f = format!("{}factory_{}", crate::ir::COLLECTIONS_FN_PREFIX, st.counter);
            let ty = st.types.of(body, &HashMap::new());
            st.new_functions.push(IRFunction {
                name: f.clone(),
                params: Vec::new(),
                body: IRBody {
                    statements: vec![ret_val((**body).clone())],
                },
                return_type: if known(&ty) {
                    ty.clone()
                } else {
                    IRType::Unknown
                },
                decorators: Vec::new(),
            });
            st.types.returns.insert(f.clone(), ty.clone());
            (Factory::Function(f), Some(ty).filter(known), None)
        }
        _ => {
            return Err(refused(
                "defaultdict()'s factory must be a type, a function, or a lambda".to_string(),
            ))
        }
    })
}

fn free_names(expr: &IRExpr, out: &mut Vec<String>) {
    let mut body = IRBody {
        statements: vec![IRStatement::Expression(expr.clone())],
    };
    let _ = crate::ir::visit_body(&mut body, &mut |e: &mut IRExpr| {
        if let IRExpr::Variable(n) | IRExpr::Param(n) = e {
            out.push(n.clone());
        }
        Ok(())
    });
}

/// The pass's state across the module.
struct State {
    bindings: Bindings,
    types: Types,
    /// Module-level names a lambda factory may read.
    globals: HashSet<String>,
    /// Declared parameter types of the module's functions, for the type a
    /// constructor call passed to one must have.
    params: HashMap<String, Vec<IRType>>,
    /// Every specialization made: class name -> (kind, args, factory).
    made: BTreeMap<String, (Kind, Vec<IRType>, Factory)>,
    new_functions: Vec<IRFunction>,
    counter: usize,
}

/// The class a constructor call makes, and the call that makes it.
fn construct(
    kind: Kind,
    arguments: &[IRExpr],
    expected: Option<&IRType>,
    usage: &Usage,
    env: &HashMap<String, IRType>,
    st: &mut State,
) -> Result<(String, IRExpr)> {
    let declared = match expected {
        Some(IRType::Class(c)) => match spec::parse(c) {
            Some((k, args)) if k == kind => Some(args),
            Some((k, _)) => {
                return Err(refused(format!(
                    "a {}() is assigned where a {} is declared",
                    kind.name(),
                    k.name()
                )))
            }
            None => None,
        },
        _ => None,
    };
    let positional: Vec<&IRExpr> = arguments
        .iter()
        .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
        .collect();
    let keywords: Vec<(&String, &IRExpr)> = arguments
        .iter()
        .filter_map(|a| match a {
            IRExpr::Keyword { name, value } => Some((name, value.as_ref())),
            _ => None,
        })
        .collect();
    // A defaultdict's factory may become a function of the module, so it is
    // read before anything borrows the types.
    let factory_info = if kind == Kind::DefaultDict {
        Some(factory_of(positional.first().copied(), st)?)
    } else {
        None
    };
    let of = |e: &IRExpr| st.types.of(e, env);
    // An empty one, given nothing and used for nothing that types it,
    // answers alike whatever its types are.
    let bare = positional.is_empty()
        && (keywords.is_empty() || kind == Kind::Deque)
        && declared.is_none()
        && usage.is_empty();
    let hint = |what: &str, example: &str| {
        refused(format!(
            "{}() here needs its {what}, which nothing shows: no annotation, argument, or later \
             use types it. Hint: annotate it, for example '{example}'",
            kind.name()
        ))
    };
    // What a mapping argument is: a dict, a list of pairs, or keywords.
    let mapping = |src: Option<&IRExpr>| -> (Option<IRType>, Option<IRType>) {
        if !keywords.is_empty() && kind != Kind::Deque {
            let values: Vec<IRType> = keywords.iter().map(|(_, v)| of(v)).collect();
            let first = values
                .first()
                .cloned()
                .filter(|t| values.iter().all(|u| u == t));
            return (Some(IRType::String), first);
        }
        match src.map(of) {
            Some(IRType::Dict(k, v)) => (Some(*k), Some(*v)),
            Some(IRType::List(e)) => match *e {
                IRType::Tuple(ms) if ms.len() == 2 => (Some(ms[0].clone()), Some(ms[1].clone())),
                _ => (None, None),
            },
            Some(IRType::Class(c)) => match spec::parse(&c) {
                Some((_, args)) if args.len() == 2 => {
                    (Some(args[0].clone()), Some(args[1].clone()))
                }
                Some((Kind::Counter, args)) => (Some(args[0].clone()), Some(IRType::Int)),
                _ => (None, None),
            },
            _ => (None, None),
        }
    };
    let mut factory = Factory::Nothing;
    let args: Vec<IRType> = match kind {
        Kind::Counter => {
            let k = declared
                .as_ref()
                .map(|a| a[0].clone())
                .or_else(|| {
                    let (mk, mv) = mapping(positional.first().copied());
                    if mk.is_some() && mv.as_ref().map_or(true, |v| *v == IRType::Int) {
                        return mk;
                    }
                    positional
                        .first()
                        .map(|a| st.types.element(&of(a)))
                        .filter(known)
                })
                .or_else(|| first_known(&usage.keys))
                .or_else(|| first_known(&usage.elems))
                .or_else(|| bare.then_some(IRType::Int))
                .ok_or_else(|| hint("element type", "c: Counter[str] = Counter()"))?;
            vec![k]
        }
        Kind::Deque => {
            let t = declared
                .as_ref()
                .map(|a| a[0].clone())
                .or_else(|| {
                    positional
                        .first()
                        .map(|a| st.types.element(&of(a)))
                        .filter(known)
                })
                .or_else(|| first_known(&usage.elems))
                .or_else(|| bare.then_some(IRType::Int))
                .ok_or_else(|| hint("element type", "q: deque[int] = deque()"))?;
            vec![t]
        }
        Kind::OrderedDict | Kind::ChainMap => {
            let (mk, mv) = mapping(positional.first().copied());
            let k = declared
                .as_ref()
                .map(|a| a[0].clone())
                .or(mk)
                .filter(known)
                .or_else(|| first_known(&usage.keys))
                .or_else(|| bare.then_some(IRType::Int))
                .ok_or_else(|| hint("key type", "d: OrderedDict[str, int] = OrderedDict()"))?;
            let v = declared
                .as_ref()
                .map(|a| a[1].clone())
                .or(mv)
                .filter(known)
                .or_else(|| first_known(&usage.values))
                .or_else(|| bare.then_some(IRType::Int))
                .ok_or_else(|| hint("value type", "d: OrderedDict[str, int] = OrderedDict()"))?;
            vec![k, v]
        }
        Kind::DefaultDict => {
            let (f, made, shaped) = factory_info.expect("read above");
            factory = f;
            let (mk, mv) = mapping(positional.get(1).copied());
            let k = declared
                .as_ref()
                .map(|a| a[0].clone())
                .or(mk)
                .filter(known)
                .or_else(|| first_known(&usage.keys))
                .ok_or_else(|| hint("key type", "d: defaultdict[str, int] = defaultdict(int)"))?;
            // The value: what the factory makes, filled in from what is put
            // into it when it is a container.
            let shaped_value = || -> Option<IRType> {
                Some(match shaped.as_deref()? {
                    "list" => IRType::List(Box::new(first_known(&usage.value_elems)?)),
                    "set" => IRType::Set(Box::new(first_known(&usage.value_elems)?)),
                    "dict" => IRType::Dict(
                        Box::new(first_known(&usage.value_keys)?),
                        Box::new(first_known(&usage.value_values)?),
                    ),
                    other => {
                        let inner =
                            Kind::named(other).or_else(|| st.bindings.names.get(other).copied())?;
                        let args = match inner {
                            Kind::Counter => vec![first_known(&usage.value_keys)
                                .or_else(|| first_known(&usage.value_elems))?],
                            Kind::Deque => vec![first_known(&usage.value_elems)?],
                            _ => vec![
                                first_known(&usage.value_keys)?,
                                first_known(&usage.value_values)?,
                            ],
                        };
                        IRType::Class(spec::name(inner, &args)?)
                    }
                })
            };
            let v = declared
                .as_ref()
                .map(|a| a[1].clone())
                .or(made)
                .or(mv)
                .filter(known)
                .or_else(shaped_value)
                .or_else(|| first_known(&usage.values))
                .ok_or_else(|| {
                    hint(
                        "value type",
                        "d: defaultdict[str, list[int]] = defaultdict(list)",
                    )
                })?;
            // A factory named for a container whose element types only the
            // value type gave makes that type.
            if let (Factory::Type(t), IRType::Class(c)) = (&factory, &v) {
                if let Some(inner) = st.bindings.names.get(t.as_str()) {
                    let _ = inner;
                    factory = Factory::Type(c.clone());
                }
            }
            vec![k, v]
        }
    };
    let base = spec::name(kind, &args).ok_or_else(|| {
        refused(format!(
            "{}() of these element types is not supported",
            kind.name()
        ))
    })?;
    // A defaultdict whose factory is not its value type's own constructor is
    // a class of its own, so its `__missing__` and repr are its factory's.
    let class = if kind == Kind::DefaultDict && !canonical(&factory, &args[1]) {
        let label = match &factory {
            Factory::Nothing => "None".to_string(),
            Factory::Type(t) | Factory::Function(t) => t.clone(),
        };
        format!("{base}<{label}>")
    } else {
        base
    };
    if let Some((_, _, f)) = st.made.get(&class) {
        debug_assert_eq!(*f, factory);
    }
    st.made
        .entry(class.clone())
        .or_insert((kind, args.clone(), factory.clone()));

    let empty = call(&class, Vec::new());
    let fill = |from: IRExpr| mcall(empty.clone(), crate::ir::FROM_METHOD, vec![from]);
    let keyword_dict = || {
        IRExpr::DictLiteral(
            keywords
                .iter()
                .map(|(k, v)| (text(k), (*v).clone()))
                .collect(),
        )
    };
    let made = match kind {
        Kind::Deque => {
            let maxlen = keywords
                .iter()
                .find(|(k, _)| k.as_str() == "maxlen")
                .map(|(_, v)| (*v).clone())
                .or_else(|| positional.get(1).map(|e| (*e).clone()));
            if let Some((bad, _)) = keywords.iter().find(|(k, _)| k.as_str() != "maxlen") {
                return Err(refused(format!(
                    "deque() got an unexpected keyword argument '{bad}'"
                )));
            }
            let start = call(&class, maxlen.into_iter().collect());
            match positional.first() {
                Some(src) => mcall(start, crate::ir::FROM_METHOD, vec![(*src).clone()]),
                None => start,
            }
        }
        Kind::ChainMap => {
            if positional.is_empty() {
                empty.clone()
            } else {
                fill(IRExpr::ListLiteral(
                    positional.iter().map(|e| (*e).clone()).collect(),
                ))
            }
        }
        Kind::DefaultDict => match positional.get(1) {
            Some(src) => fill((*src).clone()),
            None if !keywords.is_empty() => fill(keyword_dict()),
            None => empty.clone(),
        },
        Kind::Counter | Kind::OrderedDict => match positional.first() {
            Some(_) if !keywords.is_empty() => {
                return Err(refused(format!(
                    "{}() with both an argument and keywords is not supported",
                    kind.name()
                )))
            }
            Some(src) => fill((*src).clone()),
            None if !keywords.is_empty() => fill(keyword_dict()),
            None => empty.clone(),
        },
    };
    Ok((class, made))
}

/// Whether `factory` is the value type's own constructor, which the class
/// named for the types alone uses.
fn canonical(factory: &Factory, val: &IRType) -> bool {
    let Factory::Type(t) = factory else {
        return false;
    };
    match val {
        IRType::Int => t == "int",
        IRType::Float => t == "float",
        IRType::String => t == "str",
        IRType::Bool => t == "bool",
        IRType::List(_) => t == "list",
        IRType::Set(_) => t == "set",
        IRType::Dict(..) => t == "dict",
        IRType::Class(c) => t == c,
        _ => false,
    }
}

/// The factory the class named for `val` alone uses.
pub(crate) fn canonical_factory(val: &IRType) -> Factory {
    match val {
        IRType::Int => Factory::Type("int".into()),
        IRType::Float => Factory::Type("float".into()),
        IRType::String => Factory::Type("str".into()),
        IRType::Bool => Factory::Type("bool".into()),
        IRType::List(_) => Factory::Type("list".into()),
        IRType::Set(_) => Factory::Type("set".into()),
        IRType::Dict(..) => Factory::Type("dict".into()),
        IRType::Class(c) => Factory::Type(c.clone()),
        _ => Factory::Nothing,
    }
}

/// Rewrite the constructor calls in a body.
struct Rewriter<'a> {
    st: &'a mut State,
    /// What the function does with each of its variables.
    usage: &'a HashMap<String, Usage>,
    /// What the class does with each of its fields.
    field_usage: &'a HashMap<String, Usage>,
    returns: IRType,
}

impl Rewriter<'_> {
    fn body(&mut self, body: &mut IRBody, env: &mut HashMap<String, IRType>) -> Result<()> {
        for stmt in &mut body.statements {
            self.stmt(stmt, env)?;
            crate::ir::infer::bind(stmt, &self.st.types, env);
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &mut IRStatement, env: &mut HashMap<String, IRType>) -> Result<()> {
        let none = Usage::default();
        match stmt {
            IRStatement::Assign {
                target,
                value,
                var_type,
            } => {
                if let Some((kind, args)) = self.st.bindings.constructor(value) {
                    let args = args.to_vec();
                    let usage = self.usage.get(target).unwrap_or(&none);
                    let (class, made) =
                        construct(kind, &args, var_type.as_ref(), usage, env, self.st)?;
                    *value = made;
                    *var_type = Some(IRType::Class(class));
                    return self.expr(value, env, None);
                }
                self.expr(value, env, var_type.clone().as_ref())
            }
            IRStatement::AttributeAssign {
                object,
                attribute,
                value,
                annotation,
            } => {
                if let (Some((kind, args)), IRExpr::Variable(s)) =
                    (self.st.bindings.constructor(value), &*object)
                {
                    if s == "self" {
                        let args = args.to_vec();
                        let usage = self.field_usage.get(attribute).unwrap_or(&none);
                        let (class, made) =
                            construct(kind, &args, annotation.as_ref(), usage, env, self.st)?;
                        *value = made;
                        *annotation = Some(IRType::Class(class));
                        return self.expr(value, env, None);
                    }
                }
                self.expr(object, env, None)?;
                self.expr(value, env, annotation.clone().as_ref())
            }
            IRStatement::Return(Some(value)) => {
                let returns = self.returns.clone();
                self.expr(value, env, Some(&returns))
            }
            _ => {
                let mut result = Ok(());
                let mut exprs: Vec<&mut IRExpr> = Vec::new();
                statement_exprs_mut(stmt, &mut exprs);
                for e in exprs {
                    if result.is_ok() {
                        result = self.expr(e, env, None);
                    }
                }
                result?;
                for nested in nested_bodies_mut(stmt) {
                    self.body(nested, env)?;
                }
                Ok(())
            }
        }
    }

    fn expr(
        &mut self,
        expr: &mut IRExpr,
        env: &HashMap<String, IRType>,
        expected: Option<&IRType>,
    ) -> Result<()> {
        if let Some((kind, args)) = self.st.bindings.constructor(expr) {
            let args = args.to_vec();
            let (_, made) = construct(kind, &args, expected, &Usage::default(), env, self.st)?;
            *expr = made;
            // The arguments may hold constructors of their own.
            return self.children(expr, env);
        }
        // An operand beside one of a known class is expected to be one
        // (`c + Counter()`, `q == deque()`).
        if let IRExpr::BinaryOp { left, right, .. } | IRExpr::CompareOp { left, right, .. } = expr {
            let lt = self.st.types.of(left, env);
            let rt = self.st.types.of(right, env);
            let class = |t: &IRType| matches!(t, IRType::Class(c) if spec::parse(c).is_some());
            let (lt, rt) = (Some(lt).filter(class), Some(rt).filter(class));
            self.expr(left, env, rt.as_ref())?;
            return self.expr(right, env, lt.as_ref());
        }
        // A call of a module function passes each argument where its
        // parameter's declared type is expected.
        if let IRExpr::FunctionCall {
            function_name,
            arguments,
        } = expr
        {
            if let Some(params) = self.st.params.get(function_name).cloned() {
                for (a, p) in arguments.iter_mut().zip(params.iter()) {
                    self.expr(a, env, Some(p))?;
                }
                return Ok(());
            }
        }
        self.children(expr, env)
    }

    fn children(&mut self, expr: &mut IRExpr, env: &HashMap<String, IRType>) -> Result<()> {
        let mut kids: Vec<&mut IRExpr> = Vec::new();
        child_exprs_mut(expr, &mut kids);
        for k in kids {
            self.expr(k, env, None)?;
        }
        Ok(())
    }
}

/// The expressions a statement holds directly, for the rewriter's walk.
fn statement_exprs_mut<'s>(stmt: &'s mut IRStatement, out: &mut Vec<&'s mut IRExpr>) {
    match stmt {
        IRStatement::Return(Some(e)) | IRStatement::Yield { value: Some(e) } => out.push(e),
        IRStatement::AugAssign { value, .. }
        | IRStatement::TupleUnpack { value, .. }
        | IRStatement::Expression(value) => out.push(value),
        IRStatement::Raise { exception, cause } => {
            out.extend(exception.iter_mut());
            out.extend(cause.iter_mut());
        }
        IRStatement::If { condition, .. } | IRStatement::While { condition, .. } => {
            out.push(condition)
        }
        IRStatement::For { iterable, .. } => out.push(iterable),
        IRStatement::With { context_expr, .. } => out.push(context_expr),
        IRStatement::AttributeAugAssign { object, value, .. } => {
            out.push(object);
            out.push(value);
        }
        IRStatement::IndexAssign {
            container,
            index,
            value,
        } => {
            out.push(container);
            out.push(index);
            out.push(value);
        }
        _ => {}
    }
}

fn nested_bodies_mut(stmt: &mut IRStatement) -> Vec<&mut IRBody> {
    match stmt {
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            let mut v = vec![then_body.as_mut()];
            if let Some(e) = else_body {
                v.push(e.as_mut());
            }
            v
        }
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => vec![body.as_mut()],
        IRStatement::For {
            body, else_body, ..
        } => {
            let mut v = vec![body.as_mut()];
            if let Some(e) = else_body {
                v.push(e.as_mut());
            }
            v
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            let mut v = vec![try_body.as_mut()];
            v.extend(except_handlers.iter_mut().map(|h| &mut h.body));
            if let Some(f) = finally_body {
                v.push(f.as_mut());
            }
            v
        }
        _ => Vec::new(),
    }
}

fn child_exprs_mut<'e>(expr: &'e mut IRExpr, out: &mut Vec<&'e mut IRExpr>) {
    match expr {
        IRExpr::BinaryOp { left, right, .. }
        | IRExpr::CompareOp { left, right, .. }
        | IRExpr::BoolOp { left, right, .. } => {
            out.push(left);
            out.push(right);
        }
        IRExpr::UnaryOp { operand, .. } => out.push(operand),
        IRExpr::FunctionCall { arguments, .. } => out.extend(arguments.iter_mut()),
        IRExpr::MethodCall {
            object, arguments, ..
        } => {
            out.push(object);
            out.extend(arguments.iter_mut());
        }
        IRExpr::Keyword { value, .. } | IRExpr::CellStore { value, .. } => out.push(value),
        IRExpr::Conditional {
            condition,
            body,
            orelse,
        } => {
            out.push(condition);
            out.push(body);
            out.push(orelse);
        }
        IRExpr::ListLiteral(items) | IRExpr::SetLiteral(items) | IRExpr::TupleLiteral(items) => {
            out.extend(items.iter_mut())
        }
        IRExpr::DictLiteral(pairs) => {
            for (k, v) in pairs {
                out.push(k);
                out.push(v);
            }
        }
        IRExpr::Indexing { container, index } => {
            out.push(container);
            out.push(index);
        }
        IRExpr::Slicing {
            container,
            start,
            end,
            step,
        } => {
            out.push(container);
            for part in [start, end, step].into_iter().flatten() {
                out.push(part);
            }
        }
        IRExpr::Attribute { object, .. } => out.push(object),
        IRExpr::Comprehension {
            element,
            value,
            generators,
            ..
        } => {
            for g in generators {
                out.push(&mut g.iterable);
                out.extend(g.conditions.iter_mut());
            }
            out.push(element);
            if let Some(v) = value {
                out.push(v);
            }
        }
        IRExpr::RangeCall { start, stop, step } => {
            out.push(stop);
            for part in [start, step].into_iter().flatten() {
                out.push(part);
            }
        }
        IRExpr::Lambda { body, .. } => out.push(body),
        _ => {}
    }
}

/// Every class type a type mentions, the specializations among them.
fn mentioned(ty: &IRType, out: &mut Vec<String>) {
    match ty {
        IRType::Class(c) => {
            if let Some((_, args)) = spec::parse(c) {
                out.push(c.clone());
                for a in &args {
                    mentioned(a, out);
                }
            }
        }
        IRType::List(e)
        | IRType::Set(e)
        | IRType::VarTuple(e)
        | IRType::Optional(e)
        | IRType::Generator(e) => mentioned(e, out),
        IRType::Dict(k, v) => {
            mentioned(k, out);
            mentioned(v, out);
        }
        IRType::Tuple(ms) | IRType::Union(ms) => ms.iter().for_each(|m| mentioned(m, out)),
        IRType::Callable {
            params,
            return_type,
        } => {
            params.iter().for_each(|m| mentioned(m, out));
            mentioned(return_type, out);
        }
        _ => {}
    }
}

fn body_types(body: &IRBody, out: &mut Vec<String>) {
    for stmt in &body.statements {
        match stmt {
            IRStatement::Assign {
                var_type: Some(t), ..
            } => mentioned(t, out),
            IRStatement::AttributeAssign {
                annotation: Some(t),
                ..
            } => mentioned(t, out),
            _ => {}
        }
        for nested in crate::ir::infer::nested_bodies(stmt) {
            body_types(nested, out);
        }
    }
}

/// Make each constructor call a call of its specialization, and add the
/// specializations the module names to it.
pub(crate) fn lower(module: &mut IRModule) -> Result<()> {
    let bindings = Bindings::of(module);
    let mut named: Vec<String> = Vec::new();
    for f in &module.functions {
        f.params
            .iter()
            .for_each(|p| mentioned(&p.param_type, &mut named));
        mentioned(&f.return_type, &mut named);
        body_types(&f.body, &mut named);
    }
    for c in &module.classes {
        for m in &c.methods {
            m.params
                .iter()
                .for_each(|p| mentioned(&p.param_type, &mut named));
            mentioned(&m.return_type, &mut named);
            body_types(&m.body, &mut named);
        }
    }
    for var in &module.variables {
        if let Some(t) = &var.var_type {
            mentioned(t, &mut named);
        }
    }
    if bindings.names.is_empty() && bindings.modules.is_empty() && named.is_empty() {
        return Ok(());
    }
    let types = Types::of_module(module);
    let mut globals: HashSet<String> = module.functions.iter().map(|f| f.name.clone()).collect();
    globals.extend(module.classes.iter().map(|c| c.name.clone()));
    globals.extend(module.variables.iter().map(|v| v.name.clone()));
    let params = module
        .functions
        .iter()
        .map(|f| {
            (
                f.name.clone(),
                f.params.iter().map(|p| p.param_type.clone()).collect(),
            )
        })
        .collect();
    let mut st = State {
        bindings,
        types,
        globals,
        params,
        made: BTreeMap::new(),
        new_functions: Vec::new(),
        counter: 0,
    };

    // Functions: each variable's later uses.
    for i in 0..module.functions.len() {
        let f = &module.functions[i];
        let env: HashMap<String, IRType> = f
            .params
            .iter()
            .map(|p| (p.name.clone(), p.param_type.clone()))
            .collect();
        let usage = usage_of_locals(&f.body, &st.types, &env);
        let returns = f.return_type.clone();
        let mut env = env;
        let mut body = std::mem::replace(
            &mut module.functions[i].body,
            IRBody {
                statements: Vec::new(),
            },
        );
        let empty = HashMap::new();
        let result = Rewriter {
            st: &mut st,
            usage: &usage,
            field_usage: &empty,
            returns,
        }
        .body(&mut body, &mut env);
        module.functions[i].body = body;
        result?;
    }
    // Methods: their locals' uses, and each field's across the class.
    for ci in 0..module.classes.len() {
        let mut field_usage: HashMap<String, Usage> = HashMap::new();
        let fields: HashSet<String> = module.classes[ci]
            .methods
            .iter()
            .flat_map(|m| assigned_fields(&m.body))
            .collect();
        for field in &fields {
            let mut usage = Usage::default();
            for m in &module.classes[ci].methods {
                let mut env: HashMap<String, IRType> = m
                    .params
                    .iter()
                    .map(|p| (p.name.clone(), p.param_type.clone()))
                    .collect();
                let is_field = |e: &IRExpr| {
                    matches!(e, IRExpr::Attribute { object, attribute }
                        if attribute == field && matches!(object.as_ref(), IRExpr::Variable(s) if s == "self"))
                };
                observe(&m.body, &st.types, &mut env, &is_field, &mut usage);
            }
            field_usage.insert(field.clone(), usage);
        }
        for mi in 0..module.classes[ci].methods.len() {
            let m = &module.classes[ci].methods[mi];
            let env: HashMap<String, IRType> = m
                .params
                .iter()
                .map(|p| (p.name.clone(), p.param_type.clone()))
                .collect();
            let usage = usage_of_locals(&m.body, &st.types, &env);
            let returns = m.return_type.clone();
            let mut env = env;
            let mut body = std::mem::replace(
                &mut module.classes[ci].methods[mi].body,
                IRBody {
                    statements: Vec::new(),
                },
            );
            let result = Rewriter {
                st: &mut st,
                usage: &usage,
                field_usage: &field_usage,
                returns,
            }
            .body(&mut body, &mut env);
            module.classes[ci].methods[mi].body = body;
            result?;
        }
    }
    // Module-level definitions.
    for vi in 0..module.variables.len() {
        let mut value = module.variables[vi].value.clone();
        let mut var_type = module.variables[vi].var_type.clone();
        if let Some((kind, args)) = st.bindings.constructor(&value) {
            let args = args.to_vec();
            let (class, made) = construct(
                kind,
                &args,
                var_type.as_ref(),
                &Usage::default(),
                &HashMap::new(),
                &mut st,
            )?;
            value = made;
            var_type = Some(IRType::Class(class));
        } else {
            let empty = HashMap::new();
            Rewriter {
                st: &mut st,
                usage: &empty,
                field_usage: &empty,
                returns: IRType::Unknown,
            }
            .expr(&mut value, &HashMap::new(), var_type.clone().as_ref())?;
        }
        module.variables[vi].value = value;
        module.variables[vi].var_type = var_type;
    }

    // A bare `Counter` (or `deque`, ...) left in an annotation says nothing
    // of what it holds, and every class here is made for its types.
    // (A class of the module's own of that name is the module's.)
    let own: HashSet<String> = module.classes.iter().map(|c| c.name.clone()).collect();
    let bare = |t: &IRType| -> Option<String> {
        let mut out = None;
        fn walk(t: &IRType, own: &HashSet<String>, out: &mut Option<String>) {
            match t {
                IRType::Class(c) if Kind::named(c).is_some() && !own.contains(c) => {
                    *out = Some(c.clone())
                }
                IRType::List(e) | IRType::Set(e) | IRType::VarTuple(e) | IRType::Optional(e) => {
                    walk(e, own, out)
                }
                IRType::Dict(k, v) => {
                    walk(k, own, out);
                    walk(v, own, out);
                }
                IRType::Tuple(ms) => ms.iter().for_each(|m| walk(m, own, out)),
                _ => {}
            }
        }
        walk(t, &own, &mut out);
        out
    };
    let mut annotated: Vec<(String, IRType)> = Vec::new();
    for f in &module.functions {
        for p in &f.params {
            annotated.push((f.name.clone(), p.param_type.clone()));
        }
        annotated.push((f.name.clone(), f.return_type.clone()));
    }
    for c in &module.classes {
        for m in &c.methods {
            for p in &m.params {
                annotated.push((format!("{}.{}", c.name, m.name), p.param_type.clone()));
            }
            annotated.push((format!("{}.{}", c.name, m.name), m.return_type.clone()));
        }
    }
    for (place, t) in &annotated {
        if let Some(kind) = bare(t) {
            let example = match Kind::named(&kind).map(Kind::arity) {
                Some(1) => format!("{kind}[str]"),
                _ => format!("{kind}[str, int]"),
            };
            return Err(refused(format!(
                "a bare '{kind}' annotation (in '{place}') does not say what it holds, which \
                 the compiler needs. Hint: write its types, for example '{example}'"
            )));
        }
    }

    // Every specialization named anywhere, and the ones inside them.
    let mut pending: Vec<String> = named;
    pending.extend(st.made.keys().cloned());
    let mut done: HashSet<String> = HashSet::new();
    let generators: Vec<(String, IRType)> = st
        .types
        .yields
        .iter()
        .filter(|(_, y)| known(y))
        .map(|(f, y)| (format!("__Gen_{f}"), y.clone()))
        .collect();
    while let Some(class) = pending.pop() {
        if !done.insert(class.clone()) {
            continue;
        }
        let (kind, args, factory) = match st.made.get(&class) {
            Some(m) => m.clone(),
            None => match spec::parse(&class) {
                Some((kind, args)) => {
                    let factory = if kind == Kind::DefaultDict {
                        canonical_factory(&args[1])
                    } else {
                        Factory::Nothing
                    };
                    (kind, args, factory)
                }
                None => continue,
            },
        };
        for a in &args {
            let mut inner = Vec::new();
            mentioned(a, &mut inner);
            pending.extend(inner);
        }
        module.classes.extend(super::classes::synthesize(
            kind,
            &class,
            &args,
            &factory,
            &generators,
        ));
    }
    module.functions.extend(st.new_functions);
    Ok(())
}

/// The fields a method body assigns (`self.f = ...`).
fn assigned_fields(body: &IRBody) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in &body.statements {
        if let IRStatement::AttributeAssign {
            object: IRExpr::Variable(s),
            attribute,
            ..
        } = stmt
        {
            if s == "self" {
                out.push(attribute.clone());
            }
        }
        for nested in crate::ir::infer::nested_bodies(stmt) {
            out.extend(assigned_fields(nested));
        }
    }
    out
}

/// What a body does with each variable it assigns.
fn usage_of_locals(
    body: &IRBody,
    types: &Types,
    env: &HashMap<String, IRType>,
) -> HashMap<String, Usage> {
    let mut names = HashSet::new();
    fn assigned(body: &IRBody, out: &mut HashSet<String>) {
        for stmt in &body.statements {
            if let IRStatement::Assign { target, .. } = stmt {
                out.insert(target.clone());
            }
            for nested in crate::ir::infer::nested_bodies(stmt) {
                assigned(nested, out);
            }
        }
    }
    assigned(body, &mut names);
    names
        .into_iter()
        .map(|name| {
            let mut usage = Usage::default();
            let is_var = |e: &IRExpr| matches!(e, IRExpr::Variable(n) if *n == name);
            let mut env = env.clone();
            observe(body, types, &mut env, &is_var, &mut usage);
            (name.clone(), usage)
        })
        .collect()
}
