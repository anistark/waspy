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
