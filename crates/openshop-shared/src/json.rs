//! JavaScript's rendering of JSON values.

use serde_json::{Number, Value};

/// `String(value)` for a parsed JSON value.
pub fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => js_number(number),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| if item.is_null() { String::new() } else { js_string(item) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// `String(number)`. JavaScript reads every JSON number as a double and
/// writes it with the fewest digits that read back as the same double, in
/// exponent notation below `1e-6` and from `1e21` only.
fn js_number(number: &Number) -> String {
    let Some(value) = number.as_f64() else {
        return number.to_string();
    };
    if value == 0.0 {
        return "0".to_string();
    }
    // `{:e}` writes the fewest digits too, as `d.ddde±x`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits = mantissa.replace('.', "");
    let exponent: i32 = exponent.parse().unwrap_or_default();
    // The value is `0.digits` × 10^point, and `digits` has `count` of them.
    let (point, count) = (exponent + 1, digits.len() as i32);
    let sign = if value < 0.0 { "-" } else { "" };
    let text = if count <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - count) as usize))
    } else if 0 < point && point <= 21 {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{whole}.{fraction}")
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat(-point as usize))
    } else {
        let mantissa = match digits.split_at(1) {
            (first, "") => first.to_string(),
            (first, rest) => format!("{first}.{rest}"),
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{exponent_sign}{}", exponent.abs())
    };
    format!("{sign}{text}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn coerces_values_to_strings() {
        assert_eq!(js_string(&json!("text")), "text");
        assert_eq!(js_string(&json!(true)), "true");
        assert_eq!(js_string(&json!(null)), "null");
        assert_eq!(js_string(&json!([1, null, "a"])), "1,,a");
        assert_eq!(js_string(&json!({ "a": 1 })), "[object Object]");
    }

    #[test]
    fn writes_numbers_like_javascript() {
        let cases = [
            (json!(42), "42"),
            (json!(-7), "-7"),
            (json!(1.5), "1.5"),
            (json!(2.0), "2"),
            (json!(-0.0), "0"),
            (json!(0.1), "0.1"),
            (json!(123.456), "123.456"),
            (json!(0.000001), "0.000001"),
            (json!(0.0000015), "0.0000015"),
            (json!(1.5e-7), "1.5e-7"),
            (json!(1e-7), "1e-7"),
            // Past `i64::MAX`, where a cast to an integer saturates.
            (json!(1e20), "100000000000000000000"),
            (json!(-9.3e18), "-9300000000000000000"),
            (json!(1e21), "1e+21"),
            (json!(1.25e22), "1.25e+22"),
            // A double has no more than 17 significant digits.
            (json!(1_152_921_504_606_846_976_u64), "1152921504606847000"),
            (json!(u64::MAX), "18446744073709552000"),
        ];
        for (value, expected) in cases {
            assert_eq!(js_string(&value), expected, "{value}");
        }
    }
}
