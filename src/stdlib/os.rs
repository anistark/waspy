use crate::stdlib::StdlibValue;

pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        "name" => Some(StdlibValue::String("wasm".to_string())),
        "sep" => Some(StdlibValue::String("/".to_string())),
        "pathsep" => Some(StdlibValue::String(":".to_string())),
        "linesep" => Some(StdlibValue::String("\n".to_string())),
        "devnull" => Some(StdlibValue::String("/dev/null".to_string())),
        "curdir" => Some(StdlibValue::String(".".to_string())),
        "pardir" => Some(StdlibValue::String("..".to_string())),
        "extsep" => Some(StdlibValue::String(".".to_string())),
        "environ" => Some(StdlibValue::Dict(vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("HOME".to_string(), "/".to_string()),
            ("USER".to_string(), "wasm".to_string()),
        ])),
        _ => None,
    }
}

pub fn get_function(func: &str) -> Option<OsFunction> {
    match func {
        "getcwd" => Some(OsFunction::Getcwd),
        "getenv" => Some(OsFunction::Getenv),
        "getpid" => Some(OsFunction::Getpid),
        "urandom" => Some(OsFunction::Urandom),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum OsFunction {
    Getcwd,
    Getenv,
    Getpid,
    Urandom,
}

pub mod path {
    use crate::stdlib::StdlibValue;

    pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
        match attr {
            "sep" => Some(StdlibValue::String("/".to_string())),
            "pathsep" => Some(StdlibValue::String(":".to_string())),
            "curdir" => Some(StdlibValue::String(".".to_string())),
            "pardir" => Some(StdlibValue::String("..".to_string())),
            _ => None,
        }
    }

    pub fn get_function(func: &str) -> Option<PathFunction> {
        match func {
            "join" => Some(PathFunction::Join),
            "exists" => Some(PathFunction::Exists),
            "isfile" => Some(PathFunction::Isfile),
            "isdir" => Some(PathFunction::Isdir),
            "basename" => Some(PathFunction::Basename),
            "dirname" => Some(PathFunction::Dirname),
            "abspath" => Some(PathFunction::Abspath),
            "split" => Some(PathFunction::Split),
            "splitext" => Some(PathFunction::Splitext),
            _ => None,
        }
    }

    #[derive(Debug, Clone)]
    pub enum PathFunction {
        Join,
        Exists,
        Isfile,
        Isdir,
        Basename,
        Dirname,
        Abspath,
        Split,
        Splitext,
    }
}
