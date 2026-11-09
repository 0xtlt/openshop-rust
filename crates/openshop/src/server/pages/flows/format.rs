//! What a row of runs says, computed by the server: the tone of a status,
//! how long a run took, how long ago it started.

use chrono::{DateTime, Utc};

use crate::db::schema::FlowRun;

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

/// The tone of the badge of a run or step status.
pub(super) fn status_tone(status: &str) -> &'static str {
    match status {
        "completed" => "success",
        "failed" => "critical",
        "running" => "warning",
        "pending" | "sleeping" => "info",
        "canceled" => "neutral",
        _ => "auto",
    }
}

/// `Number.prototype.toFixed(1)`: like `{:.1}`, but exact ties round away from zero.
fn to_fixed_1(value: f64) -> String {
    let magnitude = value.abs();
    let quarters = magnitude * 4.0;
    let tie_rounding_down = quarters.fract() == 0.0 && quarters % 4.0 == 1.0;
    let rounded = if tie_rounding_down { magnitude + 0.05 } else { magnitude };
    let sign = if value < 0.0 { "-" } else { "" };
    format!("{sign}{rounded:.1}")
}

fn seconds(ms: i64) -> String {
    format!("{}s", to_fixed_1(ms as f64 / 1_000.0))
}

/// How long the run took, `850ms`, `12.4s`, `2m 5s`; `…` while it runs and
/// `—` when it has not started or not finished.
pub(super) fn run_duration(run: &FlowRun) -> String {
    let elapsed = match (run.started_at, run.completed_at) {
        (Some(started), Some(completed)) => Some((completed - started).num_milliseconds()),
        _ => None,
    };
    match elapsed {
        Some(ms) if ms < 1_000 => format!("{ms}ms"),
        Some(ms) if ms < MINUTE_MS => seconds(ms),
        Some(ms) => {
            let rest_seconds = ((ms % MINUTE_MS) as f64 / 1_000.0).round() as i64;
            format!("{}m {rest_seconds}s", ms / MINUTE_MS)
        }
        None if run.status == "running" => "…".to_string(),
        None => "—".to_string(),
    }
}

/// How long ago `then` was: `just now`, `45m ago`, `4h ago`, `3d ago`.
pub(super) fn time_ago(now: DateTime<Utc>, then: DateTime<Utc>) -> String {
    let ms = (now - then).num_milliseconds();
    if ms < MINUTE_MS {
        "just now".to_string()
    } else if ms < HOUR_MS {
        format!("{}m ago", ms / MINUTE_MS)
    } else if ms < DAY_MS {
        format!("{}h ago", ms / HOUR_MS)
    } else {
        format!("{}d ago", ms / DAY_MS)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use chrono::Duration;

    /// A run of `syncOrders` that started and finished `started` and `completed` ms after 2026-10-06 12:00 UTC.
    pub(in crate::server::pages::flows) fn run(status: &str, started: Option<i64>, completed: Option<i64>) -> FlowRun {
        let base = DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z").unwrap().with_timezone(&Utc);
        FlowRun {
            id: uuid::Uuid::nil(),
            app_handle: "default".to_string(),
            shop: "shop.myshopify.com".to_string(),
            flow_name: "syncOrders".to_string(),
            status: status.to_string(),
            input: None,
            error: None,
            deadline_at: None,
            parent_run_id: None,
            attempts: 0,
            available_at: None,
            worker_id: None,
            retry_policy: None,
            started_at: started.map(|ms| base + Duration::milliseconds(ms)),
            completed_at: completed.map(|ms| base + Duration::milliseconds(ms)),
            created_at: base,
        }
    }

    #[test]
    fn maps_statuses_to_badge_tones() {
        assert_eq!(status_tone("completed"), "success");
        assert_eq!(status_tone("failed"), "critical");
        assert_eq!(status_tone("running"), "warning");
        assert_eq!(status_tone("pending"), "info");
        assert_eq!(status_tone("sleeping"), "info");
        assert_eq!(status_tone("canceled"), "neutral");
        assert_eq!(status_tone("skipped"), "auto");
    }

    #[test]
    fn rounds_like_javascript() {
        assert_eq!(to_fixed_1(0.25), "0.3");
        assert_eq!(to_fixed_1(1.15), "1.1");
        assert_eq!(to_fixed_1(1.25), "1.3");
        assert_eq!(to_fixed_1(59.999), "60.0");
    }

    #[test]
    fn says_how_long_a_run_took() {
        assert_eq!(run_duration(&run("completed", Some(0), Some(999))), "999ms");
        assert_eq!(run_duration(&run("completed", Some(0), Some(1_250))), "1.3s");
        assert_eq!(run_duration(&run("completed", Some(0), Some(59_940))), "59.9s");
        assert_eq!(run_duration(&run("failed", Some(0), Some(125_400))), "2m 5s");
        assert_eq!(run_duration(&run("running", Some(0), None)), "…");
        assert_eq!(run_duration(&run("pending", None, None)), "—");
        assert_eq!(run_duration(&run("sleeping", Some(0), None)), "—");
    }

    #[test]
    fn says_how_long_ago() {
        let now = DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z").unwrap().with_timezone(&Utc);
        let ago = |seconds: i64| time_ago(now, now - Duration::seconds(seconds));
        assert_eq!(ago(30), "just now");
        assert_eq!(ago(-300), "just now");
        assert_eq!(ago(45 * 60), "45m ago");
        assert_eq!(ago(5 * 3600 - 1), "4h ago");
        assert_eq!(ago(3 * 86_400), "3d ago");
    }
}
