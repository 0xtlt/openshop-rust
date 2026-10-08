//! Ready-made field validators for provider and Shopify Function configuration.
//!
//! A validator returns the (possibly normalized) value or a short summary that
//! is shown to the merchant after the field name.

use serde_json::Value;

type Validator = Box<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;

fn describe(value: &Value) -> String {
    match value {
        Value::String(text) => format!("\"{text}\""),
        other => other.to_string(),
    }
}

fn format_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        number.to_string()
    }
}

/// Any string.
pub fn string() -> Validator {
    Box::new(|value| match value {
        Value::String(_) => Ok(value.clone()),
        other => Err(format!("must be a string (was {})", describe(other))),
    })
}

/// A string with at least one character.
pub fn non_empty() -> Validator {
    Box::new(|value| match value {
        Value::String(text) if !text.is_empty() => Ok(value.clone()),
        Value::String(_) => Err("must be non-empty".to_string()),
        other => Err(format!("must be a string (was {})", describe(other))),
    })
}

/// An absolute URL.
pub fn url() -> Validator {
    Box::new(|value| match value {
        Value::String(text) if url::Url::parse(text).is_ok() => Ok(value.clone()),
        other => Err(format!("must be a URL string (was {})", describe(other))),
    })
}

/// Any finite number.
pub fn number() -> Validator {
    Box::new(|value| match value.as_f64() {
        Some(number) if number.is_finite() => Ok(value.clone()),
        _ => Err(format!("must be a number (was {})", describe(value))),
    })
}

/// A number within `min..=max`.
pub fn number_range(min: f64, max: f64) -> Validator {
    Box::new(move |value| match value.as_f64() {
        Some(number) if number < min => Err(format!(
            "must be at least {} (was {})",
            format_number(min),
            format_number(number)
        )),
        Some(number) if number > max => Err(format!(
            "must be at most {} (was {})",
            format_number(max),
            format_number(number)
        )),
        Some(_) => Ok(value.clone()),
        None => Err(format!("must be a number (was {})", describe(value))),
    })
}

/// A number no smaller than `min` and no larger than `max`, for the bounds
/// that are given. What `#[field(range(min = 1, max = 1000))]` checks.
pub fn range(min: Option<f64>, max: Option<f64>) -> Validator {
    Box::new(move |value| match value.as_f64() {
        Some(number) if min.is_some_and(|min| number < min) => Err(format!(
            "must be at least {} (was {})",
            format_number(min.unwrap_or_default()),
            format_number(number)
        )),
        Some(number) if max.is_some_and(|max| number > max) => Err(format!(
            "must be at most {} (was {})",
            format_number(max.unwrap_or_default()),
            format_number(number)
        )),
        Some(_) => Ok(value.clone()),
        None => Err(format!("must be a number (was {})", describe(value))),
    })
}

/// A string of at least `min` and at most `max` characters, for the bounds
/// that are given. What `#[field(length(min = 1))]` checks.
pub fn length(min: Option<usize>, max: Option<usize>) -> Validator {
    Box::new(move |value| match value.as_str().map(|text| text.chars().count()) {
        Some(length) if min.is_some_and(|min| length < min) => Err(format!(
            "must be at least {} characters (was {length})",
            min.unwrap_or_default()
        )),
        Some(length) if max.is_some_and(|max| length > max) => Err(format!(
            "must be at most {} characters (was {length})",
            max.unwrap_or_default()
        )),
        Some(_) => Ok(value.clone()),
        None => Err(format!("must be a string (was {})", describe(value))),
    })
}

/// A whole number.
pub fn integer() -> Validator {
    Box::new(|value| match value.as_f64() {
        Some(number) if number.fract() != 0.0 => Err(format!("must be an integer (was {})", format_number(number))),
        Some(_) => Ok(value.clone()),
        None => Err(format!("must be a number (was {})", describe(value))),
    })
}

/// A whole number greater than or equal to `min`.
pub fn integer_min(min: i64) -> Validator {
    Box::new(move |value| match value.as_f64() {
        Some(number) if number.fract() != 0.0 => Err(format!("must be an integer (was {})", format_number(number))),
        Some(number) if number < min as f64 => Err(format!("must be at least {min} (was {})", format_number(number))),
        Some(_) => Ok(value.clone()),
        None => Err(format!("must be a number (was {})", describe(value))),
    })
}

/// One of the listed strings.
pub fn one_of<const N: usize>(options: [&'static str; N]) -> Validator {
    Box::new(move |value| match value.as_str() {
        Some(text) if options.contains(&text) => Ok(value.clone()),
        _ => Err(format!(
            "must be one of {} (was {})",
            options.map(|option| format!("\"{option}\"")).join(", "),
            describe(value)
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_strings_and_urls() {
        assert!(string()(&json!("x")).is_ok());
        assert_eq!(string()(&json!(1)).unwrap_err(), "must be a string (was 1)");
        assert!(non_empty()(&json!("x")).is_ok());
        assert_eq!(non_empty()(&json!("")).unwrap_err(), "must be non-empty");
        assert!(url()(&json!("https://warehouse.example.com/api")).is_ok());
        assert_eq!(
            url()(&json!("nope")).unwrap_err(),
            "must be a URL string (was \"nope\")"
        );
    }

    #[test]
    fn validates_numbers() {
        assert!(number()(&json!(1.5)).is_ok());
        assert!(number()(&json!("1")).is_err());
        assert!(number_range(1.0, 1000.0)(&json!(100)).is_ok());
        assert_eq!(
            number_range(1.0, 1000.0)(&json!(0)).unwrap_err(),
            "must be at least 1 (was 0)"
        );
        assert_eq!(
            number_range(1.0, 1000.0)(&json!(1001)).unwrap_err(),
            "must be at most 1000 (was 1001)"
        );
        assert!(integer()(&json!(-3)).is_ok());
        assert_eq!(integer()(&json!(2.5)).unwrap_err(), "must be an integer (was 2.5)");
        assert!(integer()(&json!("3")).is_err());
        assert!(integer_min(1)(&json!(3)).is_ok());
        assert_eq!(integer_min(1)(&json!(1.5)).unwrap_err(), "must be an integer (was 1.5)");
        assert_eq!(integer_min(1)(&json!(0)).unwrap_err(), "must be at least 1 (was 0)");
    }

    #[test]
    fn validates_bounds_that_are_given() {
        assert!(range(Some(1.0), Some(1000.0))(&json!(100)).is_ok());
        assert_eq!(
            range(Some(1.0), None)(&json!(0)).unwrap_err(),
            "must be at least 1 (was 0)"
        );
        assert!(range(Some(1.0), None)(&json!(1_000_000)).is_ok());
        assert_eq!(
            range(None, Some(100.0))(&json!(101)).unwrap_err(),
            "must be at most 100 (was 101)"
        );
        assert!(range(None, None)(&json!("x")).is_err());

        assert!(length(Some(1), Some(3))(&json!("abc")).is_ok());
        assert_eq!(
            length(Some(1), None)(&json!("")).unwrap_err(),
            "must be at least 1 characters (was 0)"
        );
        assert_eq!(
            length(None, Some(3))(&json!("abcd")).unwrap_err(),
            "must be at most 3 characters (was 4)"
        );
        assert!(length(Some(1), None)(&json!(1)).is_err());
    }

    #[test]
    fn validates_choices() {
        assert!(one_of(["a", "b"])(&json!("a")).is_ok());
        assert_eq!(
            one_of(["a", "b"])(&json!("c")).unwrap_err(),
            "must be one of \"a\", \"b\" (was \"c\")"
        );
    }
}
