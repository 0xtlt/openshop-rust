//! Timestamps are serialized like JavaScript's `Date#toJSON`:
//! UTC, millisecond precision, `Z` suffix.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serializer;

/// A duration in whole milliseconds, as retry policies and worker settings are stored.
pub(crate) fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn to_iso(value: &DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Parses what JavaScript's `new Date(string)` accepts for ISO-like input:
/// RFC 3339 timestamps, `YYYY-MM-DDTHH:MM[:SS]` (local time is treated as UTC)
/// and bare dates.
pub fn parse_date(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, format) {
            return Some(parsed.and_utc());
        }
    }
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|datetime| datetime.and_utc())
}

pub mod iso {
    use super::*;

    pub fn serialize<S: Serializer>(value: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&to_iso(value))
    }
}

pub mod iso_option {
    use super::*;

    pub fn serialize<S: Serializer>(value: &Option<DateTime<Utc>>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => serializer.serialize_str(&to_iso(value)),
            None => serializer.serialize_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn formats_like_javascript_dates() {
        let date = Utc.with_ymd_and_hms(2026, 4, 1, 12, 30, 5).unwrap() + chrono::Duration::microseconds(123_456);
        assert_eq!(to_iso(&date), "2026-04-01T12:30:05.123Z");
    }

    #[test]
    fn parses_common_date_inputs() {
        let expected = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        assert_eq!(parse_date("2026-04-01"), Some(expected));
        assert_eq!(parse_date("2026-04-01T00:00:00Z"), Some(expected));
        assert_eq!(parse_date("2026-04-01T02:00:00+02:00"), Some(expected));
        assert_eq!(parse_date("2026-04-01T00:00"), Some(expected));
        assert_eq!(parse_date("2026-04-01T00:00:00.000"), Some(expected));
        assert_eq!(parse_date("nope"), None);
    }
}
