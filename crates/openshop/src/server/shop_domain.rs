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
}
