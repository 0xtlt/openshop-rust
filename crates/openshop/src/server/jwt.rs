//! Shopify session tokens: the app a token names, and who it was issued to.
//!
//! Their verification (signature, dates, `aud`, `dest`, `iss`) is
//! `shopify_api`'s: [`ShopifyApp::verify_session_token`] for the tokens of
//! Shopify admin, [`ShopifyApp::verify_extension_session_token`] for those of
//! checkout and customer account extensions.
//!
//! [`ShopifyApp::verify_session_token`]: shopify_api::oauth::ShopifyApp::verify_session_token
//! [`ShopifyApp::verify_extension_session_token`]: shopify_api::oauth::ShopifyApp::verify_extension_session_token

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

/// The id of the customer of a `sub` claim: customer account tokens carry the
/// customer as a GID, `gid://shopify/Customer/<id>`.
pub fn customer_id(sub: &str) -> Option<&str> {
    sub.strip_prefix("gid://shopify/Customer/")
        .filter(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Whether the subject of a session token is a staff user of the admin: neither
/// a customer nor the empty subject of an anonymous buyer. Tokens of checkout and
/// customer account extensions never are, and the Admin API accepts nothing else.
pub fn is_staff_subject(sub: &str) -> bool {
    !sub.is_empty() && customer_id(sub).is_none()
}

/// Reads the `aud` claim without verifying the token, to pick the app whose
/// secret verifies it.
pub fn read_jwt_audience(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("aud")?.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use shopify_api::oauth::ShopifyApp;

    fn sign_session_token(claims: &Value, secret: &str) -> String {
        ShopifyApp::new("", secret).sign_session_token(claims)
    }

    #[test]
    fn customer_id_is_extracted_only_from_customer_gid_sub() {
        assert_eq!(customer_id("gid://shopify/Customer/42"), Some("42"));
        assert_eq!(customer_id("gid://shopify/Customer/"), None);
        assert_eq!(customer_id("gid://shopify/Customer/4x"), None);
        assert_eq!(customer_id("12345"), None);
        assert!(is_staff_subject("42"));
        assert!(!is_staff_subject("gid://shopify/Customer/42"));
        assert!(!is_staff_subject(""));
    }

    #[test]
    fn reads_audience_without_verifying() {
        let token = sign_session_token(&json!({ "aud": "api-key" }), "whatever");
        assert_eq!(read_jwt_audience(&token).as_deref(), Some("api-key"));
        assert_eq!(read_jwt_audience("garbage"), None);
        let numeric = sign_session_token(&json!({ "aud": 42 }), "whatever");
        assert_eq!(read_jwt_audience(&numeric), None);
    }
}
