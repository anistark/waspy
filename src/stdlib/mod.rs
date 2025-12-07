pub mod sys;

pub fn is_stdlib_module(name: &str) -> bool {
    matches!(
        name,
        "sys"
            | "os"
            | "math"
            | "random"
            | "json"
            | "re"
            | "datetime"
            | "collections"
            | "itertools"
            | "functools"
    )
}

pub fn get_stdlib_attributes(module: &str, attr: &str) -> Option<StdlibValue> {
    match module {
        "sys" => sys::get_attribute(attr),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum StdlibValue {
    Int(i32),
    String(String),
    List(Vec<String>),
    Float(f64),
    None,
}
