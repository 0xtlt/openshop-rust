//! The charts of the operator dashboard, drawn by the server as inline SVG:
//! counts over time, one point per bucket of the period.

use chrono::{DateTime, Duration, Utc};

/// The counts of one thing, bucket by bucket.
pub(crate) struct Series<'a> {
    /// What a count is of, in the plural: `runs`, `failed`.
    pub name: &'a str,
    /// The class that colors the line: `total` or `bad`.
    pub class: &'a str,
    pub values: &'a [i64],
}

/// The size a chart is drawn at. Its text scales with it: a chart drawn for a
/// wide page is unreadable on a phone, which gets a drawing of its own.
struct Shape {
    class: &'static str,
    width: f64,
    /// Time labels under the axis.
    time_labels: usize,
}

const WIDE: Shape = Shape {
    class: "wide",
    width: 1000.0,
    time_labels: 7,
};
const NARROW: Shape = Shape {
    class: "narrow",
    width: 420.0,
    time_labels: 4,
};

const HEIGHT: f64 = 220.0;
const LEFT: f64 = 48.0;
/// The room kept at the end of the axis.
const END: f64 = 8.0;
const TOP: f64 = 12.0;
const BOTTOM: f64 = 186.0;
/// Horizontal lines above the axis.
const DIVISIONS: i64 = 5;

/// The top of the scale: five steps of 1, 2 or 5 times a power of ten, the
/// smallest that hold `max`.
fn ceiling(max: i64) -> i64 {
    let mut magnitude = 1;
    loop {
        for unit in [1, 2, 5] {
            let top = unit * magnitude * DIVISIONS;
            if top >= max {
                return top;
            }
        }
        magnitude *= 10;
    }
}

/// `1,250`: digits in groups of three.
pub(crate) fn grouped(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if value < 0 { format!("-{grouped}") } else { grouped }
}

/// How a bucket is named: the hour within a day, the day beyond.
fn time_label(at: &DateTime<Utc>, bucket: Duration, buckets: usize) -> String {
    let period = bucket * buckets as i32;
    if period <= Duration::hours(24) {
        at.format("%H:%M").to_string()
    } else if bucket < Duration::hours(12) {
        at.format("%b %d, %H:%M").to_string()
    } else {
        at.format("%b %d").to_string()
    }
}

/// A chart of `series` over the buckets that start at `start`, drawn twice:
/// for a wide page and for a narrow one, the style sheet showing the one that
/// fits. The first series is filled under its line. Each bucket says its
/// counts on hover.
pub(crate) fn area_chart(start: DateTime<Utc>, bucket: Duration, series: &[Series<'_>]) -> String {
    [WIDE, NARROW]
        .iter()
        .map(|shape| draw(shape, start, bucket, series))
        .collect()
}

fn draw(shape: &Shape, start: DateTime<Utc>, bucket: Duration, series: &[Series<'_>]) -> String {
    let buckets = series.iter().map(|series| series.values.len()).max().unwrap_or(0);
    if buckets < 2 {
        return String::new();
    }
    let (width, right) = (shape.width, shape.width - END);
    let top = ceiling(
        series
            .iter()
            .flat_map(|series| series.values)
            .copied()
            .max()
            .unwrap_or(0),
    );
    let x = |index: usize| LEFT + (right - LEFT) * index as f64 / (buckets - 1) as f64;
    let y = |value: i64| BOTTOM - (BOTTOM - TOP) * value as f64 / top as f64;

    let mut svg = format!(
        "<svg class=\"chart {}\" viewBox=\"0 0 {width} {HEIGHT}\" role=\"img\" aria-label=\"{}\">",
        shape.class,
        series
            .iter()
            .map(|series| series.name)
            .collect::<Vec<_>>()
            .join(" and ")
            + " over time"
    );

    for division in 0..=DIVISIONS {
        let value = top * division / DIVISIONS;
        let line = y(value);
        svg.push_str(&format!(
            "<line class=\"grid\" x1=\"{LEFT}\" x2=\"{right}\" y1=\"{line:.1}\" y2=\"{line:.1}\"/>\
             <text class=\"axis\" x=\"{:.0}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
            LEFT - 8.0,
            line + 3.5,
            grouped(value)
        ));
    }
    let mut labeled = None;
    for label in 0..shape.time_labels {
        let index = (buckets - 1) * label / (shape.time_labels - 1);
        // Fewer buckets than labels: a bucket is named once.
        if labeled.replace(index) == Some(index) {
            continue;
        }
        let anchor = match label {
            0 => "start",
            label if label == shape.time_labels - 1 => "end",
            _ => "middle",
        };
        svg.push_str(&format!(
            "<text class=\"axis\" x=\"{:.1}\" y=\"{:.0}\" text-anchor=\"{anchor}\">{}</text>",
            x(index),
            BOTTOM + 20.0,
            time_label(&(start + bucket * index as i32), bucket, buckets)
        ));
    }

    for (position, series) in series.iter().enumerate() {
        let points: Vec<String> = series
            .values
            .iter()
            .enumerate()
            .map(|(index, value)| format!("{:.1},{:.1}", x(index), y(*value)))
            .collect();
        if position == 0 {
            svg.push_str(&format!(
                "<path class=\"area {}\" d=\"M{LEFT},{BOTTOM} L{} L{right},{BOTTOM} Z\"/>",
                series.class,
                points.join(" L")
            ));
        }
        svg.push_str(&format!(
            "<path class=\"line {}\" d=\"M{}\"/>",
            series.class,
            points.join(" L")
        ));
    }

    // One strip per bucket: the browser shows its title on hover.
    let strip = (right - LEFT) / (buckets - 1) as f64;
    for index in 0..buckets {
        let counts: Vec<String> = series
            .iter()
            .map(|series| {
                format!(
                    "{} {}",
                    grouped(series.values.get(index).copied().unwrap_or(0)),
                    series.name
                )
            })
            .collect();
        svg.push_str(&format!(
            "<rect class=\"hit\" x=\"{:.1}\" y=\"{TOP}\" width=\"{strip:.1}\" height=\"{:.0}\"><title>{}: {}</title></rect>",
            x(index) - strip / 2.0,
            BOTTOM - TOP,
            (start + bucket * index as i32).format("%b %d, %H:%M"),
            counts.join(", ")
        ));
    }
    svg.push_str("</svg>");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_scale_ends_on_a_round_number_that_holds_the_largest_count() {
        for (max, top) in [
            (0, 5),
            (1, 5),
            (5, 5),
            (6, 10),
            (11, 25),
            (26, 50),
            (2_100, 2_500),
            (2_501, 5_000),
        ] {
            assert_eq!(ceiling(max), top, "{max}");
        }
    }

    #[test]
    fn groups_digits_by_three() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1_250), "1,250");
        assert_eq!(grouped(1_234_567), "1,234,567");
    }

    #[test]
    fn draws_each_series_on_one_scale_and_says_the_counts_of_a_bucket() {
        let start = Utc.with_ymd_and_hms(2026, 10, 10, 8, 0, 0).unwrap();
        let svg = area_chart(
            start,
            Duration::hours(1),
            &[
                Series {
                    name: "runs",
                    class: "total",
                    values: &[0, 10, 5],
                },
                Series {
                    name: "failed",
                    class: "bad",
                    values: &[0, 5, 0],
                },
            ],
        );
        assert!(
            svg.starts_with("<svg class=\"chart wide\" viewBox=\"0 0 1000 220\""),
            "{svg}"
        );
        // The same chart follows, drawn for a narrow screen.
        assert!(
            svg.contains("</svg><svg class=\"chart narrow\" viewBox=\"0 0 420 220\""),
            "{svg}"
        );
        assert!(
            svg.contains("<path class=\"line total\" d=\"M48.0,186.0 L230.0,12.0 L412.0,99.0\"/>"),
            "{svg}"
        );
        assert!(svg.contains("aria-label=\"runs and failed over time\""), "{svg}");
        // 10 is the top of the scale: the second point of the runs touches it, half way for the failures.
        assert!(
            svg.contains("<path class=\"line total\" d=\"M48.0,186.0 L520.0,12.0 L992.0,99.0\"/>"),
            "{svg}"
        );
        assert!(
            svg.contains("<path class=\"line bad\" d=\"M48.0,186.0 L520.0,99.0 L992.0,186.0\"/>"),
            "{svg}"
        );
        // Only the first series is filled.
        assert_eq!(svg.matches("class=\"area ").count(), 2, "{svg}");
        assert!(svg.contains("<title>Oct 10, 09:00: 10 runs, 5 failed</title>"), "{svg}");
        assert!(svg.contains(">10</text>"), "{svg}");
        assert!(svg.contains(">08:00</text>"), "{svg}");
        assert!(svg.ends_with("</svg>"));
    }

    #[test]
    fn names_the_day_when_the_period_is_longer_than_one() {
        let at = Utc.with_ymd_and_hms(2026, 10, 10, 14, 0, 0).unwrap();
        assert_eq!(time_label(&at, Duration::minutes(30), 48), "14:00");
        assert_eq!(time_label(&at, Duration::hours(3), 56), "Oct 10, 14:00");
        assert_eq!(time_label(&at, Duration::hours(12), 60), "Oct 10");
    }

    #[test]
    fn a_chart_needs_two_buckets() {
        let start = Utc.with_ymd_and_hms(2026, 10, 10, 8, 0, 0).unwrap();
        let single = [Series {
            name: "runs",
            class: "total",
            values: &[3],
        }];
        assert_eq!(area_chart(start, Duration::hours(1), &single), "");
        assert_eq!(area_chart(start, Duration::hours(1), &[]), "");
    }
}
