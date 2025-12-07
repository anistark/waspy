use crate::stdlib::StdlibValue;

pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        "IGNORECASE" | "I" => Some(StdlibValue::Int(2)),
        "MULTILINE" | "M" => Some(StdlibValue::Int(8)),
        "DOTALL" | "S" => Some(StdlibValue::Int(16)),
        "VERBOSE" | "X" => Some(StdlibValue::Int(64)),
        "ASCII" | "A" => Some(StdlibValue::Int(256)),
        _ => None,
    }
}

pub fn get_function(func: &str) -> Option<ReFunction> {
    match func {
        "compile" => Some(ReFunction::Compile),
        "search" => Some(ReFunction::Search),
        "match" => Some(ReFunction::Match),
        "fullmatch" => Some(ReFunction::Fullmatch),
        "findall" => Some(ReFunction::Findall),
        "finditer" => Some(ReFunction::Finditer),
        "split" => Some(ReFunction::Split),
        "sub" => Some(ReFunction::Sub),
        "subn" => Some(ReFunction::Subn),
        "escape" => Some(ReFunction::Escape),
        "purge" => Some(ReFunction::Purge),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum ReFunction {
    Compile,
    Search,
    Match,
    Fullmatch,
    Findall,
    Finditer,
    Split,
    Sub,
    Subn,
    Escape,
    Purge,
}
