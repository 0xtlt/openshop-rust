//! App Bridge globals exposed on `window.shopify`.

use std::cell::RefCell;

use crate::dom::{call_method, js_error_message, member};
use js_sys::{Date, Object, Promise, Reflect};
use leptos::prelude::window;
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

fn shopify() -> Option<JsValue> {
    member(&window(), "shopify")
}

pub(crate) fn has_id_token() -> bool {
    shopify().and_then(|shopify| member(&shopify, "idToken")).is_some()
}

/// How long before it expires a session token stops being reused, in milliseconds: the request
/// it goes with reaches the server well before.
const TOKEN_MARGIN_MS: f64 = 15_000.0;

/// The session token the requests of the page share.
enum SharedToken {
    /// Asked of App Bridge: the requests made meanwhile wait for the same answer. `request`
    /// tells this request from the ones made before or after it.
    Pending { request: u64, promise: Promise },
    /// Received: reused until `until`, a time of `Date.now()`.
    Ready { token: String, until: f64 },
}

thread_local! {
    static TOKEN: RefCell<Option<SharedToken>> = const { RefCell::new(None) };
    static TOKEN_REQUESTS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The bytes of base64url text, padded or not.
fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(text.len() * 3 / 4);
    let (mut buffer, mut bits) = (0u32, 0u32);
    for character in text.trim_end_matches('=').bytes() {
        let value = match character {
            b'A'..=b'Z' => character - b'A',
            b'a'..=b'z' => character - b'a' + 26,
            b'0'..=b'9' => character - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(bytes)
}

/// Until when a session token received at `received` (milliseconds of `Date.now()`) may be
/// reused: [`TOKEN_MARGIN_MS`] before the end of its lifetime (`exp - iat`) counted from when it
/// was received, and before its `exp` on the clock of the browser, whichever is first. Its
/// claims are read without being verified: the server verifies them. `None` when it cannot be
/// reused: its claims cannot be read, or it is that close to its end.
pub(crate) fn reuse_until(token: &str, received: f64) -> Option<f64> {
    let claims: Value = serde_json::from_slice(&base64url_decode(token.split('.').nth(1)?)?).ok()?;
    let seconds = |name: &str| claims.get(name).and_then(Value::as_f64);
    let (expires, issued) = (seconds("exp")?, seconds("iat")?);
    let until = (received + (expires - issued) * 1000.0).min(expires * 1000.0) - TOKEN_MARGIN_MS;
    (until > received).then_some(until)
}

/// A session token from `shopify.idToken()`, or `None` outside Shopify admin.
///
/// A token lives a minute: the requests of the page share one until shortly before it expires,
/// and the requests made while App Bridge is asked wait for its answer, instead of asking for a
/// token each.
pub async fn id_token() -> Result<Option<String>, String> {
    let Some(shopify) = shopify().filter(|shopify| member(shopify, "idToken").is_some()) else {
        return Ok(None);
    };
    let now = Date::now();
    let shared = TOKEN.with(|shared| match &*shared.borrow() {
        Some(SharedToken::Ready { token, until }) if *until > now => Some(Ok(token.clone())),
        Some(SharedToken::Pending { request, promise }) => Some(Err((*request, promise.clone()))),
        _ => None,
    });
    let (request, promise) = match shared {
        Some(Ok(token)) => return Ok(Some(token)),
        Some(Err(pending)) => pending,
        None => {
            let promise = call_method(&shopify, "idToken", &[]).map_err(|error| js_error_message(&error))?;
            let promise = Promise::resolve(&promise);
            let request = TOKEN_REQUESTS.with(|requests| requests.replace(requests.get() + 1));
            TOKEN.with(|shared| {
                *shared.borrow_mut() = Some(SharedToken::Pending {
                    request,
                    promise: promise.clone(),
                })
            });
            (request, promise)
        }
    };
    let answer = JsFuture::from(promise).await;
    let received = Date::now();
    TOKEN.with(|shared| {
        let mut shared = shared.borrow_mut();
        // The first request to get the answer keeps it; a token forgotten meanwhile stays so.
        if matches!(&*shared, Some(SharedToken::Pending { request: pending, .. }) if *pending == request) {
            *shared = answer
                .as_ref()
                .ok()
                .and_then(JsValue::as_string)
                .and_then(|token| reuse_until(&token, received).map(|until| SharedToken::Ready { token, until }));
        }
    });
    let token = answer.map_err(|error| js_error_message(&error))?;
    Ok(token.as_string())
}

/// Forgets the shared session token, which the server refused: the next request asks for
/// another.
pub(crate) fn forget_id_token() {
    TOKEN.with(|shared| *shared.borrow_mut() = None);
}

/// Options of `shopify.toast.show`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToastOptions {
    /// Display time in milliseconds.
    pub duration: Option<u32>,
    pub is_error: bool,
}

/// Shows an App Bridge toast. Returns `false` when toasts are unavailable.
pub fn toast(message: &str) -> bool {
    toast_with(message, ToastOptions::default())
}

/// Shows an App Bridge error toast. Returns `false` when toasts are unavailable.
pub fn toast_error(message: &str) -> bool {
    toast_with(
        message,
        ToastOptions {
            is_error: true,
            ..ToastOptions::default()
        },
    )
}

/// Shows an App Bridge toast with options. Returns `false` when toasts are unavailable.
pub fn toast_with(message: &str, options: ToastOptions) -> bool {
    let Some(toast) = shopify().and_then(|shopify| member(&shopify, "toast")) else {
        return false;
    };
    let mut arguments = vec![JsValue::from_str(message)];
    if options != ToastOptions::default() {
        let object = Object::new();
        if let Some(duration) = options.duration {
            let _ = Reflect::set(&object, &"duration".into(), &duration.into());
        }
        if options.is_error {
            let _ = Reflect::set(&object, &"isError".into(), &true.into());
        }
        arguments.push(object.unchecked_into());
    }
    let _ = call_method(&toast, "show", &arguments);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token with these claims; its header and signature do not matter here.
    fn token(claims: &str) -> String {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut encoded = String::new();
        for chunk in claims.as_bytes().chunks(3) {
            let value =
                chunk.iter().fold(0u32, |value, byte| (value << 8) | u32::from(*byte)) << (8 * (3 - chunk.len()));
            for index in 0..=chunk.len() {
                encoded.push(ALPHABET[((value >> (18 - 6 * index)) & 63) as usize] as char);
            }
        }
        format!("eyJhbGciOiJIUzI1NiJ9.{encoded}.signature")
    }

    #[test]
    fn decodes_base64url() {
        assert_eq!(base64url_decode("aGk").unwrap(), b"hi");
        assert_eq!(base64url_decode("aGk=").unwrap(), b"hi");
        assert_eq!(base64url_decode("-_8").unwrap(), [0xfb, 0xff]);
        assert!(base64url_decode("a.b").is_none());
    }

    #[test]
    fn a_token_is_reused_until_shortly_before_it_expires() {
        // A token of Shopify lives a minute.
        let issued = 1_800_000_000.0;
        let minute = token(&format!(
            r#"{{"iss":"x","exp":{},"iat":{issued},"nbf":{issued}}}"#,
            issued + 60.0
        ));
        // Received half a second after it was issued: until 15 s before its `exp`.
        let received = issued * 1000.0 + 500.0;
        assert_eq!(reuse_until(&minute, received), Some(issued * 1000.0 + 45_000.0));
        // The clock of the browser is 20 s behind the one of Shopify: its `exp` would come late,
        // its lifetime counted from when it was received comes first.
        let behind = issued * 1000.0 - 20_000.0;
        assert_eq!(reuse_until(&minute, behind), Some(behind + 45_000.0));
        // Too close to its end, or past it: not reused.
        assert_eq!(reuse_until(&minute, received + 50_000.0), None);

        // A lifetime shorter than the margin, or claims that cannot be read: not reused.
        let short = token(&format!(r#"{{"exp":{},"iat":{issued}}}"#, issued + 10.0));
        assert_eq!(reuse_until(&short, received), None);
        assert_eq!(reuse_until(&token(r#"{"exp":"soon"}"#), received), None);
        assert_eq!(reuse_until("not-a-token", received), None);
    }
}
