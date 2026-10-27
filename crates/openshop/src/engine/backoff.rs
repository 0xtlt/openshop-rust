//! Retry policies and exponential backoff.

use rand::RngExt;
use chrono::{DateTime, Duration, Utc};

use crate::runtime::time::millis;
use crate::types::{ResolvedRetryPolicy, RetryPolicy};

pub const DEFAULT_RETRY_POLICY: ResolvedRetryPolicy = ResolvedRetryPolicy {
    max_attempts: 3,
    initial_interval_ms: 1_000,
    backoff_coefficient: 2.0,
    max_interval_ms: 30_000,
};

/// Later overrides win, field by field.
pub fn resolve_retry_policy(overrides: &[Option<RetryPolicy>]) -> ResolvedRetryPolicy {
    let mut policy = DEFAULT_RETRY_POLICY;
    for partial in overrides.iter().flatten() {
        if let Some(value) = partial.max_attempts {
            policy.max_attempts = value;
        }
        if let Some(value) = partial.initial_interval {
            policy.initial_interval_ms = millis(value);
        }
        if let Some(value) = partial.backoff_coefficient {
            policy.backoff_coefficient = value;
        }
        if let Some(value) = partial.max_interval {
            policy.max_interval_ms = millis(value);
        }
    }
    policy
}

/// The longest delay before the try that follows the `attempt`-th: `initial_ms`
/// multiplied by `coefficient` for each attempt after the first, capped by
/// `max_ms`. The delay really waited is drawn below it by [`jitter_ms`].
pub(crate) fn compute_backoff_ms(initial_ms: u64, coefficient: f64, max_ms: u64, attempt: i64) -> u64 {
    let exponent = i32::try_from(attempt - 1).unwrap_or(i32::MAX);
    let delay = initial_ms as f64 * coefficient.powi(exponent);
    let rounded = if delay.is_finite() && delay > 0.0 {
        delay.round()
    } else if delay.is_infinite() && delay > 0.0 {
        f64::MAX
    } else {
        0.0
    };
    (rounded.min(max_ms as f64)) as u64
}

/// Spreads a delay over its upper half: the result is `delay_ms` for a `unit`
/// of 1 and half of it for 0. Runs that fail together, because a provider went
/// down, then do not all come back at the same moment, and none of them comes
/// back sooner than half its backoff. `unit` is drawn in `[0, 1)`.
pub(crate) fn jitter_ms(delay_ms: u64, unit: f64) -> u64 {
    let unit = if unit.is_finite() { unit.clamp(0.0, 1.0) } else { 1.0 };
    (delay_ms as f64 * (0.5 + unit * 0.5)).round() as u64
}

/// When to try again, as a date on the clock `now` (the one of the database).
/// `None` when attempts are exhausted or the retry would land past the deadline.
pub fn compute_next_retry_at(
    attempts: i64,
    policy: &ResolvedRetryPolicy,
    deadline_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    next_retry_at(attempts, policy, deadline_at, now, rand::rng().random())
}

/// [`compute_next_retry_at`] with the draw of the jitter given.
fn next_retry_at(
    attempts: i64,
    policy: &ResolvedRetryPolicy,
    deadline_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    unit: f64,
) -> Option<DateTime<Utc>> {
    if attempts >= policy.max_attempts as i64 {
        return None;
    }
    let backoff_ms = compute_backoff_ms(
        policy.initial_interval_ms,
        policy.backoff_coefficient,
        policy.max_interval_ms,
        attempts,
    );
    let delay_ms = jitter_ms(backoff_ms, unit);
    let next_at = now + Duration::milliseconds(delay_ms.min(i64::MAX as u64) as i64);
    if deadline_at.is_some_and(|deadline| next_at >= deadline) {
        return None;
    }
    Some(next_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_defaults_with_no_overrides() {
        assert_eq!(resolve_retry_policy(&[]), DEFAULT_RETRY_POLICY);
    }

    #[test]
    fn merges_a_single_partial_override() {
        let policy = resolve_retry_policy(&[Some(RetryPolicy::new().max_attempts(5))]);
        assert_eq!(policy.max_attempts, 5);
        assert_eq!(policy.initial_interval_ms, DEFAULT_RETRY_POLICY.initial_interval_ms);
    }

    #[test]
    fn last_override_wins() {
        let policy = resolve_retry_policy(&[
            Some(RetryPolicy::new().max_attempts(5)),
            Some(RetryPolicy::new().max_attempts(10)),
        ]);
        assert_eq!(policy.max_attempts, 10);
    }

    #[test]
    fn missing_overrides_are_skipped() {
        let policy = resolve_retry_policy(&[None, Some(RetryPolicy::new().max_attempts(7)), None]);
        assert_eq!(policy.max_attempts, 7);
    }

    #[test]
    fn computes_exponential_backoff() {
        let backoff = |attempt| compute_backoff_ms(1_000, 2.0, 30_000, attempt);
        assert_eq!(backoff(1), 1_000);
        assert_eq!(backoff(2), 2_000);
        assert_eq!(backoff(5), 16_000);
        assert_eq!(backoff(20), 30_000);
        assert_eq!(backoff(0), 500);
        assert_eq!(backoff(5_000), 30_000);
        assert_eq!(backoff(i64::MAX), 30_000);
    }

    #[test]
    fn returns_a_date_when_attempts_remain() {
        assert!(compute_next_retry_at(0, &DEFAULT_RETRY_POLICY, None, Utc::now()).is_some());
    }

    #[test]
    fn returns_none_when_max_attempts_reached() {
        assert!(compute_next_retry_at(3, &DEFAULT_RETRY_POLICY, None, Utc::now()).is_none());
    }

    #[test]
    fn returns_none_when_next_retry_exceeds_deadline() {
        let deadline = Utc::now() + Duration::milliseconds(100);
        assert!(compute_next_retry_at(0, &DEFAULT_RETRY_POLICY, Some(deadline), Utc::now()).is_none());
    }

    #[test]
    fn returns_a_date_when_deadline_is_far_enough() {
        let deadline = Utc::now() + Duration::milliseconds(60_000);
        assert!(compute_next_retry_at(0, &DEFAULT_RETRY_POLICY, Some(deadline), Utc::now()).is_some());
    }

    #[test]
    fn jitter_stays_between_half_the_backoff_and_the_backoff() {
        assert_eq!(jitter_ms(1_000, 0.0), 500);
        assert_eq!(jitter_ms(1_000, 0.5), 750);
        assert_eq!(jitter_ms(1_000, 1.0), 1_000);
        assert_eq!(jitter_ms(0, 0.7), 0);
        // A draw out of range, or not a number, cannot leave the bounds.
        assert_eq!(jitter_ms(1_000, -3.0), 500);
        assert_eq!(jitter_ms(1_000, 9.0), 1_000);
        assert_eq!(jitter_ms(1_000, f64::NAN), 1_000);
        assert_eq!(jitter_ms(u64::MAX, 1.0), u64::MAX);
    }

    #[test]
    fn the_retry_is_dated_with_the_clock_it_is_given_and_the_jitter_drawn() {
        let now = Utc::now() + Duration::hours(3);
        let policy = DEFAULT_RETRY_POLICY;
        let earliest = next_retry_at(2, &policy, None, now, 0.0).unwrap();
        let latest = next_retry_at(2, &policy, None, now, 0.999).unwrap();
        assert_eq!(earliest - now, Duration::milliseconds(1_000));
        assert!(latest - now <= Duration::milliseconds(2_000), "{}", latest - now);
        assert!(latest - now >= Duration::milliseconds(1_998), "{}", latest - now);
    }

    #[test]
    fn retries_of_the_same_attempt_do_not_all_land_together() {
        let now = Utc::now();
        let dates: std::collections::HashSet<_> = (0..50)
            .filter_map(|_| compute_next_retry_at(1, &DEFAULT_RETRY_POLICY, None, now))
            .collect();
        assert!(dates.len() > 1);
        for date in dates {
            let delay = (date - now).num_milliseconds();
            assert!((500..=1_000).contains(&delay), "{delay}");
        }
    }

    #[test]
    fn the_jitter_never_goes_past_the_max_interval() {
        let policy = ResolvedRetryPolicy {
            max_attempts: 20,
            max_interval_ms: 2_000,
            ..DEFAULT_RETRY_POLICY
        };
        let now = Utc::now();
        for _ in 0..50 {
            let date = compute_next_retry_at(10, &policy, None, now).unwrap();
            assert!((date - now).num_milliseconds() <= 2_000);
        }
    }
}
