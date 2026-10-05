use crate::stdlib::StdlibValue;

/// `sys` constants. `maxsize` is the largest `int` under the documented 32-bit
/// integer model. `argv`, `platform`, `version`, the standard streams, and
/// `path` describe the host and are refused until the host interface exists:
/// they answered an empty list, "wasm32", a made-up version, and None.
pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        "maxsize" => Some(StdlibValue::Int(i32::MAX)),
        _ => None,
    }
}
