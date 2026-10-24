//! The names of the specialized `collections` classes.
//!
//! `Counter[str]` and `Counter[int]` hold different element types, so each
//! is a class of its own, named as the annotation that names it is written:
//! `Counter[str]`, `defaultdict[str, list[int]]`, `deque[tuple[int, str]]`.
//! The name is the specialization: [`parse`] reads the kind and the argument
//! types back from it, so a class annotated anywhere in the module is the
//! same class as one a constructor call makes.

use crate::ir::IRType;

/// A `collections` class that is specialized by element types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Counter,
    DefaultDict,
    OrderedDict,
    Deque,
    ChainMap,
}

impl Kind {
    /// The kind a name spells, in `collections` or as `typing`'s alias.
    pub(crate) fn named(name: &str) -> Option<Kind> {
        match name {
            "Counter" => Some(Kind::Counter),
            "defaultdict" | "DefaultDict" => Some(Kind::DefaultDict),
            "OrderedDict" => Some(Kind::OrderedDict),
            "deque" | "Deque" => Some(Kind::Deque),
            "ChainMap" => Some(Kind::ChainMap),
            _ => None,
        }
    }

    /// The class's name in `collections`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Counter => "Counter",
            Kind::DefaultDict => "defaultdict",
            Kind::OrderedDict => "OrderedDict",
            Kind::Deque => "deque",
            Kind::ChainMap => "ChainMap",
        }
    }

    /// How many type arguments it takes: the element, or the key and value.
    pub(crate) fn arity(self) -> usize {
        match self {
            Kind::Counter | Kind::Deque => 1,
            _ => 2,
        }
    }
}

/// A type as an annotation writes it, or `None` for one no annotation names
/// (an unknown type, a function, a generator).
pub(crate) fn encode(ty: &IRType) -> Option<String> {
    Some(match ty {
        IRType::Int => "int".to_string(),
        IRType::Float => "float".to_string(),
        IRType::Bool => "bool".to_string(),
        IRType::String => "str".to_string(),
        IRType::Bytes => "bytes".to_string(),
        IRType::None => "None".to_string(),
        IRType::Range => "range".to_string(),
        IRType::List(e) => format!("list[{}]", encode(e)?),
        IRType::Set(e) => format!("set[{}]", encode(e)?),
        IRType::Dict(k, v) => format!("dict[{}, {}]", encode(k)?, encode(v)?),
        IRType::VarTuple(e) => format!("tuple[{}, ...]", encode(e)?),
        IRType::Tuple(ms) if ms.is_empty() => "tuple[()]".to_string(),
        IRType::Tuple(ms) => format!(
            "tuple[{}]",
            ms.iter()
                .map(encode)
                .collect::<Option<Vec<_>>>()?
                .join(", ")
        ),
        IRType::Optional(e) => format!("Optional[{}]", encode(e)?),
        IRType::Class(c) => c.clone(),
        _ => return None,
    })
}

/// The name of the variant of method `base` that takes an argument of `ty`
/// (`update<list[str]>`).
pub(crate) fn variant(base: &str, ty: &IRType) -> Option<String> {
    Some(format!("{base}<{}>", encode(ty)?))
}

/// The class name of `kind` specialized by `args`.
pub(crate) fn name(kind: Kind, args: &[IRType]) -> Option<String> {
    Some(format!(
        "{}[{}]",
        kind.name(),
        args.iter()
            .map(encode)
            .collect::<Option<Vec<_>>>()?
            .join(", ")
    ))
}

/// The type an annotation-style spelling names (the inverse of [`encode`]).
pub(crate) fn decode(text: &str) -> Option<IRType> {
    let mut p = Parser { s: text, at: 0 };
    let ty = p.ty()?;
    (p.at == text.len()).then_some(ty)
}

/// The `collections` class a specialized class is (a defaultdict's
/// factory variant, `defaultdict[str, int]<f>`, included), for its name.
pub(crate) fn kind_of_class(class: &str) -> Option<Kind> {
    let base = match class.rfind(">") {
        Some(end) if end + 1 == class.len() && class.ends_with('>') && !class.ends_with("]") => {
            class.rfind("]<").map(|at| &class[..=at]).unwrap_or(class)
        }
        _ => class,
    };
    parse(base).map(|(kind, _)| kind)
}

/// The kind and type arguments of a specialized class's name.
pub(crate) fn parse(class: &str) -> Option<(Kind, Vec<IRType>)> {
    let open = class.find('[')?;
    let kind = Kind::named(&class[..open])?;
    let mut p = Parser {
        s: class,
        at: open + 1,
    };
    let args = p.list(']')?;
    (p.at == class.len() && args.len() == kind.arity()).then_some((kind, args))
}

struct Parser<'a> {
    s: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn rest(&self) -> &str {
        &self.s[self.at..]
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.rest().starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    /// Types separated by `, ` up to `close`, which is consumed.
    fn list(&mut self, close: char) -> Option<Vec<IRType>> {
        let mut out = Vec::new();
        loop {
            out.push(self.ty()?);
            if self.eat(", ") {
                continue;
            }
            if self.rest().starts_with(close) {
                self.at += close.len_utf8();
                return Some(out);
            }
            return None;
        }
    }

    fn ty(&mut self) -> Option<IRType> {
        let start = self.at;
        let len = self
            .rest()
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(self.rest().len());
        let word = self.s[start..start + len].to_string();
        self.at += len;
        if word.is_empty() {
            return None;
        }
        if !self.eat("[") {
            return Some(match word.as_str() {
                "int" => IRType::Int,
                "float" => IRType::Float,
                "bool" => IRType::Bool,
                "str" => IRType::String,
                "bytes" => IRType::Bytes,
                "None" => IRType::None,
                "range" => IRType::Range,
                _ => IRType::Class(word),
            });
        }
        if word == "tuple" && self.eat("()]") {
            return Some(IRType::Tuple(Vec::new()));
        }
        let args = self.list_or_ellipsis()?;
        let one = |args: &[IRType]| match args {
            [t] => Some(Box::new(t.clone())),
            _ => None,
        };
        Some(match (word.as_str(), args) {
            ("list", (a, false)) => IRType::List(one(&a)?),
            ("set", (a, false)) => IRType::Set(one(&a)?),
            ("Optional", (a, false)) => IRType::Optional(one(&a)?),
            ("dict", (a, false)) if a.len() == 2 => {
                IRType::Dict(Box::new(a[0].clone()), Box::new(a[1].clone()))
            }
            ("tuple", (a, true)) => IRType::VarTuple(one(&a)?),
            ("tuple", (a, false)) => IRType::Tuple(a),
            // A specialized class inside another (`defaultdict[str,
            // Counter[str]]`): its own name, spelled back.
            (kind, (a, false)) if Kind::named(kind).is_some() => {
                IRType::Class(name(Kind::named(kind)?, &a)?)
            }
            _ => return None,
        })
    }

    /// Like [`Parser::list`] up to `]`, also taking a trailing `, ...`.
    fn list_or_ellipsis(&mut self) -> Option<(Vec<IRType>, bool)> {
        let mut out = Vec::new();
        loop {
            out.push(self.ty()?);
            if self.eat(", ...]") {
                return Some((out, true));
            }
            if self.eat(", ") {
                continue;
            }
            if self.eat("]") {
                return Some((out, false));
            }
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_read_back() {
        let cases = [
            (Kind::Counter, vec![IRType::String]),
            (
                Kind::DefaultDict,
                vec![IRType::String, IRType::List(Box::new(IRType::Int))],
            ),
            (
                Kind::Deque,
                vec![IRType::Tuple(vec![IRType::Int, IRType::String])],
            ),
            (
                Kind::DefaultDict,
                vec![
                    IRType::String,
                    IRType::Class(name(Kind::Counter, &[IRType::String]).unwrap()),
                ],
            ),
            (
                Kind::OrderedDict,
                vec![IRType::VarTuple(Box::new(IRType::Float)), IRType::Bool],
            ),
        ];
        for (kind, args) in cases {
            let n = name(kind, &args).unwrap();
            assert_eq!(parse(&n), Some((kind, args)), "{n}");
        }
        assert_eq!(parse("Counter"), None);
        assert_eq!(parse("Point"), None);
    }
}
