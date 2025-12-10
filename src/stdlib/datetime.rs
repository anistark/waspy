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

/// Add timedelta to datetime (compile-time helper)
/// timedelta is (days, seconds, microseconds)
/// Returns new datetime tuple (year, month, day, hour, minute, second, microsecond)
#[allow(clippy::too_many_arguments)]
pub fn datetime_add_timedelta(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    microsecond: u32,
    td_days: i32,
    td_seconds: i32,
    td_microseconds: i32,
) -> Option<(i32, u32, u32, u32, u32, u32, u32)> {
    use chrono::Duration;

    let dt = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_micro_opt(
        hour,
        minute,
        second,
        microsecond,
    )?;

    let duration = Duration::days(td_days as i64)
        + Duration::seconds(td_seconds as i64)
        + Duration::microseconds(td_microseconds as i64);

    let new_dt = dt.checked_add_signed(duration)?;

    Some((
        new_dt.year(),
        new_dt.month(),
        new_dt.day(),
        new_dt.hour(),
        new_dt.minute(),
        new_dt.second(),
        new_dt.nanosecond() / 1000, // Convert nanoseconds to microseconds
    ))
}

/// Subtract timedelta from datetime (compile-time helper)
#[allow(clippy::too_many_arguments)]
pub fn datetime_sub_timedelta(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    microsecond: u32,
    td_days: i32,
    td_seconds: i32,
    td_microseconds: i32,
) -> Option<(i32, u32, u32, u32, u32, u32, u32)> {
    datetime_add_timedelta(
        year,
        month,
        day,
        hour,
        minute,
        second,
        microsecond,
        -td_days,
        -td_seconds,
        -td_microseconds,
    )
}

/// Subtract two datetimes to get timedelta (compile-time helper)
/// Returns (days, seconds, microseconds)
#[allow(clippy::too_many_arguments)]
pub fn datetime_diff(
    year1: i32,
    month1: u32,
    day1: u32,
    hour1: u32,
    minute1: u32,
    second1: u32,
    microsecond1: u32,
    year2: i32,
    month2: u32,
    day2: u32,
    hour2: u32,
    minute2: u32,
    second2: u32,
    microsecond2: u32,
) -> Option<(i32, i32, i32)> {
    let dt1 = NaiveDate::from_ymd_opt(year1, month1, day1)?.and_hms_micro_opt(
        hour1,
        minute1,
        second1,
        microsecond1,
    )?;
    let dt2 = NaiveDate::from_ymd_opt(year2, month2, day2)?.and_hms_micro_opt(
        hour2,
        minute2,
        second2,
        microsecond2,
    )?;

    let duration = dt1.signed_duration_since(dt2);

    let total_seconds = duration.num_seconds();
    let days = (total_seconds / 86400) as i32;
    let remaining_seconds = (total_seconds % 86400) as i32;
    let microseconds = (duration.num_microseconds().unwrap_or(0) % 1_000_000) as i32;

    Some((days, remaining_seconds, microseconds))
}

/// Add timedelta to date (compile-time helper)
/// Returns new date tuple (year, month, day)
pub fn date_add_timedelta(
    year: i32,
    month: u32,
    day: u32,
    td_days: i32,
) -> Option<(i32, u32, u32)> {
    use chrono::Duration;

    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let new_date = date.checked_add_signed(Duration::days(td_days as i64))?;

    Some((new_date.year(), new_date.month(), new_date.day()))
}

/// Subtract two dates to get timedelta days (compile-time helper)
pub fn date_diff(
    year1: i32,
    month1: u32,
    day1: u32,
    year2: i32,
    month2: u32,
    day2: u32,
) -> Option<i32> {
    let date1 = NaiveDate::from_ymd_opt(year1, month1, day1)?;
    let date2 = NaiveDate::from_ymd_opt(year2, month2, day2)?;

    Some(date1.signed_duration_since(date2).num_days() as i32)
}

/// Get weekday from date (0=Monday, 6=Sunday)
pub fn date_weekday(year: i32, month: u32, day: u32) -> Option<u32> {
    NaiveDate::from_ymd_opt(year, month, day).map(|d| d.weekday().num_days_from_monday())
}

/// Get ISO weekday from date (1=Monday, 7=Sunday)
pub fn date_isoweekday(year: i32, month: u32, day: u32) -> Option<u32> {
    NaiveDate::from_ymd_opt(year, month, day).map(|d| d.weekday().number_from_monday())
}

/// Format date to ISO string (YYYY-MM-DD)
pub fn date_to_iso(year: i32, month: u32, day: u32) -> Option<String> {
    NaiveDate::from_ymd_opt(year, month, day).map(|d| d.format("%Y-%m-%d").to_string())
}

/// Format time to ISO string (HH:MM:SS or HH:MM:SS.ffffff)
pub fn time_to_iso(hour: u32, minute: u32, second: u32, microsecond: u32) -> String {
    if microsecond == 0 {
        format!("{hour:02}:{minute:02}:{second:02}")
    } else {
        format!("{hour:02}:{minute:02}:{second:02}.{microsecond:06}")
    }
}

/// Parse date from ISO string (YYYY-MM-DD)
pub fn date_from_iso(iso_str: &str) -> Result<(i32, u32, u32), String> {
    NaiveDate::parse_from_str(iso_str, "%Y-%m-%d")
        .map(|d| (d.year(), d.month(), d.day()))
        .map_err(|e| format!("Failed to parse date: {e}"))
}

/// Parse time from ISO string (HH:MM:SS or HH:MM:SS.ffffff)
pub fn time_from_iso(iso_str: &str) -> Result<(u32, u32, u32, u32), String> {
    // Try with microseconds first
    if let Ok(t) = chrono::NaiveTime::parse_from_str(iso_str, "%H:%M:%S%.f") {
        return Ok((t.hour(), t.minute(), t.second(), t.nanosecond() / 1000));
    }
    // Try without microseconds
    chrono::NaiveTime::parse_from_str(iso_str, "%H:%M:%S")
        .map(|t| (t.hour(), t.minute(), t.second(), 0))
        .map_err(|e| format!("Failed to parse time: {e}"))
}
