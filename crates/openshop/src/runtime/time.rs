//! Timestamps are serialized like JavaScript's `Date#toJSON`:
//! UTC, millisecond precision, `Z` suffix.

use jiff::civil;
use jiff::tz::TimeZone;
use serde::Serializer;

use crate::Timestamp;

/// A duration in whole milliseconds, as retry policies and worker settings are stored.
pub(crate) fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn to_iso(value: &Timestamp) -> String {
    value.to_iso()
}

/// Parses what JavaScript's `new Date(string)` accepts for ISO-like input:
/// RFC 3339 timestamps, `YYYY-MM-DDTHH:MM[:SS]` (local time is treated as UTC)
/// and bare dates.
pub fn parse_date(value: &str) -> Option<Timestamp> {
    let value = value.trim();
    if let Ok(parsed) = value.parse::<Timestamp>() {
        return Some(parsed);
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(parsed) = civil::DateTime::strptime(format, value) {
            return utc(parsed);
        }
    }
    civil::Date::strptime("%Y-%m-%d", value)
        .ok()
        .and_then(|date| utc(date.to_datetime(civil::Time::midnight())))
}

fn utc(datetime: civil::DateTime) -> Option<Timestamp> {
    TimeZone::UTC.to_timestamp(datetime).ok().map(Timestamp::from)
}

pub mod iso {
    use super::*;

    pub fn serialize<S: Serializer>(value: &Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&to_iso(value))
    }
}

pub mod iso_option {
    use super::*;

    pub fn serialize<S: Serializer>(value: &Option<Timestamp>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => serializer.serialize_str(&to_iso(value)),
            None => serializer.serialize_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_like_javascript_dates() {
        let date: Timestamp = "2026-04-01T12:30:05.123456Z".parse().unwrap();
        assert_eq!(to_iso(&date), "2026-04-01T12:30:05.123Z");
    }

    #[test]
    fn parses_common_date_inputs() {
        let expected: Timestamp = "2026-04-01T00:00:00Z".parse().unwrap();
        assert_eq!(parse_date("2026-04-01"), Some(expected));
        assert_eq!(parse_date("2026-04-01T00:00:00Z"), Some(expected));
        assert_eq!(parse_date("2026-04-01T02:00:00+02:00"), Some(expected));
        assert_eq!(parse_date("2026-04-01T00:00"), Some(expected));
        assert_eq!(parse_date("2026-04-01T00:00:00.000"), Some(expected));
        assert_eq!(parse_date("2026-04-01 00:00:00.000"), Some(expected));
        assert_eq!(parse_date("nope"), None);
    }
}
