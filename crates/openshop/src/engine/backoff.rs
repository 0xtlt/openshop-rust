//! Retry policies and exponential backoff.

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

pub fn compute_backoff_ms(attempt: i64, policy: &ResolvedRetryPolicy) -> u64 {
    let delay = policy.initial_interval_ms as f64 * policy.backoff_coefficient.powi((attempt - 1) as i32);
    let rounded = if delay.is_finite() && delay > 0.0 {
        delay.round()
    } else if delay.is_infinite() && delay > 0.0 {
        f64::MAX
    } else {
        0.0
    };
    (rounded.min(policy.max_interval_ms as f64)) as u64
}

/// `None` when attempts are exhausted or the retry would land past the deadline.
pub fn compute_next_retry_at(
    attempts: i64,
    policy: &ResolvedRetryPolicy,
    deadline_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    if attempts >= policy.max_attempts as i64 {
        return None;
    }
    let delay_ms = compute_backoff_ms(attempts, policy);
    let next_at = Utc::now() + Duration::milliseconds(delay_ms as i64);
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
        let policy = DEFAULT_RETRY_POLICY;
        assert_eq!(compute_backoff_ms(1, &policy), 1_000);
        assert_eq!(compute_backoff_ms(2, &policy), 2_000);
        assert_eq!(compute_backoff_ms(5, &policy), 16_000);
        assert_eq!(compute_backoff_ms(20, &policy), 30_000);
        assert_eq!(compute_backoff_ms(0, &policy), 500);
        assert_eq!(compute_backoff_ms(5_000, &policy), 30_000);
    }

    #[test]
    fn returns_a_date_when_attempts_remain() {
        assert!(compute_next_retry_at(0, &DEFAULT_RETRY_POLICY, None).is_some());
    }

    #[test]
    fn returns_none_when_max_attempts_reached() {
        assert!(compute_next_retry_at(3, &DEFAULT_RETRY_POLICY, None).is_none());
    }

    #[test]
    fn returns_none_when_next_retry_exceeds_deadline() {
        let deadline = Utc::now() + Duration::milliseconds(100);
        assert!(compute_next_retry_at(0, &DEFAULT_RETRY_POLICY, Some(deadline)).is_none());
    }

    #[test]
    fn returns_a_date_when_deadline_is_far_enough() {
        let deadline = Utc::now() + Duration::milliseconds(60_000);
        assert!(compute_next_retry_at(0, &DEFAULT_RETRY_POLICY, Some(deadline)).is_some());
    }
}
