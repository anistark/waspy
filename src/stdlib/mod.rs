pub mod collections;
pub mod datetime;
pub mod functools;
pub mod itertools;
pub mod json;
pub mod math;
pub mod os;
pub mod random;
pub mod re;
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
        "os" => os::get_attribute(attr),
        "math" => math::get_attribute(attr),
        "random" => random::get_attribute(attr),
        "json" => json::get_attribute(attr),
        "re" => re::get_attribute(attr),
        "datetime" => datetime::get_attribute(attr),
        "collections" => collections::get_attribute(attr),
        "itertools" => itertools::get_attribute(attr),
        "functools" => functools::get_attribute(attr),
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
