//! Shopify session token (HS256 JWT) verification.

use std::sync::LazyLock;

use anyhow::{Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::hmac::{constant_time_eq, hmac_sha256};
use super::shop_domain::normalize_shop_domain;

/// Claims of a verified session token.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JwtPayload {
    #[serde(default)]
    pub iss: String,
    #[serde(default)]
    pub dest: String,
    #[serde(default)]
    pub aud: String,
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub exp: f64,
    #[serde(default)]
    pub nbf: f64,
    #[serde(default)]
    pub iat: f64,
    #[serde(default)]
    pub jti: String,
    #[serde(default)]
    pub sid: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedSessionToken {
    pub shop: String,
    pub payload: JwtPayload,
}

fn decode_part(part: &str) -> Result<Value> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .map_err(|_| anyhow::anyhow!("Invalid JWT encoding"))?;
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid JWT encoding"))
}

fn numeric_date(claims: &Value, key: &str) -> Option<f64> {
    claims.get(key)?.as_f64().filter(|value| value.is_finite())
}

fn string_claim(claims: &Value, key: &str) -> String {
    claims.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

static CUSTOMER_GID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^gid://shopify/Customer/(\d+)$").expect("valid regex"));

/// Customer account tokens carry the customer as a GID in `sub`.
pub fn customer_id_from_jwt_sub(sub: &str) -> Option<String> {
    CUSTOMER_GID_RE.captures(sub).map(|captures| captures[1].to_string())
}

/// Reads the `aud` claim without verifying the token, to pick the matching app.
pub fn read_jwt_audience(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    decode_part(payload).ok()?.get("aud")?.as_str().map(str::to_string)
}

pub fn verify_session_token(token: &str, secret: &str, audience: &str) -> Result<VerifiedSessionToken> {
    // A blank secret is no secret, as for webhook and query signatures.
    if secret.trim().is_empty() {
        bail!("SHOPIFY_API_SECRET is not configured");
    }

    let parts: Vec<&str> = token.split('.').collect();
    let [header, payload, signature] = parts[..] else {
        bail!("Invalid JWT format");
    };

    let decoded_header = decode_part(header)?;
    if decoded_header.get("alg").and_then(Value::as_str) != Some("HS256") {
        bail!("Invalid JWT algorithm");
    }

    let expected = hmac_sha256(secret, format!("{header}.{payload}").as_bytes());
    let provided = URL_SAFE_NO_PAD
        .decode(signature.trim_end_matches('='))
        .unwrap_or_default();
    if !constant_time_eq(&provided, &expected) {
        bail!("Invalid JWT signature");
    }

    let claims = decode_part(payload)?;

    if audience.is_empty() {
        bail!("SHOPIFY_API_KEY is not configured");
    }
    if claims.get("aud").and_then(Value::as_str) != Some(audience) {
        bail!("Invalid JWT audience");
    }

    let now = chrono::Utc::now().timestamp() as f64;
    let Some(exp) = numeric_date(&claims, "exp") else {
        bail!("JWT missing exp claim");
    };
    if exp < now {
        bail!("JWT expired");
    }
    let Some(nbf) = numeric_date(&claims, "nbf") else {
        bail!("JWT missing nbf claim");
    };
    if nbf > now + 60.0 {
        bail!("JWT not yet valid");
    }

    let Some(dest_shop) = normalize_shop_domain(&string_claim(&claims, "dest")) else {
        bail!("JWT missing or invalid dest claim");
    };
    let Some(iss_shop) = normalize_shop_domain(&string_claim(&claims, "iss")) else {
        bail!("JWT missing or invalid iss claim");
    };
    if dest_shop != iss_shop {
        bail!("JWT dest/iss mismatch");
    }

    Ok(VerifiedSessionToken {
        shop: dest_shop,
        payload: JwtPayload {
            iss: string_claim(&claims, "iss"),
            dest: string_claim(&claims, "dest"),
            aud: string_claim(&claims, "aud"),
            sub: string_claim(&claims, "sub"),
            exp,
            nbf,
            iat: numeric_date(&claims, "iat").unwrap_or_default(),
            jti: string_claim(&claims, "jti"),
            sid: string_claim(&claims, "sid"),
        },
    })
}

/// Signs a session token the way Shopify App Bridge does. Used by tests and tooling.
pub fn sign_session_token(claims: &Value, secret: &str) -> String {
    sign_jwt(&serde_json::json!({ "alg": "HS256", "typ": "JWT" }), claims, secret)
}

pub(crate) fn sign_jwt(header: &Value, claims: &Value, secret: &str) -> String {
    let header = URL_SAFE_NO_PAD.encode(header.to_string());
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = URL_SAFE_NO_PAD.encode(hmac_sha256(secret, format!("{header}.{payload}").as_bytes()));
    format!("{header}.{payload}.{signature}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "test-secret-key-for-jwt";
    const AUDIENCE: &str = "app-id";

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn valid_payload() -> Value {
        json!({
            "iss": "https://test.myshopify.com/admin",
            "dest": "https://test.myshopify.com",
            "aud": AUDIENCE,
            "sub": "12345",
            "exp": now() + 3600,
            "nbf": now() - 10,
            "iat": now(),
            "jti": "jti-1",
            "sid": "sid-1",
        })
    }

    fn with(overrides: Value) -> Value {
        let mut payload = valid_payload();
        for (key, value) in overrides.as_object().unwrap() {
            payload[key] = value.clone();
        }
        payload
    }

    fn without(keys: &[&str]) -> Value {
        let mut payload = valid_payload();
        for key in keys {
            payload.as_object_mut().unwrap().remove(*key);
        }
        payload
    }

    fn verify(token: &str) -> Result<VerifiedSessionToken> {
        verify_session_token(token, SECRET, AUDIENCE)
    }

    fn error_of(claims: Value) -> String {
        verify(&sign_session_token(&claims, SECRET)).unwrap_err().to_string()
    }

    #[test]
    fn valid_token_returns_shop_and_payload() {
        let result = verify(&sign_session_token(&valid_payload(), SECRET)).unwrap();
        assert_eq!(result.shop, "test.myshopify.com");
        assert_eq!(result.payload.sub, "12345");
        assert_eq!(result.payload.sid, "sid-1");
    }

    #[test]
    fn expired_token_fails() {
        assert_eq!(error_of(with(json!({ "exp": now() - 3600 }))), "JWT expired");
    }

    #[test]
    fn not_yet_valid_token_fails() {
        assert_eq!(error_of(with(json!({ "nbf": now() + 7200 }))), "JWT not yet valid");
    }

    #[test]
    fn nbf_within_60s_tolerance_succeeds() {
        let token = sign_session_token(&with(json!({ "nbf": now() + 30 })), SECRET);
        assert_eq!(verify(&token).unwrap().shop, "test.myshopify.com");
    }

    #[test]
    fn wrong_secret_fails() {
        let token = sign_session_token(&valid_payload(), "wrong-secret");
        assert_eq!(verify(&token).unwrap_err().to_string(), "Invalid JWT signature");
    }

    #[test]
    fn malformed_token_fails() {
        assert_eq!(
            verify("not.a.jwt.at.all").unwrap_err().to_string(),
            "Invalid JWT format"
        );
    }

    #[test]
    fn wrong_audience_fails() {
        assert_eq!(error_of(with(json!({ "aud": "other-app" }))), "Invalid JWT audience");
    }

    #[test]
    fn wrong_algorithm_fails() {
        let token = sign_jwt(&json!({ "alg": "none", "typ": "JWT" }), &valid_payload(), SECRET);
        assert_eq!(verify(&token).unwrap_err().to_string(), "Invalid JWT algorithm");
    }

    #[test]
    fn dest_and_iss_mismatch_fails() {
        assert_eq!(
            error_of(with(json!({ "iss": "https://other.myshopify.com/admin" }))),
            "JWT dest/iss mismatch"
        );
    }

    #[test]
    fn missing_dest_fails() {
        assert_eq!(error_of(without(&["dest", "iss"])), "JWT missing or invalid dest claim");
        assert_eq!(error_of(without(&["dest"])), "JWT missing or invalid dest claim");
    }

    #[test]
    fn missing_time_claims_fail() {
        assert_eq!(error_of(without(&["exp"])), "JWT missing exp claim");
        assert_eq!(error_of(without(&["nbf"])), "JWT missing nbf claim");
    }

    #[test]
    fn plain_domain_dest_is_accepted() {
        let token = sign_session_token(&with(json!({ "dest": "test.myshopify.com" })), SECRET);
        assert_eq!(verify(&token).unwrap().shop, "test.myshopify.com");
    }

    #[test]
    fn missing_secret_or_audience_fails() {
        let token = sign_session_token(&valid_payload(), SECRET);
        assert_eq!(
            verify_session_token(&token, "", AUDIENCE).unwrap_err().to_string(),
            "SHOPIFY_API_SECRET is not configured"
        );
        let blank = sign_session_token(&valid_payload(), "  ");
        assert_eq!(
            verify_session_token(&blank, "  ", AUDIENCE).unwrap_err().to_string(),
            "SHOPIFY_API_SECRET is not configured"
        );
        assert_eq!(
            verify_session_token(&token, SECRET, "").unwrap_err().to_string(),
            "SHOPIFY_API_KEY is not configured"
        );
    }

    #[test]
    fn customer_id_is_extracted_only_from_customer_gid_sub() {
        assert_eq!(
            customer_id_from_jwt_sub("gid://shopify/Customer/42").as_deref(),
            Some("42")
        );
        assert_eq!(customer_id_from_jwt_sub("12345"), None);
    }

    #[test]
    fn reads_audience_without_verifying() {
        let token = sign_session_token(&valid_payload(), "whatever");
        assert_eq!(read_jwt_audience(&token).as_deref(), Some(AUDIENCE));
        assert_eq!(read_jwt_audience("garbage"), None);
        let numeric = sign_session_token(&json!({ "aud": 42 }), SECRET);
        assert_eq!(read_jwt_audience(&numeric), None);
    }
}
