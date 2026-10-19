//! Log query engine (Grafana-style).
//!
//! `|= "text"` contains, `!= "text"` excludes, `|~ "regex"` matches, free text
//! contains. `C:n` / `A:n` / `B:n` add context lines, `last:5m`, `from:`, `to:`
//! and `between:a,b` restrict the time range.
//!
//! A query is compiled once ([`prepare_log_query`]) and then applied to every
//! log line. A query the engine refuses fails with [`InvalidLogQuery`], which
//! the HTTP API answers with `400`; it is never read as "no match".

use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::{Captures, Regex, RegexBuilder};
use serde_json::Value;

use crate::runtime::time::parse_date;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFilterOp {
    Contains,
    Excludes,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    pub op: LogFilterOp,
    pub value: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimeRange {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogContext {
    pub before: usize,
    pub after: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedLogQuery {
    pub filters: Vec<LogFilter>,
    pub time: TimeRange,
    pub context: LogContext,
}

/// Context lines kept around a match, per side.
pub const MAX_CONTEXT_LINES: usize = 100;
/// Longest query accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 2_000;
/// Most filters (`|=`, `!=`, `|~`, free text) one query can hold.
pub const MAX_FILTERS: usize = 20;
/// Longest `|~` pattern accepted, in characters.
const MAX_REGEX_CHARS: usize = 120;
/// Largest compiled `|~` pattern, in bytes.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// A log query the engine refuses: a bad regex, or one past the limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidLogQuery(pub String);

impl std::fmt::Display for InvalidLogQuery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidLogQuery {}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid regex")
}

static CONTEXT_BOTH: LazyLock<Regex> = LazyLock::new(|| regex(r"C:(\d+)"));
static CONTEXT_AFTER: LazyLock<Regex> = LazyLock::new(|| regex(r"A:(\d+)"));
static CONTEXT_BEFORE: LazyLock<Regex> = LazyLock::new(|| regex(r"B:(\d+)"));
static CONTAINS: LazyLock<Regex> = LazyLock::new(|| regex(r#"\|=\s*"([^"]*)""#));
static EXCLUDES: LazyLock<Regex> = LazyLock::new(|| regex(r#"!=\s*"([^"]*)""#));
static MATCHES: LazyLock<Regex> = LazyLock::new(|| regex(r#"\|~\s*"([^"]*)""#));
static BETWEEN: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)between:(\S+?),(\S+)"));
static LAST: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)last:(\d+)(s|m|h|d)"));
static FROM: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)from:(\S+)"));
static TO: LazyLock<Regex> = LazyLock::new(|| regex(r"(?i)to:(\S+)"));

static BACKREFERENCE: LazyLock<Regex> = LazyLock::new(|| regex(r"\\[1-9]"));
static NESTED_QUANTIFIER: LazyLock<Regex> = LazyLock::new(|| regex(r"\([^)]*[+*][^)]*\)\s*[+*{]"));
static REPEATED_WILDCARD: LazyLock<Regex> = LazyLock::new(|| regex(r"(?:\.\*){2,}"));

/// Why a `|~` pattern is refused before it is compiled, if it is.
fn refused_pattern(pattern: &str) -> Option<&'static str> {
    if pattern.chars().count() > MAX_REGEX_CHARS {
        Some("is longer than 120 characters")
    } else if BACKREFERENCE.is_match(pattern) {
        Some("uses a backreference")
    } else if NESTED_QUANTIFIER.is_match(pattern) {
        Some("repeats a repeated group")
    } else if REPEATED_WILDCARD.is_match(pattern) {
        Some("repeats a wildcard")
    } else {
        None
    }
}

fn compile_pattern(pattern: &str) -> Result<Regex, InvalidLogQuery> {
    if let Some(reason) = refused_pattern(pattern) {
        return Err(InvalidLogQuery(format!("Invalid regex \"{pattern}\": it {reason}")));
    }
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|error| {
            // The syntax error repeats the pattern over several lines: keep its last one.
            let detail = error.to_string();
            let detail = detail.lines().last().unwrap_or_default();
            InvalidLogQuery(format!(
                "Invalid regex \"{pattern}\": {}",
                detail.trim_start_matches("error: ")
            ))
        })
}

fn duration_unit_ms(unit: &str) -> i64 {
    match unit.to_ascii_lowercase().as_str() {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => 0,
    }
}

/// A context count, capped at [`MAX_CONTEXT_LINES`].
fn number(captures: &Captures<'_>, group: usize) -> usize {
    captures[group]
        .parse::<usize>()
        .map_or(MAX_CONTEXT_LINES, |value| value.min(MAX_CONTEXT_LINES))
}

pub fn parse_log_query(query: &str) -> ParsedLogQuery {
    let mut parsed = ParsedLogQuery::default();
    let mut rest = query.to_string();

    rest = CONTEXT_BOTH
        .replace_all(&rest, |captures: &Captures<'_>| {
            let value = number(captures, 1);
            parsed.context.before = parsed.context.before.max(value);
            parsed.context.after = parsed.context.after.max(value);
            ""
        })
        .into_owned();
    rest = CONTEXT_AFTER
        .replace_all(&rest, |captures: &Captures<'_>| {
            parsed.context.after = parsed.context.after.max(number(captures, 1));
            ""
        })
        .into_owned();
    rest = CONTEXT_BEFORE
        .replace_all(&rest, |captures: &Captures<'_>| {
            parsed.context.before = parsed.context.before.max(number(captures, 1));
            ""
        })
        .into_owned();

    for (pattern, op) in [
        (&*CONTAINS, LogFilterOp::Contains),
        (&*EXCLUDES, LogFilterOp::Excludes),
        (&*MATCHES, LogFilterOp::Regex),
    ] {
        rest = pattern
            .replace_all(&rest, |captures: &Captures<'_>| {
                let value = if op == LogFilterOp::Regex {
                    captures[1].to_string()
                } else {
                    captures[1].to_lowercase()
                };
                parsed.filters.push(LogFilter { op, value });
                ""
            })
            .into_owned();
    }

    rest = BETWEEN
        .replace_all(&rest, |captures: &Captures<'_>| {
            if let Some(from) = parse_date(&captures[1]) {
                parsed.time.from = Some(from);
            }
            if let Some(to) = parse_date(&captures[2]) {
                parsed.time.to = Some(to);
            }
            ""
        })
        .into_owned();
    rest = LAST
        .replace_all(&rest, |captures: &Captures<'_>| {
            let amount: i64 = captures[1].parse().unwrap_or(0);
            let ms = amount.saturating_mul(duration_unit_ms(&captures[2]));
            if ms > 0 {
                parsed.time.from = Some(Utc::now() - chrono::Duration::milliseconds(ms));
            }
            ""
        })
        .into_owned();
    rest = FROM
        .replace_all(&rest, |captures: &Captures<'_>| {
            if let Some(from) = parse_date(&captures[1]) {
                parsed.time.from = Some(from);
            }
            ""
        })
        .into_owned();
    rest = TO
        .replace_all(&rest, |captures: &Captures<'_>| {
            if let Some(to) = parse_date(&captures[1]) {
                parsed.time.to = Some(to);
            }
            ""
        })
        .into_owned();

    let rest = rest.trim();
    if !rest.is_empty() {
        parsed.filters.push(LogFilter {
            op: LogFilterOp::Contains,
            value: rest.to_lowercase(),
        });
    }
    parsed
}

/// Stringifies like JavaScript's `String(value)`.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() && float.fract() == 0.0 && float.abs() < 1e21 => {
                format!("{}", float as i64)
            }
            _ => number.to_string(),
        },
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| if item.is_null() { String::new() } else { js_string(item) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// Lowercased message followed by `key=value` pairs of an object payload.
pub fn log_search_text(message: Option<&str>, payload: Option<&Value>) -> String {
    let mut text = message.unwrap_or_default().to_string();
    if let Some(Value::Object(payload)) = payload {
        let pairs: Vec<String> = payload
            .iter()
            .map(|(key, value)| format!("{key}={}", js_string(value)))
            .collect();
        text.push(' ');
        text.push_str(&pairs.join(" "));
    }
    text.to_lowercase()
}

enum Check {
    Contains(String),
    Excludes(String),
    Regex(Regex),
}

/// The filters of a query, compiled once for all the lines it is applied to.
pub struct LogMatcher {
    checks: Vec<Check>,
}

impl LogMatcher {
    /// Fails when a `|~` pattern is refused or does not compile, or when there
    /// are more than [`MAX_FILTERS`] filters.
    pub fn new(filters: &[LogFilter]) -> Result<Self, InvalidLogQuery> {
        if filters.len() > MAX_FILTERS {
            return Err(InvalidLogQuery(format!(
                "Too many filters: {} at most, got {}",
                MAX_FILTERS,
                filters.len()
            )));
        }
        let checks = filters
            .iter()
            .map(|filter| {
                Ok(match filter.op {
                    LogFilterOp::Contains => Check::Contains(filter.value.clone()),
                    LogFilterOp::Excludes => Check::Excludes(filter.value.clone()),
                    LogFilterOp::Regex => Check::Regex(compile_pattern(&filter.value)?),
                })
            })
            .collect::<Result<_, InvalidLogQuery>>()?;
        Ok(Self { checks })
    }

    pub fn matches(&self, message: Option<&str>, payload: Option<&Value>) -> bool {
        let text = log_search_text(message, payload);
        self.checks.iter().all(|check| match check {
            Check::Contains(value) => text.contains(value.as_str()),
            Check::Excludes(value) => !text.contains(value.as_str()),
            Check::Regex(pattern) => pattern.is_match(&text),
        })
    }
}

/// Parses a query and compiles its filters, or says why it is refused.
pub fn prepare_log_query(query: &str) -> Result<(ParsedLogQuery, LogMatcher), InvalidLogQuery> {
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(InvalidLogQuery(format!(
            "Log query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    let parsed = parse_log_query(query);
    let matcher = LogMatcher::new(&parsed.filters)?;
    Ok((parsed, matcher))
}

/// Adds `before` / `after` neighbours of every matched item.
pub fn apply_context_expansion<'a>(
    ids: &[&'a str],
    matched: &HashSet<&'a str>,
    context: LogContext,
) -> HashSet<&'a str> {
    if context.before == 0 && context.after == 0 {
        return matched.clone();
    }
    let mut visible = matched.clone();
    for (index, id) in ids.iter().enumerate() {
        if !matched.contains(id) {
            continue;
        }
        let start = index.saturating_sub(context.before);
        let end = index.saturating_add(context.after).min(ids.len() - 1);
        visible.extend(&ids[start..=end]);
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn filter(op: LogFilterOp, value: &str) -> LogFilter {
        LogFilter {
            op,
            value: value.to_string(),
        }
    }

    #[test]
    fn free_text_becomes_contains_filter() {
        assert_eq!(
            parse_log_query("Hello").filters,
            vec![filter(LogFilterOp::Contains, "hello")]
        );
    }

    #[test]
    fn operators_create_their_filters() {
        assert_eq!(
            parse_log_query(r#"|= "Order""#).filters,
            vec![filter(LogFilterOp::Contains, "order")]
        );
        assert_eq!(
            parse_log_query(r#"!= "retry""#).filters,
            vec![filter(LogFilterOp::Excludes, "retry")]
        );
        assert_eq!(
            parse_log_query(r#"|~ "order-\d+""#).filters,
            vec![filter(LogFilterOp::Regex, r"order-\d+")]
        );
    }

    #[test]
    fn multiple_filters_combined() {
        let filters = parse_log_query(r#"|= "order" != "retry" |~ "id-\d+""#).filters;
        assert_eq!(
            filters.iter().map(|filter| filter.op).collect::<Vec<_>>(),
            vec![LogFilterOp::Contains, LogFilterOp::Excludes, LogFilterOp::Regex]
        );
    }

    #[test]
    fn context_flags_are_parsed() {
        let parsed = parse_log_query("hello C:3");
        assert_eq!(parsed.context, LogContext { before: 3, after: 3 });
        assert_eq!(parsed.filters, vec![filter(LogFilterOp::Contains, "hello")]);

        let parsed = parse_log_query("B:2 A:5");
        assert_eq!(parsed.context, LogContext { before: 2, after: 5 });
        assert!(parsed.filters.is_empty());
    }

    #[test]
    fn last_sets_from_date() {
        let before = Utc::now();
        let from = parse_log_query("last:5m").time.from.unwrap();
        let diff = (before - from).num_milliseconds();
        assert!((5 * 60_000 - 100..5 * 60_000 + 100).contains(&diff));
    }

    #[test]
    fn from_and_to_set_date_range() {
        let time = parse_log_query("from:2026-03-01 to:2026-03-02").time;
        assert_eq!(time.from.unwrap().date_naive().to_string(), "2026-03-01");
        assert_eq!(time.to.unwrap().date_naive().to_string(), "2026-03-02");
    }

    #[test]
    fn between_sets_both_from_and_to() {
        let time = parse_log_query("between:2026-01-01,2026-02-01").time;
        assert!(time.from.is_some());
        assert!(time.to.is_some());
    }

    #[test]
    fn empty_string_returns_no_filters() {
        assert_eq!(parse_log_query(""), ParsedLogQuery::default());
    }

    #[test]
    fn search_text_combines_message_and_payload() {
        let text = log_search_text(Some("Fetching orders"), Some(&json!({ "limit": 10 })));
        assert!(text.contains("fetching orders"));
        assert!(text.contains("limit=10"));
    }

    #[test]
    fn search_text_handles_missing_and_array_payloads() {
        assert_eq!(log_search_text(None, None), "");
        assert_eq!(log_search_text(Some("test"), Some(&json!([1, 2, 3]))), "test");
    }

    #[test]
    fn search_text_stringifies_values_like_javascript() {
        let payload = json!({ "a": null, "b": true, "c": 1.5, "d": [1, null, "x"], "e": { "k": 1 }, "f": 2.0 });
        assert_eq!(
            log_search_text(Some(""), Some(&payload)),
            " a=null b=true c=1.5 d=1,,x e=[object object] f=2"
        );
    }

    fn matcher(filters: &[LogFilter]) -> Result<LogMatcher, InvalidLogQuery> {
        LogMatcher::new(filters)
    }

    fn matches(filters: &[LogFilter]) -> bool {
        matcher(filters).expect("valid filters").matches(
            Some("Fetching orders from Shopify"),
            Some(&json!({ "limit": 10, "shop": "test.myshopify.com" })),
        )
    }

    #[test]
    fn contains_and_excludes_filters() {
        assert!(matches(&[filter(LogFilterOp::Contains, "orders")]));
        assert!(!matches(&[filter(LogFilterOp::Contains, "products")]));
        assert!(matches(&[filter(LogFilterOp::Excludes, "error")]));
        assert!(!matches(&[filter(LogFilterOp::Excludes, "orders")]));
    }

    #[test]
    fn regex_filters() {
        assert!(matches(&[filter(LogFilterOp::Regex, r"limit=\d+")]));
        assert!(matches(&[filter(LogFilterOp::Regex, "FETCHING")]));
        assert!(!matches(&[filter(LogFilterOp::Regex, "nothing like this")]));
    }

    #[test]
    fn an_invalid_regex_is_an_error_not_an_empty_result() {
        let error = matcher(&[filter(LogFilterOp::Regex, "[invalid")])
            .err()
            .expect("refused");
        assert!(error.0.starts_with("Invalid regex \"[invalid\": "), "{error}");
        assert!(!error.0.contains('\n'), "{error}");
        assert!(prepare_log_query(r#"|~ "[invalid""#).is_err());
    }

    #[test]
    fn unsafe_regexes_are_refused() {
        for pattern in ["(a+)+", ".*.*orders", r"(o)\1"] {
            assert!(matcher(&[filter(LogFilterOp::Regex, pattern)]).is_err(), "{pattern}");
        }
        assert!(matcher(&[filter(LogFilterOp::Regex, &"o".repeat(121))]).is_err());
        assert!(matcher(&[filter(LogFilterOp::Regex, &"o".repeat(120))]).is_ok());
    }

    #[test]
    fn context_counts_are_capped() {
        let parsed = parse_log_query("hello C:5000 A:99999999999999999999999999 B:3");
        assert_eq!(
            parsed.context,
            LogContext {
                before: MAX_CONTEXT_LINES,
                after: MAX_CONTEXT_LINES
            }
        );
        assert_eq!(parse_log_query("C:100").context.after, 100);
    }

    #[test]
    fn the_query_and_its_filters_are_bounded() {
        assert!(prepare_log_query(&"a".repeat(MAX_QUERY_CHARS)).is_ok());
        assert!(prepare_log_query(&"a".repeat(MAX_QUERY_CHARS + 1)).is_err());
        let many = r#"|= "a" "#.repeat(MAX_FILTERS + 1);
        assert!(prepare_log_query(&many).is_err());
        assert!(prepare_log_query(&many.replacen(r#"|= "a" "#, "", 1)).is_ok());
    }

    #[test]
    fn multiple_filters_are_and_ed() {
        assert!(matches(&[
            filter(LogFilterOp::Contains, "orders"),
            filter(LogFilterOp::Excludes, "error"),
        ]));
        assert!(!matches(&[
            filter(LogFilterOp::Contains, "orders"),
            filter(LogFilterOp::Contains, "products"),
        ]));
    }

    const ITEMS: [&str; 5] = ["a", "b", "c", "d", "e"];

    fn expand(matched: &[&'static str], before: usize, after: usize) -> Vec<&'static str> {
        let matched: HashSet<&str> = matched.iter().copied().collect();
        let mut visible: Vec<&str> = apply_context_expansion(&ITEMS, &matched, LogContext { before, after })
            .into_iter()
            .collect();
        visible.sort_unstable();
        visible
    }

    #[test]
    fn context_expansion() {
        assert_eq!(expand(&["c"], 0, 0), vec!["c"]);
        assert_eq!(expand(&["c"], 1, 1), vec!["b", "c", "d"]);
        assert_eq!(expand(&["a"], 5, 0), vec!["a"]);
        assert_eq!(expand(&["a", "e"], 0, 1), vec!["a", "b", "e"]);
    }
}
