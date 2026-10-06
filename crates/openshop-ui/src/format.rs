//! Time, duration and size formatting. Timestamps are ISO-8601 strings as produced by `Date#toJSON`.

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn offset_minutes(suffix: &str) -> Option<i64> {
    if suffix.is_empty() || suffix.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let sign = match suffix.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = suffix[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 60 + minutes))
}

/// Milliseconds since the Unix epoch, or `None` when the value is not an ISO-8601 date-time.
pub fn parse_timestamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 19 || !value.is_char_boundary(19) {
        return None;
    }
    let separators = [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')];
    if separators.iter().any(|(index, byte)| bytes[*index] != *byte) || !matches!(bytes[10], b'T' | b't' | b' ') {
        return None;
    }
    let number = |start: usize, end: usize| -> Option<i64> {
        let digits = value.get(start..end)?;
        digits
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 24 || minute > 59 || second > 59 {
        return None;
    }

    let mut rest = &value[19..];
    let mut millis = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let kept = &fraction[..digits.min(3)];
        millis = kept.parse::<i64>().ok()? * 10_i64.pow(3 - kept.len() as u32);
        rest = &fraction[digits..];
    }

    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(seconds * 1_000 + millis - offset_minutes(rest)? * MINUTE_MS)
}

/// `Number.prototype.toFixed(1)`: like `{:.1}` but exact ties round away from zero.
pub fn to_fixed_1(value: f64) -> String {
    let magnitude = value.abs();
    let quarters = magnitude * 4.0;
    let tie_rounding_down = quarters.fract() == 0.0 && quarters % 4.0 == 1.0;
    let rounded = if tie_rounding_down { magnitude + 0.05 } else { magnitude };
    let sign = if value < 0.0 { "-" } else { "" };
    format!("{sign}{rounded:.1}")
}

/// Elapsed milliseconds between two timestamps when both are present.
pub fn elapsed_ms(started_at: Option<&str>, completed_at: Option<&str>) -> Option<i64> {
    Some(parse_timestamp(completed_at?)? - parse_timestamp(started_at?)?)
}

fn seconds(ms: i64) -> String {
    format!("{}s", to_fixed_1(ms as f64 / 1_000.0))
}

/// Relative age used on the dashboard.
pub fn time_ago(now_ms: i64, date: Option<&str>) -> String {
    let Some(then) = date.and_then(parse_timestamp) else {
        return "—".to_string();
    };
    let ms = now_ms - then;
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

fn pending_duration(status: &str) -> String {
    if status == "running" { "…" } else { "—" }.to_string()
}

/// Run duration in the dashboard's recent runs.
pub fn dashboard_duration(elapsed: Option<i64>, status: &str) -> String {
    match elapsed.map(|ms| ms.max(0)) {
        Some(ms) if ms >= 1_000 => seconds(ms),
        Some(ms) => format!("{ms}ms"),
        None => pending_duration(status),
    }
}

/// Run duration in the runs table.
pub fn run_duration(elapsed: Option<i64>, status: &str) -> String {
    match elapsed {
        Some(ms) if ms < 1_000 => format!("{ms}ms"),
        Some(ms) if ms < MINUTE_MS => seconds(ms),
        Some(ms) => {
            let rest_seconds = ((ms % MINUTE_MS) as f64 / 1_000.0).round() as i64;
            format!("{}m {rest_seconds}s", ms / MINUTE_MS)
        }
        None => pending_duration(status),
    }
}

/// Run duration in the run summary.
pub fn summary_duration(elapsed: Option<i64>, status: &str) -> String {
    match elapsed {
        Some(ms) => seconds(ms),
        None if status == "running" => "Running…".to_string(),
        None => "—".to_string(),
    }
}

/// Whole days between two instants, as used to pick a relative date label.
pub fn days_between(now_ms: i64, then_ms: i64) -> i64 {
    (now_ms - then_ms).div_euclid(DAY_MS)
}

/// Human-readable byte size.
pub fn format_size(bytes: usize) -> String {
    const KIB: usize = 1024;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < KIB * KIB {
        format!("{} KB", to_fixed_1(bytes as f64 / KIB as f64))
    } else {
        format!("{} MB", to_fixed_1(bytes as f64 / (KIB * KIB) as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_date_to_json_timestamps() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_timestamp("2026-10-06T12:34:56.789Z"), Some(1_791_290_096_789));
        assert_eq!(parse_timestamp("2024-02-29T23:59:59Z"), Some(1_709_251_199_000));
        assert_eq!(parse_timestamp("1969-12-31T23:59:59.999Z"), Some(-1));
    }

    #[test]
    fn parses_offsets_and_fraction_lengths() {
        assert_eq!(
            parse_timestamp("2026-10-06T14:34:56.7+02:00"),
            parse_timestamp("2026-10-06T12:34:56.700Z")
        );
        assert_eq!(
            parse_timestamp("2026-10-06T12:34:56.789123Z"),
            parse_timestamp("2026-10-06T12:34:56.789Z")
        );
        assert_eq!(
            parse_timestamp("2026-10-06T07:04:56-0530"),
            parse_timestamp("2026-10-06T12:34:56Z")
        );
    }

    #[test]
    fn rejects_malformed_timestamps() {
        for value in [
            "",
            "not a date",
            "2026-10-06",
            "2026-13-06T00:00:00Z",
            "2026-10-06T00:00:00.Z",
            "2026-10-06T00:00:00X",
        ] {
            assert_eq!(parse_timestamp(value), None, "{value}");
        }
    }

    #[test]
    fn to_fixed_matches_javascript() {
        assert_eq!(to_fixed_1(0.25), "0.3");
        assert_eq!(to_fixed_1(0.75), "0.8");
        assert_eq!(to_fixed_1(1.15), "1.1");
        assert_eq!(to_fixed_1(1.0), "1.0");
        assert_eq!(to_fixed_1(59.999), "60.0");
        assert_eq!(to_fixed_1(-1.25), "-1.3");
        assert_eq!(to_fixed_1(-0.01), "-0.0");
    }

    #[test]
    fn formats_time_ago() {
        let now = parse_timestamp("2026-10-06T12:00:00.000Z").unwrap();
        assert_eq!(time_ago(now, None), "—");
        assert_eq!(time_ago(now, Some("2026-10-06T11:59:30.000Z")), "just now");
        assert_eq!(time_ago(now, Some("2026-10-06T12:05:00.000Z")), "just now");
        assert_eq!(time_ago(now, Some("2026-10-06T11:15:00.000Z")), "45m ago");
        assert_eq!(time_ago(now, Some("2026-10-06T07:00:01.000Z")), "4h ago");
        assert_eq!(time_ago(now, Some("2026-10-03T12:00:00.000Z")), "3d ago");
    }

    #[test]
    fn formats_durations_per_screen() {
        let elapsed = elapsed_ms(Some("2026-10-06T12:00:00.000Z"), Some("2026-10-06T12:00:01.250Z"));
        assert_eq!(elapsed, Some(1_250));
        assert_eq!(elapsed_ms(Some("2026-10-06T12:00:00.000Z"), None), None);

        assert_eq!(dashboard_duration(Some(1_250), "completed"), "1.3s");
        assert_eq!(dashboard_duration(Some(999), "completed"), "999ms");
        assert_eq!(dashboard_duration(Some(-5), "completed"), "0ms");
        assert_eq!(dashboard_duration(None, "running"), "…");
        assert_eq!(dashboard_duration(None, "pending"), "—");

        assert_eq!(run_duration(Some(999), "completed"), "999ms");
        assert_eq!(run_duration(Some(59_940), "completed"), "59.9s");
        assert_eq!(run_duration(Some(125_400), "failed"), "2m 5s");
        assert_eq!(run_duration(Some(119_600), "failed"), "1m 60s");
        assert_eq!(run_duration(None, "running"), "…");
        assert_eq!(run_duration(None, "sleeping"), "—");

        assert_eq!(summary_duration(Some(65_040), "completed"), "65.0s");
        assert_eq!(summary_duration(None, "running"), "Running…");
        assert_eq!(summary_duration(None, "pending"), "—");
    }

    #[test]
    fn counts_whole_days() {
        assert_eq!(days_between(DAY_MS - 1, 0), 0);
        assert_eq!(days_between(DAY_MS, 0), 1);
        assert_eq!(days_between(0, 1), -1);
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
    }
}
