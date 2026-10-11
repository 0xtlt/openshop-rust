//! URL scheme for custom admin page loaders and actions.

/// Percent-encodes like JavaScript's `encodeURIComponent`.
pub fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// `kind` is `"loader"` or `"action"`.
pub fn build_admin_function_rpc_url(kind: &str, name: &str, pathname: &str) -> String {
    // The home of the admin, `/`, has no segment of its own.
    let page = pathname.strip_suffix('/').unwrap_or(pathname);
    format!("/api/pages/custom{page}/_rpc/{kind}/{}", encode_uri_component(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_rpc_urls_for_static_and_dynamic_pages() {
        assert_eq!(
            build_admin_function_rpc_url("loader", "listReviews", "/reviews"),
            "/api/pages/custom/reviews/_rpc/loader/listReviews"
        );
        assert_eq!(
            build_admin_function_rpc_url("action", "save review", "/reviews/42/"),
            "/api/pages/custom/reviews/42/_rpc/action/save%20review"
        );
    }

    #[test]
    fn builds_the_rpc_url_of_the_home_page() {
        assert_eq!(
            build_admin_function_rpc_url("loader", "summary", "/"),
            "/api/pages/custom/_rpc/loader/summary"
        );
    }

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(encode_uri_component("a b/c?d=é"), "a%20b%2Fc%3Fd%3D%C3%A9");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
    }
}
