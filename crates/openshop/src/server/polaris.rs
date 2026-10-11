//! Shopify's Polaris web components, for the page the server renders
//! itself in the style of the admin: what a visitor without a shop is told.
//! Outside Shopify admin they need no App Bridge.

/// The script that defines the components.
pub(crate) const SCRIPT: &str = "https://cdn.shopify.com/shopifycloud/polaris.js";

/// A page that stays readable while Polaris is not loaded, or when Shopify's
/// CDN cannot be reached.
pub(crate) const FALLBACK_STYLE: &str = "
body { margin: 0; font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; }
s-page:not(:defined), s-section:not(:defined), s-stack:not(:defined), s-grid:not(:defined), s-box:not(:defined),
s-banner:not(:defined), s-table:not(:defined), s-table-header-row:not(:defined), s-table-row:not(:defined),
s-heading:not(:defined), s-paragraph:not(:defined) { display: block; margin: 8px 16px; }
s-heading:not(:defined) { font-weight: 600; }
s-table-header:not(:defined), s-table-cell:not(:defined) { display: inline-block; margin-right: 16px; }
";

/// The content security policy of a page written with Polaris: the script,
/// then what it loads from Shopify's CDN, each under its own path (the styles
/// and the files of its font, its icons). `$page` is the directives of the
/// page itself.
macro_rules! content_security_policy {
    ($page:literal) => {
        concat!(
            "default-src 'none'; ",
            "script-src https://cdn.shopify.com/shopifycloud/polaris.js; ",
            "style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/; ",
            "font-src https://cdn.shopify.com/static/fonts/; ",
            "connect-src https://cdn.shopify.com/shopifycloud/; ",
            $page
        )
    };
}
pub(crate) use content_security_policy;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_names_the_script_and_nothing_wider() {
        let policy = content_security_policy!("base-uri 'none'");
        assert_eq!(
            policy,
            "default-src 'none'; script-src https://cdn.shopify.com/shopifycloud/polaris.js; \
             style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/; \
             font-src https://cdn.shopify.com/static/fonts/; connect-src https://cdn.shopify.com/shopifycloud/; \
             base-uri 'none'"
        );
        assert!(policy.contains(&format!("script-src {SCRIPT}; ")));
        for forbidden in ["'unsafe-eval'", "script-src 'unsafe-inline'", " *", "  "] {
            assert!(!policy.contains(forbidden), "{forbidden}");
        }
    }
}
