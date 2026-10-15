//! Shop domain normalization: only `*.myshopify.com` hosts are accepted.

use std::sync::LazyLock;

use regex::Regex;

static SHOP_DOMAIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*\.myshopify\.com$").expect("valid regex"));

/// Accepts a bare domain or a URL and returns the lowercase shop hostname.
pub fn normalize_shop_domain(value: &str) -> Option<String> {
    let input = value.trim().to_lowercase();
    if input.is_empty() {
        return None;
    }

    let candidate = if input.contains("://") {
        input
    } else {
        format!("https://{input}")
    };
    let url = url::Url::parse(&candidate).ok()?;
    let hostname = url.host_str()?.to_lowercase();

    SHOP_DOMAIN_RE.is_match(&hostname).then_some(hostname)
}

/// A shop domain written exactly as one: `<name>.myshopify.com`, or a bare
/// `<name>` it expands to, trimmed and lowercased. Unlike
/// [`normalize_shop_domain`], it reads no URL: a scheme, a path, a port, a
/// user, a query, a fragment or a trailing dot make it `None`.
///
/// Every address a token is sent to is built from what it returns.
pub fn strict_shop_domain(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    let domain = if value.contains('.') {
        value
    } else {
        format!("{value}.myshopify.com")
    };
    SHOP_DOMAIN_RE.is_match(&domain).then_some(domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_domains_and_urls() {
        assert_eq!(
            normalize_shop_domain("Test.MyShopify.com").as_deref(),
            Some("test.myshopify.com")
        );
        assert_eq!(
            normalize_shop_domain("https://test.myshopify.com/admin").as_deref(),
            Some("test.myshopify.com")
        );
        assert_eq!(
            normalize_shop_domain("  test-shop.myshopify.com  ").as_deref(),
            Some("test-shop.myshopify.com")
        );
    }

    #[test]
    fn rejects_foreign_or_malformed_hosts() {
        for value in [
            "",
            "   ",
            "example.com",
            "test.myshopify.com.evil.com",
            "evil.com/test.myshopify.com",
            "-bad.myshopify.com",
            "a.b.myshopify.com",
            "test.myshopify.io",
            "https://",
            "test.myshopify.com@evil.com",
        ] {
            assert_eq!(normalize_shop_domain(value), None, "{value}");
        }
    }

    #[test]
    fn strict_domains_are_a_shop_and_nothing_else() {
        for (value, expected) in [
            ("dev-store-1.myshopify.com", "dev-store-1.myshopify.com"),
            ("  Dev-Store-1.MyShopify.com ", "dev-store-1.myshopify.com"),
            ("dev-store-1", "dev-store-1.myshopify.com"),
            ("DEV-STORE-1", "dev-store-1.myshopify.com"),
        ] {
            assert_eq!(strict_shop_domain(value).as_deref(), Some(expected), "{value}");
        }
        for value in [
            "",
            "   ",
            "evil.example://dev-store-1.myshopify.com",
            "https://dev-store-1.myshopify.com",
            "https://dev-store-1.myshopify.com/admin",
            "dev-store-1.myshopify.com:8443",
            "dev-store-1.myshopify.com:443",
            "evil.example@dev-store-1.myshopify.com",
            "dev-store-1.myshopify.com@evil.example",
            "dev-store-1.myshopify.com/evil",
            "dev-store-1.myshopify.com?x=1",
            "dev-store-1.myshopify.com#x",
            "dev-store-1.myshopify.com.",
            "dev-store-1.myshopify.com.evil.example",
            "a.b.myshopify.com",
            "-dev.myshopify.com",
            "dev store.myshopify.com",
            "dev-store-1.myshopify.com\nx",
            "evil.example",
            "dev:8443",
            "dev/x",
            "dev@x",
            "d\u{212A}.myshopify.com",
        ] {
            assert_eq!(strict_shop_domain(value), None, "{value}");
        }
    }
}
