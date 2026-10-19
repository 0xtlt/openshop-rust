//! Shopify request signatures.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

pub(crate) fn hmac_sha256(secret: &str, message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

/// Compares two byte strings without revealing where they differ.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn sorted_message(query: &IndexMap<String, String>, excluded: &str, separator: &str) -> String {
    let mut keys: Vec<&String> = query.keys().filter(|key| key.as_str() != excluded).collect();
    keys.sort();
    keys.into_iter()
        .map(|key| format!("{key}={}", query[key]))
        .collect::<Vec<_>>()
        .join(separator)
}

/// Verify Shopify OAuth callback HMAC.
/// Shopify signs the query string (excluding the `hmac` param) with the app secret.
pub fn verify_query_hmac(query: &IndexMap<String, String>, secret: &str) -> bool {
    if secret.trim().is_empty() {
        return false;
    }
    let Some(hmac) = query.get("hmac").filter(|hmac| !hmac.is_empty()) else {
        return false;
    };

    let computed = hex::encode(hmac_sha256(secret, sorted_message(query, "hmac", "&").as_bytes()));
    constant_time_eq(computed.as_bytes(), hmac.as_bytes())
}

/// Verify Shopify webhook HMAC.
/// Shopify sends the HMAC in the `X-Shopify-Hmac-Sha256` header.
pub fn verify_webhook_hmac(body: &str, hmac: &str, secret: &str) -> bool {
    if secret.trim().is_empty() {
        return false;
    }
    let computed = STANDARD.encode(hmac_sha256(secret, body.as_bytes()));
    constant_time_eq(computed.as_bytes(), hmac.as_bytes())
}

/// How far, in seconds, the `timestamp` of a signed app proxy request may be
/// from now, on either side to allow for clock skew. A signed URL is not a
/// credential for longer than that.
pub(crate) const PROXY_SIGNATURE_FRESHNESS: u64 = 300;

/// Parameters Shopify itself sets on an app proxy request. Each appears once:
/// a copy added by the sender would be joined with the real one in the signed
/// message, and read as another value by the handler.
const PROXY_OWN_PARAMETERS: [&str; 5] = ["logged_in_customer_id", "path_prefix", "shop", "signature", "timestamp"];

/// The message Shopify signs for an app proxy request: a `key=value` string per
/// parameter other than `signature`, the values of a repeated key joined by
/// `,`, sorted, with no separator.
fn proxy_message<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let mut grouped: IndexMap<&str, Vec<&str>> = IndexMap::new();
    for (key, value) in pairs {
        if key != "signature" {
            grouped.entry(key).or_default().push(value);
        }
    }
    let mut parts: Vec<String> = grouped
        .into_iter()
        .map(|(key, values)| format!("{key}={}", values.join(",")))
        .collect();
    parts.sort();
    parts.concat()
}

/// Hex signature of an app proxy query (sorted `key=value` pairs, no separator).
pub fn proxy_signature(query: &IndexMap<String, String>, secret: &str) -> String {
    let message = proxy_message(query.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    hex::encode(hmac_sha256(secret, message.as_bytes()))
}

/// Whether the parameters can only be read one way from the message that is
/// signed. The pairs are glued without separator, so with a `=` in a key or a
/// value the same signature stands for another split of the same text, with
/// another `shop` for instance.
fn proxy_params_are_unambiguous(params: &[(String, String)]) -> bool {
    let repeated = |name: &str| params.iter().filter(|(key, _)| key == name).count() > 1;
    params
        .iter()
        .all(|(key, value)| !key.is_empty() && !key.contains('=') && !value.contains('='))
        && !PROXY_OWN_PARAMETERS.iter().any(|name| repeated(name))
}

/// Whether `timestamp` is a Unix time within [`PROXY_SIGNATURE_FRESHNESS`] of `now`.
fn proxy_timestamp_is_fresh(timestamp: &str, now: i64) -> bool {
    timestamp
        .parse::<i64>()
        .is_ok_and(|timestamp| now.abs_diff(timestamp) <= PROXY_SIGNATURE_FRESHNESS)
}

/// Verify the `signature` param Shopify adds to app proxy requests.
///
/// `params` are the query pairs as received, repeated keys included, and `now`
/// is the Unix time in seconds. A request is refused when its `timestamp` is
/// missing or too far from `now`, so that a signed URL cannot be replayed
/// later, and when its parameters could be split another way, see
/// [`proxy_params_are_unambiguous`]. The signature covers neither the path, nor
/// the method, nor the body.
pub fn verify_proxy_signature(params: &[(String, String)], secret: &str, now: i64) -> bool {
    if secret.trim().is_empty() || !proxy_params_are_unambiguous(params) {
        return false;
    }
    let value = |name: &str| params.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    let Some(signature) = value("signature").filter(|signature| !signature.is_empty()) else {
        return false;
    };
    if !value("timestamp").is_some_and(|timestamp| proxy_timestamp_is_fresh(timestamp, now)) {
        return false;
    }
    let Ok(provided) = hex::decode(signature) else {
        return false;
    };
    let message = proxy_message(params.iter().map(|(key, value)| (key.as_str(), value.as_str())));
    constant_time_eq(&hmac_sha256(secret, message.as_bytes()), &provided)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const SECRET: &str = "test-hmac-secret";

    pub(crate) fn sign_query(params: &[(&str, &str)], secret: &str) -> IndexMap<String, String> {
        let mut query: IndexMap<String, String> = params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let hmac = hex::encode(hmac_sha256(secret, sorted_message(&query, "hmac", "&").as_bytes()));
        query.insert("hmac".into(), hmac);
        query
    }

    pub(crate) fn sign_body(body: &str, secret: &str) -> String {
        STANDARD.encode(hmac_sha256(secret, body.as_bytes()))
    }

    #[test]
    fn valid_signed_query_returns_true() {
        let query = sign_query(&[("shop", "test.myshopify.com"), ("timestamp", "123456")], SECRET);
        assert!(verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn missing_hmac_returns_false() {
        let query: IndexMap<String, String> = [("shop".to_string(), "test.myshopify.com".to_string())].into();
        assert!(!verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn empty_secret_returns_false_even_when_query_is_signed_with_an_empty_secret() {
        let query = sign_query(&[("shop", "test.myshopify.com"), ("timestamp", "123456")], "");
        assert!(!verify_query_hmac(&query, ""));
        assert!(!verify_query_hmac(&query, "   "));
    }

    #[test]
    fn wrong_hmac_returns_false() {
        let mut query = sign_query(&[("shop", "test.myshopify.com")], SECRET);
        query.insert("hmac".into(), "deadbeef".into());
        assert!(!verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn param_order_does_not_matter() {
        let query = sign_query(&[("z", "1"), ("a", "2"), ("m", "3")], SECRET);
        assert!(verify_query_hmac(&query, SECRET));
    }

    #[test]
    fn valid_body_hmac_returns_true() {
        let body = r#"{"id":123,"name":"order"}"#;
        assert!(verify_webhook_hmac(body, &sign_body(body, SECRET), SECRET));
    }

    #[test]
    fn wrong_body_hmac_returns_false() {
        assert!(!verify_webhook_hmac(r#"{"id":123}"#, "wrong-hmac", SECRET));
    }

    #[test]
    fn different_body_returns_false() {
        let hmac = sign_body(r#"{"id":456}"#, SECRET);
        assert!(!verify_webhook_hmac(r#"{"id":123}"#, &hmac, SECRET));
    }

    #[test]
    fn empty_secret_returns_false_even_when_body_is_signed_with_an_empty_secret() {
        let body = r#"{"id":123}"#;
        let hmac = sign_body(body, "");
        assert!(!verify_webhook_hmac(body, &hmac, ""));
        assert!(!verify_webhook_hmac(body, &hmac, "   "));
    }

    const NOW: i64 = 1_780_000_000;

    fn pairs(params: &[(&str, &str)]) -> Vec<(String, String)> {
        params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    /// The pairs as Shopify sends them: with the signature of the others.
    fn signed_pairs(params: &[(&str, &str)], secret: &str) -> Vec<(String, String)> {
        let mut signed = pairs(params);
        let message = proxy_message(params.iter().copied());
        signed.push(("signature".to_string(), hex::encode(hmac_sha256(secret, message.as_bytes()))));
        signed
    }

    fn verifies(params: &[(&str, &str)]) -> bool {
        verify_proxy_signature(&signed_pairs(params, SECRET), SECRET, NOW)
    }

    fn shop_at(timestamp: i64, now: i64) -> bool {
        let timestamp = timestamp.to_string();
        let params = [("shop", "test.myshopify.com"), ("timestamp", timestamp.as_str())];
        verify_proxy_signature(&signed_pairs(&params, SECRET), SECRET, now)
    }

    #[test]
    fn verifies_proxy_signatures() {
        let params = [
            ("shop", "test.myshopify.com"),
            ("path_prefix", "/apps/openshop"),
            ("timestamp", "1780000000"),
        ];
        let signed = signed_pairs(&params, SECRET);
        assert!(verify_proxy_signature(&signed, SECRET, NOW));
        assert!(!verify_proxy_signature(&signed, "other-secret", NOW));
        assert!(!verify_proxy_signature(&signed, "", NOW));
        assert!(!verify_proxy_signature(&signed, "  ", NOW));

        let mut bad = signed.clone();
        bad[3].1.push_str("zz");
        assert!(!verify_proxy_signature(&bad, SECRET, NOW));
        bad[3].1 = String::new();
        assert!(!verify_proxy_signature(&bad, SECRET, NOW));

        let mut other_shop = signed.clone();
        other_shop[0].1 = "other.myshopify.com".to_string();
        assert!(!verify_proxy_signature(&other_shop, SECRET, NOW));

        // The order of the parameters does not matter, nor does where the signature is.
        let mut shuffled = signed.clone();
        shuffled.reverse();
        assert!(verify_proxy_signature(&shuffled, SECRET, NOW));

        // The map form signs the same message.
        let query: IndexMap<String, String> = signed[..3].iter().cloned().collect();
        assert_eq!(proxy_signature(&query, SECRET), signed[3].1);
    }

    #[test]
    fn a_proxy_signature_is_only_fresh_for_a_few_minutes() {
        let window = PROXY_SIGNATURE_FRESHNESS as i64;
        assert_eq!(window, 300);

        // Either side of now, within the window.
        assert!(shop_at(NOW, NOW));
        assert!(shop_at(NOW - window, NOW));
        assert!(shop_at(NOW + window, NOW));
        // Replayed later, or signed by a clock that is ahead.
        assert!(!shop_at(NOW - window - 1, NOW));
        assert!(!shop_at(NOW + window + 1, NOW));
        assert!(!shop_at(NOW - 86_400, NOW));
        assert!(!shop_at(i64::MIN, NOW));
        assert!(!shop_at(i64::MAX, NOW));
    }

    #[test]
    fn a_proxy_signature_needs_a_numeric_timestamp() {
        let shop = ("shop", "test.myshopify.com");
        assert!(!verifies(&[shop]));
        for timestamp in ["", "soon", "1780000000.5", "0x6a0b3c00", " 1780000000"] {
            assert!(!verifies(&[shop, ("timestamp", timestamp)]), "{timestamp}");
        }
        assert!(verifies(&[shop, ("timestamp", "1780000000")]));
    }

    #[test]
    fn a_parameter_sent_several_times_is_signed_with_its_values_joined() {
        // Shopify signs `extra=1,2` for `extra=1&extra=2`.
        let joined = signed_pairs(
            &[("shop", "test.myshopify.com"), ("extra", "1,2"), ("timestamp", "1780000000")],
            SECRET,
        );
        let signature = joined[3].clone();
        let mut repeated = pairs(&[
            ("shop", "test.myshopify.com"),
            ("extra", "1"),
            ("timestamp", "1780000000"),
            ("extra", "2"),
        ]);
        repeated.push(signature);
        assert!(verify_proxy_signature(&joined, SECRET, NOW));
        assert!(verify_proxy_signature(&repeated, SECRET, NOW));

        // The order of the values is part of what is signed.
        repeated[1].1 = "2".to_string();
        repeated[3].1 = "1".to_string();
        assert!(!verify_proxy_signature(&repeated, SECRET, NOW));
    }

    #[test]
    fn the_proxy_message_sorts_the_pairs_as_strings() {
        // `a-b=…` comes before `a=…`, which sorting the keys alone would not say.
        let message = proxy_message([("a", "1"), ("a-b", "2"), ("b", "3")]);
        assert_eq!(message, "a-b=2a=1b=3");
        assert_eq!(proxy_message([("signature", "x"), ("k", "v")]), "k=v");
    }

    #[test]
    fn the_pairs_of_a_proxy_signature_cannot_be_split_again() {
        let timestamp = NOW.to_string();
        // The request for a shop of the sender's, whose parameter `m` carries the
        // text of the request for another shop.
        let note = format!("1path_prefix=/apps/xshop=victim.myshopify.comtimestamp={timestamp}");
        let own = [
            ("m", note.as_str()),
            ("path_prefix", "/apps/x"),
            ("shop", "attacker.myshopify.com"),
            ("timestamp", timestamp.as_str()),
        ];
        // The same text, cut so that the shop is the victim's.
        let tail = format!("{timestamp}path_prefix=/apps/xshop=attacker.myshopify.comtimestamp={timestamp}");
        let forged = [
            ("m", "1"),
            ("path_prefix", "/apps/x"),
            ("shop", "victim.myshopify.com"),
            ("timestamp", tail.as_str()),
        ];
        // They read the same to the signature.
        assert_eq!(proxy_message(own), proxy_message(forged));

        let signed = signed_pairs(&own, SECRET);
        let mut resplit = pairs(&forged);
        resplit.push(signed[4].clone());
        assert!(!verify_proxy_signature(&signed, SECRET, NOW));
        assert!(!verify_proxy_signature(&resplit, SECRET, NOW));
    }

    #[test]
    fn a_proxy_parameter_with_an_equal_sign_is_refused_whatever_its_timestamp() {
        let timestamp = NOW.to_string();
        let shop = ("shop", "a.myshopify.com");
        let at = ("timestamp", timestamp.as_str());
        assert!(verifies(&[shop, at, ("q", "shoes")]));
        assert!(!verifies(&[shop, at, ("q", "a=b")]));
        assert!(!verifies(&[shop, at, ("q=a", "b")]));
        assert!(!verifies(&[shop, at, ("", "b")]));
        assert!(!verifies(&[("shop", "a=b.myshopify.com"), at]));
    }

    #[test]
    fn a_parameter_of_shopify_is_not_repeated() {
        let timestamp = NOW.to_string();
        let shop = ("shop", "a.myshopify.com");
        let at = ("timestamp", timestamp.as_str());
        assert!(verifies(&[shop, at, ("tag", "a"), ("tag", "b")]));
        assert!(!verifies(&[shop, at, ("shop", "b.myshopify.com")]));
        assert!(!verifies(&[shop, at, at]));
        for name in ["path_prefix", "logged_in_customer_id"] {
            assert!(verifies(&[shop, at, (name, "1")]), "{name}");
            assert!(!verifies(&[shop, at, (name, "1"), (name, "2")]), "{name}");
        }

        // A second `signature` leaves no telling which one signs.
        let mut twice = signed_pairs(&[shop, at], SECRET);
        twice.push(twice[2].clone());
        assert!(!verify_proxy_signature(&twice, SECRET, NOW));
    }
}
