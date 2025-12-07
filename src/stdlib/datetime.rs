use crate::stdlib::StdlibValue;
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, Timelike, Utc};

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

/// Get current UTC datetime as Unix timestamp
pub fn datetime_now_utc() -> i64 {
    Utc::now().timestamp()
}

/// Get current local datetime as Unix timestamp
pub fn datetime_now_local() -> i64 {
    Local::now().timestamp()
}

/// Get today's date as (year, month, day)
pub fn date_today() -> (i32, u32, u32) {
    let today = Local::now().date_naive();
    (today.year(), today.month(), today.day())
}

/// Create datetime from Unix timestamp
pub fn datetime_from_timestamp(timestamp: i64) -> Option<(i32, u32, u32, u32, u32, u32)> {
    DateTime::from_timestamp(timestamp, 0).map(|dt| {
        (
            dt.year(),
            dt.month(),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second(),
        )
    })
}

/// Parse ISO format datetime string (compile-time)
pub fn datetime_from_iso(iso_str: &str) -> Result<(i32, u32, u32, u32, u32, u32), String> {
    NaiveDateTime::parse_from_str(iso_str, "%Y-%m-%dT%H:%M:%S")
        .map(|dt| {
            (
                dt.year(),
                dt.month(),
                dt.day(),
                dt.hour(),
                dt.minute(),
                dt.second(),
            )
        })
        .map_err(|e| format!("Failed to parse datetime: {e}"))
}

/// Format datetime to ISO string (compile-time)
pub fn datetime_to_iso(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Result<String, String> {
    NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(hour, minute, second))
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
        .ok_or_else(|| "Invalid datetime values".to_string())
}

/// Format datetime with custom format string (compile-time)
pub fn datetime_strftime(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    format: &str,
) -> Result<String, String> {
    NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_opt(hour, minute, second))
        .map(|dt| dt.format(format).to_string())
        .ok_or_else(|| "Invalid datetime values".to_string())
}
