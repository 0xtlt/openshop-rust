//! `URLSearchParams`-compatible query strings.

use std::fmt;

/// Ordered query parameters with the parsing and serialization rules of `URLSearchParams`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QueryParams(Vec<(String, String)>);

fn decode(component: &str) -> String {
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |offset: usize| bytes.get(index + offset).and_then(|byte| (*byte as char).to_digit(16));
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' => match (hex(1), hex(2)) {
                (Some(high), Some(low)) => {
                    decoded.push((high * 16 + low) as u8);
                    index += 2;
                }
                _ => decoded.push(b'%'),
            },
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn encode(component: &str, output: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in component.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'*' | b'-' | b'.' | b'_' => write!(output, "{}", byte as char)?,
            b' ' => output.write_str("+")?,
            _ => write!(output, "%{byte:02X}")?,
        }
    }
    Ok(())
}

impl QueryParams {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses `location.search`, with or without the leading `?`.
    pub fn parse(search: &str) -> Self {
        let search = search.strip_prefix('?').unwrap_or(search);
        let pairs = search
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(name), decode(value))
            })
            .collect();
        Self(pairs)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn set(&mut self, name: &str, value: &str) {
        match self.0.iter().position(|(key, _)| key == name) {
            Some(first) => {
                self.0[first].1 = value.to_string();
                let mut index = 0;
                self.0.retain(|(key, _)| {
                    index += 1;
                    index - 1 == first || key != name
                });
            }
            None => self.0.push((name.to_string(), value.to_string())),
        }
    }

    pub fn delete(&mut self, name: &str) {
        self.0.retain(|(key, _)| key != name);
    }

    /// `path` followed by `?query` when there is one.
    pub fn append_to(&self, path: &str) -> String {
        if self.0.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{self}")
        }
    }
}

impl fmt::Display for QueryParams {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, (name, value)) in self.0.iter().enumerate() {
            if index > 0 {
                output.write_str("&")?;
            }
            encode(name, output)?;
            output.write_str("=")?;
            encode(value, output)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_like_url_search_params() {
        let mut params = QueryParams::new();
        params.set("limit", "26");
        params.set("search", "sync orders & more");
        params.set("q", "|= \"é\" C:3");
        assert_eq!(
            params.to_string(),
            "limit=26&search=sync+orders+%26+more&q=%7C%3D+%22%C3%A9%22+C%3A3"
        );
        assert_eq!(QueryParams::new().to_string(), "");
    }

    #[test]
    fn parses_and_round_trips() {
        let params = QueryParams::parse("?embedded=1&q=a+b%20c&levels=info%2Cwarn&flag");
        assert_eq!(params.get("embedded"), Some("1"));
        assert_eq!(params.get("q"), Some("a b c"));
        assert_eq!(params.get("levels"), Some("info,warn"));
        assert_eq!(params.get("flag"), Some(""));
        assert_eq!(params.get("missing"), None);
        assert_eq!(params.to_string(), "embedded=1&q=a+b+c&levels=info%2Cwarn&flag=");
    }

    #[test]
    fn set_replaces_the_first_and_drops_duplicates() {
        let mut params = QueryParams::parse("q=1&shop=x&q=2");
        params.set("q", "3");
        assert_eq!(params.to_string(), "q=3&shop=x");
        params.delete("q");
        assert_eq!(params.to_string(), "shop=x");
    }

    #[test]
    fn appends_to_paths() {
        let mut params = QueryParams::new();
        assert_eq!(params.append_to("/flows"), "/flows");
        params.set("q", "failed");
        assert_eq!(params.append_to("/flows"), "/flows?q=failed");
    }

    #[test]
    fn tolerates_malformed_escapes() {
        assert_eq!(QueryParams::parse("q=100%&r=%zz").get("q"), Some("100%"));
        assert_eq!(QueryParams::parse("q=100%&r=%zz").get("r"), Some("%zz"));
    }
}
