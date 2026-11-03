//! `json`'s classes, `JSONEncoder` and `JSONDecoder`, written out in IR.
//!
//! The module's functions are code generation (`compiler::stdlib::json`),
//! since what `dumps` writes and what `loads` parses depend on static types.
//! Its two classes are classes of the module, `json.JSONEncoder` and
//! `json.JSONDecoder`, whose `__init__` keeps CPython's options as fields,
//! so an encoder made with `indent=2` and held in a variable writes as
//! CPython's does. Their `encode()` and `decode()` are compiled where the
//! receiver's class is known, through the same writer and parser as
//! `dumps()` and `loads()`, reading the options from the instance.
//!
//! The pass makes the program's names for the classes name them, writes
//! CPython's spellings of the options in the form the fields hold (an
//! `indent` of 2 is the text it indents by, `separators` its two strings),
//! makes `dumps(obj, cls=C, ...)` the `C(...).encode(obj)` CPython makes it,
//! and `super().default(o)` in an encoder the `TypeError` CPython's raises.
//! A decoder's `object_hook` is a function of the module whose signature is
//! known here, recorded on the class (`OBJECT_HOOK_TYPE_METHOD`) for code
//! generation to call it at.

use crate::ir::build::*;
use crate::ir::{
    IRBody, IRClass, IRCompareOp, IRConstant, IRExpr, IRFunction, IRModule, IROp, IRParam,
    IRStatement, IRType,
};
use anyhow::Result;
use std::collections::HashMap;

pub const ENCODER_CLASS: &str = "json.JSONEncoder";
pub const DECODER_CLASS: &str = "json.JSONDecoder";
/// The text one level of indentation is, or None: `indent` as `dumps` uses it.
pub const INDENT_FIELD: &str = "<indent>";
/// `separators`' two strings, as `__init__` takes them.
const ITEM_SEP_PARAM: &str = "<item_separator>";
const KEY_SEP_PARAM: &str = "<key_separator>";
/// A decoder's `object_hook`, the closure or None.
pub const OBJECT_HOOK_FIELD: &str = "<object_hook>";
/// A method of `json.JSONDecoder` whose signature is the program's
/// `object_hook`'s; it is never called.
pub const OBJECT_HOOK_TYPE_METHOD: &str = "<object_hook_type>";

/// What the module calls the module and its names by.
struct Bindings {
    /// `import json [as j]`.
    modules: Vec<String>,
    /// `from json import JSONEncoder [as E]`: the name, and what it names.
    names: HashMap<String, String>,
}

impl Bindings {
    fn of(module: &IRModule) -> Bindings {
        let mut modules = Vec::new();
        let mut names = HashMap::new();
        for imp in &module.imports {
            if imp.module != "json" {
                continue;
            }
            match (&imp.name, imp.is_from_import) {
                (Some(name), true) => {
                    names.insert(
                        imp.alias.clone().unwrap_or_else(|| name.clone()),
                        name.clone(),
                    );
                }
                (None, false) => {
                    modules.push(imp.alias.clone().unwrap_or_else(|| "json".to_string()))
                }
                _ => {}
            }
        }
        Bindings { modules, names }
    }

    /// The class a name of the program is, if it is one of the module's.
    fn class(&self, name: &str) -> Option<&'static str> {
        let member = match name.split_once('.') {
            Some((m, member)) if self.modules.iter().any(|x| x == m) => member,
            Some(_) => return None,
            None => self.names.get(name)?.as_str(),
        };
        match member {
            "JSONEncoder" => Some(ENCODER_CLASS),
            "JSONDecoder" => Some(DECODER_CLASS),
            _ => None,
        }
    }

    /// The module's function a call is of (`json.dumps(...)`, or `dumps(...)`
    /// imported), with its arguments.
    fn function_call<'e>(&self, expr: &'e mut IRExpr) -> Option<(String, &'e mut Vec<IRExpr>)> {
        match expr {
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } => match object.as_ref() {
                IRExpr::Variable(m) if self.modules.contains(m) => {
                    Some((method_name.clone(), arguments))
                }
                _ => None,
            },
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } => self
                .names
                .get(function_name)
                .map(|n| (n.clone(), arguments)),
            _ => None,
        }
    }
}

fn keyword_value<'e>(arguments: &'e [IRExpr], name: &str) -> Option<&'e IRExpr> {
    arguments.iter().find_map(|a| match a {
        IRExpr::Keyword { name: n, value } if n == name => Some(value.as_ref()),
        _ => None,
    })
}

fn keyword(name: &str, value: IRExpr) -> IRExpr {
    IRExpr::Keyword {
        name: name.to_string(),
        value: Box::new(value),
    }
}

fn is_none(e: &IRExpr) -> bool {
    matches!(e, IRExpr::Const(IRConstant::None))
}

/// The module's classes and the program's, by name, with their bases and
/// whether each defines a method.
struct Classes<'m> {
    by_name: HashMap<&'m str, &'m IRClass>,
}

impl<'m> Classes<'m> {
    fn of(module: &'m IRModule) -> Classes<'m> {
        Classes {
            by_name: module
                .classes
                .iter()
                .map(|c| (c.name.as_str(), c))
                .collect(),
        }
    }

    /// Whether `class` is `root` or derives from it.
    fn derives(&self, class: &str, root: &str) -> bool {
        let mut seen = Vec::new();
        let mut stack = vec![class.to_string()];
        while let Some(c) = stack.pop() {
            if c == root {
                return true;
            }
            if seen.contains(&c) {
                continue;
            }
            if let Some(ir) = self.by_name.get(c.as_str()) {
                stack.extend(ir.bases.iter().cloned());
            }
            seen.push(c);
        }
        false
    }

    /// Whether looking `method` up from `class` (itself included) reaches
    /// `root`'s before any class of the program defines it.
    fn reaches_root(&self, class: &str, root: &str, method: &str) -> bool {
        if class == root {
            return true;
        }
        let Some(ir) = self.by_name.get(class) else {
            return false;
        };
        if ir.methods.iter().any(|m| m.name == method) {
            return false;
        }
        ir.bases
            .iter()
            .find(|b| self.derives(b, root))
            .is_some_and(|b| self.reaches_root(b, root, method))
    }

    /// The family `class` is of: `ENCODER_CLASS`, `DECODER_CLASS`, or neither.
    fn family(&self, class: &str) -> Option<&'static str> {
        [ENCODER_CLASS, DECODER_CLASS]
            .into_iter()
            .find(|root| self.derives(class, root))
    }
}

/// The arguments of a call that reaches `JSONEncoder.__init__`, its options
/// written as the fields hold them; `constants` are the module's.
fn encoder_arguments(
    arguments: Vec<IRExpr>,
    constants: &HashMap<String, IRConstant>,
) -> Result<Vec<IRExpr>> {
    let mut out = Vec::new();
    for a in arguments {
        let IRExpr::Keyword { name, value } = &a else {
            out.push(a);
            continue;
        };
        match name.as_str() {
            "indent" => {
                // An int is the number of spaces, a str the text itself; the
                // field holds the text, so the value must be known here.
                let constant = match value.as_ref() {
                    IRExpr::Const(c) => Some(c),
                    IRExpr::Variable(n) => constants.get(n),
                    _ => None,
                };
                match constant {
                    Some(IRConstant::Int(n)) => {
                        out.push(keyword("indent", text(&" ".repeat((*n).max(0) as usize))))
                    }
                    Some(c @ (IRConstant::String(_) | IRConstant::None)) => {
                        out.push(keyword("indent", IRExpr::Const(c.clone())))
                    }
                    _ => {
                        return Err(refused(
                            "a JSONEncoder's indent must be a constant int, str, or None (a \
                             literal or a module constant)"
                                .to_string(),
                        ))
                    }
                }
            }
            "separators" => match value.as_ref() {
                v if is_none(v) => {}
                IRExpr::TupleLiteral(items) if items.len() == 2 => {
                    out.push(keyword(ITEM_SEP_PARAM, items[0].clone()));
                    out.push(keyword(KEY_SEP_PARAM, items[1].clone()));
                }
                other => {
                    out.push(keyword(ITEM_SEP_PARAM, idx(other.clone(), int(0))));
                    out.push(keyword(KEY_SEP_PARAM, idx(other.clone(), int(1))));
                }
            },
            "default" if is_none(value) => {}
            "default" => {
                return Err(refused(
                    "a JSONEncoder's default= is not supported. Hint: override default() in a \
                     subclass, or pass default= to json.dumps()"
                        .to_string(),
                ))
            }
            _ => out.push(a),
        }
    }
    Ok(out)
}

/// The arguments of a call that reaches `JSONDecoder.__init__`, each
/// `object_hook` found added to `hooks`.
fn decoder_arguments(arguments: Vec<IRExpr>, hooks: &mut Vec<IRExpr>) -> Result<Vec<IRExpr>> {
    let mut out = Vec::new();
    for a in arguments {
        let IRExpr::Keyword { name, value } = &a else {
            out.push(a);
            continue;
        };
        match name.as_str() {
            "object_hook" if is_none(value) => {}
            "object_hook" => {
                hooks.push((**value).clone());
                out.push(a);
            }
            "parse_float" | "parse_int" | "parse_constant" | "object_pairs_hook" => {
                if !is_none(value) {
                    return Err(refused(format!(
                        "a JSONDecoder's {name}= is not supported. Hint: convert the values in \
                         an object_hook"
                    )));
                }
            }
            "strict" => match value.as_ref() {
                IRExpr::Const(IRConstant::Bool(true)) => {}
                _ => {
                    return Err(refused(
                        "a JSONDecoder's strict= other than True is not supported: control \
                         characters in strings are refused, as CPython's default does"
                            .to_string(),
                    ))
                }
            },
            _ => out.push(a),
        }
    }
    Ok(out)
}

struct Pass<'b> {
    b: &'b Bindings,
    /// The module's variables whose values are constants.
    constants: HashMap<String, IRConstant>,
    /// Each class of the program in `json`'s families, and whether a call of
    /// it reaches the family's `__init__`.
    families: HashMap<String, (&'static str, bool)>,
    /// The classes whose `super().__init__(...)` reaches the family's.
    super_reaches: Vec<String>,
    hooks: Vec<IRExpr>,
    uses: [bool; 2],
}

impl Pass<'_> {
    fn note(&mut self, family: &str) {
        self.uses[usize::from(family == DECODER_CLASS)] = true;
    }

    /// `C(arguments)` for a class of a family, its options normalized when
    /// the call reaches the family's `__init__`.
    fn construct(&mut self, class: &str, arguments: Vec<IRExpr>) -> Result<IRExpr> {
        let Some(&(family, reaches)) = self.families.get(class) else {
            return Ok(call(class, arguments));
        };
        self.note(family);
        if !reaches {
            return Ok(call(class, arguments));
        }
        let arguments = if family == ENCODER_CLASS {
            encoder_arguments(arguments, &self.constants)?
        } else {
            decoder_arguments(arguments, &mut self.hooks)?
        };
        Ok(call(class, arguments))
    }

    /// `json.dumps(obj, cls=C, ...)` and its kin as CPython makes them:
    /// `C(...).encode(obj)`.
    fn with_cls(&mut self, function: &str, arguments: Vec<IRExpr>) -> Result<Option<IRExpr>> {
        let Some(cls) = keyword_value(&arguments, "cls").cloned() else {
            return Ok(None);
        };
        if is_none(&cls) {
            return Ok(None);
        }
        let mut rest: Vec<IRExpr> = arguments
            .into_iter()
            .filter(|a| !matches!(a, IRExpr::Keyword { name, .. } if name == "cls"))
            .collect();
        let (encoding, takes) = match function {
            "dumps" => (true, 1),
            "dump" => (true, 2),
            "loads" | "load" => (false, 1),
            _ => return Ok(None),
        };
        let root = if encoding {
            ENCODER_CLASS
        } else {
            DECODER_CLASS
        };
        let class = match &cls {
            IRExpr::Variable(c) => self
                .families
                .get(c)
                .filter(|(f, _)| *f == root)
                .map(|(_, reaches)| (c.clone(), *reaches)),
            _ => None,
        };
        let what = if encoding {
            "JSONEncoder"
        } else {
            "JSONDecoder"
        };
        let Some((class, reaches)) = class else {
            return Err(refused(format!(
                "json.{function}()'s cls must name a {what} subclass of this module"
            )));
        };
        if !reaches {
            return Err(refused(format!(
                "json.{function}(cls={class}) where {class} defines __init__ is not supported: \
                 CPython passes every option to it by name"
            )));
        }
        let positional: Vec<IRExpr> = rest
            .iter()
            .filter(|a| !matches!(a, IRExpr::Keyword { .. }))
            .cloned()
            .collect();
        if positional.len() != takes {
            return Err(refused(format!(
                "json.{function}() takes {} positionally",
                if takes == 2 {
                    "the value and the file"
                } else {
                    "one argument"
                }
            )));
        }
        rest.retain(|a| matches!(a, IRExpr::Keyword { .. }));
        let instance = self.construct(&class, rest)?;
        let mut positional = positional.into_iter();
        let first = positional.next().expect("checked");
        Ok(Some(match function {
            "dumps" => mcall(instance, "encode", vec![first]),
            "dump" => mcall(
                positional.next().expect("checked"),
                "write",
                vec![mcall(instance, "encode", vec![first])],
            ),
            "loads" => mcall(instance, "decode", vec![first]),
            _ => mcall(instance, "decode", vec![mcall(first, "read", Vec::new())]),
        }))
    }

    fn rewrite(&mut self, expr: &mut IRExpr) -> Result<()> {
        // `json.JSONEncoder` and `JSONEncoder` imported, as a value.
        let named = match expr {
            IRExpr::Attribute { object, attribute } => match object.as_ref() {
                IRExpr::Variable(m) => self.b.class(&format!("{m}.{attribute}")),
                _ => None,
            },
            IRExpr::Variable(n) => self.b.class(n),
            _ => None,
        };
        if let Some(class) = named {
            self.note(class);
            *expr = v(class);
            return Ok(());
        }
        // A call of the module's class.
        let constructed = match expr {
            IRExpr::MethodCall {
                object,
                method_name,
                arguments,
            } => match object.as_ref() {
                IRExpr::Variable(m) => self
                    .b
                    .class(&format!("{m}.{method_name}"))
                    .map(|c| (c.to_string(), std::mem::take(arguments))),
                _ => None,
            },
            IRExpr::FunctionCall {
                function_name,
                arguments,
            } => self
                .b
                .class(function_name)
                .map(str::to_string)
                .or_else(|| {
                    self.families
                        .contains_key(function_name.as_str())
                        .then(|| function_name.clone())
                })
                .map(|c| (c, std::mem::take(arguments))),
            _ => None,
        };
        if let Some((class, arguments)) = constructed {
            *expr = self.construct(&class, arguments)?;
            return Ok(());
        }
        if let Some((function, arguments)) = self.b.function_call(expr) {
            // `cls=None` is the module's own class.
            arguments.retain(|a| {
                !matches!(a, IRExpr::Keyword { name, value } if name == "cls" && is_none(value))
            });
            let arguments = std::mem::take(arguments);
            match self.with_cls(&function, arguments.clone())? {
                Some(rewritten) => *expr = rewritten,
                None => {
                    if let Some((_, slot)) = self.b.function_call(expr) {
                        *slot = arguments;
                    }
                }
            }
        }
        Ok(())
    }

    /// `super().__init__(...)` in a method of `class`, when it reaches the
    /// family's `__init__`.
    fn super_init(&mut self, class: &str, expr: &mut IRExpr) -> Result<()> {
        let IRExpr::MethodCall {
            object,
            method_name,
            arguments,
        } = expr
        else {
            return Ok(());
        };
        if method_name != "__init__"
            || !matches!(object.as_ref(), IRExpr::FunctionCall { function_name, arguments }
                if function_name == "super" && arguments.is_empty())
        {
            return Ok(());
        }
        let Some(&(family, _)) = self.families.get(class) else {
            return Ok(());
        };
        if !self.super_reaches.iter().any(|c| c == class) {
            return Ok(());
        }
        let taken = std::mem::take(arguments);
        *arguments = if family == ENCODER_CLASS {
            encoder_arguments(taken, &self.constants)?
        } else {
            decoder_arguments(taken, &mut self.hooks)?
        };
        Ok(())
    }
}

/// `super().default(o)`, where it reaches `JSONEncoder.default`, as the
/// `TypeError` that raises.
fn not_serializable(stmt: &mut IRStatement) {
    let target = match stmt {
        IRStatement::Return(Some(e)) | IRStatement::Expression(e) => e,
        IRStatement::If {
            then_body,
            else_body,
            ..
        } => {
            body_not_serializable(then_body);
            if let Some(e) = else_body {
                body_not_serializable(e);
            }
            return;
        }
        IRStatement::While { body, .. } | IRStatement::With { body, .. } => {
            body_not_serializable(body);
            return;
        }
        IRStatement::For {
            body, else_body, ..
        } => {
            body_not_serializable(body);
            if let Some(e) = else_body {
                body_not_serializable(e);
            }
            return;
        }
        IRStatement::TryExcept {
            try_body,
            except_handlers,
            finally_body,
        } => {
            body_not_serializable(try_body);
            for h in except_handlers {
                body_not_serializable(&mut h.body);
            }
            if let Some(f) = finally_body {
                body_not_serializable(f);
            }
            return;
        }
        _ => return,
    };
    let IRExpr::MethodCall {
        object,
        method_name,
        arguments,
    } = target
    else {
        return;
    };
    let is_super = matches!(object.as_ref(), IRExpr::FunctionCall { function_name, arguments }
        if function_name == "super" && arguments.is_empty());
    if !is_super || method_name != "default" || arguments.len() != 1 {
        return;
    }
    let o = arguments[0].clone();
    let message = bin(
        bin(
            text("Object of type "),
            IROp::Add,
            attr(attr(o, "__class__"), "__name__"),
        ),
        IROp::Add,
        text(" is not JSON serializable"),
    );
    *stmt = raise_expr("TypeError", message);
}

fn body_not_serializable(body: &mut IRBody) {
    body.statements.iter_mut().for_each(not_serializable);
}

fn rename_type(ty: &mut IRType, b: &Bindings) {
    match ty {
        IRType::Class(c) | IRType::ClassRef(c) => {
            if let Some(class) = b.class(c) {
                *c = class.to_string();
            }
        }
        IRType::List(e) | IRType::Set(e) | IRType::VarTuple(e) | IRType::Optional(e) => {
            rename_type(e, b)
        }
        IRType::Dict(k, v) => {
            rename_type(k, b);
            rename_type(v, b);
        }
        IRType::Tuple(ms) | IRType::Union(ms) => ms.iter_mut().for_each(|m| rename_type(m, b)),
        _ => {}
    }
}

fn rename_function_types(f: &mut IRFunction, b: &Bindings) {
    f.params
        .iter_mut()
        .for_each(|p| rename_type(&mut p.param_type, b));
    rename_type(&mut f.return_type, b);
}

fn bool_ty() -> IRType {
    IRType::Bool
}

fn keyword_param(name: &str, ty: &IRType, default: IRExpr) -> IRParam {
    IRParam {
        keyword_only: true,
        ..param_default(name, ty, default)
    }
}

fn me(class: &str) -> IRParam {
    param("self", &IRType::Class(class.to_string()))
}

/// `json.JSONEncoder`, its options as fields.
fn encoder_class() -> IRClass {
    let flags = [
        ("skipkeys", false),
        ("ensure_ascii", true),
        ("check_circular", true),
        ("allow_nan", true),
        ("sort_keys", false),
    ];
    let optional_str = IRType::Optional(Box::new(IRType::String));
    let mut params = vec![me(ENCODER_CLASS)];
    params.extend(
        flags
            .iter()
            .map(|(n, d)| keyword_param(n, &bool_ty(), boolean(*d))),
    );
    params.push(keyword_param("indent", &optional_str, none()));
    params.push(keyword_param(ITEM_SEP_PARAM, &optional_str, none()));
    params.push(keyword_param(KEY_SEP_PARAM, &IRType::String, text(": ")));
    let mut body: Vec<IRStatement> = flags
        .iter()
        .map(|(n, _)| set_my_typed(n, v(n), &bool_ty()))
        .collect();
    body.push(set_my_typed(INDENT_FIELD, v("indent"), &optional_str));
    body.push(set_my_typed("item_separator", text(", "), &IRType::String));
    body.push(set_my_typed(
        "key_separator",
        v(KEY_SEP_PARAM),
        &IRType::String,
    ));
    body.push(if_else(
        cmp(v(ITEM_SEP_PARAM), IRCompareOp::IsNot, none()),
        vec![set_attr(v("self"), "item_separator", v(ITEM_SEP_PARAM))],
        vec![if_(
            cmp(v("indent"), IRCompareOp::IsNot, none()),
            vec![set_attr(v("self"), "item_separator", text(","))],
        )],
    ));
    IRClass {
        name: ENCODER_CLASS.to_string(),
        bases: Vec::new(),
        methods: vec![method("__init__", params, IRType::None, body)],
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// `json.JSONDecoder`, its `object_hook` a field, and the hook's signature
/// a method when the program passes one.
fn decoder_class(hook: Option<(IRType, IRType)>) -> IRClass {
    let callable = IRType::Callable {
        params: Vec::new(),
        return_type: Box::new(IRType::Unknown),
    };
    let params = vec![
        me(DECODER_CLASS),
        keyword_param("object_hook", &callable, none()),
        keyword_param("strict", &bool_ty(), boolean(true)),
    ];
    let body = vec![
        set_my_typed("strict", v("strict"), &bool_ty()),
        set_my_typed(OBJECT_HOOK_FIELD, v("object_hook"), &callable),
    ];
    let mut methods = vec![method("__init__", params, IRType::None, body)];
    if let Some((takes, gives)) = hook {
        methods.push(method(
            OBJECT_HOOK_TYPE_METHOD,
            vec![me(DECODER_CLASS), param("obj", &takes)],
            gives,
            vec![raise(
                "TypeError",
                "the object_hook's signature is never called",
            )],
        ));
    }
    IRClass {
        name: DECODER_CLASS.to_string(),
        bases: Vec::new(),
        methods,
        class_vars: Vec::new(),
        metaclass: None,
        position: 0,
        deferred_vars: Vec::new(),
    }
}

/// The signature of the `object_hook`s the program passes to decoders: a
/// function of the module taking a dict.
fn hook_signature(hooks: &[IRExpr], module: &IRModule) -> Result<Option<(IRType, IRType)>> {
    let mut found: Option<(IRType, IRType)> = None;
    for hook in hooks {
        let f = match hook {
            IRExpr::Variable(n) => module.functions.iter().find(|f| f.name == *n),
            _ => None,
        };
        let signature = f.and_then(|f| match f.params.as_slice() {
            [p] if known(&p.param_type) && known(&f.return_type) => {
                Some((p.param_type.clone(), f.return_type.clone()))
            }
            _ => None,
        });
        let Some(signature) = signature else {
            return Err(refused(
                "a JSONDecoder's object_hook must be a function of this module with one \
                 parameter, both it and the return annotated, for example \
                 'def as_point(d: dict[str, int]) -> Point'"
                    .to_string(),
            ));
        };
        match &found {
            Some(seen) if *seen != signature => {
                return Err(refused(
                    "JSONDecoders given object_hooks of different signatures are not supported"
                        .to_string(),
                ))
            }
            _ => found = Some(signature),
        }
    }
    Ok(found)
}

/// Make `json`'s classes when the program uses them, and its uses of them
/// CPython's.
pub fn lower_json(module: &mut IRModule) -> Result<()> {
    let b = Bindings::of(module);
    if b.modules.is_empty() && b.names.is_empty() {
        return Ok(());
    }
    for c in &mut module.classes {
        for base in &mut c.bases {
            if let Some(class) = b.class(base) {
                *base = class.to_string();
            }
        }
    }
    let families: HashMap<String, (&'static str, bool)> = {
        let classes = Classes::of(module);
        module
            .classes
            .iter()
            .filter_map(|c| {
                let family = classes.family(&c.name)?;
                Some((
                    c.name.clone(),
                    (family, classes.reaches_root(&c.name, family, "__init__")),
                ))
            })
            .chain([
                (ENCODER_CLASS.to_string(), (ENCODER_CLASS, true)),
                (DECODER_CLASS.to_string(), (DECODER_CLASS, true)),
            ])
            .collect()
    };
    // Which classes' `super().m(...)` reaches the family's `m`.
    let super_reaches = |method: &str, only: Option<&str>| -> Vec<String> {
        let classes = Classes::of(module);
        module
            .classes
            .iter()
            .filter_map(|c| {
                let &(family, _) = families.get(&c.name)?;
                if only.is_some_and(|o| o != family) {
                    return None;
                }
                c.bases
                    .iter()
                    .find(|base| classes.derives(base, family))
                    .filter(|base| classes.reaches_root(base, family, method))
                    .map(|_| c.name.clone())
            })
            .collect()
    };
    let default_reaches = super_reaches("default", Some(ENCODER_CLASS));
    let init_reaches = super_reaches("__init__", None);
    let constants = module
        .variables
        .iter()
        .filter_map(|var| match &var.value {
            IRExpr::Const(c) => Some((var.name.clone(), c.clone())),
            _ => None,
        })
        .collect();
    let mut pass = Pass {
        b: &b,
        constants,
        families,
        super_reaches: init_reaches,
        hooks: Vec::new(),
        uses: [false; 2],
    };
    for (name, (family, _)) in pass.families.clone() {
        if name != family {
            pass.note(family);
        }
    }
    for var in &mut module.variables {
        let mut wrapped = IRBody {
            statements: vec![IRStatement::Expression(std::mem::replace(
                &mut var.value,
                none(),
            ))],
        };
        crate::ir::visit_body(&mut wrapped, &mut |e: &mut IRExpr| pass.rewrite(e))?;
        if let Some(IRStatement::Expression(back)) = wrapped.statements.pop() {
            var.value = back;
        }
        if let Some(t) = &mut var.var_type {
            rename_type(t, &b);
        }
    }
    for f in &mut module.functions {
        crate::ir::visit_body(&mut f.body, &mut |e: &mut IRExpr| pass.rewrite(e))?;
        rename_function_types(f, &b);
    }
    for c in &mut module.classes {
        let name = c.name.clone();
        for m in &mut c.methods {
            crate::ir::visit_body(&mut m.body, &mut |e: &mut IRExpr| {
                pass.rewrite(e)?;
                pass.super_init(&name, e)
            })?;
            if default_reaches.contains(&name) {
                body_not_serializable(&mut m.body);
            }
            rename_function_types(m, &b);
        }
        for var in &mut c.class_vars {
            if let Some(t) = &mut var.var_type {
                rename_type(t, &b);
            }
        }
    }
    let hook = hook_signature(&pass.hooks, module)?;
    let [encoder, decoder] = pass.uses;
    let mut classes = Vec::new();
    if encoder {
        classes.push(encoder_class());
    }
    if decoder {
        classes.push(decoder_class(hook));
    }
    classes.append(&mut module.classes);
    module.classes = classes;
    intern_strings(module);
    Ok(())
}

/// Place every string the module's code now uses in memory: the classes'
/// and the options this pass wrote.
fn intern_strings(module: &mut IRModule) {
    let mut strings = Vec::new();
    for c in &module.classes {
        for m in &c.methods {
            for p in &m.params {
                if let Some(IRExpr::Const(IRConstant::String(s))) = &p.default_value {
                    strings.push(s.clone());
                }
            }
        }
    }
    let mut collect = |e: &mut IRExpr| {
        if let IRExpr::Const(IRConstant::String(s)) = e {
            strings.push(s.clone());
        }
        Ok(())
    };
    for f in module
        .functions
        .iter_mut()
        .chain(module.classes.iter_mut().flat_map(|c| c.methods.iter_mut()))
    {
        let _ = crate::ir::visit_body(&mut f.body, &mut collect);
    }
    for var in &mut module.variables {
        let mut wrapped = IRBody {
            statements: vec![IRStatement::Expression(std::mem::replace(
                &mut var.value,
                none(),
            ))],
        };
        let _ = crate::ir::visit_body(&mut wrapped, &mut collect);
        if let Some(IRStatement::Expression(back)) = wrapped.statements.pop() {
            var.value = back;
        }
    }
    for s in strings {
        module.memory_layout.add_string(&s);
    }
}
