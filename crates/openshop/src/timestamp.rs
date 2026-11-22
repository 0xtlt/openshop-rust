//! An instant in time: what a `timestamp with time zone` column holds.

use std::fmt;
use std::ops::{Add, AddAssign, Deref, Sub, SubAssign};
use std::str::FromStr;

use jiff::SignedDuration;
use jiff_sqlx::ToSqlx;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sqlx::encode::IsNull;
use sqlx::error::BoxDynError;
use sqlx::postgres::{PgArgumentBuffer, PgHasArrayType, PgTypeInfo, PgValueRef};
use sqlx::{Decode, Encode, Postgres, Type};

/// An instant in time, to the nanosecond: a [`jiff::Timestamp`] that PostgreSQL
/// stores in a `timestamp with time zone` column, to the microsecond.
///
/// It derefs to [`jiff::Timestamp`], whose methods it has (`as_second`,
/// `as_millisecond`, `strftime`, `in_tz`…), and converts from and to it with
/// `From`. Adding or subtracting a [`SignedDuration`] or a
/// [`std::time::Duration`] gives a `Timestamp`, and the difference of two is a
/// [`SignedDuration`].
///
/// It serializes, and displays, as an RFC 3339 string in UTC, with 0, 3, 6 or 9
/// digits of fractional seconds: `2026-04-01T12:30:05.123456Z`.
///
/// ```
/// use openshop::Timestamp;
/// use openshop::jiff::SignedDuration;
///
/// let created_at: Timestamp = "2026-04-01T12:30:05Z".parse()?;
/// let expires_at = created_at + SignedDuration::from_hours(1);
/// assert_eq!(expires_at.to_string(), "2026-04-01T13:30:05Z");
/// assert_eq!(expires_at - created_at, SignedDuration::from_hours(1));
/// assert!(created_at < Timestamp::now());
/// # Ok::<(), openshop::jiff::Error>(())
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    /// The Unix epoch, `1970-01-01T00:00:00Z`.
    pub const UNIX_EPOCH: Self = Self(jiff::Timestamp::UNIX_EPOCH);

    /// The current instant, from the system clock.
    pub fn now() -> Self {
        Self(jiff::Timestamp::now())
    }

    /// The instant a number of whole seconds after the Unix epoch, `None` out of range.
    pub fn from_second(second: i64) -> Option<Self> {
        jiff::Timestamp::from_second(second).ok().map(Self)
    }

    /// The instant a number of milliseconds after the Unix epoch, `None` out of range.
    pub fn from_millisecond(millisecond: i64) -> Option<Self> {
        jiff::Timestamp::from_millisecond(millisecond).ok().map(Self)
    }

    /// The instant a number of microseconds after the Unix epoch, `None` out of range.
    pub fn from_microsecond(microsecond: i64) -> Option<Self> {
        jiff::Timestamp::from_microsecond(microsecond).ok().map(Self)
    }

    /// The [`jiff::Timestamp`] it holds.
    pub const fn to_jiff(self) -> jiff::Timestamp {
        self.0
    }

    /// The instant a duration later, `None` out of range.
    pub fn checked_add(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_add(duration).ok().map(Self)
    }

    /// The instant a duration earlier, `None` out of range.
    pub fn checked_sub(self, duration: SignedDuration) -> Option<Self> {
        self.0.checked_sub(duration).ok().map(Self)
    }

    /// The instant a duration later, the last or the first one there is when out of range.
    pub fn saturating_add(self, duration: SignedDuration) -> Self {
        Self(self.0.saturating_add(duration).unwrap_or(if duration.is_negative() {
            jiff::Timestamp::MIN
        } else {
            jiff::Timestamp::MAX
        }))
    }

    /// The instant a duration earlier, the first or the last one there is when out of range.
    pub fn saturating_sub(self, duration: SignedDuration) -> Self {
        self.saturating_add(duration.checked_neg().unwrap_or(SignedDuration::MAX))
    }

    /// The RFC 3339 form JavaScript's `Date#toJSON` writes: UTC, milliseconds
    /// (truncated), `Z`: `2026-04-01T12:30:05.123Z`.
    pub fn to_iso(self) -> String {
        format!("{:.3}", self.0)
    }

    /// The digits of fractional seconds it is written with: 0, 3, 6 or 9, the fewest that hold it.
    fn precision(self) -> usize {
        match self.0.subsec_nanosecond() {
            0 => 0,
            nanos if nanos % 1_000_000 == 0 => 3,
            nanos if nanos % 1_000 == 0 => 6,
            _ => 9,
        }
    }
}

/// The instant of a date and a time of day in UTC, for the tests.
#[cfg(test)]
pub(crate) fn utc(year: i16, month: i8, day: i8, hour: i8, minute: i8, second: i8) -> Timestamp {
    let datetime = jiff::civil::date(year, month, day).at(hour, minute, second, 0);
    Timestamp(jiff::tz::TimeZone::UTC.to_timestamp(datetime).unwrap())
}

impl Deref for Timestamp {
    type Target = jiff::Timestamp;

    fn deref(&self) -> &jiff::Timestamp {
        &self.0
    }
}

impl From<jiff::Timestamp> for Timestamp {
    fn from(timestamp: jiff::Timestamp) -> Self {
        Self(timestamp)
    }
}

impl From<Timestamp> for jiff::Timestamp {
    fn from(timestamp: Timestamp) -> Self {
        timestamp.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.*}", self.precision(), self.0)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Timestamp {
    type Err = jiff::Error;

    /// An RFC 3339 timestamp, with an offset: `2026-04-01T12:30:05Z`, `2026-04-01T14:30:05+02:00`.
    fn from_str(value: &str) -> Result<Self, jiff::Error> {
        value.parse().map(Self)
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = std::borrow::Cow::<str>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl Add<SignedDuration> for Timestamp {
    type Output = Self;

    /// Panics out of range, as `jiff::Timestamp` does: `checked_add` does not.
    fn add(self, duration: SignedDuration) -> Self {
        Self(self.0 + duration)
    }
}

impl Sub<SignedDuration> for Timestamp {
    type Output = Self;

    /// Panics out of range, as `jiff::Timestamp` does: `checked_sub` does not.
    fn sub(self, duration: SignedDuration) -> Self {
        Self(self.0 - duration)
    }
}

impl Add<std::time::Duration> for Timestamp {
    type Output = Self;

    fn add(self, duration: std::time::Duration) -> Self {
        Self(self.0 + duration)
    }
}

impl Sub<std::time::Duration> for Timestamp {
    type Output = Self;

    fn sub(self, duration: std::time::Duration) -> Self {
        Self(self.0 - duration)
    }
}

impl AddAssign<SignedDuration> for Timestamp {
    fn add_assign(&mut self, duration: SignedDuration) {
        *self = *self + duration;
    }
}

impl SubAssign<SignedDuration> for Timestamp {
    fn sub_assign(&mut self, duration: SignedDuration) {
        *self = *self - duration;
    }
}

impl Sub for Timestamp {
    type Output = SignedDuration;

    /// How long after `other` it is, negative when before.
    fn sub(self, other: Self) -> SignedDuration {
        self.0.duration_since(other.0)
    }
}

impl Type<Postgres> for Timestamp {
    fn type_info() -> PgTypeInfo {
        <jiff_sqlx::Timestamp as Type<Postgres>>::type_info()
    }
}

impl PgHasArrayType for Timestamp {
    fn array_type_info() -> PgTypeInfo {
        <jiff_sqlx::Timestamp as PgHasArrayType>::array_type_info()
    }
}

impl Encode<'_, Postgres> for Timestamp {
    fn encode_by_ref(&self, buf: &mut PgArgumentBuffer) -> Result<IsNull, BoxDynError> {
        Encode::<Postgres>::encode(self.0.to_sqlx(), buf)
    }
}

impl<'r> Decode<'r, Postgres> for Timestamp {
    fn decode(value: PgValueRef<'r>) -> Result<Self, BoxDynError> {
        <jiff_sqlx::Timestamp as Decode<'r, Postgres>>::decode(value).map(|timestamp| Self(timestamp.to_jiff()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> Timestamp {
        value.parse().unwrap()
    }

    #[test]
    fn writes_the_fewest_groups_of_three_digits_that_hold_it() {
        assert_eq!(at("2026-04-01T12:30:05Z").to_string(), "2026-04-01T12:30:05Z");
        assert_eq!(at("2026-04-01T12:30:05.12Z").to_string(), "2026-04-01T12:30:05.120Z");
        assert_eq!(
            at("2026-04-01T12:30:05.1234Z").to_string(),
            "2026-04-01T12:30:05.123400Z"
        );
        assert_eq!(
            at("2026-04-01T12:30:05.1234567Z").to_string(),
            "2026-04-01T12:30:05.123456700Z"
        );
        assert_eq!(at("2026-04-01T14:30:05+02:00").to_string(), "2026-04-01T12:30:05Z");
    }

    #[test]
    fn serializes_as_its_string_and_reads_it_back() {
        let timestamp = at("2026-04-01T12:30:05.123456Z");
        let json = serde_json::to_value(timestamp).unwrap();
        assert_eq!(json, serde_json::json!("2026-04-01T12:30:05.123456Z"));
        assert_eq!(serde_json::from_value::<Timestamp>(json).unwrap(), timestamp);
        assert!(serde_json::from_value::<Timestamp>(serde_json::json!("2026-04-01")).is_err());
    }

    #[test]
    fn writes_the_milliseconds_of_javascript() {
        assert_eq!(at("2026-04-01T12:30:05.123999Z").to_iso(), "2026-04-01T12:30:05.123Z");
        assert_eq!(at("2026-04-01T12:30:05Z").to_iso(), "2026-04-01T12:30:05.000Z");
    }

    #[test]
    fn the_difference_of_two_is_a_duration() {
        let start = at("2026-04-01T12:00:00Z");
        assert_eq!(
            start + SignedDuration::from_mins(90) - start,
            SignedDuration::from_mins(90)
        );
        assert_eq!(
            start - (start + SignedDuration::from_secs(1)),
            SignedDuration::from_secs(-1)
        );
        assert_eq!(
            start.saturating_add(SignedDuration::MAX),
            Timestamp(jiff::Timestamp::MAX)
        );
    }
}
