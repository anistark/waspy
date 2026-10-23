use crate::stdlib::StdlibValue;

/// `sys` constants. `maxsize` is the largest `int` under the documented 32-bit
/// integer model; `platform` is what CPython on WASI reports, and `version`
/// names the CPython the subset follows and the compiler that built the
/// module. `argv`, `path`, and the standard streams are values codegen builds.
pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        "maxsize" => Some(StdlibValue::Int(i32::MAX)),
        "platform" => Some(StdlibValue::String("wasi".to_string())),
        "version" => Some(StdlibValue::String(version())),
        _ => None,
    }
}

/// `sys.version`: CPython's version first, as every reader of it expects.
pub fn version() -> String {
    format!("3.13.0 (waspy {})", env!("CARGO_PKG_VERSION"))
}
