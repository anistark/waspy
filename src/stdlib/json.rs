use crate::stdlib::StdlibValue;

pub fn get_attribute(_attr: &str) -> Option<StdlibValue> {
    None
}

pub fn get_function(func: &str) -> Option<JsonFunction> {
    match func {
        "loads" => Some(JsonFunction::Loads),
        "dumps" => Some(JsonFunction::Dumps),
        "load" => Some(JsonFunction::Load),
        "dump" => Some(JsonFunction::Dump),
        "JSONEncoder" => Some(JsonFunction::JSONEncoder),
        "JSONDecoder" => Some(JsonFunction::JSONDecoder),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum JsonFunction {
    Loads,
    Dumps,
    Load,
    Dump,
    JSONEncoder,
    JSONDecoder,
}

/// Parse a JSON string at compile time (for constant strings)
pub fn parse_json_string(json_str: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(json_str).map_err(|e| format!("JSON parse error: {e}"))
}

/// Serialize a value to JSON at compile time
pub fn serialize_to_json(value: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string(value).map_err(|e| format!("JSON serialize error: {e}"))
}

/// Serialize a value to pretty JSON at compile time
pub fn serialize_to_json_pretty(value: &serde_json::Value) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|e| format!("JSON serialize error: {e}"))
}
