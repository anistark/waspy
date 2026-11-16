//! Packages from outside the standard library that programs use every day,
//! in the shape their own code expects (`pydantic`, `fastapi`, `httpx`,
//! `anthropic`). Each is a module written in Python (`src/ir/pystdlib/`),
//! embedded as the standard-library ones are; what a package does with the
//! program's own classes and functions (a model's validation, an app's
//! routes) is written out here from the program's syntax tree, before it is
//! lowered, since nothing inspects a class or a function when it runs.

use anyhow::{anyhow, Result};
use rustpython_parser::ast::{self, Expr, Stmt};
use std::collections::HashMap;

fn refuse(message: String) -> anyhow::Error {
    crate::core::errors::unsupported_feature(message, None).into()
}

/// A model field's type, as far as pydantic's lax validation is written.
#[derive(Clone, Debug)]
enum FType {
    Str,
    Int,
    Float,
    Bool,
    Any,
    Model(String),
    List(Box<FType>),
    Dict(Box<FType>),
    Optional(Box<FType>),
}

impl FType {
    /// The annotation the written code gives it.
    fn ann(&self) -> String {
        match self {
            FType::Str => "str".into(),
            FType::Int => "int".into(),
            FType::Float => "float".into(),
            FType::Bool => "bool".into(),
            FType::Any => "Any".into(),
            FType::Model(m) => m.clone(),
            FType::List(t) => format!("list[{}]", t.ann()),
            FType::Dict(t) => format!("dict[str, {}]", t.ann()),
            FType::Optional(t) => format!("Optional[{}]", t.ann()),
        }
    }

    /// Whether a value of this type holds a model (compared through its dump).
    fn holds_model(&self) -> bool {
        match self {
            FType::Model(_) => true,
            FType::List(t) | FType::Dict(t) | FType::Optional(t) => t.holds_model(),
            _ => false,
        }
    }

    /// The value a field holds while its input is being checked.
    fn zero(&self) -> String {
        match self {
            FType::Str => "\"\"".into(),
            FType::Int => "0".into(),
            FType::Float => "0.0".into(),
            FType::Bool => "False".into(),
            FType::Any | FType::Optional(_) => "None".into(),
            FType::Model(m) => format!("{VALIDATE_PREFIX}{m}(pydantic._empty(), [], [])"),
            FType::List(_) => "[]".into(),
            FType::Dict(_) => "{}".into(),
        }
    }
}

/// The prefix of the function that validates a model's input.
const VALIDATE_PREFIX: &str = "__py_pydantic_v_";

/// The placeholder a field's default stands under in the written source.
const DEFAULT_PREFIX: &str = "__py_pydantic_default_";

/// The names a program binds pydantic's `BaseModel` to.
fn base_model_names(body: &[Stmt]) -> Vec<String> {
    let mut out = Vec::new();
    for stmt in body {
        if let Stmt::ImportFrom(s) = stmt {
            if s.module.as_ref().is_some_and(|m| m.as_str() == "pydantic") {
                for alias in &s.names {
                    if alias.name.as_str() == "BaseModel" {
                        out.push(alias.asname.as_ref().unwrap_or(&alias.name).to_string());
                    }
                }
            }
        }
    }
    out
}

fn field_type(e: &Expr, models: &HashMap<String, Vec<Field>>, owner: &str) -> Result<FType> {
    let bad = || {
        refuse(format!(
            "the model '{owner}' has a field of a type pydantic's validation is not written for \
             here: models take str, int, float, bool, Any, another model, list[...], \
             dict[str, ...], and Optional[...] of those"
        ))
    };
    let named = |name: &str| -> Option<FType> {
        Some(match name {
            "str" => FType::Str,
            "int" => FType::Int,
            "float" => FType::Float,
            "bool" => FType::Bool,
            "Any" => FType::Any,
            m if models.contains_key(m) || m == owner => FType::Model(m.to_string()),
            _ => return None,
        })
    };
    match e {
        Expr::Name(n) => named(n.id.as_str()).ok_or_else(bad),
        Expr::Constant(c) => match &c.value {
            ast::Constant::Str(s) => named(s.as_str()).ok_or_else(bad),
            _ => Err(bad()),
        },
        Expr::Attribute(a) if matches!(&*a.value, Expr::Name(m) if m.id.as_str() == "typing") => {
            named(a.attr.as_str()).ok_or_else(bad)
        }
        Expr::BinOp(b) if matches!(b.op, ast::Operator::BitOr) => {
            let none =
                |x: &Expr| matches!(x, Expr::Constant(c) if matches!(c.value, ast::Constant::None));
            if none(&b.right) {
                Ok(FType::Optional(Box::new(field_type(
                    &b.left, models, owner,
                )?)))
            } else if none(&b.left) {
                Ok(FType::Optional(Box::new(field_type(
                    &b.right, models, owner,
                )?)))
            } else {
                Err(bad())
            }
        }
        Expr::Subscript(s) => {
            let generic = match &*s.value {
                Expr::Name(n) => n.id.to_string(),
                Expr::Attribute(a) => a.attr.to_string(),
                _ => return Err(bad()),
            };
            let inner = |x: &Expr| field_type(x, models, owner);
            let t = match generic.as_str() {
                "list" | "List" => FType::List(Box::new(inner(&s.slice)?)),
                "Optional" => FType::Optional(Box::new(inner(&s.slice)?)),
                "dict" | "Dict" => {
                    let Expr::Tuple(t) = &*s.slice else {
                        return Err(bad());
                    };
                    if t.elts.len() != 2
                        || !matches!(&t.elts[0], Expr::Name(n) if n.id.as_str() == "str")
                    {
                        return Err(bad());
                    }
                    FType::Dict(Box::new(inner(&t.elts[1])?))
                }
                "Union" => {
                    let Expr::Tuple(t) = &*s.slice else {
                        return Err(bad());
                    };
                    let none = |x: &Expr| matches!(x, Expr::Constant(c) if matches!(c.value, ast::Constant::None));
                    if t.elts.len() != 2 || !none(&t.elts[1]) {
                        return Err(bad());
                    }
                    FType::Optional(Box::new(inner(&t.elts[0])?))
                }
                _ => return Err(bad()),
            };
            // One level of collection: a list of lists is not written.
            match &t {
                FType::List(x) | FType::Dict(x)
                    if matches!(**x, FType::List(_) | FType::Dict(_) | FType::Optional(_)) =>
                {
                    Err(bad())
                }
                FType::Optional(x)
                    if matches!(**x, FType::List(_) | FType::Dict(_) | FType::Optional(_)) =>
                {
                    Err(refuse(format!(
                        "the model '{owner}' has an Optional field holding a collection, which is \
                         not supported; give it an empty default instead (tags: list[str] = [])"
                    )))
                }
                _ => Ok(t),
            }
        }
        _ => Err(bad()),
    }
}

#[derive(Clone, Debug)]
struct Field {
    name: String,
    ty: FType,
    default: Option<Expr>,
}

/// A counter for the written code's temporaries.
struct Names(usize);

impl Names {
    fn next(&mut self) -> usize {
        self.0 += 1;
        self.0
    }
}

/// The statements that convert `src` (a value of `Any`) into `target`, a
/// variable already declared with the field's type, appending each failure
/// at `loc` to `errs`.
fn convert(
    out: &mut Vec<String>,
    pad: &str,
    target: &str,
    src: &str,
    loc: &str,
    ty: &FType,
    n: &mut Names,
) {
    let scalar = |f: &str| format!("{pad}{target} = pydantic.{f}({src}, {loc}, errs)");
    match ty {
        FType::Str => out.push(scalar("_v_str")),
        FType::Int => out.push(scalar("_v_int")),
        FType::Float => out.push(scalar("_v_float")),
        FType::Bool => out.push(scalar("_v_bool")),
        FType::Any => out.push(format!("{pad}{target} = {src}")),
        FType::Model(m) => out.push(format!(
            "{pad}{target} = {VALIDATE_PREFIX}{m}({src}, {loc}, errs)"
        )),
        FType::Optional(inner) => {
            let k = n.next();
            out.push(format!("{pad}x{k} = {src}"));
            out.push(format!("{pad}if x{k} is None:"));
            out.push(format!("{pad}    {target} = None"));
            out.push(format!("{pad}else:"));
            out.push(format!("{pad}    y{k}: {} = {}", inner.ann(), inner.zero()));
            convert(
                out,
                &format!("{pad}    "),
                &format!("y{k}"),
                &format!("x{k}"),
                loc,
                inner,
                n,
            );
            out.push(format!("{pad}    {target} = y{k}"));
        }
        FType::List(inner) => {
            let k = n.next();
            out.push(format!("{pad}l{k} = pydantic._v_list({src}, {loc}, errs)"));
            out.push(format!("{pad}t{k}: list[{}] = []", inner.ann()));
            out.push(format!("{pad}i{k} = 0"));
            out.push(format!("{pad}for e{k} in l{k}:"));
            let inner_pad = format!("{pad}    ");
            out.push(format!(
                "{inner_pad}y{k}: {} = {}",
                inner.ann(),
                inner.zero()
            ));
            convert(
                out,
                &inner_pad,
                &format!("y{k}"),
                &format!("e{k}"),
                &format!("{loc} + [\"#\" + str(i{k})]"),
                inner,
                n,
            );
            out.push(format!("{inner_pad}t{k}.append(y{k})"));
            out.push(format!("{inner_pad}i{k} += 1"));
            out.push(format!("{pad}{target} = t{k}"));
        }
        FType::Dict(inner) => {
            let k = n.next();
            out.push(format!("{pad}m{k} = pydantic._v_dict({src}, {loc}, errs)"));
            out.push(format!("{pad}t{k}: dict[str, {}] = {{}}", inner.ann()));
            out.push(format!("{pad}for key{k} in m{k}:"));
            let inner_pad = format!("{pad}    ");
            out.push(format!(
                "{inner_pad}y{k}: {} = {}",
                inner.ann(),
                inner.zero()
            ));
            convert(
                out,
                &inner_pad,
                &format!("y{k}"),
                &format!("m{k}[key{k}]"),
                &format!("{loc} + [key{k}]"),
                inner,
                n,
            );
            out.push(format!("{inner_pad}t{k}[key{k}] = y{k}"));
            out.push(format!("{pad}{target} = t{k}"));
        }
    }
}

/// The statements that put `src` (a value of the field's type) as JSON-shaped
/// data into `target` (a place holding `Any`), as `model_dump()` does.
fn dump(out: &mut Vec<String>, pad: &str, target: &str, src: &str, ty: &FType, n: &mut Names) {
    match ty {
        FType::Str | FType::Int | FType::Float | FType::Bool | FType::Any => {
            out.push(format!("{pad}{target} = {src}"))
        }
        FType::Model(_) => out.push(format!("{pad}{target} = {src}.model_dump()")),
        FType::Optional(inner) => {
            let k = n.next();
            out.push(format!("{pad}if {src} is None:"));
            out.push(format!("{pad}    {target} = None"));
            out.push(format!("{pad}else:"));
            let inner_pad = format!("{pad}    ");
            match &**inner {
                FType::Model(_) => out.push(format!("{inner_pad}{target} = {src}.model_dump()")),
                FType::Str => out.push(format!("{inner_pad}{target} = str({src})")),
                other => {
                    out.push(format!("{inner_pad}w{k}: {} = {src}", other.ann()));
                    dump(out, &inner_pad, target, &format!("w{k}"), other, n);
                }
            }
        }
        FType::List(inner) => {
            let k = n.next();
            out.push(format!("{pad}t{k}: list[Any] = []"));
            out.push(format!("{pad}for e{k} in {src}:"));
            let inner_pad = format!("{pad}    ");
            out.push(format!("{inner_pad}y{k}: Any = None"));
            dump(
                out,
                &inner_pad,
                &format!("y{k}"),
                &format!("e{k}"),
                inner,
                n,
            );
            out.push(format!("{inner_pad}t{k}.append(y{k})"));
            out.push(format!("{pad}{target} = t{k}"));
        }
        FType::Dict(inner) => {
            let k = n.next();
            out.push(format!("{pad}t{k}: dict[str, Any] = {{}}"));
            out.push(format!("{pad}for key{k} in {src}:"));
            let inner_pad = format!("{pad}    ");
            out.push(format!("{inner_pad}y{k}: Any = None"));
            dump(
                out,
                &inner_pad,
                &format!("y{k}"),
                &format!("{src}[key{k}]"),
                inner,
                n,
            );
            out.push(format!("{inner_pad}t{k}[key{k}] = y{k}"));
            out.push(format!("{pad}{target} = t{k}"));
        }
    }
}

/// A model's class and the function validating its input, as source.
fn model_source(name: &str, bases: &str, fields: &[Field]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut n = Names(0);
    out.push(format!("class {name}({bases}):"));
    for f in fields {
        out.push(format!("    {}: {}", f.name, f.ty.ann()));
    }
    // The constructor: every field by keyword, a collection copied.
    let params: Vec<String> = fields
        .iter()
        .enumerate()
        .map(|(k, f)| match &f.default {
            Some(_) => format!("{}: {} = {DEFAULT_PREFIX}{k}", f.name, f.ty.ann()),
            None => format!("{}: {}", f.name, f.ty.ann()),
        })
        .collect();
    if params.is_empty() {
        out.push("    def __init__(self) -> None:".into());
        out.push("        pass".into());
    } else {
        out.push(format!(
            "    def __init__(self, *, {}) -> None:",
            params.join(", ")
        ));
    }
    for f in fields {
        let value = match &f.ty {
            FType::List(_) => format!("[x for x in {}]", f.name),
            FType::Dict(_) => format!("{{k: v for k, v in {}.items()}}", f.name),
            _ => f.name.clone(),
        };
        out.push(format!("        self.{} = {value}", f.name));
    }
    let shown: Vec<String> = fields
        .iter()
        .map(|f| format!("\"{}=\" + repr(self.{})", f.name, f.name))
        .collect();
    out.push("    def __repr__(self) -> str:".into());
    if shown.is_empty() {
        out.push(format!("        return \"{name}()\""));
    } else {
        out.push(format!(
            "        return \"{name}(\" + {} + \")\"",
            shown.join(" + \", \" + ")
        ));
    }
    out.push("    def __str__(self) -> str:".into());
    if shown.is_empty() {
        out.push("        return \"\"".into());
    } else {
        out.push(format!("        return {}", shown.join(" + \" \" + ")));
    }
    // pydantic compares the fields' values, which the dumps hold.
    out.push(format!("    def __eq__(self, other: \"{name}\") -> bool:"));
    if fields.is_empty() {
        out.push("        return True".into());
    } else {
        let same: Vec<String> = fields
            .iter()
            .map(|f| {
                if f.ty.holds_model() {
                    format!(
                        "pydantic._json(self.model_dump()[\"{0}\"]) == \
                         pydantic._json(other.model_dump()[\"{0}\"])",
                        f.name
                    )
                } else {
                    format!("self.{0} == other.{0}", f.name)
                }
            })
            .collect();
        out.push(format!("        return {}", same.join(" and ")));
    }
    out.push("    def model_dump(self) -> dict[str, Any]:".into());
    out.push("        out: dict[str, Any] = {}".into());
    for f in fields {
        dump(
            &mut out,
            "        ",
            &format!("out[\"{}\"]", f.name),
            &format!("self.{}", f.name),
            &f.ty,
            &mut n,
        );
    }
    out.push("        return out".into());
    out.push("    def model_dump_json(self) -> str:".into());
    out.push("        return pydantic._json(self.model_dump())".into());
    out.push("    @staticmethod".into());
    out.push(format!("    def model_validate(obj: Any) -> \"{name}\":"));
    out.push("        errs: list[pydantic._Err] = []".into());
    out.push(format!(
        "        m = {VALIDATE_PREFIX}{name}(obj, [], errs)"
    ));
    out.push("        if len(errs) > 0:".into());
    out.push(format!(
        "            raise pydantic._fail(\"{name}\", errs)"
    ));
    out.push("        return m".into());
    out.push("    @staticmethod".into());
    out.push(format!(
        "    def model_validate_json(text: str) -> \"{name}\":"
    ));
    out.push(format!(
        "        data = pydantic._parse_json(text, \"{name}\")"
    ));
    out.push("        pydantic._Mode.json = True".into());
    out.push("        try:".into());
    out.push(format!("            return {name}.model_validate(data)"));
    out.push("        finally:".into());
    out.push("            pydantic._Mode.json = False".into());
    // The validation: each field from the dict, its failures collected.
    out.push(format!(
        "def {VALIDATE_PREFIX}{name}(v: Any, loc: list[str], errs: list[pydantic._Err]) -> {name}:"
    ));
    out.push(format!(
        "    d = pydantic._v_model(v, \"{name}\", loc, errs)"
    ));
    for (k, f) in fields.iter().enumerate() {
        let start = match &f.default {
            Some(_) => format!("{DEFAULT_PREFIX}{k}"),
            None => f.ty.zero(),
        };
        out.push(format!("    f_{}: {} = {start}", f.name, f.ty.ann()));
        out.push(format!("    if \"{}\" in d:", f.name));
        convert(
            &mut out,
            "        ",
            &format!("f_{}", f.name),
            &format!("d[\"{}\"]", f.name),
            &format!("loc + [\"{}\"]", f.name),
            &f.ty,
            &mut n,
        );
        if f.default.is_none() {
            out.push("    elif pydantic._is_dict(v):".into());
            out.push(format!(
                "        errs.append(pydantic._missing(loc + [\"{}\"], v))",
                f.name
            ));
        }
    }
    let args: Vec<String> = fields
        .iter()
        .map(|f| format!("{}=f_{}", f.name, f.name))
        .collect();
    out.push(format!("    return {name}({})", args.join(", ")));
    out.push(String::new());
    out.join("\n")
}

/// Replace each `__py_pydantic_default_<k>` name with the field's default.
struct Defaults<'a>(&'a [Field]);

impl ast::fold::Fold<ast::text_size::TextRange> for Defaults<'_> {
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
        if let Expr::Name(n) = &expr {
            if let Some(k) = n.id.as_str().strip_prefix(DEFAULT_PREFIX) {
                if let Some(d) = k.parse::<usize>().ok().and_then(|k| self.0.get(k)) {
                    if let Some(value) = &d.default {
                        return Ok(value.clone());
                    }
                }
            }
        }
        ast::fold::fold_expr(self, expr)
    }
}

/// The program's pydantic models, written out: each class a plain class with
/// its fields, constructor, `repr`, `==`, `model_dump`, `model_dump_json`,
/// `model_validate`, and `model_validate_json`, its methods kept.
pub(crate) fn write_models(body: &mut Vec<Stmt>) -> Result<Vec<String>> {
    use rustpython_parser::Parse;
    let base_names = base_model_names(body);
    let is_base = |b: &Expr| match b {
        Expr::Name(n) => base_names.iter().any(|x| x == n.id.as_str()),
        Expr::Attribute(a) => {
            a.attr.as_str() == "BaseModel"
                && matches!(&*a.value, Expr::Name(m) if m.id.as_str() == "pydantic")
        }
        _ => false,
    };
    let mut models: HashMap<String, Vec<Field>> = HashMap::new();
    let mut out: Vec<Stmt> = Vec::with_capacity(body.len());
    for stmt in std::mem::take(body) {
        let Stmt::ClassDef(class) = &stmt else {
            out.push(stmt);
            continue;
        };
        let name = class.name.to_string();
        // A model's base: BaseModel, or another model (whose fields come first).
        let mut fields: Vec<Field> = Vec::new();
        let mut is_model = false;
        for b in &class.bases {
            if is_base(b) {
                is_model = true;
            } else if let Expr::Name(n) = b {
                if let Some(inherited) = models.get(n.id.as_str()) {
                    fields.extend(inherited.iter().cloned());
                    is_model = true;
                }
            }
        }
        if !is_model {
            out.push(stmt);
            continue;
        }
        if class.bases.len() != 1 || !class.keywords.is_empty() {
            return Err(refuse(format!(
                "the model '{name}' has more than one base or a class keyword, which is not \
                 supported"
            )));
        }
        if !class.decorator_list.is_empty() {
            return Err(refuse(format!(
                "the model '{name}' has a class decorator, which is not supported"
            )));
        }
        let mut kept: Vec<Stmt> = Vec::new();
        for item in &class.body {
            match item {
                Stmt::AnnAssign(a) => {
                    let Expr::Name(target) = &*a.target else {
                        return Err(refuse(format!(
                            "the model '{name}' annotates something other than a field name"
                        )));
                    };
                    if target.id.as_str() == "model_config" {
                        return Err(refuse(format!(
                            "the model '{name}' sets model_config, which is not supported"
                        )));
                    }
                    let ty = field_type(&a.annotation, &models, &name)?;
                    if let Some(Expr::Call(c)) = a.value.as_deref() {
                        if matches!(&*c.func, Expr::Name(f) if f.id.as_str() == "Field") {
                            return Err(refuse(format!(
                                "the field '{}' of the model '{name}' uses Field(), which is not \
                                 supported; give its default directly",
                                target.id
                            )));
                        }
                    }
                    let field = Field {
                        name: target.id.to_string(),
                        ty,
                        default: a.value.as_deref().cloned(),
                    };
                    match fields.iter_mut().find(|f| f.name == field.name) {
                        Some(existing) => *existing = field,
                        None => fields.push(field),
                    }
                }
                Stmt::FunctionDef(f) => {
                    let decorated = f.decorator_list.iter().any(|d| {
                        matches!(d, Expr::Call(c) if matches!(&*c.func, Expr::Name(n)
                            if matches!(n.id.as_str(), "field_validator" | "model_validator" | "validator" | "root_validator" | "computed_field")))
                            || matches!(d, Expr::Name(n) if matches!(n.id.as_str(), "computed_field" | "model_validator"))
                    });
                    if decorated {
                        return Err(refuse(format!(
                            "the model '{name}' has a validator or computed field ('{}'), which is \
                             not supported",
                            f.name
                        )));
                    }
                    if f.name.as_str() == "__init__" {
                        return Err(refuse(format!(
                            "the model '{name}' defines __init__, which pydantic writes itself; \
                             this is not supported"
                        )));
                    }
                    kept.push(item.clone());
                }
                Stmt::Expr(e) if matches!(&*e.value, Expr::Constant(_)) => {}
                Stmt::Pass(_) => {}
                _ => {
                    return Err(refuse(format!(
                        "the model '{name}' has a class statement other than a field or a \
                         method, which is not supported"
                    )))
                }
            }
        }
        let base = match &class.bases[0] {
            b if is_base(b) => "pydantic.BaseModel".to_string(),
            Expr::Name(n) => n.id.to_string(),
            _ => unreachable!(),
        };
        let source = model_source(&name, &base, &fields);
        let parsed = ast::Suite::parse(&source, "<pydantic model>").map_err(|e| {
            anyhow!("the model '{name}' was written as invalid source: {e}\n{source}")
        })?;
        let mut defaults = Defaults(&fields);
        for (k, stmt) in parsed.into_iter().enumerate() {
            let mut stmt = match ast::fold::Fold::fold_stmt(&mut defaults, stmt) {
                Ok(s) => s,
                Err(never) => match never {},
            };
            if k == 0 {
                if let Stmt::ClassDef(c) = &mut stmt {
                    c.range = class.range;
                    c.body.extend(kept.iter().cloned());
                }
            }
            out.push(stmt);
        }
        models.insert(name, fields);
    }
    *body = out;
    Ok(models.into_keys().collect())
}

/// One route an application declares: `@app.<verb>(path, ...)` on a
/// function of the module.
struct Route {
    verb: String,
    segments: Vec<String>,
    handler: String,
    params: Vec<(String, Option<Expr>, Option<Expr>)>,
    returns: Option<Expr>,
    response_model: Option<Expr>,
    status: i64,
    is_async: bool,
    returns_value: bool,
}

/// Whether a function body returns a value somewhere (not counting nested
/// functions).
fn returns_value(body: &[Stmt]) -> bool {
    body.iter().any(|s| match s {
        Stmt::Return(r) => r.value.is_some(),
        Stmt::If(i) => returns_value(&i.body) || returns_value(&i.orelse),
        Stmt::For(f) => returns_value(&f.body) || returns_value(&f.orelse),
        Stmt::While(w) => returns_value(&w.body) || returns_value(&w.orelse),
        Stmt::With(w) => returns_value(&w.body),
        Stmt::Try(t) => {
            returns_value(&t.body)
                || returns_value(&t.orelse)
                || returns_value(&t.finalbody)
                || t.handlers.iter().any(|h| {
                    let ast::ExceptHandler::ExceptHandler(h) = h;
                    returns_value(&h.body)
                })
        }
        Stmt::Match(m) => m.cases.iter().any(|c| returns_value(&c.body)),
        _ => false,
    })
}

const VERBS: &[&str] = &["get", "post", "put", "delete", "patch"];

/// The program's FastAPI routes, written out: the decorators removed, and
/// `__py_fastapi__routes(method, url, body)`, which the application's
/// `handle` calls, matching a request to its route in declaration order.
pub(crate) fn write_routes(body: &mut Vec<Stmt>, model_names: &[String]) -> Result<()> {
    use rustpython_parser::Parse;
    // The names bound to FastAPI's class, and the applications made of it.
    let mut classes: Vec<String> = Vec::new();
    for stmt in body.iter() {
        if let Stmt::ImportFrom(s) = stmt {
            if s.module.as_ref().is_some_and(|m| m.as_str() == "fastapi") {
                for alias in &s.names {
                    match alias.name.as_str() {
                        "FastAPI" => {
                            classes.push(alias.asname.as_ref().unwrap_or(&alias.name).to_string())
                        }
                        "APIRouter" | "Depends" | "Request" | "BackgroundTasks" | "WebSocket" => {
                            return Err(refuse(format!(
                                "fastapi.{} is not supported here; routes are declared on the \
                                 FastAPI application",
                                alias.name
                            )))
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let makes_app = |e: &Expr| match e {
        Expr::Call(c) => match &*c.func {
            Expr::Name(n) => classes.iter().any(|x| x == n.id.as_str()),
            Expr::Attribute(a) => {
                a.attr.as_str() == "FastAPI"
                    && matches!(&*a.value, Expr::Name(m) if m.id.as_str() == "fastapi")
            }
            _ => false,
        },
        _ => false,
    };
    let mut apps: Vec<String> = Vec::new();
    for stmt in body.iter() {
        if let Stmt::Assign(a) = stmt {
            if a.targets.len() == 1 && makes_app(&a.value) {
                if let Expr::Name(n) = &a.targets[0] {
                    apps.push(n.id.to_string());
                }
            }
        }
    }
    let mut routes: Vec<Route> = Vec::new();
    let mut used_apps: Vec<String> = Vec::new();
    for stmt in body.iter_mut() {
        let (decorators, name, args, returns, fbody, is_async) = match stmt {
            Stmt::FunctionDef(f) => (
                &mut f.decorator_list,
                f.name.to_string(),
                &f.args,
                &f.returns,
                &f.body,
                false,
            ),
            Stmt::AsyncFunctionDef(f) => (
                &mut f.decorator_list,
                f.name.to_string(),
                &f.args,
                &f.returns,
                &f.body,
                true,
            ),
            _ => continue,
        };
        let mut route: Option<Route> = None;
        let mut kept: Vec<Expr> = Vec::new();
        for d in decorators.drain(..) {
            let Expr::Call(call) = &d else {
                kept.push(d);
                continue;
            };
            let Expr::Attribute(attr) = &*call.func else {
                kept.push(d);
                continue;
            };
            let Expr::Name(app) = &*attr.value else {
                kept.push(d);
                continue;
            };
            if !apps.iter().any(|a| a == app.id.as_str()) {
                kept.push(d);
                continue;
            }
            let verb = attr.attr.to_string();
            if !VERBS.contains(&verb.as_str()) {
                return Err(refuse(format!(
                    "@{}.{verb}(...) is not supported; routes are declared with get, post, put, \
                     delete, and patch",
                    app.id
                )));
            }
            if route.is_some() {
                return Err(refuse(format!("the function '{name}' is declared as more than one route, which is not supported")));
            }
            let path = match call.args.first() {
                Some(Expr::Constant(c)) => match &c.value {
                    ast::Constant::Str(s) => s.clone(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            if !path.starts_with('/') || call.args.len() != 1 {
                return Err(refuse(format!(
                    "the route on '{name}' does not give its path as one str literal starting \
                     with '/'"
                )));
            }
            let mut status = 200;
            let mut response_model = None;
            for kw in &call.keywords {
                let key = kw.arg.as_ref().map(|a| a.to_string()).unwrap_or_default();
                match key.as_str() {
                    "status_code" => match &kw.value {
                        Expr::Constant(c) => match &c.value {
                            ast::Constant::Int(i) => status = i.to_string().parse().unwrap_or(200),
                            _ => return Err(refuse(format!("the route on '{name}' gives a status_code that is not an int literal"))),
                        },
                        _ => return Err(refuse(format!("the route on '{name}' gives a status_code that is not an int literal"))),
                    },
                    "response_model" => response_model = Some(kw.value.clone()),
                    "tags" | "summary" | "description" | "name" | "deprecated" => {}
                    other => return Err(refuse(format!("the route on '{name}' passes '{other}', which is not supported"))),
                }
            }
            let segments: Vec<String> = path
                .split('/')
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect();
            for seg in &segments {
                if seg.starts_with('{') && (seg.contains(':') || !seg.ends_with('}')) {
                    return Err(refuse(format!("the route '{path}' uses a path converter or a partial parameter, which is not supported")));
                }
            }
            if args.vararg.is_some() || args.kwarg.is_some() || !args.posonlyargs.is_empty() {
                return Err(refuse(format!("the route handler '{name}' takes *args, **kwargs, or positional-only parameters, which is not supported")));
            }
            let mut params = Vec::new();
            for a in args.args.iter().chain(args.kwonlyargs.iter()) {
                params.push((
                    a.def.arg.to_string(),
                    a.def.annotation.as_deref().cloned(),
                    a.default.as_deref().cloned(),
                ));
            }
            used_apps.push(app.id.to_string());
            route = Some(Route {
                verb: verb.to_uppercase(),
                segments,
                handler: name.clone(),
                params,
                returns: returns.as_deref().cloned(),
                response_model,
                status,
                is_async,
                returns_value: returns_value(fbody),
            });
        }
        *decorators = kept;
        if let Some(r) = route {
            if !decorators.is_empty() {
                return Err(refuse(format!(
                    "the route handler '{name}' has another decorator, which is not supported"
                )));
            }
            routes.push(r);
        }
    }
    used_apps.dedup();
    if used_apps.len() > 1 {
        return Err(refuse(
            "routes are declared on more than one FastAPI application, which is not supported"
                .to_string(),
        ));
    }
    let model_of = |e: &Expr| -> Option<String> {
        match e {
            Expr::Name(n) if model_names.iter().any(|m| m == n.id.as_str()) => {
                Some(n.id.to_string())
            }
            Expr::Constant(c) => match &c.value {
                ast::Constant::Str(s) if model_names.iter().any(|m| m == s.as_str()) => {
                    Some(s.clone())
                }
                _ => None,
            },
            _ => None,
        }
    };
    let list_model_of = |e: &Expr| -> Option<String> {
        if let Expr::Subscript(s) = e {
            if matches!(&*s.value, Expr::Name(n) if matches!(n.id.as_str(), "list" | "List")) {
                return model_of(&s.slice);
            }
        }
        None
    };
    let no_models: HashMap<String, Vec<Field>> = HashMap::new();
    let mut src: Vec<String> = Vec::new();
    if routes.iter().any(|r| r.is_async) {
        src.push("import asyncio".into());
    }
    src.push("def __py_fastapi__routes(method: str, url: str, body: Optional[str], json_body: bool) -> fastapi.Response:".into());
    src.push("    parts = fastapi._path(url)".into());
    src.push("    query = fastapi._query(url)".into());
    src.push("    allowed = False".into());
    for (k, r) in routes.iter().enumerate() {
        let mut test = vec![format!("len(parts) == {}", r.segments.len())];
        let mut path_params: HashMap<String, usize> = HashMap::new();
        for (i, seg) in r.segments.iter().enumerate() {
            if let Some(p) = seg.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                path_params.insert(p.to_string(), i);
            } else {
                test.push(format!("parts[{i}] == {}", python_str(seg)));
            }
        }
        for p in path_params.keys() {
            if !r.params.iter().any(|(n, _, _)| n == p) {
                return Err(refuse(format!(
                    "the route handler '{}' has no parameter for the path parameter '{p}'",
                    r.handler
                )));
            }
        }
        src.push(format!("    if {}:", test.join(" and ")));
        src.push("        allowed = True".into());
        src.push(format!("        if method == \"{}\":", r.verb));
        let pad = "            ";
        src.push(format!("{pad}errs: list[pydantic._Err] = []"));
        let mut call_args = Vec::new();
        let mut n = Names(1000 * (k + 1));
        let mut has_body = false;
        for (pname, ann, default) in &r.params {
            let var = format!("p{k}_{pname}");
            call_args.push(var.clone());
            let model = ann.as_ref().and_then(model_of);
            if let Some(m) = model {
                if path_params.contains_key(pname) {
                    return Err(refuse(format!(
                        "the path parameter '{pname}' of '{}' is annotated with a model",
                        r.handler
                    )));
                }
                if has_body {
                    return Err(refuse(format!("the route handler '{}' takes more than one body model, which is not supported", r.handler)));
                }
                has_body = true;
                let ty = FType::Model(m.clone());
                src.push(format!("{pad}e{k} = len(errs)"));
                src.push(format!("{pad}b{k} = fastapi._body(body, json_body, errs)"));
                src.push(format!("{pad}{var}: {m} = {}", ty.zero()));
                src.push(format!("{pad}if len(errs) == e{k}:"));
                src.push(format!("{pad}    pydantic._Mode.attributes = True"));
                src.push(format!(
                    "{pad}    {var} = {VALIDATE_PREFIX}{m}(b{k}, [\"body\"], errs)"
                ));
                src.push(format!("{pad}    pydantic._Mode.attributes = False"));
                continue;
            }
            let ty = match ann {
                Some(a) => field_type(a, &no_models, "")
                    .ok()
                    .filter(|t| match t {
                        FType::Str | FType::Int | FType::Float | FType::Bool => true,
                        FType::Optional(x) => {
                            matches!(**x, FType::Str | FType::Int | FType::Float | FType::Bool)
                        }
                        _ => false,
                    })
                    .ok_or_else(|| {
                        refuse(format!(
                            "the parameter '{pname}' of the route handler '{}' has a type a \
                             request parameter is not read as here: str, int, float, bool, a \
                             model (the JSON body), or Optional[...] of a scalar",
                            r.handler
                        ))
                    })?,
                None => FType::Str,
            };
            if let Some(&i) = path_params.get(pname) {
                convert(
                    &mut src,
                    pad,
                    &var,
                    &format!("fastapi._param(parts[{i}])"),
                    &format!("[\"path\", {}]", python_str(pname)),
                    &ty,
                    &mut n,
                );
                // `convert` assigns; declare it first.
                let at = src.len() - 1;
                src.insert(at, format!("{pad}{var}: {} = {}", ty.ann(), ty.zero()));
                continue;
            }
            // A query parameter: its default when absent, else required.
            match default {
                Some(_) => {
                    let placeholder = format!("{DEFAULT_PREFIX}r{k}_{pname}");
                    src.push(format!("{pad}{var}: {} = {placeholder}", ty.ann()));
                }
                None => src.push(format!("{pad}{var}: {} = {}", ty.ann(), ty.zero())),
            }
            src.push(format!("{pad}if {} in query:", python_str(pname)));
            convert(
                &mut src,
                &format!("{pad}    "),
                &var,
                &format!("fastapi._param(query[{}])", python_str(pname)),
                &format!("[\"query\", {}]", python_str(pname)),
                &ty,
                &mut n,
            );
            if default.is_none() {
                src.push(format!("{pad}else:"));
                src.push(format!(
                    "{pad}    errs.append(pydantic._missing([\"query\", {}], None))",
                    python_str(pname)
                ));
            }
        }
        src.push(format!("{pad}if len(errs) > 0:"));
        src.push(format!("{pad}    return fastapi._invalid(errs)"));
        let call = format!("{}({})", r.handler, call_args.join(", "));
        let call = if r.is_async {
            format!("asyncio.run({call})")
        } else {
            call
        };
        let none_result = matches!(&r.returns, Some(Expr::Constant(c)) if matches!(c.value, ast::Constant::None))
            || (r.returns.is_none() && !r.returns_value);
        src.push(format!("{pad}try:"));
        if none_result {
            src.push(format!("{pad}    {call}"));
        } else {
            src.push(format!("{pad}    r{k} = {call}"));
        }
        src.push(format!("{pad}except fastapi.HTTPException as x{k}:"));
        src.push(format!(
            "{pad}    return fastapi._error(x{k}.status_code, x{k}.detail)"
        ));
        let shape = r.response_model.as_ref().or(r.returns.as_ref());
        let text = if none_result {
            "\"null\"".to_string()
        } else if let Some(m) = shape.and_then(model_of) {
            let _ = m;
            format!("r{k}.model_dump_json()")
        } else if shape.and_then(list_model_of).is_some() {
            format!("\"[\" + \",\".join([x.model_dump_json() for x in r{k}]) + \"]\"")
        } else {
            format!("json.dumps(r{k}, ensure_ascii=False, separators=(\",\", \":\"))")
        };
        src.push(format!(
            "{pad}return fastapi.Response({}, {text})",
            r.status
        ));
    }
    src.push("    if allowed:".into());
    src.push("        return fastapi._error(405, \"Method Not Allowed\")".into());
    src.push("    return fastapi._error(404, \"Not Found\")".into());
    src.push(String::new());
    let source = src.join("\n");
    let parsed = ast::Suite::parse(&source, "<fastapi routes>").map_err(|e| {
        anyhow!("the application's routes were written as invalid source: {e}\n{source}")
    })?;
    // Each query parameter's default, from the handler.
    let mut defaults: HashMap<String, Expr> = HashMap::new();
    for (k, r) in routes.iter().enumerate() {
        for (pname, _, default) in &r.params {
            if let Some(d) = default {
                defaults.insert(format!("{DEFAULT_PREFIX}r{k}_{pname}"), d.clone());
            }
        }
    }
    let mut fill = NamedDefaults(&defaults);
    for stmt in parsed {
        match ast::fold::Fold::fold_stmt(&mut fill, stmt) {
            Ok(s) => body.push(s),
            Err(never) => match never {},
        }
    }
    Ok(())
}

/// A str literal's source.
fn python_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Replace each placeholder name with the expression it stands for.
struct NamedDefaults<'a>(&'a HashMap<String, Expr>);

impl ast::fold::Fold<ast::text_size::TextRange> for NamedDefaults<'_> {
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
        if let Expr::Name(n) = &expr {
            if let Some(value) = self.0.get(n.id.as_str()) {
                return Ok(value.clone());
            }
        }
        ast::fold::fold_expr(self, expr)
    }
}
