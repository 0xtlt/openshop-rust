//! Serves the embedded admin UI: static assets and the single-page shell.
//!
//! The shell is only delivered to a shop that opens the app from Shopify admin
//! (signed query) and has it installed; everyone else is sent to OAuth, or
//! gets a page that says where the app opens from, with a 401. A recent launch
//! for a shop without a token first asks Shopify for one with the client
//! credentials grant, since the signed query vouches for the shop: a
//! development store of the organization that owns the app opens with no OAuth
//! redirect inside the admin iframe.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use axum::extract::{Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERER};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use indexmap::IndexMap;
use regex::Regex;

use super::auth::redirect;
use super::connectors::is_installed_shop;
use super::http::{header, not_found, parse_query};
use super::polaris;
use super::reload::Served;
use super::shop_domain::normalize_shop_domain;
use super::shopify_apps::DEFAULT_SHOPIFY_APP_HANDLE;
use super::{ROBOTS_HEADER, ServerOptions};
use crate::app::OpenShop;
use crate::shopify::client_credentials::Grant;
use openshop_shared::rpc::encode_uri_component;

/// How far, in seconds, the `timestamp` of a signed launch may be from now for
/// the launch to vouch for its shop and ask Shopify for a token.
pub(crate) const SIGNED_LAUNCH_FRESHNESS: i64 = 90;

/// What a launch whose query has a verified signature vouches for: its shop,
/// written as a shop domain, for the app that signed it, when its `timestamp`
/// is within [`SIGNED_LAUNCH_FRESHNESS`] of `now`. Nothing otherwise.
fn launch_grant(query: &IndexMap<String, String>, shopify_app: &str, shop: &str, now: i64) -> Option<Grant> {
    let timestamp: i64 = query.get("timestamp")?.trim().parse().ok()?;
    if now.abs_diff(timestamp) > SIGNED_LAUNCH_FRESHNESS.unsigned_abs() {
        return None;
    }
    let grant = Grant::vouched(shopify_app, query.get("shop")?)?;
    matches!(&grant, Grant::Vouched { shop: vouched, .. } if vouched == shop).then_some(grant)
}

/// Looks up an asset of a UI bundle compiled into the application binary.
#[derive(Clone, Copy)]
pub struct UiAssets(pub fn(&str) -> Option<Cow<'static, [u8]>>);

impl PartialEq for UiAssets {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::fn_addr_eq(self.0, other.0)
    }
}

impl Eq for UiAssets {}

impl std::fmt::Debug for UiAssets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UiAssets(..)")
    }
}

/// Where the admin UI assets come from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UiSource {
    /// API only: no UI is served.
    #[default]
    None,
    /// The UI bundle compiled into the framework.
    Embedded,
    /// A directory with a custom bundle (for apps that add their own admin pages).
    Dir(PathBuf),
    /// A custom bundle compiled into the application, e.g. with `rust-embed`.
    Assets(UiAssets),
}

#[cfg(feature = "embedded-ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "ui-dist/"]
struct EmbeddedUi;

impl UiSource {
    /// A bundle served by a lookup function taking the asset path (`index.html`,
    /// `app-1a2b3c.js`) and returning its bytes.
    pub fn assets(lookup: fn(&str) -> Option<Cow<'static, [u8]>>) -> Self {
        Self::Assets(UiAssets(lookup))
    }

    pub fn is_enabled(&self) -> bool {
        *self != Self::None
    }

    /// The asset at `path` (relative, no leading slash).
    fn read(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        if path.is_empty()
            || path
                .split(['/', '\\'])
                .any(|segment| segment == ".." || segment.is_empty())
        {
            return None;
        }
        match self {
            Self::None => None,
            #[cfg(feature = "embedded-ui")]
            Self::Embedded => EmbeddedUi::get(path).map(|file| file.data),
            #[cfg(not(feature = "embedded-ui"))]
            Self::Embedded => None,
            Self::Assets(assets) => (assets.0)(path),
            Self::Dir(directory) => {
                // Joining a rooted or drive-prefixed path would replace the directory.
                let relative = Path::new(path);
                if !relative
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                {
                    return None;
                }
                let file = directory.join(relative);
                file.is_file()
                    .then(|| std::fs::read(file).ok())
                    .flatten()
                    .map(Cow::Owned)
            }
        }
    }
}

const RESERVED_PREFIXES: [&str; 8] = [
    "/api",
    "/auth",
    "/webhooks",
    "/proxy",
    "/ext",
    "/routes",
    "/health",
    "/ops",
];

/// Whether a path is a page of the single-page app rather than a file or an API.
pub fn is_ui_shell_path(pathname: &str) -> bool {
    if RESERVED_PREFIXES
        .iter()
        .any(|prefix| pathname == *prefix || pathname.starts_with(&format!("{prefix}/")))
    {
        return false;
    }
    if pathname == "/" || pathname.ends_with('/') || pathname.ends_with(".html") {
        return true;
    }
    !pathname.rsplit('/').next().unwrap_or_default().contains('.')
}

static API_KEY_META: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"<meta name="?shopify-api-key"? content="[^"]*"\s*/?>"#).expect("valid regex"));
static APP_BRIDGE_SCRIPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:data-api-key="[^"]*" )?src="?https://cdn\.shopify\.com/shopifycloud/app-bridge\.js"?"#)
        .expect("valid regex")
});

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Injects the Shopify API key App Bridge needs to start.
pub fn inject_api_key(html: &str, api_key: &str) -> String {
    let api_key = escape_attribute(api_key);
    let html = API_KEY_META.replace(html, |_: &regex::Captures<'_>| {
        format!(r#"<meta name="shopify-api-key" content="{api_key}" />"#)
    });
    APP_BRIDGE_SCRIPT
        .replace(&html, |_: &regex::Captures<'_>| {
            format!(r#"data-api-key="{api_key}" src="https://cdn.shopify.com/shopifycloud/app-bridge.js""#)
        })
        .into_owned()
}

/// Decodes the `%XX` escapes of a request path. `None` when an escape is malformed,
/// hides a path separator or a NUL, or when the result is not UTF-8.
fn decode_asset_path(path: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut rest = path.as_bytes();
    while let [byte, tail @ ..] = rest {
        rest = tail;
        if *byte != b'%' {
            decoded.push(*byte);
            continue;
        }
        let [high, low, tail @ ..] = rest else {
            return None;
        };
        rest = tail;
        let value = (char::from(*high).to_digit(16)? * 16 + char::from(*low).to_digit(16)?) as u8;
        if matches!(value, b'/' | b'\\' | 0) {
            return None;
        }
        decoded.push(value);
    }
    String::from_utf8(decoded).ok()
}

/// Polaris, no form, and nothing else. The page can be framed: a shop whose
/// launch is refused reads it inside Shopify admin.
const WELCOME_POLICY: &str = polaris::content_security_policy!("form-action 'none'; base-uri 'none'");

/// What a visitor is told when the admin is not opened for a shop: where the
/// app opens from. It shows nothing of the application.
fn open_from_shopify_admin(status: StatusCode) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>Open this app from Shopify admin</title><style>{}</style><script src=\"{}\"></script>\
         </head><body><s-page inlineSize=\"small\"><s-section><s-stack gap=\"base\">\
         <s-heading>Open this app from Shopify admin</s-heading>\
         <s-paragraph>This app works inside Shopify admin. Open it from the Apps section of your store.</s-paragraph>\
         </s-stack></s-section></s-page></body></html>",
        polaris::FALLBACK_STYLE,
        polaris::SCRIPT,
    );
    (
        status,
        [
            (CONTENT_TYPE.as_str(), "text/html; charset=utf-8"),
            (CACHE_CONTROL.as_str(), "no-store"),
            (CONTENT_SECURITY_POLICY.as_str(), WELCOME_POLICY),
            ("x-robots-tag", ROBOTS_HEADER),
        ],
        html,
    )
        .into_response()
}

fn unauthorized_ui() -> Response {
    open_from_shopify_admin(StatusCode::UNAUTHORIZED)
}

fn shell_response(html: String, frame_ancestors: &str) -> Response {
    let mut response = (
        [(CONTENT_TYPE, "text/html; charset=UTF-8"), (CACHE_CONTROL, "no-store")],
        html,
    )
        .into_response();
    if let Ok(policy) = HeaderValue::from_str(&format!("frame-ancestors {frame_ancestors};")) {
        response.headers_mut().insert(CONTENT_SECURITY_POLICY, policy);
    }
    response
}

static CONTENT_HASH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[-.][0-9a-f]{8,}[-._]").expect("valid regex"));

/// Bundled assets carry a content hash in their name and never change;
/// anything else must be revalidated.
fn cache_control(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    if CONTENT_HASH.is_match(name) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

fn asset_response(path: &str, data: Cow<'static, [u8]>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = cache_control(path);
    (
        [
            (CONTENT_TYPE, mime.as_ref().to_string()),
            (CACHE_CONTROL, cache.to_string()),
        ],
        data.into_owned(),
    )
        .into_response()
}

pub(crate) async fn serve_ui(
    State(app): State<OpenShop>,
    options: ServerOptions,
    reload: Option<Arc<Served>>,
    request: Request,
) -> Response {
    let pathname = request.uri().path().to_string();

    if !is_ui_shell_path(&pathname) {
        let asset =
            decode_asset_path(pathname.trim_start_matches('/')).and_then(|path| Some((options.ui.read(&path)?, path)));
        return match asset {
            Some((data, path)) if matches!(*request.method(), Method::GET | Method::HEAD) => {
                asset_response(&path, data)
            }
            _ => not_found(),
        };
    }

    let query = parse_query(request.uri().query());
    let (api_key, frame_ancestors) = if options.dev {
        // Development: no signature gate, but keep stray visitors out of tunnels.
        let headers = request.headers();
        let host = header(headers, "host").unwrap_or_default();
        let referer = header(headers, REFERER.as_str()).unwrap_or_default();
        let allowed = host.contains("localhost")
            || host.starts_with("127.0.0.1")
            || host.starts_with("[::1]")
            || header(headers, "sec-fetch-dest") == Some("iframe")
            || referer.contains("myshopify.com")
            || referer.contains("admin.shopify.com")
            || query.contains_key("shop");
        if !allowed {
            return open_from_shopify_admin(StatusCode::FORBIDDEN);
        }
        let shopify_app = app
            .apps()
            .by_signed_query(&query)
            .ok()
            .or_else(|| app.apps().all().first());
        (
            shopify_app
                .map(|shopify_app| shopify_app.api_key.clone())
                .unwrap_or_default(),
            "https://*.myshopify.com https://admin.shopify.com http://localhost:*".to_string(),
        )
    } else {
        let Some(shop) = query.get("shop").and_then(|shop| normalize_shop_domain(shop)) else {
            return unauthorized_ui();
        };
        let Ok(shopify_app) = app.apps().by_signed_query(&query) else {
            return unauthorized_ui();
        };
        // Only a recent launch vouches for its shop: an older one, replayed,
        // is served as before, from the stored token alone.
        let grant = launch_grant(&query, &shopify_app.handle, &shop, Utc::now().timestamp());
        match is_installed_shop(&app, &shopify_app.handle, &shop, grant.as_ref()).await {
            Ok(true) => {}
            Ok(false) => {
                let app_param = if shopify_app.handle == DEFAULT_SHOPIFY_APP_HANDLE {
                    String::new()
                } else {
                    format!("&app={}", encode_uri_component(&shopify_app.handle))
                };
                return redirect(&format!("/auth?shop={}{app_param}", encode_uri_component(&shop)));
            }
            Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response(),
        }
        (
            shopify_app.api_key.clone(),
            format!("https://{shop} https://admin.shopify.com"),
        )
    };

    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return not_found();
    }
    let Some(index) = options.ui.read("index.html") else {
        return not_found();
    };
    let mut shell = inject_api_key(&String::from_utf8_lossy(&index), &api_key);
    if let Some(served) = reload {
        shell = served.inject(&shell);
    }
    shell_response(shell, &frame_ancestors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_recent_signed_launch_vouches_for_its_shop() {
        let now = 1_790_000_000;
        let query = |shop: &str, timestamp: i64| -> IndexMap<String, String> {
            [("shop", shop.to_string()), ("timestamp", timestamp.to_string())]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect()
        };
        let shop = "dev-store-1.myshopify.com";
        let vouched = Some(Grant::Vouched {
            shopify_app: "default".to_string(),
            shop: shop.to_string(),
        });
        for timestamp in [now, now - SIGNED_LAUNCH_FRESHNESS, now + SIGNED_LAUNCH_FRESHNESS] {
            assert_eq!(launch_grant(&query(shop, timestamp), "default", shop, now), vouched);
        }
        for timestamp in [now - SIGNED_LAUNCH_FRESHNESS - 1, now + SIGNED_LAUNCH_FRESHNESS + 1, 1_770_000_000] {
            assert_eq!(launch_grant(&query(shop, timestamp), "default", shop, now), None);
        }
        let mut no_timestamp = query(shop, now);
        no_timestamp.shift_remove("timestamp");
        assert_eq!(launch_grant(&no_timestamp, "default", shop, now), None);
        let mut garbled = query(shop, now);
        garbled.insert("timestamp".to_string(), "soon".to_string());
        assert_eq!(launch_grant(&garbled, "default", shop, now), None);
        // A shop that is not written as a shop domain vouches for nothing.
        let url = query("https://dev-store-1.myshopify.com/admin", now);
        assert_eq!(launch_grant(&url, "default", shop, now), None);
    }

    #[test]
    fn caches_only_content_hashed_assets_forever() {
        for path in [
            "/openshop-ui-659c6cf8147c7a81.js",
            "/openshop-ui-659c6cf8147c7a81_bg.wasm",
            "/assets/app.1a2b3c4d5e.css",
        ] {
            assert_eq!(cache_control(path), "public, max-age=31536000, immutable", "{path}");
        }
        for path in ["/favicon.ico", "/assets/app.js", "/logo-dark.svg", "/deadbeef/app.js"] {
            assert_eq!(cache_control(path), "no-cache", "{path}");
        }
    }

    #[test]
    fn serves_assets_from_a_lookup_function() {
        let source = UiSource::assets(|path| (path == "index.html").then(|| Cow::Borrowed(b"<html>".as_slice())));
        assert!(source.is_enabled());
        assert_eq!(source.read("index.html").as_deref(), Some(b"<html>".as_slice()));
        assert!(source.read("missing.js").is_none());
        assert!(source.read("../index.html").is_none());
        assert_eq!(source, source.clone());
    }

    #[test]
    fn recognizes_shell_paths() {
        for path in [
            "/",
            "/flows",
            "/flows/",
            "/runs/123",
            "/index.html",
            "/reviews/42",
            "/apis",
        ] {
            assert!(is_ui_shell_path(path), "{path}");
        }
        for path in [
            "/api",
            "/api/flows",
            "/auth/callback",
            "/webhooks",
            "/proxy/reviews",
            "/ext/reviews",
            "/routes/ping",
            "/health",
            "/assets/app.js",
            "/favicon.ico",
            "/openshop-ui_bg.wasm",
        ] {
            assert!(!is_ui_shell_path(path), "{path}");
        }
    }

    #[test]
    fn injects_the_api_key_into_the_shell() {
        let html = r#"<head><meta name="shopify-api-key" content="" />
<script src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script></head>"#;
        let injected = inject_api_key(html, "key-123");
        assert!(injected.contains(r#"<meta name="shopify-api-key" content="key-123" />"#));
        assert!(
            injected.contains(
                r#"<script data-api-key="key-123" src="https://cdn.shopify.com/shopifycloud/app-bridge.js">"#
            )
        );

        let again = inject_api_key(&injected, "key-456");
        assert!(again.contains(r#"content="key-456""#));
        assert!(again.contains(r#"data-api-key="key-456" src="#));
        assert!(!again.contains("key-123"));
    }

    #[test]
    fn tolerates_minified_shell_markup_and_escapes_the_key() {
        let html = r#"<meta name=shopify-api-key content=""><script src=https://cdn.shopify.com/shopifycloud/app-bridge.js></script>"#;
        let injected = inject_api_key(html, "a\"b");
        assert!(injected.contains(r#"content="a&quot;b""#));
        assert!(
            injected.contains(r#"data-api-key="a&quot;b" src="https://cdn.shopify.com/shopifycloud/app-bridge.js""#)
        );
    }

    #[test]
    fn refuses_path_traversal() {
        let dir = UiSource::Dir(std::env::temp_dir());
        assert!(dir.read("../etc/passwd").is_none());
        assert!(dir.read("a//b").is_none());
        assert!(dir.read("").is_none());
        assert!(dir.read("/etc/hosts").is_none());
        assert!(dir.read("..\\secret.txt").is_none());
        assert!(dir.read("assets\\..\\..\\secret.txt").is_none());
        assert!(UiSource::None.read("index.html").is_none());
    }

    #[test]
    fn decodes_asset_paths_without_revealing_separators() {
        assert_eq!(decode_asset_path("assets/app.js").as_deref(), Some("assets/app.js"));
        assert_eq!(decode_asset_path("My%20File.txt").as_deref(), Some("My File.txt"));
        assert_eq!(decode_asset_path("%C3%A9t%c3%a9.txt").as_deref(), Some("été.txt"));
        assert_eq!(decode_asset_path("%252e%252e").as_deref(), Some("%2e%2e"));
        assert_eq!(decode_asset_path("%2e%2e/secret.txt").as_deref(), Some("../secret.txt"));
        for path in [
            "..%2fsecret.txt",
            "..%5Csecret.txt",
            "a%00.txt",
            "a%2.txt",
            "a%",
            "a%zz.txt",
            "a%+f.txt",
            "%ff.txt",
        ] {
            assert_eq!(decode_asset_path(path), None, "{path}");
        }
    }
}
