use crate::ir::IRFunction;
use anyhow::Result;
use std::collections::HashMap;

/// How a method binds its first argument, derived from the standard method
/// decorators. Plain (undecorated) methods bind `self`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    /// Ordinary method: implicit `self` instance pointer.
    Instance,
    /// `@staticmethod`: no implicit first argument.
    Static,
    /// `@classmethod`: implicit `cls`, bound to the class itself.
    Class,
    /// `@property`: `obj.attr` reads compile to a call of this method.
    PropertyGetter,
    /// `@<name>.setter`: `obj.attr = v` assignments compile to a call.
    PropertySetter,
}

/// Classify a method by its decorator stack, rejecting unsupported or
/// conflicting combinations (e.g. `@staticmethod` + `@classmethod`) with a
/// clear error instead of silently compiling the wrong dispatch.
///
/// Decorators that don't affect binding but are otherwise harmless
/// (`@abstractmethod`, the caching decorators) are ignored here. Any other
/// decorator is an error: one the compiler does not implement used to be
/// dropped silently, so `@cached_property` read as a plain method whose
/// attribute access answered 0 (#120).
pub fn method_kind(
    class_name: &str,
    method_name: &str,
    decorators: &[String],
) -> Result<MethodKind> {
    let mut kind: Option<(MethodKind, &str)> = None;

    for dec in decorators {
        let new_kind = match dec.as_str() {
            "staticmethod" => Some(MethodKind::Static),
            "classmethod" => Some(MethodKind::Class),
            "property" => Some(MethodKind::PropertyGetter),
            d if d.ends_with(".setter") || d.ends_with(".getter") => {
                let (prop, suffix) = d.rsplit_once('.').expect("checked by ends_with");
                if prop != method_name {
                    return Err(crate::core::errors::unsupported_feature(
                        format!(
                            "decorator '@{d}' on method '{class_name}.{method_name}' must match \
                             the property name (expected '@{method_name}.{suffix}')"
                        ),
                        None,
                    )
                    .into());
                }
                Some(if suffix == "setter" {
                    MethodKind::PropertySetter
                } else {
                    MethodKind::PropertyGetter
                })
            }
            d if d.ends_with(".deleter") => {
                return Err(crate::core::errors::unsupported_feature(
                    format!(
                        "property deleters are not supported: '@{d}' on method \
                         '{class_name}.{method_name}'"
                    ),
                    None,
                )
                .into());
            }
            "abstractmethod" | "abc.abstractmethod" => None,
            d if matches!(
                d.strip_prefix("functools.").unwrap_or(d),
                "lru_cache" | "lru_cache(...)" | "cache" | "wraps(...)"
            ) =>
            {
                None
            }
            d => {
                let hint = match d.strip_prefix("functools.").unwrap_or(d) {
                    "cached_property" => "; use '@property' (the value is recomputed on each read)",
                    "singledispatchmethod" => "; dispatch on the argument in the method body",
                    _ => "",
                };
                return Err(crate::core::errors::unsupported_feature(
                    format!(
                        "decorator '@{d}' on method '{class_name}.{method_name}' is not \
                         supported{hint}"
                    ),
                    None,
                )
                .into());
            }
        };

        if let Some(new_kind) = new_kind {
            if let Some((prev_kind, prev_dec)) = kind {
                if prev_kind != new_kind {
                    return Err(crate::core::errors::unsupported_feature(
                        format!(
                            "method '{class_name}.{method_name}' combines decorators '@{prev_dec}' \
                             and '@{dec}'; only one of @staticmethod, @classmethod, @property, or \
                             @<name>.setter is supported per method"
                        ),
                        None,
                    )
                    .into());
                }
            }
            kind = Some((new_kind, dec.as_str()));
        }
    }

    Ok(kind.map(|(k, _)| k).unwrap_or(MethodKind::Instance))
}

/// Applies decorators a library user registers by name to a compiled
/// function. The compiler registers none: every decorator it implements is
/// lowered during conversion, and one it does not is refused there.
///
/// It used to carry built-in `@memoize`, `@debug`, `@timer`, `@default_value`,
/// `@type_check`, and `@pure`. None of them is defined in Python, where each
/// raises `NameError`, and their implementations were placeholders (a timer
/// that always read 0), so they were removed rather than kept under names
/// CPython rejects.
pub struct DecoratorRegistry {
    /// Map of custom decorator names to their implementations
    custom_decorators: HashMap<String, Box<dyn Fn(IRFunction) -> IRFunction>>,
}

impl Default for DecoratorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DecoratorRegistry {
    pub fn new() -> Self {
        DecoratorRegistry {
            custom_decorators: HashMap::new(),
        }
    }

    /// Register a custom decorator
    pub fn register<F>(&mut self, name: &str, decorator: F)
    where
        F: Fn(IRFunction) -> IRFunction + 'static,
    {
        self.custom_decorators
            .insert(name.to_string(), Box::new(decorator));
    }

    /// Apply the registered decorators named on `func`, innermost first. A
    /// name nothing is registered for leaves the function unchanged; the
    /// converter has already accepted only decorators with no observable
    /// effect.
    pub fn apply_decorators(&self, mut func: IRFunction) -> IRFunction {
        let decorator_names: Vec<String> = func.decorators.clone();
        for decorator_name in decorator_names.iter().rev() {
            if let Some(decorator) = self.custom_decorators.get(decorator_name) {
                func = decorator(func);
            }
        }
        func.decorators.clear();
        func
    }
}
