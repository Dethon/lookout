//! Converting Graph's dates, and showing them in the user's time zone.

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

/// Seconds since the epoch of an ISO 8601 instant such as
/// `2024-03-01T09:30:00Z`; 0 if it doesn't parse.
pub fn parse_instant(text: &str) -> i64 {
    DateTime::parse_from_rfc3339(text)
        .map(|time| time.timestamp())
        .unwrap_or(0)
}

/// Seconds since the epoch of a Graph `dateTimeTimeZone` in UTC, which
/// carries no offset, e.g. `2024-03-01T09:30:00.0000000`.
pub fn parse_utc(text: &str) -> i64 {
    let trimmed = text.trim_end_matches('Z');
    NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
        .map(|time| time.and_utc().timestamp())
        .unwrap_or(0)
}

/// Graph's form of a UTC instant for `dateTimeTimeZone` values.
pub fn format_utc(seconds: i64) -> String {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string()
}

pub fn local(seconds: i64) -> DateTime<Local> {
    Local.timestamp_opt(seconds, 0).single().unwrap_or_default()
}

/// How the message list shows when a message came: the time today, the
/// weekday this week, the date otherwise.
pub fn list_date(seconds: i64, now: DateTime<Local>) -> String {
    let time = local(seconds);
    let days = (now.date_naive() - time.date_naive()).num_days();
    if days == 0 {
        time.format("%H:%M").to_string()
    } else if (1..7).contains(&days) {
        time.format("%a %H:%M").to_string()
    } else if time.year() == now.year() {
        time.format("%a %d/%m").to_string()
    } else {
        time.format("%d/%m/%Y").to_string()
    }
}

/// The full date and time, for the reading pane.
pub fn long_date(seconds: i64) -> String {
    local(seconds).format("%a %d/%m/%Y %H:%M").to_string()
}

/// The time of day in the user's zone.
pub fn clock(seconds: i64) -> String {
    local(seconds).format("%H:%M").to_string()
}

/// Midnight UTC of a calendar date, as all-day events are kept.
pub fn date_seconds(date: NaiveDate) -> i64 {
    date.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc().timestamp()
}

/// The local date an all-day event's stored start falls on.
pub fn floating_date(seconds: i64) -> NaiveDate {
    Utc.timestamp_opt(seconds, 0).single().unwrap_or_default().date_naive()
}

/// Seconds of local midnight starting `date`.
pub fn local_midnight(date: NaiveDate) -> i64 {
    let naive = date.and_hms_opt(0, 0, 0).expect("midnight exists");
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|time| time.timestamp())
        // Midnight skipped by a clock change: the first moment of the day.
        .unwrap_or_else(|| naive.and_utc().timestamp())
}

/// The IANA name of the user's zone, for events created here.
pub fn zone_name() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instants_parse() {
        assert_eq!(parse_instant("1970-01-01T00:01:00Z"), 60);
        assert_eq!(parse_instant("garbage"), 0);
    }

    #[test]
    fn graph_utc_times_parse_with_fractions() {
        assert_eq!(parse_utc("1970-01-01T00:01:00.0000000"), 60);
        assert_eq!(parse_utc("1970-01-01T00:01:00"), 60);
        assert_eq!(format_utc(60), "1970-01-01T00:01:00");
    }
}
