//! Log filter state of the run page: level set, query URLs and bookkeeping of new lines.

use crate::{json, query::QueryParams, types::LogEntry};
use serde_json::Value;
use std::collections::HashSet;

pub const LOG_LEVELS: [&str; 3] = ["info", "warn", "error"];
const DEFAULT_LEVELS_PARAM: &str = "info,warn,error";

/// Enabled levels from the `levels` URL parameter, in the order they were enabled.
pub fn parse_levels(param: Option<&str>) -> Vec<String> {
    let param = param.filter(|param| !param.is_empty()).unwrap_or(DEFAULT_LEVELS_PARAM);
    let mut levels: Vec<String> = Vec::new();
    for level in param.split(',').filter(|level| !level.is_empty()) {
        if !levels.iter().any(|known| known == level) {
            levels.push(level.to_string());
        }
    }
    levels
}

pub fn toggle_level(levels: &mut Vec<String>, level: &str) {
    match levels.iter().position(|known| known == level) {
        Some(index) => {
            levels.remove(index);
        }
        None => levels.push(level.to_string()),
    }
}

/// Order-independent form of the level set, sent to the logs endpoint.
pub fn levels_key(levels: &[String]) -> String {
    let mut sorted: Vec<&str> = levels.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.join(",")
}

/// Page URL mirroring the filters; default levels are left out.
pub fn page_url(pathname: &str, query: &str, levels: &[String]) -> String {
    let mut params = QueryParams::new();
    if !query.is_empty() {
        params.set("q", query);
    }
    let levels = levels.join(",");
    if levels != DEFAULT_LEVELS_PARAM {
        params.set("levels", &levels);
    }
    params.append_to(pathname)
}

pub fn logs_url(run_id: &str, query: &str, levels: &[String]) -> String {
    let mut params = QueryParams::new();
    if !query.is_empty() {
        params.set("q", query);
    }
    params.set("levels", &levels_key(levels));
    params.append_to(&format!("/api/runs/{run_id}/logs"))
}

/// Unfiltered logs, used for the per-level counters.
pub fn all_logs_url(run_id: &str) -> String {
    format!("/api/runs/{run_id}/logs?levels={DEFAULT_LEVELS_PARAM}")
}

pub fn export_url(run_id: &str, query: &str, levels: &[String], format: &str) -> String {
    let mut params = QueryParams::new();
    if !query.is_empty() {
        params.set("q", query);
    }
    params.set("levels", &levels.join(","));
    params.set("format", format);
    params.append_to(&format!("/api/runs/{run_id}/logs/export"))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LevelCounts {
    pub info: usize,
    pub warn: usize,
    pub error: usize,
}

impl LevelCounts {
    pub fn get(&self, level: &str) -> usize {
        match level {
            "info" => self.info,
            "warn" => self.warn,
            "error" => self.error,
            _ => 0,
        }
    }
}

pub fn count_levels(logs: &[LogEntry]) -> LevelCounts {
    let count = |level: &str| logs.iter().filter(|log| log.level == level).count();
    LevelCounts {
        info: count("info"),
        warn: count("warn"),
        error: count("error"),
    }
}

/// Records the ids of `logs` and returns those not seen before. The first load only seeds
/// the known set so that existing lines are not animated.
pub fn note_logs(known: &mut HashSet<String>, logs: &[LogEntry], initial_load: bool) -> HashSet<String> {
    let mut fresh = HashSet::new();
    for log in logs {
        if known.insert(log.id.clone()) && !initial_load {
            fresh.insert(log.id.clone());
        }
    }
    fresh
}

/// `key=value` pairs of a log payload, or `None` when there is nothing to show.
pub fn payload_text(payload: Option<&Value>) -> Option<String> {
    let entries = json::entries(payload.filter(|payload| json::is_truthy(payload))?);
    (!entries.is_empty()).then(|| {
        entries
            .iter()
            .map(|(key, value)| format!("{key}={}", json::compact(value)))
            .collect::<Vec<_>>()
            .join(" ")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn levels(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn log(id: &str, level: &str) -> LogEntry {
        LogEntry {
            id: id.to_string(),
            level: level.to_string(),
            message: None,
            payload: None,
            created_at: None,
            matched: None,
        }
    }

    #[test]
    fn parses_levels_from_the_url() {
        assert_eq!(parse_levels(None), levels(&LOG_LEVELS));
        assert_eq!(parse_levels(Some("")), levels(&LOG_LEVELS));
        assert_eq!(parse_levels(Some("error,,warn,error")), levels(&["error", "warn"]));
    }

    #[test]
    fn toggles_levels_keeping_insertion_order() {
        let mut enabled = levels(&LOG_LEVELS);
        toggle_level(&mut enabled, "info");
        assert_eq!(enabled, levels(&["warn", "error"]));
        toggle_level(&mut enabled, "info");
        assert_eq!(enabled, levels(&["warn", "error", "info"]));
        assert_eq!(levels_key(&enabled), "error,info,warn");
    }

    #[test]
    fn mirrors_filters_in_the_page_url() {
        assert_eq!(page_url("/runs/1", "", &levels(&LOG_LEVELS)), "/runs/1");
        assert_eq!(page_url("/runs/1", "order", &levels(&LOG_LEVELS)), "/runs/1?q=order");
        assert_eq!(
            page_url("/runs/1", "|= \"x\"", &levels(&["warn", "error", "info"])),
            "/runs/1?q=%7C%3D+%22x%22&levels=warn%2Cerror%2Cinfo"
        );
        assert_eq!(page_url("/runs/1", "", &[]), "/runs/1?levels=");
    }

    #[test]
    fn builds_log_api_urls() {
        assert_eq!(
            logs_url("abc", "", &levels(&["warn", "error", "info"])),
            "/api/runs/abc/logs?levels=error%2Cinfo%2Cwarn"
        );
        assert_eq!(
            logs_url("abc", "last:5m", &levels(&["error"])),
            "/api/runs/abc/logs?q=last%3A5m&levels=error"
        );
        assert_eq!(all_logs_url("abc"), "/api/runs/abc/logs?levels=info,warn,error");
        assert_eq!(
            export_url("abc", "order", &levels(&["warn", "info"]), "csv"),
            "/api/runs/abc/logs/export?q=order&levels=warn%2Cinfo&format=csv"
        );
    }

    #[test]
    fn counts_logs_per_level() {
        let logs = [log("1", "info"), log("2", "error"), log("3", "info"), log("4", "debug")];
        let counts = count_levels(&logs);
        assert_eq!(
            counts,
            LevelCounts {
                info: 2,
                warn: 0,
                error: 1
            }
        );
        assert_eq!(counts.get("info"), 2);
        assert_eq!(counts.get("debug"), 0);
    }

    #[test]
    fn flags_only_logs_that_arrive_after_the_first_load() {
        let mut known = HashSet::new();
        assert!(note_logs(&mut known, &[log("1", "info"), log("2", "info")], true).is_empty());
        let fresh = note_logs(&mut known, &[log("2", "info"), log("3", "warn")], false);
        assert_eq!(fresh, HashSet::from(["3".to_string()]));
        assert!(note_logs(&mut known, &[log("3", "warn")], false).is_empty());
    }

    #[test]
    fn renders_payloads_as_key_value_pairs() {
        assert_eq!(
            payload_text(Some(&json!({ "orderId": 42, "tags": ["a"], "note": "x y" }))).as_deref(),
            Some(r#"orderId=42 tags=["a"] note="x y""#)
        );
        assert_eq!(payload_text(Some(&json!({}))), None);
        assert_eq!(payload_text(Some(&json!(null))), None);
        assert_eq!(payload_text(None), None);
    }
}
