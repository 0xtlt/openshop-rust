//! The kind of a function of a custom admin page, and the encoding of a
//! segment of a URL, as JavaScript's `encodeURIComponent` writes it.

/// A loader, a read, or an action, a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdminFunctionKind {
    Loader,
    Action,
}

impl AdminFunctionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loader => "loader",
            Self::Action => "action",
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

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
