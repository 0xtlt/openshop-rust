//! Access to the operator dashboard: one password, and a signed session
//! cookie that needs no table.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, bail};
use axum::http::HeaderMap;
use axum::http::header::COOKIE;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use crate::server::client::client_key;
use crate::server::hmac::{constant_time_eq, hmac_sha256};
use crate::server::http::header;

/// In production, a shorter password is refused when the application starts.
const MIN_PRODUCTION_PASSWORD_LENGTH: usize = 16;

const COOKIE_NAME: &str = "openshop_ops";
const SESSION_SECONDS: i64 = 12 * 60 * 60;
/// What a session is signed with is derived from the password under this label.
const KEY_LABEL: &str = "openshop:ops-session:v1:";
const FIRST_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BACKOFF_DOUBLINGS: u32 = 5;
/// How long a client that stopped guessing is remembered.
const FORGET_AFTER: Duration = Duration::from_secs(15 * 60);
/// How many clients are remembered at once.
const MAX_CLIENTS: usize = 10_000;

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
    /// This client attempted too recently: the attempt was not looked at, and
    /// may be made again after the delay.
    Busy(Duration),
}

/// What the dashboard remembers of a client that guessed.
struct Guesses {
    failures: u32,
    /// Until when its attempts are not looked at.
    next_attempt: Instant,
}

pub(crate) struct OpsAuth {
    password_digest: [u8; 32],
    session_key: String,
    /// Off in development, where the server is reached over plain HTTP.
    secure_cookie: bool,
    /// The clients that attempted, by address. Only held to read and write
    /// these, never while a password is checked or a response waits.
    guesses: Mutex<HashMap<Option<IpAddr>, Guesses>>,
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
            guesses: Mutex::new(HashMap::new()),
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

    /// Checks a password for a client. Each client may attempt once, then
    /// again after a delay that doubles with each failure, so that guessing
    /// stays slow however many connections it uses. Every client has its own
    /// delay: one that guesses cannot keep another from signing in, and an
    /// attempt is answered at once, whatever the delay.
    ///
    /// Behind a proxy that does not say who called, every client is the same
    /// one: see [`client_ip`](crate::server::client::client_ip).
    pub(crate) fn login(&self, client: Option<IpAddr>, candidate: &str) -> Login {
        let client = client.map(client_key);
        let now = Instant::now();
        {
            let mut guesses = self.guesses.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if guesses.len() >= MAX_CLIENTS && !guesses.contains_key(&client) {
                guesses.retain(|_, known| now < known.next_attempt + FORGET_AFTER);
                if guesses.len() >= MAX_CLIENTS {
                    // Still full: the one that attempted longest ago is forgotten.
                    if let Some(oldest) = guesses
                        .iter()
                        .min_by_key(|(_, known)| known.next_attempt)
                        .map(|(key, _)| *key)
                    {
                        guesses.remove(&oldest);
                    }
                }
            }
            let known = guesses.entry(client).or_insert(Guesses {
                failures: 0,
                next_attempt: now,
            });
            if now < known.next_attempt {
                return Login::Busy(known.next_attempt - now);
            }
            // The attempt counts as a failure until the password is right, so
            // that attempts racing each other are not all looked at.
            if now >= known.next_attempt + FORGET_AFTER {
                known.failures = 0;
            }
            known.next_attempt = now + FIRST_BACKOFF * 2u32.pow(known.failures.min(MAX_BACKOFF_DOUBLINGS));
            known.failures = known.failures.saturating_add(1);
        }

        // Digests have one length: the comparison says nothing of the password's.
        if constant_time_eq(&digest(candidate), &self.password_digest) {
            self.guesses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&client);
            return Login::Granted(self.cookie(&self.session_token(Utc::now().timestamp()), SESSION_SECONDS));
        }
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

    const RIGHT: &str = "correct horse battery staple";

    fn client(address: &str) -> Option<IpAddr> {
        Some(address.parse().unwrap())
    }

    #[tokio::test(start_paused = true)]
    async fn the_right_password_opens_a_session_the_server_recognizes() {
        let auth = auth();
        let Login::Granted(cookie) = auth.login(client("203.0.113.1"), RIGHT) else {
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
        let Login::Granted(cookie) = development.login(None, "dev") else {
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
    async fn a_client_may_attempt_again_after_a_delay_that_doubles() {
        let auth = auth();
        let me = client("203.0.113.1");
        for delay in [250, 500, 1_000, 2_000, 4_000, 8_000, 8_000] {
            assert!(matches!(auth.login(me, "guess"), Login::Refused));
            // Too soon, whatever the password.
            let Login::Busy(left) = auth.login(me, RIGHT) else {
                panic!("looked at");
            };
            assert_eq!(left.as_millis(), delay);
            tokio::time::advance(Duration::from_millis(delay as u64 - 1)).await;
            assert!(matches!(auth.login(me, RIGHT), Login::Busy(_)));
            tokio::time::advance(Duration::from_millis(1)).await;
        }

        // A success forgets the failures.
        assert!(matches!(auth.login(me, RIGHT), Login::Granted(_)));
        assert!(matches!(auth.login(me, "guess"), Login::Refused));
        let Login::Busy(left) = auth.login(me, "guess") else {
            panic!("looked at");
        };
        assert_eq!(left.as_millis(), 250);
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_guesses_does_not_keep_another_from_signing_in() {
        let auth = auth();
        let attacker = client("203.0.113.1");
        // The attacker never stops, from as many connections as it likes.
        for _ in 0..50 {
            let _ = auth.login(attacker, "guess");
        }
        assert!(matches!(auth.login(attacker, RIGHT), Login::Busy(_)));

        // Nobody else waits for it, and the time it takes does not matter.
        assert!(matches!(auth.login(client("203.0.113.2"), RIGHT), Login::Granted(_)));
        assert!(matches!(auth.login(client("2001:db8::1"), "guess"), Login::Refused));
        assert!(matches!(auth.login(client("2001:db8::1"), RIGHT), Login::Busy(_)));
        // A client whose address is unknown is one client of its own.
        assert!(matches!(auth.login(None, RIGHT), Login::Granted(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn clients_that_stopped_guessing_are_forgotten() {
        let auth = auth();
        for index in 0..MAX_CLIENTS {
            let address = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + index as u32));
            assert!(matches!(auth.login(Some(address), "guess"), Login::Refused));
        }
        assert_eq!(auth.guesses.lock().unwrap().len(), MAX_CLIENTS);
        tokio::time::advance(FORGET_AFTER + Duration::from_secs(60)).await;
        assert!(matches!(auth.login(client("203.0.113.9"), "guess"), Login::Refused));
        assert_eq!(auth.guesses.lock().unwrap().len(), 1);

        // Full of clients that are still guessing: the oldest makes room.
        for index in 0..MAX_CLIENTS {
            let address = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + index as u32));
            let _ = auth.login(Some(address), "guess");
        }
        assert!(auth.guesses.lock().unwrap().len() <= MAX_CLIENTS);
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
