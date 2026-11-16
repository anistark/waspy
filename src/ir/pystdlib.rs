//! Standard-library modules written in Python, compiled with the program.
//!
//! A module CPython writes in Python (or that is short in Python: `heapq`,
//! `bisect`, `textwrap`, `csv`, `urllib.parse`, `pathlib`) is kept here as
//! source in the supported subset (`src/ir/pystdlib/*.py`). When a program
//! imports one, its statements are placed ahead of the program's own, before
//! anything is lowered, so every pass treats them as the program's code: a
//! class's `with` is desugared, a loop over its iterator consumed, its
//! functions' untyped parameters typed from the calls the program makes.
//! Nothing is interpreted (critical rule 6).
//!
//! Each top-level name of a module `m` is renamed `__py_<m>_<name>` (dots in
//! `m` made underscores), which the program cannot spell, and the program's
//! reads of the module (`m.name`, a `from m import name`) become reads of
//! that name. The prefix keeps the functions out of exports and metadata
//! (`ir::is_internal_function`), and lets a merge of several files keep one
//! copy of a module two files import (`ir::merge`).

use anyhow::{anyhow, Result};
use rustpython_parser::ast::{self, Expr, Stmt, Suite};
use std::collections::{HashMap, HashSet};

/// The prefix of every name a module written in Python defines.
pub const PY_PREFIX: &str = "__py_";

/// What code generation reports for a call whose instance is not made yet;
/// the module is compiled again with it, so it never reaches the user.
pub const PENDING_INSTANCE: &str = "a standard-library function's instance is not made yet";

/// An instance code generation asked for: of a function template, of a
/// class whose `__init__` takes untyped parameters, or of a method that does,
/// with the types of those parameters (after `self`).
#[derive(Clone, Debug)]
pub(crate) enum Instance {
    Function(String, Vec<crate::ir::IRType>),
    Class(String, Vec<crate::ir::IRType>),
    Method(String, String, Vec<crate::ir::IRType>),
}

thread_local! {
    static INSTANCES: std::cell::RefCell<Vec<Instance>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The name of `template`'s instance for parameters of `types`.
pub fn instance_name(template: &str, types: &[crate::ir::IRType]) -> String {
    let spelled: Vec<String> = types.iter().map(crate::type_to_string).collect();
    format!("{template}<{}>", spelled.join(","))
}

/// Ask for `template`'s instance for `types`.
pub(crate) fn need_instance(template: &str, types: Vec<crate::ir::IRType>) {
    INSTANCES.with(|i| {
        i.borrow_mut()
            .push(Instance::Function(template.to_string(), types))
    });
}

/// Ask for an instance of a class or a method.
pub(crate) fn need(instance: Instance) {
    INSTANCES.with(|i| i.borrow_mut().push(instance));
}

/// The instances asked for since the last call.
pub(crate) fn take_instances() -> Vec<Instance> {
    INSTANCES.with(|i| std::mem::take(&mut *i.borrow_mut()))
}

/// Whether a method takes a parameter its calls type (after `self`).
fn has_untyped(f: &crate::ir::IRFunction, skip: usize) -> bool {
    f.params.iter().skip(skip).any(|p| {
        matches!(p.param_type, crate::ir::IRType::Unknown)
            && matches!(p.star, crate::ir::ParamStar::None)
    })
}

/// Whether `class` is a class template: a class of a module written in
/// Python whose `__init__` takes a parameter its calls type.
pub(crate) fn is_class_template(class: &crate::ir::IRClass) -> bool {
    class.name.starts_with(PY_PREFIX)
        && !class.name.contains('<')
        && class
            .methods
            .iter()
            .any(|m| m.name == "__init__" && has_untyped(m, 1))
}

/// Whether `method` of a class of a module written in Python is a method
/// template (not `__init__`, which makes its class one).
pub(crate) fn is_method_template(
    class: &crate::ir::IRClass,
    method: &crate::ir::IRFunction,
) -> bool {
    class.name.starts_with(PY_PREFIX)
        && method.name != "__init__"
        && !method.name.contains('<')
        && !method.name.starts_with("__")
        && has_untyped(method, 1)
}

/// Whether `f` is a template: a function of a module written in Python with
/// a parameter its calls type (an instance's name carries its types).
pub(crate) fn is_template(f: &crate::ir::IRFunction) -> bool {
    f.name.starts_with(PY_PREFIX)
        && !f.name.contains('<')
        && f.params.iter().any(|p| {
            matches!(p.param_type, crate::ir::IRType::Unknown)
                && matches!(p.star, crate::ir::ParamStar::None)
        })
}

/// `module` with the instances in `wanted` added that it does not have yet,
/// or None when it has them all.
pub(crate) fn with_instances(
    module: &crate::ir::IRModule,
    wanted: &[Instance],
) -> Option<crate::ir::IRModule> {
    fn typed(f: &mut crate::ir::IRFunction, skip: usize, types: &[crate::ir::IRType]) {
        for (p, ty) in f.params.iter_mut().skip(skip).zip(types) {
            if matches!(p.param_type, crate::ir::IRType::Unknown) {
                p.param_type = ty.clone();
            }
        }
    }
    let mut out: Option<crate::ir::IRModule> = None;
    for wanted in wanted {
        let current = out.as_ref().unwrap_or(module);
        match wanted {
            Instance::Function(template, types) => {
                let name = instance_name(template, types);
                if current.functions.iter().any(|f| f.name == name) {
                    continue;
                }
                let Some(t) = current.functions.iter().find(|f| f.name == *template) else {
                    continue;
                };
                let mut instance = t.clone();
                instance.name = name;
                typed(&mut instance, 0, types);
                out.get_or_insert_with(|| module.clone())
                    .functions
                    .push(instance);
            }
            Instance::Class(template, types) => {
                let name = instance_name(template, types);
                if current.classes.iter().any(|c| c.name == name) {
                    continue;
                }
                let Some(t) = current.classes.iter().find(|c| c.name == *template) else {
                    continue;
                };
                let mut instance = t.clone();
                instance.name = name.clone();
                // A subclass of the template, so it passes wherever the
                // template is declared (the helpers the iterator pass writes
                // for it), its methods reached virtually.
                instance.bases = vec![template.clone()];
                // Its methods' `self`, and what they declare of the class,
                // are the instance.
                let template_type = crate::ir::IRType::Class(template.clone());
                let instance_type = crate::ir::IRType::Class(name);
                // A field `__init__` sets from a parameter takes its type, and
                // so does a method that returns it (`closing.__enter__`).
                let mut field_types: HashMap<String, crate::ir::IRType> = HashMap::new();
                if let Some(init) = instance.methods.iter().find(|m| m.name == "__init__") {
                    let params: HashMap<&str, crate::ir::IRType> = init
                        .params
                        .iter()
                        .skip(1)
                        .zip(types)
                        .map(|(p, ty)| (p.name.as_str(), ty.clone()))
                        .collect();
                    for stmt in &init.body.statements {
                        if let crate::ir::IRStatement::AttributeAssign {
                            object,
                            attribute,
                            value: crate::ir::IRExpr::Variable(v) | crate::ir::IRExpr::Param(v),
                            ..
                        } = stmt
                        {
                            if matches!(object, crate::ir::IRExpr::Variable(s) if s == "self") {
                                if let Some(ty) = params.get(v.as_str()) {
                                    field_types.insert(attribute.clone(), ty.clone());
                                }
                            }
                        }
                    }
                }
                for m in &mut instance.methods {
                    if matches!(m.return_type, crate::ir::IRType::Unknown) {
                        if let [crate::ir::IRStatement::Return(Some(
                            crate::ir::IRExpr::Attribute { object, attribute },
                        ))] = m.body.statements.as_slice()
                        {
                            if matches!(&**object, crate::ir::IRExpr::Variable(s) if s == "self") {
                                if let Some(ty) = field_types.get(attribute) {
                                    m.return_type = ty.clone();
                                }
                            }
                        }
                    }
                }
                for m in &mut instance.methods {
                    if m.name == "__init__" {
                        typed(m, 1, types);
                    }
                    for p in &mut m.params {
                        if p.param_type == template_type {
                            p.param_type = instance_type.clone();
                        }
                    }
                    if m.return_type == template_type {
                        m.return_type = instance_type.clone();
                    }
                }
                out.get_or_insert_with(|| module.clone())
                    .classes
                    .push(instance);
            }
            Instance::Method(class, method, types) => {
                let name = instance_name(method, types);
                let Some(c) = current.classes.iter().find(|c| c.name == *class) else {
                    continue;
                };
                if c.methods.iter().any(|m| m.name == name) {
                    continue;
                }
                let Some(t) = c.methods.iter().find(|m| m.name == *method) else {
                    continue;
                };
                let mut instance = t.clone();
                instance.name = name;
                typed(&mut instance, 1, types);
                let module = out.get_or_insert_with(|| module.clone());
                if let Some(c) = module.classes.iter_mut().find(|c| c.name == *class) {
                    c.methods.push(instance);
                }
            }
        }
    }
    out
}

/// The modules, by their dotted names, and their sources.
const MODULES: &[(&str, &str)] = &[
    (
        "anthropic",
        concat!(
            include_str!("pystdlib/anthropic.py"),
            include_str!("pystdlib/anthropic_sync.py")
        ),
    ),
    ("base64", include_str!("pystdlib/base64.py")),
    ("bisect", include_str!("pystdlib/bisect.py")),
    ("builtins", include_str!("pystdlib/builtins.py")),
    ("argparse", include_str!("pystdlib/argparse.py")),
    ("contextlib", include_str!("pystdlib/contextlib.py")),
    ("copy", include_str!("pystdlib/copy.py")),
    ("csv", include_str!("pystdlib/csv.py")),
    ("enum", include_str!("pystdlib/enum.py")),
    ("fastapi", include_str!("pystdlib/fastapi.py")),
    (
        "fastapi.testclient",
        include_str!("pystdlib/fastapi_testclient.py"),
    ),
    ("hashlib", include_str!("pystdlib/hashlib.py")),
    ("heapq", include_str!("pystdlib/heapq.py")),
    ("httpx", include_str!("pystdlib/httpx.py")),
    ("io", include_str!("pystdlib/io.py")),
    ("operator", include_str!("pystdlib/operator.py")),
    ("pathlib", include_str!("pystdlib/pathlib.py")),
    ("pydantic", include_str!("pystdlib/pydantic.py")),
    ("requests", include_str!("pystdlib/requests.py")),
    ("string", include_str!("pystdlib/string.py")),
    ("textwrap", include_str!("pystdlib/textwrap.py")),
    ("time", include_str!("pystdlib/time.py")),
    ("urllib.parse", include_str!("pystdlib/urllib_parse.py")),
    ("uuid", include_str!("pystdlib/uuid.py")),
];

/// `anthropic` for a program that uses its AsyncAnthropic, over httpx and
/// the host's fetch, in place of the Anthropic client over requests.
const ANTHROPIC_ASYNC: &str = concat!(
    include_str!("pystdlib/anthropic.py"),
    include_str!("pystdlib/anthropic_async.py")
);

/// Whether `name` is one of the modules written in Python (or a package
/// holding one, `urllib`).
pub fn is_module(name: &str) -> bool {
    MODULES
        .iter()
        .any(|(m, _)| *m == name || m.starts_with(&format!("{name}.")))
}

fn source_of(name: &str) -> Option<&'static str> {
    MODULES.iter().find(|(m, _)| *m == name).map(|(_, s)| *s)
}

/// The prefix of the names module `module` defines.
fn prefix_of(module: &str) -> String {
    format!("{PY_PREFIX}{}_", module.replace('.', "_"))
}

/// The CPython name of a class a module written in Python defines, for its
/// `type()` and its exceptions' reports, when `name` is one.
pub fn display_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix(PY_PREFIX)?;
    for (module, _) in MODULES {
        let tag = format!("{}_", module.replace('.', "_"));
        if let Some(class) = rest.strip_prefix(&tag) {
            let qualified = match (*module, class) {
                ("io", "StringIO") => "_io.StringIO".to_string(),
                ("csv", "Error") => "_csv.Error".to_string(),
                ("csv", "_Reader") => "_csv.reader".to_string(),
                ("csv", "_DictReader") => "csv.DictReader".to_string(),
                ("csv", "writer") => "_csv.writer".to_string(),
                ("base64", "Error") => "binascii.Error".to_string(),
                ("pathlib", "Path") => "pathlib._local.PosixPath".to_string(),
                ("hashlib", "_Hash") => "_hashlib.HASH".to_string(),
                ("requests", c)
                    if c.ends_with("Error")
                        || c.ends_with("Exception")
                        || matches!(
                            c,
                            "Timeout"
                                | "MissingSchema"
                                | "InvalidSchema"
                                | "InvalidURL"
                                | "InvalidHeader"
                        ) =>
                {
                    format!("requests.exceptions.{c}")
                }
                ("anthropic", c) if c.ends_with("Error") => format!("anthropic.{c}"),
                ("pydantic", "ValidationError") => {
                    "pydantic_core._pydantic_core.ValidationError".to_string()
                }
                _ => format!("{module}.{class}"),
            };
            return Some(qualified);
        }
    }
    None
}

/// One `add_argument(...)` the program makes, as `argparse`'s `Namespace`
/// types its field.
struct ArgSpec {
    dest: String,
    positional: bool,
    action: String,
    nargs: String,
    type_name: String,
    required: bool,
    default: Option<Expr>,
}

/// The `add_argument` calls of `body`, with their arguments written out: a
/// type by name (`type="int"`), a default's text for help (`default_text`),
/// choices as strs. A call whose arguments are not written in the program
/// is refused, as its field's name and type would not be known.
fn argparse_calls(body: &mut [Stmt]) -> Result<Vec<ArgSpec>> {
    struct Calls {
        specs: Vec<ArgSpec>,
        error: Option<String>,
    }
    fn constant_str(e: &Expr) -> Option<String> {
        match e {
            Expr::Constant(c) => match &c.value {
                ast::Constant::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }
    fn text_of(e: &Expr) -> Option<String> {
        match e {
            Expr::Constant(c) => Some(match &c.value {
                ast::Constant::Str(s) => s.to_string(),
                ast::Constant::Int(i) => i.to_string(),
                ast::Constant::Float(f) => {
                    let s = format!("{f:?}");
                    s
                }
                ast::Constant::Bool(b) => if *b { "True" } else { "False" }.to_string(),
                ast::Constant::None => "None".to_string(),
                _ => return None,
            }),
            Expr::List(l) if l.elts.is_empty() => Some("[]".to_string()),
            _ => None,
        }
    }
    fn str_expr(s: &str, range: ast::text_size::TextRange) -> Expr {
        Expr::Constant(ast::ExprConstant {
            value: ast::Constant::Str(s.to_string()),
            kind: None,
            range,
        })
    }
    impl ast::fold::Fold<ast::text_size::TextRange> for Calls {
        type TargetU = ast::text_size::TextRange;
        type Error = std::convert::Infallible;
        type UserContext = ();
        fn will_map_user(&mut self, _user: &ast::text_size::TextRange) -> Self::UserContext {}
        fn map_user(
            &mut self,
            user: ast::text_size::TextRange,
            _context: (),
        ) -> std::result::Result<ast::text_size::TextRange, Self::Error> {
            Ok(user)
        }
        fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
            let expr = ast::fold::fold_expr(self, expr)?;
            let Expr::Call(mut call) = expr else {
                return Ok(expr);
            };
            let is_add =
                matches!(&*call.func, Expr::Attribute(a) if a.attr.as_str() == "add_argument");
            if !is_add {
                return Ok(Expr::Call(call));
            }
            let range = call.range;
            let names: Option<Vec<String>> = call.args.iter().map(constant_str).collect();
            let Some(names) = names.filter(|n| !n.is_empty()) else {
                self.error = Some(
                    "add_argument() takes its names written as strs in the program".to_string(),
                );
                return Ok(Expr::Call(call));
            };
            let mut spec = ArgSpec {
                dest: String::new(),
                positional: !names[0].starts_with('-'),
                action: "store".to_string(),
                nargs: String::new(),
                type_name: "str".to_string(),
                required: false,
                default: None,
            };
            let mut keywords = Vec::new();
            for mut kw in std::mem::take(&mut call.keywords) {
                let Some(arg) = kw.arg.as_ref().map(|a| a.to_string()) else {
                    self.error = Some("add_argument(**kwargs) is not supported".to_string());
                    continue;
                };
                match arg.as_str() {
                    "type" => {
                        let name = match &kw.value {
                            Expr::Name(n) if ["int", "float", "str"].contains(&n.id.as_str()) => {
                                n.id.to_string()
                            }
                            _ => {
                                self.error = Some(
                                    "add_argument(type=...) takes int, float, or str".to_string(),
                                );
                                continue;
                            }
                        };
                        spec.type_name = name.clone();
                        kw.value = str_expr(&name, range);
                    }
                    "default" => {
                        if let Some(text) = text_of(&kw.value) {
                            keywords.push(ast::Keyword {
                                arg: Some("default_text".into()),
                                value: str_expr(&text, range),
                                range,
                            });
                        }
                        spec.default = Some(kw.value.clone());
                        continue;
                    }
                    "action" => match constant_str(&kw.value) {
                        Some(a)
                            if ["store", "store_true", "store_false", "count", "append"]
                                .contains(&a.as_str()) =>
                        {
                            spec.action = a
                        }
                        _ => {
                            self.error = Some("add_argument(action=...) takes \"store\", \"store_true\", \"store_false\", \"count\", or \"append\"".to_string());
                        }
                    },
                    "nargs" => {
                        let n = match &kw.value {
                            Expr::Constant(c) => match &c.value {
                                ast::Constant::Str(s) if ["?", "*", "+"].contains(&s.as_str()) => {
                                    s.to_string()
                                }
                                ast::Constant::Int(i) => i.to_string(),
                                _ => String::new(),
                            },
                            _ => String::new(),
                        };
                        if n.is_empty() {
                            self.error = Some(
                                "add_argument(nargs=...) takes \"?\", \"*\", \"+\", or a number"
                                    .to_string(),
                            );
                        }
                        spec.nargs = n.clone();
                        kw.value = str_expr(&n, range);
                    }
                    "required" => {
                        spec.required = matches!(&kw.value, Expr::Constant(c) if matches!(c.value, ast::Constant::Bool(true)));
                    }
                    "dest" => {
                        if let Some(d) = constant_str(&kw.value) {
                            spec.dest = d;
                        }
                    }
                    "choices" => {
                        let items: Option<Vec<String>> = match &kw.value {
                            Expr::List(l) => l.elts.iter().map(text_of).collect(),
                            Expr::Tuple(t) => t.elts.iter().map(text_of).collect(),
                            _ => None,
                        };
                        match items {
                            Some(items) => {
                                kw.value = Expr::List(ast::ExprList {
                                    elts: items.iter().map(|s| str_expr(s, range)).collect(),
                                    ctx: ast::ExprContext::Load,
                                    range,
                                });
                            }
                            None => {
                                self.error = Some(
                                    "add_argument(choices=...) takes a list written in the program"
                                        .to_string(),
                                );
                            }
                        }
                    }
                    "help" | "metavar" => {}
                    other => {
                        self.error = Some(format!("add_argument({other}=...) is not supported"));
                    }
                }
                keywords.push(kw);
            }
            call.keywords = keywords;
            // The names, as the one list the method takes.
            let names_list = Expr::List(ast::ExprList {
                elts: std::mem::take(&mut call.args),
                ctx: ast::ExprContext::Load,
                range,
            });
            call.args = vec![names_list];
            if spec.dest.is_empty() {
                spec.dest = if spec.positional {
                    names[0].clone()
                } else {
                    let first = names
                        .iter()
                        .find(|n| n.starts_with("--"))
                        .unwrap_or(&names[0]);
                    first.trim_start_matches('-').replace('-', "_")
                };
            }
            if spec.positional && (spec.nargs.is_empty() || spec.nargs == "+") {
                spec.required = true;
            }
            if !self.specs.iter().any(|s| s.dest == spec.dest) {
                self.specs.push(spec);
            }
            Ok(Expr::Call(call))
        }
    }
    let mut calls = Calls {
        specs: Vec::new(),
        error: None,
    };
    for stmt in body.iter_mut() {
        let folded = match ast::fold::Fold::fold_stmt(&mut calls, stmt.clone()) {
            Ok(s) => s,
            Err(never) => match never {},
        };
        *stmt = folded;
    }
    if let Some(error) = calls.error {
        return Err(crate::core::errors::unsupported_feature(error, None).into());
    }
    Ok(calls.specs)
}

/// The source of argparse's `Namespace` for the program's arguments: a field
/// per argument, typed from its `type=` and `action=`, and the repr CPython's
/// gives.
fn namespace_source(specs: &[ArgSpec]) -> String {
    let mut init = String::new();
    let mut repr_parts: Vec<String> = Vec::new();
    for s in specs {
        let d = &s.dest;
        let conv = |x: &str| match s.type_name.as_str() {
            "int" => format!("int({x})"),
            "float" => format!("float({x})"),
            _ => x.to_string(),
        };
        let t = s.type_name.as_str();
        let default = s.default.as_ref().map(|e| match e {
            Expr::Constant(c) => match &c.value {
                ast::Constant::Str(v) if t == "int" || t == "float" => conv(&format!("{v:?}")),
                ast::Constant::Str(v) => format!("{v:?}"),
                ast::Constant::Int(i) if t == "float" => format!("{i}.0"),
                ast::Constant::Int(i) => i.to_string(),
                ast::Constant::Float(f) => format!("{f:?}"),
                ast::Constant::Bool(b) => if *b { "True" } else { "False" }.to_string(),
                ast::Constant::None => "None".to_string(),
                _ => "None".to_string(),
            },
            Expr::List(l) if l.elts.is_empty() => "[]".to_string(),
            _ => "None".to_string(),
        });
        let present = format!("\"{d}\" in values and len(values[\"{d}\"]) > 0");
        let is_list = s.action == "append"
            || matches!(s.nargs.as_str(), "*" | "+")
            || s.nargs.chars().all(|c| c.is_ascii_digit()) && !s.nargs.is_empty();
        match s.action.as_str() {
            "store_true" => init.push_str(&format!("        self.{d}: bool = counts.get(\"{d}\", 0) > 0\n")),
            "store_false" => init.push_str(&format!("        self.{d}: bool = counts.get(\"{d}\", 0) == 0\n")),
            "count" => match default.as_deref() {
                Some(v) if v != "None" => init.push_str(&format!("        self.{d}: int = counts.get(\"{d}\", 0) + {v}\n")),
                _ => init.push_str(&format!(
                    "        self.{d}: Optional[int] = None\n        if \"{d}\" in counts:\n            self.{d} = counts[\"{d}\"]\n"
                )),
            },
            _ if is_list => {
                let items = format!("[{} for x in got]", conv("x"));
                let fallback = match default.as_deref() {
                    Some(v) if v != "None" => Some(v.to_string()),
                    _ if s.positional && s.nargs == "*" => Some("[]".to_string()),
                    _ => None,
                };
                if s.required || fallback.is_some() {
                    let fb = fallback.unwrap_or_else(|| "[]".to_string());
                    let typed_fb = fb.clone();
                    init.push_str(&format!(
                        "        self.{d}: list[{t}] = {typed_fb}\n        if \"{d}\" in values:\n            got = values[\"{d}\"]\n            self.{d} = {items}\n"
                    ));
                } else {
                    init.push_str(&format!(
                        "        self.{d}: Optional[list[{t}]] = None\n        if \"{d}\" in values:\n            got = values[\"{d}\"]\n            self.{d} = {items}\n"
                    ));
                }
            }
            _ => {
                let value = conv(&format!("values[\"{d}\"][-1]"));
                match default.as_deref() {
                    _ if s.required => init.push_str(&format!("        self.{d}: {t} = {value}\n")),
                    Some(v) if v != "None" => init.push_str(&format!(
                        "        self.{d}: {t} = {v}\n        if {present}:\n            self.{d} = {value}\n"
                    )),
                    _ => init.push_str(&format!(
                        "        self.{d}: Optional[{t}] = None\n        if {present}:\n            self.{d} = {value}\n"
                    )),
                }
            }
        }
        repr_parts.push(format!("\"{d}=\" + repr(self.{d})"));
    }
    init.insert_str(
        0,
        "        values: dict[str, list[str]] = raw.values\n        counts: dict[str, int] = raw.counts\n",
    );
    let repr_body = if repr_parts.is_empty() {
        "\"Namespace()\"".to_string()
    } else {
        format!(
            "\"Namespace(\" + {} + \")\"",
            repr_parts.join(" + \", \" + ")
        )
    };
    format!(
        "class Namespace:\n    def __init__(self, raw: _Raw):\n{init}\n    def __repr__(self) -> str:\n        return {repr_body}\n"
    )
}

/// An Enum class of the program, as it is written out.
struct EnumClass {
    name: String,
    kind: EnumKind,
    /// Each member and its value, in definition order (an alias names the
    /// member of its value).
    members: Vec<(String, Expr)>,
}

#[derive(Clone, Copy, PartialEq)]
enum EnumKind {
    Enum,
    Int,
    Str,
}

/// The program's Enum classes, written out: an Enum's as a class of members
/// with a name and a value and module variables holding them, an IntEnum's
/// and a StrEnum's members as their values. Its reads are rewritten by
/// [`rewrite_enums`].
fn write_enums(body: &mut Vec<Stmt>) -> Result<Vec<EnumClass>> {
    use rustpython_parser::Parse;
    let kind_of = |b: &Expr| match b {
        Expr::Name(n) => match n.id.as_str() {
            "Enum" => Some(EnumKind::Enum),
            "IntEnum" => Some(EnumKind::Int),
            "StrEnum" => Some(EnumKind::Str),
            _ => None,
        },
        Expr::Attribute(a) => match a.attr.as_str() {
            "Enum" => Some(EnumKind::Enum),
            "IntEnum" => Some(EnumKind::Int),
            "StrEnum" => Some(EnumKind::Str),
            _ => None,
        },
        _ => None,
    };
    let mut enums = Vec::new();
    let mut out: Vec<Stmt> = Vec::with_capacity(body.len());
    for stmt in std::mem::take(body) {
        let Stmt::ClassDef(class) = &stmt else {
            out.push(stmt);
            continue;
        };
        let Some(kind) = class.bases.iter().find_map(kind_of) else {
            out.push(stmt);
            continue;
        };
        let name = class.name.to_string();
        let refuse = |why: &str| -> anyhow::Error {
            crate::core::errors::unsupported_feature(format!("the Enum class '{name}' {why}"), None)
                .into()
        };
        let mut members: Vec<(String, Expr)> = Vec::new();
        let mut methods: Vec<Stmt> = Vec::new();
        let mut last_int: i64 = 0;
        for item in &class.body {
            match item {
                Stmt::Assign(a) if a.targets.len() == 1 => {
                    let Expr::Name(target) = &a.targets[0] else {
                        return Err(refuse("assigns something other than a member name"));
                    };
                    let value = match &*a.value {
                        Expr::Call(c)
                            if matches!(&*c.func, Expr::Name(n) if n.id.as_str() == "auto")
                                || matches!(&*c.func, Expr::Attribute(at) if at.attr.as_str() == "auto") =>
                        {
                            if kind == EnumKind::Str {
                                Expr::Constant(ast::ExprConstant {
                                    value: ast::Constant::Str(target.id.to_lowercase()),
                                    kind: None,
                                    range: a.range,
                                })
                            } else {
                                last_int += 1;
                                Expr::Constant(ast::ExprConstant {
                                    value: ast::Constant::Int(last_int.into()),
                                    kind: None,
                                    range: a.range,
                                })
                            }
                        }
                        Expr::Constant(c) => {
                            if let ast::Constant::Int(i) = &c.value {
                                if let Ok(v) = i64::try_from(i.clone()) {
                                    last_int = v;
                                }
                            }
                            (*a.value).clone()
                        }
                        Expr::UnaryOp(_) => (*a.value).clone(),
                        _ => return Err(refuse("gives a member a value that is not a constant")),
                    };
                    members.push((target.id.to_string(), value));
                }
                Stmt::FunctionDef(_) => methods.push(item.clone()),
                Stmt::Pass(_) => {}
                Stmt::Expr(e) if matches!(&*e.value, Expr::Constant(_)) => {}
                _ => return Err(refuse("has a statement other than members and methods")),
            }
        }
        // The values' one type.
        let kind_of_value = |e: &Expr| match e {
            Expr::Constant(c) => match &c.value {
                ast::Constant::Int(_) => "int",
                ast::Constant::Str(_) => "str",
                ast::Constant::Float(_) => "float",
                ast::Constant::Bool(_) => "bool",
                _ => "other",
            },
            Expr::UnaryOp(u) => match &*u.operand {
                Expr::Constant(c) if matches!(c.value, ast::Constant::Int(_)) => "int",
                Expr::Constant(c) if matches!(c.value, ast::Constant::Float(_)) => "float",
                _ => "other",
            },
            _ => "other",
        };
        let value_type = members
            .first()
            .map(|(_, v)| kind_of_value(v))
            .unwrap_or("int");
        if members.iter().any(|(_, v)| kind_of_value(v) != value_type) || value_type == "other" {
            return Err(refuse(
                "gives its members values of different types, which is not supported",
            ));
        }
        match kind {
            EnumKind::Int if value_type != "int" => {
                return Err(refuse("is an IntEnum with a value that is not an int"))
            }
            EnumKind::Str if value_type != "str" => {
                return Err(refuse("is a StrEnum with a value that is not a str"))
            }
            _ => {}
        }
        if kind == EnumKind::Enum {
            let t = value_type;
            let src = format!(
                "class {name}:\n    def __init__(self, name: str, value: {t}):\n        self._name_: str = name\n        self._value_: {t} = value\n\n    @property\n    def name(self) -> str:\n        return self._name_\n\n    @property\n    def value(self) -> {t}:\n        return self._value_\n\n    def __repr__(self) -> str:\n        return \"<{name}.\" + self._name_ + \": \" + repr(self._value_) + \">\"\n\n    def __str__(self) -> str:\n        return \"{name}.\" + self._name_\n\n    def __hash__(self) -> int:\n        return hash(self._name_)\n"
            );
            let mut generated = Suite::parse(&src, "<enum>")
                .map_err(|e| anyhow!("an Enum class does not parse: {e}"))?;
            if let Some(Stmt::ClassDef(c)) = generated.first_mut() {
                c.body.extend(methods);
            }
            out.extend(generated);
            // The members, then the lookups, after the class.
            let mut tail = String::new();
            let mut seen_values: Vec<(String, String)> = Vec::new();
            for (member, value) in &members {
                let text = constant_text(value)
                    .ok_or_else(|| refuse("gives a member a value that is not a constant"))?;
                if let Some((first, _)) = seen_values.iter().find(|(_, v)| *v == text) {
                    tail.push_str(&format!("__enum_{name}_{member} = __enum_{name}_{first}\n"));
                } else {
                    tail.push_str(&format!(
                        "__enum_{name}_{member} = {name}(\"{member}\", {text})\n"
                    ));
                    seen_values.push((member.clone(), text));
                }
            }
            let canon: Vec<String> = seen_values
                .iter()
                .map(|(m, _)| format!("__enum_{name}_{m}"))
                .collect();
            tail.push_str(&format!(
                "__enum_{name}_members: list[{name}] = [{}]\n",
                canon.join(", ")
            ));
            tail.push_str(&format!(
                "def __enum_{name}_by_value(value: {t}) -> {name}:\n    for m in __enum_{name}_members:\n        if m._value_ == value:\n            return m\n    raise ValueError(repr(value) + \" is not a valid {name}\")\n"
            ));
            tail.push_str(&format!(
                "def __enum_{name}_by_name(key: str) -> {name}:\n    for m in __enum_{name}_members:\n        if m._name_ == key:\n            return m\n    raise KeyError(key)\n"
            ));
            out.extend(
                Suite::parse(&tail, "<enum members>")
                    .map_err(|e| anyhow!("an Enum's members do not parse: {e}"))?,
            );
        } else {
            let t = value_type;
            let mut tail = String::new();
            let mut values = Vec::new();
            for (member, value) in &members {
                let text = constant_text(value)
                    .ok_or_else(|| refuse("gives a member a value that is not a constant"))?;
                tail.push_str(&format!("__enum_{name}_{member}: {t} = {text}\n"));
                if !values.contains(&text) {
                    values.push(text);
                }
            }
            tail.push_str(&format!(
                "__enum_{name}_members: list[{t}] = [{}]\n",
                values.join(", ")
            ));
            tail.push_str(&format!(
                "def __enum_{name}_by_value(value: {t}) -> {t}:\n    if value in __enum_{name}_members:\n        return value\n    raise ValueError(repr(value) + \" is not a valid {name}\")\n"
            ));
            out.extend(
                Suite::parse(&tail, "<enum members>")
                    .map_err(|e| anyhow!("an Enum's members do not parse: {e}"))?,
            );
            if !methods.is_empty() {
                return Err(refuse(
                    "is an IntEnum or StrEnum with methods, which is not supported",
                ));
            }
        }
        enums.push(EnumClass {
            name,
            kind,
            members,
        });
    }
    *body = out;
    Ok(enums)
}

/// A constant's Python text.
fn constant_text(e: &Expr) -> Option<String> {
    match e {
        Expr::Constant(c) => Some(match &c.value {
            ast::Constant::Int(i) => i.to_string(),
            ast::Constant::Str(s) => format!("{s:?}"),
            ast::Constant::Float(f) => format!("{f:?}"),
            ast::Constant::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            _ => return None,
        }),
        Expr::UnaryOp(u) if matches!(u.op, ast::UnaryOp::USub) => {
            Some(format!("-{}", constant_text(&u.operand)?))
        }
        _ => None,
    }
}

/// The program's reads of its Enum classes: `Color.RED` the member's
/// variable, `Color(v)` and `Color["RED"]` its lookups, the class walked or
/// measured its members.
fn rewrite_enums(body: Vec<Stmt>, enums: &[EnumClass]) -> Vec<Stmt> {
    struct Enums<'e> {
        enums: &'e [EnumClass],
    }
    fn name(id: &str, range: ast::text_size::TextRange) -> Expr {
        Expr::Name(ast::ExprName {
            id: id.into(),
            ctx: ast::ExprContext::Load,
            range,
        })
    }
    impl<'e> Enums<'e> {
        fn class(&self, e: &Expr) -> Option<&'e EnumClass> {
            let Expr::Name(n) = e else { return None };
            let enums: &'e [EnumClass] = self.enums;
            enums.iter().find(|c| c.name == n.id.as_str())
        }
    }
    impl ast::fold::Fold<ast::text_size::TextRange> for Enums<'_> {
        type TargetU = ast::text_size::TextRange;
        type Error = std::convert::Infallible;
        type UserContext = ();
        fn will_map_user(&mut self, _user: &ast::text_size::TextRange) -> Self::UserContext {}
        fn map_user(
            &mut self,
            user: ast::text_size::TextRange,
            _context: (),
        ) -> std::result::Result<ast::text_size::TextRange, Self::Error> {
            Ok(user)
        }
        fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
            match &expr {
                Expr::Attribute(a) => {
                    if let Some(c) = self.class(&a.value) {
                        if c.members.iter().any(|(m, _)| m == a.attr.as_str()) {
                            return Ok(name(&format!("__enum_{}_{}", c.name, a.attr), a.range));
                        }
                    }
                }
                Expr::Call(call) => {
                    // `Color(value)`, a lookup (the members are made with two
                    // arguments, the name and the value).
                    if let Some(c) = self.class(&call.func).filter(|_| call.args.len() == 1) {
                        let args = call
                            .args
                            .iter()
                            .map(|a| self.fold_expr(a.clone()))
                            .collect::<std::result::Result<Vec<_>, _>>()?;
                        return Ok(Expr::Call(ast::ExprCall {
                            func: Box::new(name(
                                &format!("__enum_{}_by_value", c.name),
                                call.range,
                            )),
                            args,
                            keywords: Vec::new(),
                            range: call.range,
                        }));
                    }
                    // list(Color), len(Color), sorted(Color), ...
                    if let Expr::Name(f) = &*call.func {
                        if matches!(
                            f.id.as_str(),
                            "list" | "len" | "tuple" | "set" | "sorted" | "enumerate" | "reversed"
                        ) {
                            if let [only] = call.args.as_slice() {
                                if let Some(c) = self.class(only) {
                                    let mut call = call.clone();
                                    call.args = vec![name(
                                        &format!("__enum_{}_members", c.name),
                                        call.range,
                                    )];
                                    return Ok(Expr::Call(call));
                                }
                            }
                        }
                    }
                }
                Expr::Subscript(s) => {
                    if let Some(c) = self.class(&s.value) {
                        if c.kind == EnumKind::Enum {
                            let key = self.fold_expr((*s.slice).clone())?;
                            return Ok(Expr::Call(ast::ExprCall {
                                func: Box::new(name(
                                    &format!("__enum_{}_by_name", c.name),
                                    s.range,
                                )),
                                args: vec![key],
                                keywords: Vec::new(),
                                range: s.range,
                            }));
                        }
                    }
                }
                // `x in Color`
                Expr::Compare(cmp)
                    if cmp.comparators.len() == 1
                        && matches!(cmp.ops[0], ast::CmpOp::In | ast::CmpOp::NotIn) =>
                {
                    if let Some(c) = self.class(&cmp.comparators[0]) {
                        let mut cmp = cmp.clone();
                        cmp.left = Box::new(self.fold_expr(*cmp.left)?);
                        cmp.comparators =
                            vec![name(&format!("__enum_{}_members", c.name), cmp.range)];
                        return Ok(Expr::Compare(cmp));
                    }
                }
                _ => {}
            }
            ast::fold::fold_expr(self, expr)
        }
        fn fold_stmt(&mut self, stmt: Stmt) -> std::result::Result<Stmt, Self::Error> {
            // `for m in Color:` walks the members.
            if let Stmt::For(f) = &stmt {
                if let Some(c) = self.class(&f.iter) {
                    let mut f = f.clone();
                    f.iter = Box::new(name(&format!("__enum_{}_members", c.name), f.range));
                    return ast::fold::fold_stmt(self, Stmt::For(f));
                }
            }
            ast::fold::fold_stmt(self, stmt)
        }
        fn fold_comprehension(
            &mut self,
            node: ast::Comprehension,
        ) -> std::result::Result<ast::Comprehension, Self::Error> {
            let mut node = node;
            if let Some(c) = self.class(&node.iter) {
                node.iter = name(&format!("__enum_{}_members", c.name), Default::default());
            }
            ast::fold::fold_comprehension(self, node)
        }
    }
    let mut f = Enums { enums };
    body.into_iter()
        .map(|s| match ast::fold::Fold::fold_stmt(&mut f, s) {
            Ok(s) => s,
            Err(never) => match never {},
        })
        .collect()
}

/// The modules a body imports at its top level (after `ir::imports` moved
/// every import there): `import m` and `from m import ...`.
fn imported(body: &[Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |m: &str| {
        if source_of(m).is_some() && !out.iter().any(|o| o == m) {
            out.push(m.to_string());
        }
    };
    for stmt in body {
        match stmt {
            Stmt::Import(s) => {
                for alias in &s.names {
                    add(alias.name.as_str());
                }
            }
            Stmt::ImportFrom(s) if s.level.map_or(0, |l| l.to_u32()) == 0 => {
                if let Some(m) = &s.module {
                    add(m.as_str());
                    // `from urllib import parse`
                    for alias in &s.names {
                        add(&format!("{m}.{}", alias.name));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Whether a body reads the name `name` anywhere (as a name or an attribute).
fn mentions(body: &[Stmt], name: &str) -> bool {
    struct Finder<'a>(&'a str, bool);
    impl ast::fold::Fold<ast::text_size::TextRange> for Finder<'_> {
        type TargetU = ast::text_size::TextRange;
        type Error = std::convert::Infallible;
        type UserContext = ();
        fn will_map_user(&mut self, _user: &ast::text_size::TextRange) -> Self::UserContext {}
        fn map_user(
            &mut self,
            user: ast::text_size::TextRange,
            _context: (),
        ) -> std::result::Result<ast::text_size::TextRange, Self::Error> {
            Ok(user)
        }
        fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
            match &expr {
                Expr::Name(n) if n.id.as_str() == self.0 => self.1 = true,
                Expr::Attribute(a) if a.attr.as_str() == self.0 => self.1 = true,
                _ => {}
            }
            ast::fold::fold_expr(self, expr)
        }
    }
    let mut finder = Finder(name, false);
    for stmt in body {
        if let Stmt::ImportFrom(s) = stmt {
            if s.names.iter().any(|a| a.name.as_str() == name) {
                return true;
            }
        }
        let _ = ast::fold::Fold::fold_stmt(&mut finder, stmt.clone());
    }
    finder.1
}

/// The top-level names a module's source defines.
fn defined(body: &[Stmt]) -> HashSet<String> {
    let mut out = HashSet::new();
    for stmt in body {
        match stmt {
            Stmt::FunctionDef(s) => {
                out.insert(s.name.to_string());
            }
            Stmt::ClassDef(s) => {
                out.insert(s.name.to_string());
            }
            Stmt::Assign(s) => {
                for t in &s.targets {
                    if let Expr::Name(n) = t {
                        out.insert(n.id.to_string());
                    }
                }
            }
            Stmt::AnnAssign(s) => {
                if let Expr::Name(n) = &*s.target {
                    out.insert(n.id.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// What a name read in some code stands for: a module's renamed definition.
struct Renames {
    /// A plain name (a module's own definition, or one a `from m import`
    /// brought in) and what it becomes.
    names: HashMap<String, String>,
    /// The builtins written in Python that a call reaches here (the ones the
    /// code does not define itself).
    builtins: HashSet<String>,
    /// A module's dotted name (`urllib.parse`, `io`) and its definitions'
    /// prefix, for `m.name`.
    modules: HashMap<String, (String, HashSet<String>)>,
}

impl Renames {
    /// `m.name`, or `pkg.m.name`, as the renamed definition it reads.
    fn attribute(&self, expr: &Expr) -> Option<String> {
        let Expr::Attribute(attr) = expr else {
            return None;
        };
        let module = dotted(&attr.value)?;
        let (prefix, names) = self.modules.get(&module)?;
        if names.contains(attr.attr.as_str()) {
            Some(format!("{prefix}{}", attr.attr))
        } else {
            None
        }
    }
}

/// The dotted name an expression of names and attributes spells.
fn dotted(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Name(n) => Some(n.id.to_string()),
        Expr::Attribute(a) => Some(format!("{}.{}", dotted(&a.value)?, a.attr)),
        _ => None,
    }
}

/// Rename reads through `renames`, leaving a function's parameters (and
/// anything they shadow) alone.
struct Renamer<'r> {
    renames: &'r Renames,
    shadowed: Vec<HashSet<String>>,
    /// How deep in `def` and `class` bodies: only a module's own top-level
    /// definitions are renamed, never a method of the same name.
    depth: usize,
}

/// The builtins written in Python (`pystdlib/builtins.py`), by the name a
/// program calls.
const BUILTINS: &[&str] = &["hex", "oct", "bin", "divmod", "pow", "int", "filter"];

/// The builtins of [`BUILTINS`] a body calls in a form written in Python
/// (`int(s, base)`, `pow(a, b, m)`), less any it defines itself.
fn builtins_used(body: &[Stmt]) -> HashSet<String> {
    struct Find {
        found: HashSet<String>,
    }
    impl ast::fold::Fold<ast::text_size::TextRange> for Find {
        type TargetU = ast::text_size::TextRange;
        type Error = std::convert::Infallible;
        type UserContext = ();
        fn will_map_user(&mut self, _user: &ast::text_size::TextRange) -> Self::UserContext {}
        fn map_user(
            &mut self,
            user: ast::text_size::TextRange,
            _context: (),
        ) -> std::result::Result<ast::text_size::TextRange, Self::Error> {
            Ok(user)
        }
        fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
            if let Expr::Call(node) = &expr {
                if let Expr::Name(n) = &*node.func {
                    let name = n.id.as_str();
                    let written = match name {
                        "int" => {
                            node.args.len() == 2
                                || node
                                    .keywords
                                    .iter()
                                    .any(|k| k.arg.as_deref() == Some("base"))
                        }
                        "pow" => !node.args.is_empty(),
                        _ => BUILTINS.contains(&name),
                    };
                    if written {
                        self.found.insert(name.to_string());
                    }
                }
            }
            ast::fold::fold_expr(self, expr)
        }
    }
    let mut find = Find {
        found: HashSet::new(),
    };
    for stmt in body {
        let _ = ast::fold::Fold::fold_stmt(&mut find, stmt.clone());
    }
    let own = defined(body);
    find.found.retain(|n| !own.contains(n));
    find.found
}

impl Renamer<'_> {
    fn shadowed(&self, name: &str) -> bool {
        self.shadowed.iter().any(|s| s.contains(name))
    }

    /// The exceptions of a `contextlib.suppress(...)` call, if `expr` is one.
    fn suppress_call(&self, expr: &Expr) -> Option<Vec<Expr>> {
        let Expr::Call(call) = expr else { return None };
        let callee = match &*call.func {
            Expr::Name(n) => self
                .renames
                .names
                .get(n.id.as_str())
                .filter(|_| !self.shadowed(n.id.as_str()))
                .cloned()?,
            other => self.renames.attribute(other)?,
        };
        (callee == format!("{}suppress", prefix_of("contextlib")) && call.keywords.is_empty())
            .then(|| call.args.clone())
    }

    /// A call of a builtin written in Python, as the call it is made.
    fn builtin_call(&mut self, call: &ast::ExprCall) -> Option<Expr> {
        let Expr::Name(n) = &*call.func else {
            return None;
        };
        let name = n.id.as_str();
        if !self.renames.builtins.contains(name) || self.shadowed(name) {
            return None;
        }
        let range = call.range;
        let fold = |r: &mut Self, e: &Expr| match ast::fold::Fold::fold_expr(r, e.clone()) {
            Ok(e) => e,
            Err(never) => match never {},
        };
        let args: Vec<Expr> = call.args.iter().map(|a| fold(self, a)).collect();
        let named = |f: &str, args: Vec<Expr>| {
            Expr::Call(ast::ExprCall {
                func: Box::new(Expr::Name(ast::ExprName {
                    id: format!("{}{f}", prefix_of("builtins")).into(),
                    ctx: ast::ExprContext::Load,
                    range,
                })),
                args,
                keywords: Vec::new(),
                range,
            })
        };
        match name {
            "hex" | "oct" | "bin" | "divmod" if call.keywords.is_empty() => Some(named(name, args)),
            "pow" if call.keywords.is_empty() => match args.len() {
                2 => {
                    let mut it = args.into_iter();
                    Some(Expr::BinOp(ast::ExprBinOp {
                        left: Box::new(it.next()?),
                        op: ast::Operator::Pow,
                        right: Box::new(it.next()?),
                        range,
                    }))
                }
                3 => Some(named("pow3", args)),
                _ => None,
            },
            "int" => {
                let base = call
                    .keywords
                    .iter()
                    .find(|k| k.arg.as_deref() == Some("base"))
                    .map(|k| fold(self, &k.value));
                match (args.len(), base) {
                    (2, None) => Some(named("int_base", args)),
                    (1, Some(b)) => Some(named("int_base", vec![args[0].clone(), b])),
                    _ => None,
                }
            }
            // `filter(f, xs)` is the generator `(x for x in xs if f(x))`, and
            // `filter(None, xs)` keeps the true elements.
            "filter" if call.keywords.is_empty() && args.len() == 2 => {
                // A lambda's own parameter and body, since a lambda cannot be
                // called where it is written.
                let (item, test) = match &args[0] {
                    Expr::Lambda(l)
                        if l.args.args.len() == 1
                            && l.args.posonlyargs.is_empty()
                            && l.args.vararg.is_none()
                            && l.args.kwonlyargs.is_empty()
                            && l.args.kwarg.is_none() =>
                    {
                        (l.args.args[0].def.arg.to_string(), (*l.body).clone())
                    }
                    Expr::Constant(c) if matches!(c.value, ast::Constant::None) => (
                        "__filter_item".to_string(),
                        Expr::Name(ast::ExprName {
                            id: "__filter_item".into(),
                            ctx: ast::ExprContext::Load,
                            range,
                        }),
                    ),
                    f => (
                        "__filter_item".to_string(),
                        Expr::Call(ast::ExprCall {
                            func: Box::new(f.clone()),
                            args: vec![Expr::Name(ast::ExprName {
                                id: "__filter_item".into(),
                                ctx: ast::ExprContext::Load,
                                range,
                            })],
                            keywords: Vec::new(),
                            range,
                        }),
                    ),
                };
                let var = || {
                    Expr::Name(ast::ExprName {
                        id: item.as_str().into(),
                        ctx: ast::ExprContext::Load,
                        range,
                    })
                };
                Some(Expr::GeneratorExp(ast::ExprGeneratorExp {
                    elt: Box::new(var()),
                    generators: vec![ast::Comprehension {
                        target: Expr::Name(ast::ExprName {
                            id: item.as_str().into(),
                            ctx: ast::ExprContext::Store,
                            range,
                        }),
                        iter: args[1].clone(),
                        ifs: vec![test],
                        is_async: false,
                        range: Default::default(),
                    }],
                    range,
                }))
            }
            _ => None,
        }
    }

    /// An annotation, where a quoted name (`-> "Path"`) is a name too.
    fn annotation(&mut self, expr: Expr) -> Expr {
        let expr = match expr {
            Expr::Constant(c) => match &c.value {
                ast::Constant::Str(s) if self.renames.names.contains_key(s.as_str()) => {
                    Expr::Name(ast::ExprName {
                        id: s.as_str().into(),
                        ctx: ast::ExprContext::Load,
                        range: c.range,
                    })
                }
                _ => Expr::Constant(c),
            },
            Expr::Subscript(mut s) => {
                *s.slice = self.annotation(*s.slice);
                Expr::Subscript(s)
            }
            Expr::Tuple(mut tup) => {
                tup.elts = tup.elts.into_iter().map(|e| self.annotation(e)).collect();
                Expr::Tuple(tup)
            }
            other => other,
        };
        match ast::fold::Fold::fold_expr(self, expr) {
            Ok(e) => e,
            Err(never) => match never {},
        }
    }

    /// `operator.itemgetter(k)`, `attrgetter("a")`, and `methodcaller("m",
    /// ...)` as the lambdas they make, so the function a sort key or a call
    /// takes is typed where it is used.
    fn operator_lambda(&mut self, call: &ast::ExprCall) -> Option<Expr> {
        let func = match &*call.func {
            Expr::Name(n) => self
                .renames
                .names
                .get(n.id.as_str())
                .filter(|_| !self.shadowed(n.id.as_str()))
                .cloned()?,
            other => self.renames.attribute(other)?,
        };
        let kind = func.strip_prefix(&prefix_of("operator"))?;
        if !call.keywords.is_empty() {
            return None;
        }
        let range = call.range;
        let param = "__operator_obj";
        let obj = || {
            Expr::Name(ast::ExprName {
                id: param.into(),
                ctx: ast::ExprContext::Load,
                range,
            })
        };
        let folded: Vec<Expr> = call
            .args
            .iter()
            .map(|a| match ast::fold::Fold::fold_expr(self, a.clone()) {
                Ok(e) => e,
                Err(never) => match never {},
            })
            .collect();
        let item = |key: Expr| {
            Expr::Subscript(ast::ExprSubscript {
                value: Box::new(obj()),
                slice: Box::new(key),
                ctx: ast::ExprContext::Load,
                range,
            })
        };
        let attr = |name: &Expr| -> Option<Expr> {
            let Expr::Constant(c) = name else { return None };
            let ast::Constant::Str(path) = &c.value else {
                return None;
            };
            let mut e = obj();
            for part in path.split('.') {
                e = Expr::Attribute(ast::ExprAttribute {
                    value: Box::new(e),
                    attr: part.into(),
                    ctx: ast::ExprContext::Load,
                    range,
                });
            }
            Some(e)
        };
        let body = match kind {
            "itemgetter" => match folded.as_slice() {
                [one] => item(one.clone()),
                many if !many.is_empty() => Expr::Tuple(ast::ExprTuple {
                    elts: many.iter().map(|k| item(k.clone())).collect(),
                    ctx: ast::ExprContext::Load,
                    range,
                }),
                _ => return None,
            },
            "attrgetter" => match folded.as_slice() {
                [one] => attr(one)?,
                many if !many.is_empty() => Expr::Tuple(ast::ExprTuple {
                    elts: many.iter().map(attr).collect::<Option<Vec<_>>>()?,
                    ctx: ast::ExprContext::Load,
                    range,
                }),
                _ => return None,
            },
            "methodcaller" => {
                let (name, rest) = folded.split_first()?;
                let Expr::Constant(c) = name else { return None };
                let ast::Constant::Str(method) = &c.value else {
                    return None;
                };
                Expr::Call(ast::ExprCall {
                    func: Box::new(Expr::Attribute(ast::ExprAttribute {
                        value: Box::new(obj()),
                        attr: method.as_str().into(),
                        ctx: ast::ExprContext::Load,
                        range,
                    })),
                    args: rest.to_vec(),
                    keywords: Vec::new(),
                    range,
                })
            }
            _ => return None,
        };
        let arg = ast::ArgWithDefault {
            def: ast::Arg {
                arg: param.into(),
                annotation: None,
                type_comment: None,
                range: Default::default(),
            },
            default: None,
            range: Default::default(),
        };
        Some(Expr::Lambda(ast::ExprLambda {
            args: Box::new(ast::Arguments {
                posonlyargs: Vec::new(),
                args: vec![arg],
                vararg: None,
                kwonlyargs: Vec::new(),
                kwarg: None,
                range: Default::default(),
            }),
            body: Box::new(body),
            range,
        }))
    }
}

fn params_of(args: &ast::Arguments) -> HashSet<String> {
    let mut out = HashSet::new();
    for a in args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
    {
        out.insert(a.def.arg.to_string());
    }
    if let Some(v) = &args.vararg {
        out.insert(v.arg.to_string());
    }
    if let Some(k) = &args.kwarg {
        out.insert(k.arg.to_string());
    }
    out
}

impl ast::fold::Fold<ast::text_size::TextRange> for Renamer<'_> {
    type TargetU = ast::text_size::TextRange;
    type Error = std::convert::Infallible;
    type UserContext = ();
    fn will_map_user(&mut self, _user: &ast::text_size::TextRange) -> Self::UserContext {}
    fn map_user(
        &mut self,
        user: ast::text_size::TextRange,
        _context: (),
    ) -> std::result::Result<ast::text_size::TextRange, Self::Error> {
        Ok(user)
    }

    fn fold_expr(&mut self, expr: Expr) -> std::result::Result<Expr, Self::Error> {
        if let Expr::Call(call) = &expr {
            if let Some(lambda) = self.operator_lambda(call) {
                return Ok(lambda);
            }
            if let Some(made) = self.builtin_call(call) {
                return Ok(made);
            }
        }
        // `csv.reader(f)` and `csv.DictReader(f)` read their source's lines:
        // called with `f.readlines()`, so the reader is typed whatever the
        // source (a file, a StringIO).
        if let Expr::Call(call) = &expr {
            let callee = match &*call.func {
                Expr::Name(n) => self
                    .renames
                    .names
                    .get(n.id.as_str())
                    .filter(|_| !self.shadowed(n.id.as_str()))
                    .cloned(),
                other => self.renames.attribute(other),
            };
            let reads = callee.as_deref().is_some_and(|c| {
                c == format!("{}reader", prefix_of("csv"))
                    || c == format!("{}DictReader", prefix_of("csv"))
            });
            if reads && !call.args.is_empty() {
                let mut call = call.clone();
                let source = call.args.remove(0);
                let range = call.range;
                call.args.insert(
                    0,
                    Expr::Call(ast::ExprCall {
                        func: Box::new(Expr::Attribute(ast::ExprAttribute {
                            value: Box::new(source),
                            attr: "readlines".into(),
                            ctx: ast::ExprContext::Load,
                            range,
                        })),
                        args: Vec::new(),
                        keywords: Vec::new(),
                        range,
                    }),
                );
                return ast::fold::fold_expr(self, Expr::Call(call));
            }
        }
        if let Some(name) = self.renames.attribute(&expr) {
            let range = match &expr {
                Expr::Attribute(a) => a.range,
                _ => unreachable!(),
            };
            let ctx = match &expr {
                Expr::Attribute(a) => a.ctx,
                _ => unreachable!(),
            };
            return Ok(Expr::Name(ast::ExprName {
                id: name.into(),
                ctx,
                range,
            }));
        }
        if let Expr::Name(n) = &expr {
            if let Some(to) = self.renames.names.get(n.id.as_str()) {
                if !self.shadowed(n.id.as_str()) {
                    return Ok(Expr::Name(ast::ExprName {
                        id: to.as_str().into(),
                        ctx: n.ctx,
                        range: n.range,
                    }));
                }
            }
        }
        if let Expr::Lambda(l) = &expr {
            self.shadowed.push(params_of(&l.args));
            let folded = ast::fold::fold_expr(self, expr);
            self.shadowed.pop();
            return folded;
        }
        ast::fold::fold_expr(self, expr)
    }

    fn fold_stmt(&mut self, stmt: Stmt) -> std::result::Result<Stmt, Self::Error> {
        match stmt {
            Stmt::FunctionDef(mut def) => {
                if self.depth == 0 {
                    if let Some(to) = self.renames.names.get(def.name.as_str()) {
                        def.name = to.as_str().into();
                    }
                }
                // The parameters' defaults and annotations are read outside.
                let mut args = self.fold_arguments(*def.args)?;
                for a in args
                    .posonlyargs
                    .iter_mut()
                    .chain(args.args.iter_mut())
                    .chain(args.kwonlyargs.iter_mut())
                {
                    if let Some(ann) = a.def.annotation.take() {
                        a.def.annotation = Some(Box::new(self.annotation(*ann)));
                    }
                }
                let returns = def.returns.map(|r| Box::new(self.annotation(*r)));
                let decorators = def
                    .decorator_list
                    .into_iter()
                    .map(|d| self.fold_expr(d))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                self.shadowed.push(params_of(&args));
                self.depth += 1;
                let body = def
                    .body
                    .into_iter()
                    .map(|s| self.fold_stmt(s))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                self.depth -= 1;
                self.shadowed.pop();
                def.args = Box::new(args);
                def.returns = returns;
                def.decorator_list = decorators;
                def.body = body;
                Ok(Stmt::FunctionDef(def))
            }
            Stmt::AsyncFunctionDef(mut def) => {
                if self.depth == 0 {
                    if let Some(to) = self.renames.names.get(def.name.as_str()) {
                        def.name = to.as_str().into();
                    }
                }
                // The parameters' defaults and annotations are read outside.
                let mut args = self.fold_arguments(*def.args)?;
                for a in args
                    .posonlyargs
                    .iter_mut()
                    .chain(args.args.iter_mut())
                    .chain(args.kwonlyargs.iter_mut())
                {
                    if let Some(ann) = a.def.annotation.take() {
                        a.def.annotation = Some(Box::new(self.annotation(*ann)));
                    }
                }
                let returns = def.returns.map(|r| Box::new(self.annotation(*r)));
                let decorators = def
                    .decorator_list
                    .into_iter()
                    .map(|d| self.fold_expr(d))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                self.shadowed.push(params_of(&args));
                self.depth += 1;
                let body = def
                    .body
                    .into_iter()
                    .map(|s| self.fold_stmt(s))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                self.depth -= 1;
                self.shadowed.pop();
                def.args = Box::new(args);
                def.returns = returns;
                def.decorator_list = decorators;
                def.body = body;
                Ok(Stmt::AsyncFunctionDef(def))
            }
            Stmt::ClassDef(mut def) => {
                if self.depth == 0 {
                    if let Some(to) = self.renames.names.get(def.name.as_str()) {
                        def.name = to.as_str().into();
                    }
                }
                let bases = def
                    .bases
                    .into_iter()
                    .map(|b| self.fold_expr(b))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                self.depth += 1;
                let body = def
                    .body
                    .into_iter()
                    .map(|s| self.fold_stmt(s))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                self.depth -= 1;
                def.bases = bases;
                def.body = body;
                Ok(Stmt::ClassDef(def))
            }
            // `with contextlib.suppress(E, ...): body` is `try: body` with
            // `except (E, ...): pass`, which it means.
            Stmt::With(w)
                if w.items.len() == 1
                    && w.items[0].optional_vars.is_none()
                    && self.suppress_call(&w.items[0].context_expr).is_some() =>
            {
                let excs = self
                    .suppress_call(&w.items[0].context_expr)
                    .expect("checked");
                let range = w.range;
                let body = w
                    .body
                    .into_iter()
                    .map(|s| self.fold_stmt(s))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let excs = excs
                    .into_iter()
                    .map(|e| self.fold_expr(e))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let kind = if excs.len() == 1 {
                    excs.into_iter().next()
                } else {
                    Some(Expr::Tuple(ast::ExprTuple {
                        elts: excs,
                        ctx: ast::ExprContext::Load,
                        range,
                    }))
                };
                Ok(Stmt::Try(ast::StmtTry {
                    body,
                    handlers: vec![ast::ExceptHandler::ExceptHandler(
                        ast::ExceptHandlerExceptHandler {
                            type_: kind.map(Box::new),
                            name: None,
                            body: vec![Stmt::Pass(ast::StmtPass { range })],
                            range,
                        },
                    )],
                    orelse: Vec::new(),
                    finalbody: Vec::new(),
                    range,
                }))
            }
            Stmt::AnnAssign(mut s) => {
                s.annotation = Box::new(self.annotation(*s.annotation));
                s.target = Box::new(self.fold_expr(*s.target)?);
                if let Some(v) = s.value.take() {
                    s.value = Some(Box::new(self.fold_expr(*v)?));
                }
                Ok(Stmt::AnnAssign(s))
            }
            other => ast::fold::fold_stmt(self, other),
        }
    }
}

fn rename(body: Vec<Stmt>, renames: &Renames) -> Vec<Stmt> {
    let mut renamer = Renamer {
        renames,
        shadowed: Vec::new(),
        depth: 0,
    };
    body.into_iter()
        .map(|s| match ast::fold::Fold::fold_stmt(&mut renamer, s) {
            Ok(s) => s,
            Err(never) => match never {},
        })
        .collect()
}

/// Whether a statement imports one of the modules written in Python, which
/// its renamed definitions stand in for.
fn imports_embedded(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Import(s) => s.names.iter().all(|a| source_of(a.name.as_str()).is_some()),
        Stmt::ImportFrom(s) => s.module.as_ref().is_some_and(|m| {
            source_of(m.as_str()).is_some()
                || s.names
                    .iter()
                    .all(|a| source_of(&format!("{m}.{}", a.name)).is_some())
        }),
        _ => false,
    }
}

/// `body` with the modules written in Python that it imports placed ahead of
/// it (after the first `hoisted` statements, the imports `ir::imports` moved
/// to the top), and its reads of them renamed. `None` when it imports none.
pub(crate) fn embed(body: &[Stmt], hoisted: usize) -> Result<Option<(Suite, usize)>> {
    use rustpython_parser::Parse;
    let mut wanted = imported(body);
    let used = builtins_used(body);
    if !used.is_empty() && !wanted.iter().any(|m| m == "builtins") {
        wanted.push("builtins".to_string());
    }
    if wanted.is_empty() {
        return Ok(None);
    }
    // `argparse`'s Namespace is written for the program's arguments.
    let mut body: Vec<Stmt> = body.to_vec();
    let namespace = if wanted.iter().any(|m| m == "argparse") {
        let specs = argparse_calls(&mut body)?;
        Some(
            Suite::parse(&namespace_source(&specs), "<argparse.Namespace>")
                .map_err(|e| anyhow!("argparse's Namespace does not parse: {e}"))?,
        )
    } else {
        None
    };
    // FastAPI needs pydantic's validation, and a test client its app.
    if wanted.iter().any(|m| m == "fastapi.testclient") && !wanted.iter().any(|m| m == "fastapi") {
        wanted.push("fastapi".to_string());
    }
    if wanted.iter().any(|m| m == "fastapi") && !wanted.iter().any(|m| m == "pydantic") {
        wanted.push("pydantic".to_string());
    }
    // The program's pydantic models and FastAPI routes, written out.
    let models = if wanted.iter().any(|m| m == "pydantic") {
        crate::ir::shims::write_models(&mut body)?
    } else {
        Vec::new()
    };
    if wanted.iter().any(|m| m == "fastapi") {
        crate::ir::shims::write_routes(&mut body, &models)?;
    }
    // The program's Enum classes, written out, and its reads of them.
    if wanted.iter().any(|m| m == "enum") {
        let enums = write_enums(&mut body)?;
        if !enums.is_empty() {
            body = rewrite_enums(body, &enums);
        }
    }
    let body = &body[..];
    // Each module, after the modules it imports.
    let mut order: Vec<String> = Vec::new();
    let mut sources: HashMap<String, Suite> = HashMap::new();
    // The Anthropic client a program's `AsyncAnthropic` names is the one
    // over httpx; any other program's is over requests.
    let async_client = mentions(body, "AsyncAnthropic");
    fn visit(
        m: &str,
        order: &mut Vec<String>,
        sources: &mut HashMap<String, Suite>,
        stack: &mut Vec<String>,
        async_client: bool,
    ) -> Result<()> {
        if order.iter().any(|o| o == m) || stack.iter().any(|s| s == m) {
            return Ok(());
        }
        let source = if m == "anthropic" && async_client {
            ANTHROPIC_ASYNC
        } else {
            source_of(m).ok_or_else(|| anyhow!("no module {m}"))?
        };
        let ast = Suite::parse(source, &format!("<{m}>"))
            .map_err(|e| anyhow!("the standard-library module {m} does not parse: {e}"))?;
        stack.push(m.to_string());
        for dep in imported(&ast) {
            visit(&dep, order, sources, stack, async_client)?;
        }
        stack.pop();
        sources.insert(m.to_string(), ast);
        order.push(m.to_string());
        Ok(())
    }
    for m in &wanted {
        visit(m, &mut order, &mut sources, &mut Vec::new(), async_client)?;
    }
    if let (Some(ns), Some(src)) = (namespace, sources.get_mut("argparse")) {
        src.extend(ns);
    }

    let mut modules: HashMap<String, (String, HashSet<String>)> = HashMap::new();
    for m in &order {
        modules.insert(m.clone(), (prefix_of(m), defined(&sources[m])));
    }
    // What a body's `from m import a as b` brings in.
    let from_imports = |body: &[Stmt]| -> Result<HashMap<String, String>> {
        let mut out = HashMap::new();
        for stmt in body {
            let Stmt::ImportFrom(s) = stmt else { continue };
            let Some(m) = &s.module else { continue };
            for alias in &s.names {
                let bound = alias.asname.as_ref().unwrap_or(&alias.name).to_string();
                if let Some((prefix, names)) = modules.get(m.as_str()) {
                    if alias.name.as_str() == "*" {
                        return Err(anyhow!("'from {m} import *' is not supported"));
                    }
                    if !names.contains(alias.name.as_str()) {
                        return Err(crate::core::errors::unsupported_feature(
                            format!(
                                "cannot import name '{}' from '{m}': this compiler's {m} does \
                                 not have it",
                                alias.name
                            ),
                            None,
                        )
                        .into());
                    }
                    out.insert(bound, format!("{prefix}{}", alias.name));
                }
            }
        }
        Ok(out)
    };

    let mut out: Vec<Stmt> = Vec::new();
    out.extend(body[..hoisted].iter().cloned());
    let mut added = 0;
    for m in &order {
        let ast = sources[m].clone();
        let mut names: HashMap<String, String> = modules[m]
            .1
            .iter()
            .map(|n| (n.clone(), format!("{}{n}", modules[m].0)))
            .collect();
        names.extend(from_imports(&ast)?);
        let renames = Renames {
            names,
            builtins: HashSet::new(),
            modules: modules.clone(),
        };
        for stmt in rename(ast, &renames) {
            // Its docstring, and its imports of the other modules here.
            if matches!(&stmt, Stmt::Expr(e) if matches!(&*e.value, Expr::Constant(_)))
                || imports_embedded(&stmt)
            {
                continue;
            }
            out.push(stmt);
            added += 1;
        }
    }
    let renames = Renames {
        names: from_imports(body)?,
        builtins: used,
        modules,
    };
    let rest: Vec<Stmt> = body[hoisted..]
        .iter()
        .filter(|s| !imports_embedded(s))
        .cloned()
        .collect();
    let kept_hoisted = out[..hoisted]
        .iter()
        .filter(|s| !imports_embedded(s))
        .count();
    let mut program: Vec<Stmt> = out[..hoisted]
        .iter()
        .filter(|s| !imports_embedded(s))
        .cloned()
        .collect();
    program.extend(out[hoisted..].iter().cloned());
    program.extend(rename(rest, &renames));
    let _ = added;
    Ok(Some((program, kept_hoisted)))
}
