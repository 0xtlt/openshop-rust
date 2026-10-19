//! Ready-made field validators for provider and Shopify Function configuration.
//!
//! A validator returns the (possibly normalized) value or a short summary that
//! is shown to the merchant after the field name.

use serde_json::Value;

type Validator = Box<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;

pub(crate) fn describe(value: &Value) -> String {
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

/// An absolute `http` or `https` URL with a host.
///
/// Other schemes (`file:`, `gopher:`, `ftp:`…) are refused because the server
/// fetches these URLs. The host is not restricted: use [`public_url`] when the
/// merchant must not be able to point the server at its own network.
pub fn url() -> Validator {
    Box::new(|value| {
        let Some(parsed) = value.as_str().and_then(|text| url::Url::parse(text).ok()) else {
            return Err(format!("must be a URL string (was {})", describe(value)));
        };
        if is_http_with_host(&parsed) {
            Ok(value.clone())
        } else {
            Err(format!("must be an http or https URL (was {})", describe(value)))
        }
    })
}

/// Like [`url`], and the host must not be on the server's own network.
///
/// Refused: `localhost`, `*.localhost`, `metadata.google.internal`, and hosts
/// written as an IP address in a loopback, private (`10/8`, `172.16/12`,
/// `192.168/16`, `fc00::/7`), link-local (`169.254/16`, `fe80::/10`, which holds
/// the cloud metadata endpoint), shared (`100.64/10`), multicast, broadcast or
/// unspecified range. Odd spellings such as `http://2130706433/` or
/// `http://0x7f.1/` are read as the IP address they mean.
///
/// This only looks at the text of the URL. A public name that resolves to a
/// private address (DNS rebinding) passes: also check the address the
/// connection is made to when the request is sent.
pub fn public_url() -> Validator {
    Box::new(|value| {
        let Some(parsed) = value.as_str().and_then(|text| url::Url::parse(text).ok()) else {
            return Err(format!("must be a URL string (was {})", describe(value)));
        };
        if !is_http_with_host(&parsed) {
            return Err(format!("must be an http or https URL (was {})", describe(value)));
        }
        let internal = match parsed.host() {
            Some(url::Host::Domain(domain)) => is_internal_domain(domain),
            Some(url::Host::Ipv4(address)) => is_internal_ipv4(address),
            Some(url::Host::Ipv6(address)) => is_internal_ipv6(address),
            None => true,
        };
        if internal {
            Err(format!("must be a public http or https URL (was {})", describe(value)))
        } else {
            Ok(value.clone())
        }
    })
}

fn is_http_with_host(parsed: &url::Url) -> bool {
    matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some_and(|host| !host.is_empty())
}

fn is_internal_domain(domain: &str) -> bool {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    domain.is_empty() || domain == "localhost" || domain.ends_with(".localhost") || domain == "metadata.google.internal"
}

fn is_internal_ipv4(address: std::net::Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_broadcast()
        || address.is_multicast()
        || first == 0
        || (first == 100 && (64..128).contains(&second))
}

fn is_internal_ipv6(address: std::net::Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_internal_ipv4(mapped);
    }
    let first = address.segments()[0];
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || first & 0xffc0 == 0xfe80
        || first & 0xfe00 == 0xfc00
}

/// Any finite number.
pub fn number()-> Validator {
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
    fn url_accepts_only_http_and_https_with_a_host() {
        for ok in [
            "http://warehouse.test",
            "https://warehouse.test/api?x=1",
            "HTTPS://Warehouse.test:8443/",
            "http://localhost:3000",
            "http://10.0.0.1/",
        ] {
            assert!(url()(&json!(ok)).is_ok(), "{ok}");
        }
        for bad in [
            "file:///etc/passwd",
            "gopher://warehouse.test:70/_x",
            "ftp://warehouse.test/",
            "javascript:alert(1)",
            "data:text/plain,hi",
            "mailto:a@b.test",
            "http://",
        ] {
            assert!(url()(&json!(bad)).is_err(), "{bad}");
        }
        assert_eq!(
            url()(&json!("file:///etc/passwd")).unwrap_err(),
            "must be an http or https URL (was \"file:///etc/passwd\")"
        );
        assert!(url()(&json!(12)).is_err());
    }

    #[test]
    fn public_url_refuses_hosts_on_the_server_network() {
        for ok in [
            "https://warehouse.test/api",
            "http://93.184.216.34/",
            "https://[2606:4700:4700::1111]/",
            "http://172.32.0.1/",
            "http://100.63.0.1/",
        ] {
            assert!(public_url()(&json!(ok)).is_ok(), "{ok}");
        }
        for bad in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:8080/",
            "http://127.1/",
            "http://2130706433/",
            "http://0x7f.0.0.1/",
            "http://[::1]/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://172.31.255.255/",
            "http://192.168.1.1/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://localhost/",
            "http://LOCALHOST:3000/",
            "http://localhost./",
            "http://admin.localhost/",
            "http://metadata.google.internal/computeMetadata/v1/",
            "file:///etc/passwd",
            "gopher://warehouse.test/",
        ] {
            assert!(public_url()(&json!(bad)).is_err(), "{bad}");
        }
        assert_eq!(
            public_url()(&json!("http://169.254.169.254/")).unwrap_err(),
            "must be a public http or https URL (was \"http://169.254.169.254/\")"
        );
        assert_eq!(
            public_url()(&json!("nope")).unwrap_err(),
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
