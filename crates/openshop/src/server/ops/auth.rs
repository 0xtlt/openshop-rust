//! Access to the operator dashboard: one password, and a signed session
//! cookie that needs no table.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::server::hmac::{constant_time_eq, hmac_sha256};
use crate::server::http::header;

/// In production, a shorter password is refused when the application starts.
pub const MIN_PRODUCTION_PASSWORD_LENGTH: usize = 16;

const COOKIE_NAME: &str = "openshop_ops";
const SESSION_SECONDS: i64 = 12 * 60 * 60;
/// What a session is signed with is derived from the password under this label.
const KEY_LABEL: &str = "openshop:ops-session:v1:";
const FIRST_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF_DOUBLINGS: u32 = 5;

/// Refuses, in production, a password an attacker could guess.
pub fn check_password(password: Option<&str>, production: bool) -> Result<()> {
    if let Some(password) = password
        && production
        && password.chars().count() < MIN_PRODUCTION_PASSWORD_LENGTH
    {
        bail!(
            "[openshop] Invalid config: opsPassword must be at least {MIN_PRODUCTION_PASSWORD_LENGTH} characters in production"
        );
    }
    Ok(())
}

pub(crate) enum Login {
    /// The `Set-Cookie` value that opens the session.
    Granted(String),
    Refused,
    /// Another attempt is being checked: this one was not looked at.
    Busy,
}

pub(crate) struct OpsAuth {
    password_digest: [u8; 32],
    session_key: String,
    /// Off in development, where the server is reached over plain HTTP.
    secure_cookie: bool,
    failures: AtomicU32,
    /// Held while a password is checked, and through the delay after a wrong one.
    attempt: Mutex<()>,
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

impl OpsAuth {
    /// `server_secret` is something only the server knows, when it has one:
    /// a session cannot then be forged from the password alone.
    pub(crate) fn new(password: &str, server_secret: Option<&str>, secure_cookie: bool) -> Self {
        let label = format!("{KEY_LABEL}{}", server_secret.unwrap_or_default());
        Self {
            password_digest: digest(password),
            session_key: hex::encode(hmac_sha256(password, label.as_bytes())),
            secure_cookie,
            failures: AtomicU32::new(0),
            attempt: Mutex::new(()),
        }
    }

    fn signature(&self, expires_at: i64) -> String {
        hex::encode(hmac_sha256(&self.session_key, format!("v1.{expires_at}").as_bytes()))
    }

    fn session_token(&self, now: i64) -> String {
        let expires_at = now + SESSION_SECONDS;
        format!("v1.{expires_at}.{}", self.signature(expires_at))
    }

    fn is_valid_token(&self, token: &str, now: i64) -> bool {
        let mut parts = token.split('.');
        let (Some("v1"), Some(expires_at), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let Ok(expires_at) = expires_at.parse::<i64>() else {
            return false;
        };
        expires_at > now && constant_time_eq(signature.as_bytes(), self.signature(expires_at).as_bytes())
    }

    fn cookie(&self, value: &str, max_age: i64) -> String {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        format!("{COOKIE_NAME}={value}; Path=/ops; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}")
    }

    /// The `Set-Cookie` value that closes the session.
    pub(crate) fn expired_cookie(&self) -> String {
        self.cookie("", 0)
    }

    /// Whether the request carries a session this server signed and that has not expired.
    pub(crate) fn has_session(&self, headers: &HeaderMap) -> bool {
        let now = Utc::now().timestamp();
        headers
            .get_all(COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|cookies| cookies.split(';'))
            .filter_map(|cookie| cookie.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))
            .any(|token| self.is_valid_token(token, now))
    }

    /// Checks a password. One attempt is looked at at a time, and a wrong
    /// one is answered after a delay that doubles with each failure, so that
    /// guessing stays slow whatever the number of connections.
    pub(crate) async fn login(&self, candidate: &str) -> Login {
        let Ok(_attempt) = self.attempt.try_lock() else {
            return Login::Busy;
        };
        // Digests have one length: the comparison says nothing of the password's.
        if constant_time_eq(&digest(candidate), &self.password_digest) {
            self.failures.store(0, Ordering::Relaxed);
            return Login::Granted(self.cookie(&self.session_token(Utc::now().timestamp()), SESSION_SECONDS));
        }
        let failures = self.failures.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(FIRST_BACKOFF * 2u32.pow(failures.min(MAX_BACKOFF_DOUBLINGS))).await;
        Login::Refused
    }
}

/// Whether a form was posted by a page of this server. Browsers that send
/// `Sec-Fetch-Site` say so; the session cookie, `SameSite=Strict`, covers the others.
pub(crate) fn is_same_site(headers: &HeaderMap) -> bool {
    header(headers, "sec-fetch-site").is_none_or(|site| matches!(site, "same-origin" | "none"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn auth() -> OpsAuth {
        OpsAuth::new("correct horse battery staple", Some("server-secret"), true)
    }

    fn with_cookie(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_str(value).unwrap());
        headers
    }

    /// The `name=value` pair of a `Set-Cookie` value.
    fn pair(set_cookie: &str) -> &str {
        set_cookie.split(';').next().unwrap()
    }

    #[test]
    fn production_refuses_a_short_password() {
        assert!(check_password(None, true).is_ok());
        assert!(check_password(Some("short"), false).is_ok());
        assert!(check_password(Some("sixteen-chars-ok"), true).is_ok());
        assert_eq!(
            check_password(Some("fifteen-chars-x"), true).unwrap_err().to_string(),
            "[openshop] Invalid config: opsPassword must be at least 16 characters in production"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_right_password_opens_a_session_the_server_recognizes() {
        let auth = auth();
        let Login::Granted(cookie) = auth.login("correct horse battery staple").await else {
            panic!("refused");
        };
        assert!(cookie.starts_with("openshop_ops=v1."), "{cookie}");
        assert!(
            cookie.ends_with("; Path=/ops; Max-Age=43200; HttpOnly; SameSite=Strict; Secure"),
            "{cookie}"
        );
        assert!(auth.has_session(&with_cookie(pair(&cookie))));
        assert!(auth.has_session(&with_cookie(&format!("theme=dark; {}", pair(&cookie)))));

        // Development serves plain HTTP: the cookie is not `Secure` there.
        let development = OpsAuth::new("dev", None, false);
        let Login::Granted(cookie) = development.login("dev").await else {
            panic!("refused");
        };
        assert!(cookie.ends_with("SameSite=Strict"), "{cookie}");
    }

    #[test]
    fn a_session_is_refused_when_forged_expired_or_signed_for_another_password() {
        let auth = auth();
        let now = Utc::now().timestamp();
        let token = auth.session_token(now);
        assert!(auth.is_valid_token(&token, now));
        assert!(!auth.is_valid_token(&token, now + SESSION_SECONDS));

        // A later expiry under the same signature.
        let signature = token.rsplit('.').next().unwrap();
        assert!(!auth.is_valid_token(&format!("v1.{}.{signature}", now + 10 * SESSION_SECONDS), now));
        for forged in ["", "v1", "v1.1.2.3", "v2.9999999999.00", "v1.soon.00"] {
            assert!(!auth.is_valid_token(forged, now), "{forged}");
        }

        // Changing the password, or the secret of the server, closes every session.
        let other_password = OpsAuth::new("another password, as long", Some("server-secret"), true);
        let other_secret = OpsAuth::new("correct horse battery staple", Some("rotated"), true);
        let without_secret = OpsAuth::new("correct horse battery staple", None, true);
        for other in [other_password, other_secret, without_secret] {
            assert!(!other.is_valid_token(&token, now));
        }

        assert!(!auth.has_session(&HeaderMap::new()));
        assert!(!auth.has_session(&with_cookie("openshop_ops=")));
        assert!(!auth.has_session(&with_cookie(&format!("other={token}"))));
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_passwords_are_answered_later_and_later() {
        let auth = auth();
        for expected in [250, 500, 1_000, 2_000, 4_000, 8_000, 8_000] {
            let started = tokio::time::Instant::now();
            assert!(matches!(auth.login("guess").await, Login::Refused));
            assert_eq!(started.elapsed().as_millis(), expected);
        }

        // A success forgets the failures.
        assert!(matches!(auth.login("correct horse battery staple").await, Login::Granted(_)));
        let started = tokio::time::Instant::now();
        assert!(matches!(auth.login("guess").await, Login::Refused));
        assert_eq!(started.elapsed().as_millis(), 250);
    }

    #[tokio::test(start_paused = true)]
    async fn one_attempt_is_checked_at_a_time() {
        let auth = std::sync::Arc::new(auth());
        let first = tokio::spawn({
            let auth = auth.clone();
            async move { auth.login("guess").await }
        });
        // Let the first attempt reach its delay.
        tokio::task::yield_now().await;

        // Even the right password is not looked at while another attempt waits.
        assert!(matches!(auth.login("correct horse battery staple").await, Login::Busy));
        assert!(matches!(first.await.unwrap(), Login::Refused));
        assert!(matches!(auth.login("correct horse battery staple").await, Login::Granted(_)));
    }

    #[test]
    fn a_form_posted_from_another_site_is_recognized() {
        let site = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("sec-fetch-site", HeaderValue::from_str(value).unwrap());
            headers
        };
        assert!(is_same_site(&HeaderMap::new()));
        assert!(is_same_site(&site("same-origin")));
        assert!(is_same_site(&site("none")));
        assert!(!is_same_site(&site("cross-site")));
        assert!(!is_same_site(&site("same-site")));
    }
}
