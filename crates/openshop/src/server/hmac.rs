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

/// Hex signature of an app proxy query (sorted `key=value` pairs, no separator).
pub fn proxy_signature(query: &IndexMap<String, String>, secret: &str) -> String {
    hex::encode(hmac_sha256(secret, sorted_message(query, "signature", "").as_bytes()))
}

/// Verify the `signature` param Shopify adds to app proxy requests.
pub fn verify_proxy_signature(query: &IndexMap<String, String>, secret: &str) -> bool {
    let Some(signature) = query.get("signature").filter(|signature| !signature.is_empty()) else {
        return false;
    };
    if secret.is_empty() {
        return false;
    }
    let Ok(provided) = hex::decode(signature) else {
        return false;
    };
    let computed = hmac_sha256(secret, sorted_message(query, "signature", "").as_bytes());
    constant_time_eq(&computed, &provided)
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

    #[test]
    fn verifies_proxy_signatures() {
        let mut query: IndexMap<String, String> = [
            ("shop".to_string(), "test.myshopify.com".to_string()),
            ("path_prefix".to_string(), "/apps/openshop".to_string()),
            ("timestamp".to_string(), "1780000000".to_string()),
        ]
        .into();
        let signature = proxy_signature(&query, SECRET);
        query.insert("signature".into(), signature.clone());
        assert!(verify_proxy_signature(&query, SECRET));
        assert!(!verify_proxy_signature(&query, "other-secret"));
        assert!(!verify_proxy_signature(&query, ""));

        query.insert("signature".into(), format!("{signature}zz"));
        assert!(!verify_proxy_signature(&query, SECRET));

        query.insert("shop".into(), "other.myshopify.com".into());
        query.insert("signature".into(), signature);
        assert!(!verify_proxy_signature(&query, SECRET));
    }
}
