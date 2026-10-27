//! What `Debug` shows of a secret: that there is one, never its value.
//!
//! A configuration is easily printed (`{config:?}`, an `unwrap` on a result
//! that holds it, a failed `assert_eq!`) and then lands in a log or a report.

use std::fmt;

#[cfg(any(test, feature = "testing"))]
use serde_json::Value;

const HIDDEN: &str = "[redacted]";

/// Shows `[redacted]` for a secret that is set, and nothing for one that is empty.
pub(crate) struct Redacted<'a>(&'a str);

impl fmt::Debug for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            f.write_str("\"\"")
        } else {
            f.write_str(HIDDEN)
        }
    }
}

pub(crate) fn redacted(secret: &str) -> Redacted<'_> {
    Redacted(secret)
}

/// Like [`redacted`] for a secret that may be missing.
pub(crate) fn redacted_option(secret: &Option<String>) -> Option<Redacted<'_>> {
    secret.as_deref().map(Redacted)
}

/// Whether the name of a header or of a field says that it holds a secret.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["secret", "token", "password", "authorization", "cookie", "code", "signature", "key"]
        .iter()
        .any(|word| name.contains(word))
}

/// A JSON value for display, with the fields that hold a secret hidden at any depth.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn redacted_json(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, field)| {
                    let shown = if is_secret_name(name) && !field.is_null() {
                        Value::String(HIDDEN.to_string())
                    } else {
                        redacted_json(field)
                    };
                    (name.clone(), shown)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redacted_json).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_secret_is_shown_as_set_or_empty() {
        assert_eq!(format!("{:?}", redacted("hunter2")), "[redacted]");
        assert_eq!(format!("{:?}", redacted("")), "\"\"");
        assert_eq!(format!("{:?}", redacted_option(&Some("hunter2".into()))), "Some([redacted])");
        assert_eq!(format!("{:?}", redacted_option(&None)), "None");
    }

    #[test]
    fn json_hides_the_fields_named_like_secrets() {
        let shown = redacted_json(&json!({
            "client_id": "id",
            "client_secret": "s3cret",
            "nested": [{ "access_token": "t0ken", "shop": "a.myshopify.com" }],
            "code": null,
        }));
        assert_eq!(
            shown,
            json!({
                "client_id": "id",
                "client_secret": "[redacted]",
                "nested": [{ "access_token": "[redacted]", "shop": "a.myshopify.com" }],
                "code": null,
            })
        );
    }

    const SECRETS: [&str; 8] = [
        "api-secret-value",
        "encryption-key-value",
        "access-token-value",
        "nonce-value",
        "provider-password",
        "scrypt:hash-value",
        "oshp_mcp_",
        "client-secret-value",
    ];

    fn assert_hidden(shown: &str) {
        for secret in SECRETS {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
    }

    #[test]
    fn debug_never_shows_the_secrets_of_a_configuration() {
        use crate::server::mcp::tokens::{create_mcp_token, parse_mcp_token};
        use crate::server::shopify_apps::ResolvedShopifyApp;
        use crate::types::ShopifyAppConfig;

        let keys = ShopifyAppConfig::Keys {
            api_key: "public-key".into(),
            api_secret: "api-secret-value".into(),
            app_url: Some("https://app.example.com".into()),
        };
        let toml = ShopifyAppConfig::Toml {
            toml: "shopify.app.toml".into(),
            api_secret: "api-secret-value".into(),
            app_url: None,
        };
        let shown = format!("{keys:?} {toml:?}");
        assert_hidden(&shown);
        // What is not a secret is still there to debug with.
        assert!(shown.contains("public-key") && shown.contains("shopify.app.toml"), "{shown}");
        assert!(shown.contains("api_secret: [redacted]"), "{shown}");

        let env = crate::runtime::RuntimeEnv {
            shopify_api_key: Some("public-key".into()),
            shopify_api_secret: Some("api-secret-value".into()),
            encryption_key: Some("encryption-key-value".into()),
            ..Default::default()
        };
        let shown = format!("{env:?}");
        assert_hidden(&shown);
        assert!(shown.contains("public-key"), "{shown}");

        let resolved = ResolvedShopifyApp {
            handle: "default".into(),
            api_key: "public-key".into(),
            api_secret: "api-secret-value".into(),
            app_url: "https://app.example.com".into(),
            scopes: "read_products".into(),
            oauth: shopify_api::oauth::ShopifyApp::new("public-key", "api-secret-value"),
        };
        let shown = format!("{resolved:?} {:?}", [&resolved]);
        assert_hidden(&shown);
        assert!(shown.contains("public-key"), "{shown}");

        let generated = create_mcp_token();
        let parsed = parse_mcp_token(&generated.token).unwrap();
        let shown = format!("{generated:?} {parsed:?}");
        assert!(!shown.contains(&generated.token), "{shown}");
        assert!(!shown.contains(&generated.token_hash), "{shown}");
        assert!(shown.contains(&generated.token_id), "{shown}");
    }

    #[test]
    fn debug_never_shows_the_secrets_of_a_stored_row() {
        use chrono::Utc;
        use uuid::Uuid;

        use crate::db::schema::{InstallationRow, McpToken, ProviderConfigRow};

        let now = Utc::now();
        let installation = InstallationRow {
            id: Uuid::nil(),
            app_handle: "default".into(),
            shop: "a.myshopify.com".into(),
            access_token: Some("access-token-value".into()),
            scopes: None,
            nonce: Some("nonce-value".into()),
            installed_at: now,
            uninstalled_at: None,
        };
        let provider = ProviderConfigRow {
            id: Uuid::nil(),
            app_handle: "default".into(),
            shop: "a.myshopify.com".into(),
            provider_name: "warehouse".into(),
            config: Some(json!({ "token": "provider-password" })),
            last_checked_at: None,
            last_check_ok: None,
            updated_at: now,
        };
        let token = McpToken {
            id: Uuid::nil(),
            app_handle: "default".into(),
            shop: "a.myshopify.com".into(),
            name: "ci".into(),
            token_id: "mtk_abc".into(),
            token_hash: "scrypt:hash-value".into(),
            token_fingerprint: "fingerprint".into(),
            status: "active".into(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            created_at: now,
            updated_at: now,
        };
        let shown = format!("{installation:?} {provider:?} {token:?}");
        assert_hidden(&shown);
        assert!(shown.contains("a.myshopify.com") && shown.contains("mtk_abc"), "{shown}");
    }

    #[test]
    fn debug_of_an_outbound_request_hides_tokens_and_client_secrets() {
        use crate::testing::OutboundRequest;

        let request = OutboundRequest {
            method: "POST",
            url: "https://a.myshopify.com/admin/oauth/access_token".into(),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-Shopify-Access-Token".into(), "access-token-value".into()),
                ("Authorization".into(), "Bearer access-token-value".into()),
            ],
            body: json!({ "client_id": "public-key", "client_secret": "client-secret-value", "code": "access-token-value" }),
        };
        let shown = format!("{request:?}");
        assert_hidden(&shown);
        assert!(shown.contains("application/json") && shown.contains("public-key"), "{shown}");
    }
}
