use crate::stdlib::StdlibValue;

/// Log levels as defined in Python's logging module
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i32)]
pub enum LogLevel {
    NotSet = 0,
    Debug = 10,
    Info = 20,
    Warning = 30,
    Error = 40,
    Critical = 50,
}

impl LogLevel {
    pub fn from_i32(value: i32) -> Self {
        match value {
            0 => LogLevel::NotSet,
            10 => LogLevel::Debug,
            20 => LogLevel::Info,
            30 => LogLevel::Warning,
            40 => LogLevel::Error,
            50 => LogLevel::Critical,
            _ if value < 10 => LogLevel::NotSet,
            _ if value < 20 => LogLevel::Debug,
            _ if value < 30 => LogLevel::Info,
            _ if value < 40 => LogLevel::Warning,
            _ if value < 50 => LogLevel::Error,
            _ => LogLevel::Critical,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            LogLevel::NotSet => "NOTSET",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warning => "WARNING",
            LogLevel::Error => "ERROR",
            LogLevel::Critical => "CRITICAL",
        }
    }
}

pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        // Log level constants
        "NOTSET" => Some(StdlibValue::Int(LogLevel::NotSet as i32)),
        "DEBUG" => Some(StdlibValue::Int(LogLevel::Debug as i32)),
        "INFO" => Some(StdlibValue::Int(LogLevel::Info as i32)),
        "WARNING" => Some(StdlibValue::Int(LogLevel::Warning as i32)),
        "WARN" => Some(StdlibValue::Int(LogLevel::Warning as i32)), // Alias
        "ERROR" => Some(StdlibValue::Int(LogLevel::Error as i32)),
        "CRITICAL" => Some(StdlibValue::Int(LogLevel::Critical as i32)),
        "FATAL" => Some(StdlibValue::Int(LogLevel::Critical as i32)), // Alias
        _ => None,
    }
}

pub fn get_function(func: &str) -> Option<LoggingFunction> {
    match func {
        // Logging functions
        "debug" => Some(LoggingFunction::Debug),
        "info" => Some(LoggingFunction::Info),
        "warning" => Some(LoggingFunction::Warning),
        "warn" => Some(LoggingFunction::Warning), // Alias
        "error" => Some(LoggingFunction::Error),
        "critical" => Some(LoggingFunction::Critical),
        "fatal" => Some(LoggingFunction::Critical), // Alias
        "log" => Some(LoggingFunction::Log),
        "exception" => Some(LoggingFunction::Exception),

        // Configuration functions
        "basicConfig" => Some(LoggingFunction::BasicConfig),
        "getLogger" => Some(LoggingFunction::GetLogger),
        "setLevel" => Some(LoggingFunction::SetLevel),
        "disable" => Some(LoggingFunction::Disable),

        // Handler/Formatter functions
        "addHandler" => Some(LoggingFunction::AddHandler),
        "removeHandler" => Some(LoggingFunction::RemoveHandler),

        // Classes (used as constructors)
        "Logger" => Some(LoggingFunction::Logger),
        "Handler" => Some(LoggingFunction::Handler),
        "StreamHandler" => Some(LoggingFunction::StreamHandler),
        "FileHandler" => Some(LoggingFunction::FileHandler),
        "Formatter" => Some(LoggingFunction::Formatter),
        "Filter" => Some(LoggingFunction::Filter),
        "LogRecord" => Some(LoggingFunction::LogRecord),

        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum LoggingFunction {
    // Logging functions
    Debug,
    Info,
    Warning,
    Error,
    Critical,
    Log,
    Exception,

    // Configuration
    BasicConfig,
    GetLogger,
    SetLevel,
    Disable,

    // Handler management
    AddHandler,
    RemoveHandler,

    // Classes
    Logger,
    Handler,
    StreamHandler,
    FileHandler,
    Formatter,
    Filter,
    LogRecord,
}

impl LoggingFunction {
    pub fn log_level(&self) -> Option<LogLevel> {
        match self {
            LoggingFunction::Debug => Some(LogLevel::Debug),
            LoggingFunction::Info => Some(LogLevel::Info),
            LoggingFunction::Warning => Some(LogLevel::Warning),
            LoggingFunction::Error => Some(LogLevel::Error),
            LoggingFunction::Critical => Some(LogLevel::Critical),
            LoggingFunction::Exception => Some(LogLevel::Error),
            _ => None,
        }
    }
}
