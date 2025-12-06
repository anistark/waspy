use crate::stdlib::StdlibValue;

pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        "argv" => Some(StdlibValue::List(vec![])),
        "platform" => Some(StdlibValue::String("wasm32".to_string())),
        "version" => Some(StdlibValue::String("3.11.0 (waspy)".to_string())),
        "maxsize" => Some(StdlibValue::Int(i32::MAX)),
        "stdin" | "stdout" | "stderr" => Some(StdlibValue::None),
        "path" => Some(StdlibValue::List(vec![])),
        _ => None,
    }
}
