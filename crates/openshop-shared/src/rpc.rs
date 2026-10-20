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

/// Decodes `%XX` escapes like JavaScript's `decodeURIComponent`: `None` when
/// an escape is cut short or not hexadecimal, or when the bytes are not UTF-8.
/// `+` stays a plus: only a form-encoded query string reads it as a space, and
/// that is `url::form_urlencoded`'s to decode.
pub fn decode_uri_component(value: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut rest = value.as_bytes();
    while let [byte, tail @ ..] = rest {
        rest = tail;
        if *byte != b'%' {
            decoded.push(*byte);
            continue;
        }
        let [high, low, tail @ ..] = rest else {
            return None;
        };
        rest = tail;
        // `to_digit` reads digits only, where `from_str_radix` takes a sign.
        decoded.push((char::from(*high).to_digit(16)? * 16 + char::from(*low).to_digit(16)?) as u8);
    }
    String::from_utf8(decoded).ok()
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
    fn decodes_like_decode_uri_component() {
        assert_eq!(decode_uri_component("save%20review").as_deref(), Some("save review"));
        assert_eq!(decode_uri_component("caf%C3%A9").as_deref(), Some("café"));
        assert_eq!(decode_uri_component("%c3%a9").as_deref(), Some("é"));
        assert_eq!(decode_uri_component("plain").as_deref(), Some("plain"));
        assert_eq!(decode_uri_component("a+b").as_deref(), Some("a+b"));
        assert_eq!(decode_uri_component("%252e").as_deref(), Some("%2e"));
        assert_eq!(decode_uri_component("%2F%00").as_deref(), Some("/\0"));
        for malformed in ["%", "a%2", "%zz", "%ff", "%+1", "%-1", "% 1", "%é1"] {
            assert_eq!(decode_uri_component(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn decoding_undoes_encoding() {
        let text = "a b/c?d=é+%\\ \u{1F600}";
        assert_eq!(decode_uri_component(&encode_uri_component(text)).as_deref(), Some(text));
    }

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(encode_uri_component("a b/c?d=é"), "a%20b%2Fc%3Fd%3D%C3%A9");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
    }
}
