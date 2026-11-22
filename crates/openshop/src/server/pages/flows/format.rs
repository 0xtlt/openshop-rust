//! How long a run of the list took, computed by the server.

use super::super::ui::to_fixed_1;
use crate::db::schema::FlowRun;

const MINUTE_MS: i64 = 60_000;

fn seconds(ms: i64) -> String {
    format!("{}s", to_fixed_1(ms as f64 / 1_000.0))
}

/// How long the run took, `850ms`, `12.4s`, `2m 5s`; `…` while it runs and
/// `—` when it has not started or not finished.
pub(in crate::server::pages) fn run_duration(run: &FlowRun) -> String {
    let elapsed = match (run.started_at, run.completed_at) {
        (Some(started), Some(completed)) => Some((completed - started).as_millis() as i64),
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

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::Timestamp;
    use jiff::SignedDuration;

    /// A run of `syncOrders` that started and finished `started` and `completed` ms after 2026-10-06 12:00 UTC.
    pub(in crate::server::pages::flows) fn run(status: &str, started: Option<i64>, completed: Option<i64>) -> FlowRun {
        let base = "2026-10-06T12:00:00Z".parse::<Timestamp>().unwrap();
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
            started_at: started.map(|ms| base + SignedDuration::from_millis(ms)),
            completed_at: completed.map(|ms| base + SignedDuration::from_millis(ms)),
            created_at: base,
            concurrency_key: None,
        }
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
}
