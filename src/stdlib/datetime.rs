use crate::stdlib::StdlibValue;

pub fn get_attribute(attr: &str) -> Option<StdlibValue> {
    match attr {
        "MINYEAR" => Some(StdlibValue::Int(1)),
        "MAXYEAR" => Some(StdlibValue::Int(9999)),
        _ => None,
    }
}

pub fn get_function(func: &str) -> Option<DatetimeFunction> {
    match func {
        "datetime" => Some(DatetimeFunction::Datetime),
        "date" => Some(DatetimeFunction::Date),
        "time" => Some(DatetimeFunction::Time),
        "timedelta" => Some(DatetimeFunction::Timedelta),
        "timezone" => Some(DatetimeFunction::Timezone),
        "tzinfo" => Some(DatetimeFunction::Tzinfo),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum DatetimeFunction {
    Datetime,
    Date,
    Time,
    Timedelta,
    Timezone,
    Tzinfo,
}

pub fn get_datetime_method(method: &str) -> Option<DatetimeMethod> {
    match method {
        "now" => Some(DatetimeMethod::Now),
        "today" => Some(DatetimeMethod::Today),
        "fromtimestamp" => Some(DatetimeMethod::Fromtimestamp),
        "fromisoformat" => Some(DatetimeMethod::Fromisoformat),
        "strftime" => Some(DatetimeMethod::Strftime),
        "strptime" => Some(DatetimeMethod::Strptime),
        "replace" => Some(DatetimeMethod::Replace),
        "timestamp" => Some(DatetimeMethod::Timestamp),
        "isoformat" => Some(DatetimeMethod::Isoformat),
        "weekday" => Some(DatetimeMethod::Weekday),
        "isoweekday" => Some(DatetimeMethod::Isoweekday),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum DatetimeMethod {
    Now,
    Today,
    Fromtimestamp,
    Fromisoformat,
    Strftime,
    Strptime,
    Replace,
    Timestamp,
    Isoformat,
    Weekday,
    Isoweekday,
}
