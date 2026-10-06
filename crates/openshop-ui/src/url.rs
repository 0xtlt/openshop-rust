//! URL parsing: the browser's own `URL` on wasm, the `url` crate for native test builds.

/// The parts of a resolved URL the UI reasons about, named like the `URL` properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUrl {
    pub origin: String,
    pub pathname: String,
    pub search: String,
    pub hash: String,
}

/// `new URL(href, base)`, or `None` when it throws.
#[cfg(target_arch = "wasm32")]
pub fn resolve(href: &str, base: &str) -> Option<ResolvedUrl> {
    let url = web_sys::Url::new_with_base(href, base).ok()?;
    Some(ResolvedUrl {
        origin: url.origin(),
        pathname: url.pathname(),
        search: url.search(),
        hash: url.hash(),
    })
}

/// `new URL(href, base)`, or `None` when it throws.
#[cfg(not(target_arch = "wasm32"))]
pub fn resolve(href: &str, base: &str) -> Option<ResolvedUrl> {
    let base = url::Url::parse(base).ok()?;
    let url = base.join(href).ok()?;
    let prefixed = |prefix: char, part: Option<&str>| match part {
        Some(part) if !part.is_empty() => format!("{prefix}{part}"),
        _ => String::new(),
    };
    Some(ResolvedUrl {
        origin: url.origin().ascii_serialization(),
        pathname: url.path().to_string(),
        search: prefixed('?', url.query()),
        hash: prefixed('#', url.fragment()),
    })
}

/// `decodeURIComponent(value)`; malformed input is returned unchanged instead of throwing.
pub fn decode_uri_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = |offset: usize| bytes.get(index + offset).and_then(|byte| (*byte as char).to_digit(16));
            let (Some(high), Some(low)) = (hex(1), hex(2)) else {
                return value.to_string();
            };
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_relative_and_absolute_urls() {
        let url = resolve("/flows/sync?q=failed#latest", "https://example.test").unwrap();
        assert_eq!(url.origin, "https://example.test");
        assert_eq!(url.pathname, "/flows/sync");
        assert_eq!(url.search, "?q=failed");
        assert_eq!(url.hash, "#latest");

        let external = resolve("//evil.test/x", "https://example.test").unwrap();
        assert_eq!(external.origin, "https://evil.test");
        assert_eq!(
            resolve("javascript:alert(1)", "https://example.test").unwrap().origin,
            "null"
        );
    }

    #[test]
    fn decodes_uri_components() {
        assert_eq!(
            decode_uri_component("gid%3A%2F%2Fshopify%2FDiscountNode%2F1"),
            "gid://shopify/DiscountNode/1"
        );
        assert_eq!(decode_uri_component("caf%C3%A9+au+lait"), "café+au+lait");
        assert_eq!(decode_uri_component("100%"), "100%");
        assert_eq!(decode_uri_component("%FF"), "%FF");
    }
}
