//! MCP bearer tokens: `oshp_mcp_<tokenId>.<secret>`, stored as scrypt hashes.

use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;
use regex::Regex;
use sha2::{Digest, Sha256};

use crate::server::hmac::constant_time_eq;

const TOKEN_PREFIX: &str = "oshp_mcp_";
const HASH_PREFIX: &str = "scrypt:";
const KEY_LENGTH: usize = 32;

static TOKEN_ID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^mtk_[A-Za-z0-9_-]+$").expect("valid regex"));

/// `Debug` does not show the token or its hash.
#[derive(Clone, PartialEq, Eq)]
pub struct GeneratedMcpToken {
    pub token_id: String,
    /// The full secret token. Shown once, never stored.
    pub token: String,
    pub token_hash: String,
    pub token_fingerprint: String,
}

impl std::fmt::Debug for GeneratedMcpToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::runtime::redact::redacted;
        f.debug_struct("GeneratedMcpToken")
            .field("token_id", &self.token_id)
            .field("token", &redacted(&self.token))
            .field("token_hash", &redacted(&self.token_hash))
            .field("token_fingerprint", &self.token_fingerprint)
            .finish()
    }
}

/// `Debug` does not show the token.
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedMcpToken {
    pub token_id: String,
    pub token: String,
}

impl std::fmt::Debug for ParsedMcpToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParsedMcpToken")
            .field("token_id", &self.token_id)
            .field("token", &crate::runtime::redact::redacted(&self.token))
            .finish()
    }
}

fn random_token_part(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

/// Same parameters as Node's `scryptSync` defaults (N=16384, r=8, p=1).
fn scrypt_key(token: &str, salt: &str) -> Option<[u8; KEY_LENGTH]> {
    let params = scrypt::Params::new(14, 8, 1, KEY_LENGTH).ok()?;
    let mut key = [0u8; KEY_LENGTH];
    scrypt::scrypt(token.as_bytes(), salt.as_bytes(), &params, &mut key).ok()?;
    Some(key)
}

pub fn create_mcp_token() -> GeneratedMcpToken {
    create_mcp_token_for_id(&format!("mtk_{}", random_token_part(12)))
}

/// New secret for an existing token id (rotation).
pub fn create_mcp_token_for_id(token_id: &str) -> GeneratedMcpToken {
    let token = format!("{TOKEN_PREFIX}{token_id}.{}", random_token_part(32));
    GeneratedMcpToken {
        token_id: token_id.to_string(),
        token_hash: hash_mcp_token(&token),
        token_fingerprint: fingerprint_mcp_token(&token),
        token,
    }
}

pub fn parse_mcp_token(value: &str) -> Option<ParsedMcpToken> {
    let rest = value.strip_prefix(TOKEN_PREFIX)?;
    let dot = rest.find('.')?;
    if dot == 0 || dot == rest.len() - 1 {
        return None;
    }
    let token_id = &rest[..dot];
    TOKEN_ID_RE.is_match(token_id).then(|| ParsedMcpToken {
        token_id: token_id.to_string(),
        token: value.to_string(),
    })
}

/// Short, non-secret identifier shown in the admin UI.
fn fingerprint_mcp_token(token: &str) -> String {
    let digest = URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()));
    digest[..16].to_string()
}

fn hash_mcp_token(token: &str) -> String {
    let salt = random_token_part(16);
    let hash = scrypt_key(token, &salt)
        .map(|key| URL_SAFE_NO_PAD.encode(key))
        .unwrap_or_default();
    format!("{HASH_PREFIX}{salt}:{hash}")
}

pub(super) fn verify_mcp_token(token: &str, stored_hash: &str) -> bool {
    let Some(payload) = stored_hash.strip_prefix(HASH_PREFIX) else {
        return false;
    };
    let mut parts = payload.split(':');
    let (Some(salt), Some(expected)) = (parts.next(), parts.next()) else {
        return false;
    };
    if salt.is_empty() || expected.is_empty() {
        return false;
    }

    let Some(actual) = scrypt_key(token, salt) else {
        return false;
    };
    let Ok(expected) = URL_SAFE_NO_PAD.decode(expected) else {
        return false;
    };
    constant_time_eq(&actual, &expected)
}

pub fn extract_bearer_token(header: Option<&str>) -> Option<String> {
    let token = header?.strip_prefix("Bearer ")?.trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_parseable_tokens() {
        let generated = create_mcp_token();
        assert!(generated.token.starts_with("oshp_mcp_mtk_"));
        assert!(generated.token_id.starts_with("mtk_"));
        assert_eq!(generated.token_fingerprint.len(), 16);
        assert!(generated.token_hash.starts_with("scrypt:"));

        let parsed = parse_mcp_token(&generated.token).unwrap();
        assert_eq!(parsed.token_id, generated.token_id);
        assert_eq!(parsed.token, generated.token);
    }

    #[test]
    fn rejects_malformed_tokens() {
        for value in [
            "",
            "mtk_abc.secret",
            "oshp_mcp_",
            "oshp_mcp_.secret",
            "oshp_mcp_mtk_abc.",
            "oshp_mcp_mtk_abc",
            "oshp_mcp_bad id.secret",
            "oshp_mcp_tok_abc.secret",
        ] {
            assert_eq!(parse_mcp_token(value), None, "{value}");
        }
    }

    #[test]
    fn verifies_only_the_matching_token() {
        let generated = create_mcp_token();
        assert!(verify_mcp_token(&generated.token, &generated.token_hash));
        assert!(!verify_mcp_token("oshp_mcp_mtk_x.other", &generated.token_hash));
        assert!(!verify_mcp_token(&generated.token, "plain"));
        assert!(!verify_mcp_token(&generated.token, "scrypt:only-salt"));
        assert!(!verify_mcp_token(&generated.token, "scrypt::"));
    }

    #[test]
    fn rotation_keeps_the_id_and_changes_the_secret() {
        let first = create_mcp_token();
        let second = create_mcp_token_for_id(&first.token_id);
        assert_eq!(first.token_id, second.token_id);
        assert_ne!(first.token, second.token);
        assert!(!verify_mcp_token(&first.token, &second.token_hash));
        assert!(verify_mcp_token(&second.token, &second.token_hash));
    }

    #[test]
    fn verifies_hashes_written_by_node_scrypt() {
        // scryptSync('oshp_mcp_mtk_test.secret', 'c2FsdA', 32).toString('base64url')
        let key = scrypt_key("oshp_mcp_mtk_test.secret", "c2FsdA").unwrap();
        let stored = format!("scrypt:c2FsdA:{}", URL_SAFE_NO_PAD.encode(key));
        assert!(verify_mcp_token("oshp_mcp_mtk_test.secret", &stored));
    }

    #[test]
    fn fingerprints_are_stable() {
        assert_eq!(fingerprint_mcp_token("abc"), fingerprint_mcp_token("abc"));
        assert_ne!(fingerprint_mcp_token("abc"), fingerprint_mcp_token("abd"));
    }

    #[test]
    fn extracts_bearer_tokens() {
        assert_eq!(extract_bearer_token(Some("Bearer abc")).as_deref(), Some("abc"));
        assert_eq!(extract_bearer_token(Some("Bearer   abc  ")).as_deref(), Some("abc"));
        assert_eq!(extract_bearer_token(Some("Bearer ")), None);
        assert_eq!(extract_bearer_token(Some("Basic abc")), None);
        assert_eq!(extract_bearer_token(None), None);
    }
}
