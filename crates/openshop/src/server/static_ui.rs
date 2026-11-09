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
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_ENCODING, CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_NONE_MATCH, REFERER, VARY,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use regex::Regex;
use sha2::{Digest, Sha256};

use super::auth::redirect;
use super::compression;
use super::connectors::is_installed_shop;
use super::ServerOptions;
use super::http::{escape, header, not_found, parse_query};
use super::jwt::is_staff_subject;
use super::reload::Served;
use super::shop::ensure_installation;
use super::shop_domain::normalize_shop_domain;
use super::shopify_apps::{DEFAULT_APP_NAME, ResolvedShopifyApp};
use crate::app::OpenShop;
use crate::shopify::client_credentials::Grant;
use crate::shopify::endpoints::{Endpoints, SHOPIFY_APP_BRIDGE, SHOPIFY_POLARIS, ScriptUrl};
use shopify_api::oauth::{SessionToken, SignedQuery};
use openshop_shared::rpc::{decode_uri_component, encode_uri_component};

/// What a launch whose query verified vouches for: its shop, written as a
/// shop domain, for the app that signed it. `shopify_api` verifies only a
/// launch of the last 90 seconds: an older one, replayed, is not one.
fn launch_grant(query: &SignedQuery, shopify_app: &str, shop: &str) -> Option<Grant> {
    let grant = Grant::vouched(shopify_app, query.get("shop")?)?;
    matches!(&grant, Grant::Vouched { shop: vouched, .. } if vouched == shop).then_some(grant)
}

/// The session token Shopify managed installation opens the app with, as
/// `id_token`, when it is valid for the app that signed the launch, for its
/// shop, and for a staff member.
fn launch_session(shopify_app: &ResolvedShopifyApp, query: &SignedQuery, shop: &str) -> Option<SessionToken> {
    let session = shopify_app.oauth.verify_session_token(query.get("id_token")?).ok()?;
    let staff = is_staff_subject(session.user_id().unwrap_or_default());
    (staff && session.shop().as_str() == shop).then_some(session)
}

/// Whether the shop of a verified launch has the app installed, once given a
/// token when it had none: by the token exchange of the launch's session
/// token first, as the first `/api` request would, then by the client
/// credentials grant the launch vouches for. The shop renders without the
/// OAuth redirect, which Shopify does not display inside the admin frame.
pub(crate) async fn is_installed_for_launch(
    app: &OpenShop,
    shopify_app: &ResolvedShopifyApp,
    shop: &str,
    query: &SignedQuery,
) -> anyhow::Result<bool> {
    if is_installed_shop(app, &shopify_app.handle, shop, None).await? {
        return Ok(true);
    }
    if let Some(session) = launch_session(shopify_app, query, shop) {
        // Best-effort, as for an `/api` request: a failure keeps the fallbacks below.
        let _ = ensure_installation(app, shopify_app, shop, &session).await;
    }
    let grant = launch_grant(query, &shopify_app.handle, shop);
    is_installed_shop(app, &shopify_app.handle, shop, grant.as_ref()).await
}

/// A verified launch of the app with `plan_handle` (or `charge_id`) in its
/// query is the welcome link of a plan the merchant just chose: the plan
/// kept for the shop is forgotten, so that the next check reads the new one.
/// Not an answer that has that plan and is younger than ten seconds: the
/// route of the welcome link read it, then sent the launch here.
pub(crate) async fn forget_plan_on_welcome(app: &OpenShop, shopify_app: &str, query: &SignedQuery) {
    if query.get("plan_handle").is_none() && query.get("charge_id").is_none() {
        return;
    }
    if let Some(shop) = query.get("shop").and_then(normalize_shop_domain) {
        app.plans()
            .forget_on_welcome(shopify_app, &shop, query.get("plan_handle"))
            .await;
    }
}

/// The admin the server serves: the one it renders itself, by default.
#[derive(Debug, Clone, Default)]
pub enum UiSource {
    /// API only: no UI is served.
    None,
    /// The UI bundle compiled into the framework.
    Embedded,
    /// A directory with a custom bundle (for apps that add their own admin pages).
    Dir(PathBuf),
    /// A custom bundle compiled into the application, e.g. with `rust-embed`:
    /// a lookup function taking the asset path (`index.html`, `app-1a2b3c.js`)
    /// and returning its bytes.
    Assets(fn(&str) -> Option<Cow<'static, [u8]>>),
    /// The admin rendered by the server, page by page: no bundle, no
    /// WebAssembly. The default.
    #[default]
    Server,
}

#[cfg(feature = "embedded-ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "ui-dist/"]
struct EmbeddedUi;

impl UiSource {
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// The asset at `path` (relative, no leading slash). A directory is read
    /// without holding a thread of the runtime.
    async fn read(&self, path: &str) -> Option<Cow<'static, [u8]>> {
        if path.is_empty()
            || path
                .split(['/', '\\'])
                .any(|segment| segment == ".." || segment.is_empty())
        {
            return None;
        }
        match self {
            Self::None | Self::Server => None,
            #[cfg(feature = "embedded-ui")]
            Self::Embedded => EmbeddedUi::get(path).map(|file| file.data),
            #[cfg(not(feature = "embedded-ui"))]
            Self::Embedded => None,
            Self::Assets(lookup) => lookup(path),
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
                if !tokio::fs::metadata(&file).await.ok()?.is_file() {
                    return None;
                }
                tokio::fs::read(file).await.ok().map(Cow::Owned)
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

/// Injects the Shopify API key App Bridge needs to start.
pub fn inject_api_key(html: &str, api_key: &str) -> String {
    let api_key = escape(api_key);
    let html = API_KEY_META.replace(html, |_: &regex::Captures<'_>| {
        format!(r#"<meta name="shopify-api-key" content="{api_key}" />"#)
    });
    APP_BRIDGE_SCRIPT
        .replace(&html, |_: &regex::Captures<'_>| {
            format!(r#"data-api-key="{api_key}" src="{SHOPIFY_APP_BRIDGE}""#)
        })
        .into_owned()
}

/// The `src` of Shopify's App Bridge and of its Polaris, quoted or not.
static APP_BRIDGE_SRC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"src="?https://cdn\.shopify\.com/shopifycloud/app-bridge\.js"?"#).expect("valid regex")
});
static POLARIS_SRC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"src="?https://cdn\.shopify\.com/shopifycloud/polaris\.js"?"#).expect("valid regex"));

/// A page with its scripts of Shopify's App Bridge and Polaris loaded from
/// `app_bridge` and `polaris` instead, when they are given. A tag keeps its
/// other attributes, `data-api-key` included. Each URL is escaped where it is
/// written.
fn use_scripts(mut html: String, app_bridge: Option<&str>, polaris: Option<&str>) -> String {
    for (src, url) in [(&*APP_BRIDGE_SRC, app_bridge), (&*POLARIS_SRC, polaris)] {
        if let Some(url) = url {
            let replacement = format!(r#"src="{}""#, escape(url));
            html = src.replace_all(&html, regex::NoExpand(&replacement)).into_owned();
        }
    }
    html
}

/// The `<title>` of the shell of every bundle, `OpenShop`, which stands for the
/// name of the app.
static DEFAULT_TITLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"<title>\s*{DEFAULT_APP_NAME}\s*</title>")).expect("valid regex"));

/// A page with the name of the app as its title, in place of the `OpenShop` of
/// the shell. App Bridge titles a page whose `s-page` has no `heading` (and
/// that has no `ui-title-bar`) with it in the title bar of the admin, and the
/// browser tab shows it. A title of its own is kept.
fn name_the_app(html: &str, name: &str) -> String {
    let title = format!("<title>{}</title>", escape(name));
    DEFAULT_TITLE.replace(html, regex::NoExpand(&title)).into_owned()
}

/// The shell as it is served: the name of the app as its title, the API key
/// App Bridge starts with, and the App Bridge and Polaris of the Shopify
/// endpoints.
fn render_shell(index: &str, app: &ShellApp<'_>, endpoints: &Endpoints) -> String {
    use_scripts(
        inject_api_key(&name_the_app(index, app.name), app.api_key),
        endpoints.app_bridge.as_ref().map(ScriptUrl::as_str),
        endpoints.polaris.as_ref().map(ScriptUrl::as_str),
    )
}

/// The app the shell is served for.
struct ShellApp<'a> {
    api_key: &'a str,
    name: &'a str,
}

impl<'a> ShellApp<'a> {
    fn of(app: Option<&'a ResolvedShopifyApp>) -> Self {
        Self {
            api_key: app.map_or("", |app| app.api_key.as_str()),
            name: app.map_or(DEFAULT_APP_NAME, |app| app.name.as_str()),
        }
    }
}

/// Decodes the `%XX` escapes of a request path. `None` when an escape is malformed,
/// hides a path separator or a NUL, or when the result is not UTF-8.
fn decode_asset_path(path: &str) -> Option<String> {
    let segments: Option<Vec<String>> = path
        .split('/')
        .map(|segment| decode_uri_component(segment).filter(|decoded| !decoded.contains(['/', '\\', '\0'])))
        .collect();
    Some(segments?.join("/"))
}

/// Keeps the page readable while Polaris is not loaded, or when Shopify's CDN
/// cannot be reached.
const POLARIS_FALLBACK_STYLE: &str = "
body { margin: 0; font-family: system-ui, -apple-system, 'Segoe UI', sans-serif; }
s-page:not(:defined), s-section:not(:defined), s-stack:not(:defined), s-heading:not(:defined),
s-paragraph:not(:defined) { display: block; margin: 8px 16px; }
s-heading:not(:defined) { font-weight: 600; }
";

/// The Polaris script, what it loads from Shopify's CDN, each under its own
/// path (the styles and the files of its font, its icons), no form, and
/// nothing else. The page can be framed: a shop whose launch is refused reads
/// it inside Shopify admin.
const WELCOME_POLICY: &str = "default-src 'none'; \
    script-src https://cdn.shopify.com/shopifycloud/polaris.js; \
    style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/; \
    font-src https://cdn.shopify.com/static/fonts/; \
    connect-src https://cdn.shopify.com/shopifycloud/; \
    form-action 'none'; base-uri 'none'";

/// [`WELCOME_POLICY`] for a Polaris that is not Shopify's: its origin in
/// `script-src`, and in the directives of what it loads, its styles, its font
/// and its icons, which a copy of Polaris serves from where it is. Shopify's
/// CDN stays: a stand-in may redirect there.
fn welcome_policy(polaris: Option<&ScriptUrl>) -> Cow<'static, str> {
    let Some(polaris) = polaris else {
        return Cow::Borrowed(WELCOME_POLICY);
    };
    let directives: Vec<String> = WELCOME_POLICY
        .split("; ")
        .map(|directive| match directive.split_once(' ') {
            Some(("script-src" | "style-src" | "font-src" | "connect-src", _)) => {
                format!("{directive} {}", polaris.origin())
            }
            _ => directive.to_string(),
        })
        .collect();
    Cow::Owned(directives.join("; "))
}

/// What a visitor is told when the admin is not opened for a shop: where the
/// app opens from. It shows nothing of the application. Its Polaris is the
/// one of the Shopify endpoints.
pub(crate) fn open_from_shopify_admin(status: StatusCode, endpoints: &Endpoints) -> Response {
    let polaris = match &endpoints.polaris {
        Some(polaris) => Cow::Owned(escape(polaris.as_str())),
        None => Cow::Borrowed(SHOPIFY_POLARIS),
    };
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>Open this app from Shopify admin</title><style>{}</style><script src=\"{}\"></script>\
         </head><body><s-page inlineSize=\"small\"><s-section><s-stack gap=\"base\">\
         <s-heading>Open this app from Shopify admin</s-heading>\
         <s-paragraph>This app works inside Shopify admin. Open it from the Apps section of your store.</s-paragraph>\
         </s-stack></s-section></s-page></body></html>",
        POLARIS_FALLBACK_STYLE,
        polaris,
    );
    let policy = welcome_policy(endpoints.polaris.as_ref());
    (
        status,
        [
            (CONTENT_TYPE.as_str(), "text/html; charset=utf-8"),
            (CACHE_CONTROL.as_str(), "no-store"),
            (CONTENT_SECURITY_POLICY.as_str(), policy.as_ref()),
        ],
        html,
    )
        .into_response()
}

fn unauthorized_ui(endpoints: &Endpoints) -> Response {
    open_from_shopify_admin(StatusCode::UNAUTHORIZED, endpoints)
}

fn shell_response(html: String, frame_ancestors: &str) -> Response {
    let mut response = (
        [
            (CONTENT_TYPE, "text/html; charset=UTF-8"),
            (CACHE_CONTROL, "no-store"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
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

/// The ETag of the bytes of an asset that is compiled in, by where they are:
/// such bytes never change nor move, so each is hashed once.
static COMPILED_ETAGS: LazyLock<Mutex<HashMap<(usize, usize), HeaderValue>>> = LazyLock::new(Mutex::default);

/// A strong ETag: the start of the SHA-256 of the bytes, quoted.
fn etag_of(data: &[u8]) -> HeaderValue {
    let hash = Sha256::digest(data);
    HeaderValue::from_str(&format!("\"{}\"", hex::encode(&hash[..16]))).expect("hex is a header value")
}

/// The bytes of an asset, shared with the binary when they are compiled in
/// (nothing is copied per request), and their ETag.
fn shared(data: Cow<'static, [u8]>) -> (Bytes, HeaderValue) {
    match data {
        Cow::Borrowed(data) => {
            let key = (data.as_ptr() as usize, data.len());
            let etag = COMPILED_ETAGS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key)
                .or_insert_with(|| etag_of(data))
                .clone();
            (Bytes::from_static(data), etag)
        }
        Cow::Owned(data) => {
            let etag = etag_of(&data);
            (Bytes::from(data), etag)
        }
    }
}

/// Whether an `If-None-Match` names the ETag (weak comparison, as for a `GET`).
fn is_not_modified(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let Some(sent) = header(headers, IF_NONE_MATCH.as_str()) else {
        return false;
    };
    let strong = |tag: &str| tag.trim().trim_start_matches("W/").to_string();
    let current = strong(etag.to_str().unwrap_or_default());
    sent.trim() == "*" || sent.split(',').any(|tag| strong(tag) == current)
}

async fn asset_response(path: &str, data: Cow<'static, [u8]>, request_headers: &HeaderMap) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let (body, etag) = shared(data);
    // The representation a `200` sends, decided first so that a `304`
    // describes the same one: a compressed body has a weak ETag, and every body
    // that may be compressed varies with `Accept-Encoding`.
    let compressible = compression::is_compressible(body.len());
    let encoding = compressible
        .then(|| compression::preferred_encoding(request_headers))
        .flatten();
    let mut headers = HeaderMap::new();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache_control(path)));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    let sent_etag = match encoding {
        Some(_) => compression::weak_etag(&etag),
        None => etag.clone(),
    };
    headers.insert(ETAG, sent_etag.clone());
    if compressible {
        headers.insert(VARY, HeaderValue::from_static("accept-encoding"));
    }
    let mut response = if is_not_modified(request_headers, &sent_etag) {
        (StatusCode::NOT_MODIFIED, headers).into_response()
    } else {
        // Compressed once per ETag and encoding, then kept: an asset never
        // changes under its ETag.
        let body = match encoding {
            Some(encoding) => match compression::encode_asset(&etag, encoding, body.clone()).await {
                Some(encoded) => {
                    headers.insert(CONTENT_ENCODING, encoding.header_value());
                    encoded
                }
                None => body,
            },
            None => body,
        };
        let content_type =
            HeaderValue::from_str(mime.as_ref()).unwrap_or(HeaderValue::from_static("application/octet-stream"));
        ([(CONTENT_TYPE, content_type)], headers, body).into_response()
    };
    response.extensions_mut().insert(compression::Negotiated);
    response
}

pub(crate) async fn serve_ui(
    State(app): State<OpenShop>,
    options: ServerOptions,
    reload: Option<Arc<Served>>,
    request: Request,
) -> Response {
    let pathname = request.uri().path().to_string();

    if !is_ui_shell_path(&pathname) {
        if !matches!(*request.method(), Method::GET | Method::HEAD) {
            return not_found();
        }
        let Some(path) = decode_asset_path(pathname.trim_start_matches('/')) else {
            return not_found();
        };
        return match options.ui.read(&path).await {
            Some(data) => asset_response(&path, data, request.headers()).await,
            None => not_found(),
        };
    }

    let query = parse_query(request.uri().query());
    let endpoints = app.endpoints();
    let (shell_app, frame_ancestors) = if options.dev {
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
            return open_from_shopify_admin(StatusCode::FORBIDDEN, endpoints);
        }
        let signed = app.apps().by_signed_query(request.uri().query().unwrap_or_default()).ok();
        if let Some((shopify_app, query)) = &signed {
            forget_plan_on_welcome(&app, &shopify_app.handle, query).await;
        }
        let shopify_app = signed
            .map(|(shopify_app, _)| shopify_app)
            .or_else(|| app.apps().all().first());
        (
            ShellApp::of(shopify_app),
            "https://*.myshopify.com https://admin.shopify.com http://localhost:*".to_string(),
        )
    } else {
        let Some(shop) = query.get("shop").and_then(|shop| normalize_shop_domain(shop)) else {
            return unauthorized_ui(endpoints);
        };
        let Ok((shopify_app, signed)) = app.apps().by_signed_query(request.uri().query().unwrap_or_default()) else {
            return unauthorized_ui(endpoints);
        };
        match is_installed_for_launch(&app, shopify_app, &shop, &signed).await {
            Ok(true) => forget_plan_on_welcome(&app, &shopify_app.handle, &signed).await,
            Ok(false) => {
                // Always name the app, `default` included: with several apps,
                // `/auth` cannot tell which one to install otherwise.
                return redirect(&format!(
                    "/auth?shop={}&app={}",
                    encode_uri_component(&shop),
                    encode_uri_component(&shopify_app.handle)
                ));
            }
            Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response(),
        }
        (
            ShellApp::of(Some(shopify_app)),
            format!("https://{shop} https://admin.shopify.com"),
        )
    };

    // The admin origin frames the app too: a stand-in of Shopify serves its
    // admin there.
    let frame_ancestors = match &endpoints.admin_origin {
        Some(origin) => format!("{frame_ancestors} {origin}"),
        None => frame_ancestors,
    };

    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return not_found();
    }
    let Some(index) = options.ui.read("index.html").await else {
        return not_found();
    };
    let mut shell = render_shell(&String::from_utf8_lossy(&index), &shell_app, endpoints);
    if let Some(served) = reload {
        shell = served.inject(&shell);
    }
    shell_response(shell, &frame_ancestors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shopify::ShopifyEndpoints;

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

    #[tokio::test]
    async fn serves_assets_from_a_lookup_function() {
        let source = UiSource::Assets(|path| (path == "index.html").then(|| Cow::Borrowed(b"<html>".as_slice())));
        assert!(source.is_enabled());
        assert_eq!(source.read("index.html").await.as_deref(), Some(b"<html>".as_slice()));
        assert!(source.read("missing.js").await.is_none());
        assert!(source.read("../index.html").await.is_none());
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

    const CDN_APP_BRIDGE: &str = "https://cdn.shopify.com/shopifycloud/app-bridge.js";
    const CDN_POLARIS: &str = "https://cdn.shopify.com/shopifycloud/polaris.js";
    const APP_BRIDGE: &str = "http://localhost:9393/admin/app-bridge.js";
    const POLARIS: &str = "http://localhost:9393/cdn/polaris.js";

    fn endpoints(endpoints: ShopifyEndpoints) -> Endpoints {
        Endpoints::resolve(&endpoints, &ShopifyEndpoints::new()).unwrap()
    }

    /// An app with `api_key` that nothing names.
    fn app(api_key: &str) -> ShellApp<'_> {
        ShellApp {
            api_key,
            name: DEFAULT_APP_NAME,
        }
    }

    #[test]
    fn the_shell_is_titled_with_the_name_of_the_app() {
        // The shell the framework embeds, as every bundle has it.
        let index = include_str!("../../ui-dist/index.html");
        assert!(index.contains("<title>OpenShop</title>"), "{index}");
        let named = ShellApp {
            api_key: "key-123",
            name: "Easy hide PayPal Express",
        };
        let shell = render_shell(index, &named, &Endpoints::default());
        assert!(shell.contains("<title>Easy hide PayPal Express</title>"), "{shell}");
        assert!(!shell.contains("OpenShop"), "{shell}");
        // Nothing else moves.
        assert_eq!(
            shell.replace("<title>Easy hide PayPal Express</title>", "<title>OpenShop</title>"),
            render_shell(index, &app("key-123"), &Endpoints::default())
        );

        // The name is text, escaped, and `$` is no group of the replacement.
        assert_eq!(
            name_the_app("<head><title>OpenShop</title></head>", "</title><script>x</script> $1 & co"),
            "<head><title>&lt;/title&gt;&lt;script&gt;x&lt;/script&gt; $1 &amp; co</title></head>"
        );
        // A bundle with a title of its own keeps it.
        let own = "<head><title>Reviews admin</title></head>";
        assert_eq!(name_the_app(own, "Reviews"), own);
        // The app of an application without a `shopify.app.toml`, or without one in development.
        assert_eq!(ShellApp::of(None).name, DEFAULT_APP_NAME);
        assert_eq!(ShellApp::of(None).api_key, "");
    }

    fn simulator() -> Endpoints {
        endpoints(
            ShopifyEndpoints::new()
                .admin_origin("http://localhost:9393")
                .app_bridge(APP_BRIDGE)
                .polaris(POLARIS),
        )
    }

    #[test]
    fn the_endpoints_replace_the_app_bridge_and_the_polaris_of_the_shell() {
        // The shell the framework embeds, as every bundle has it.
        let index = include_str!("../../ui-dist/index.html");
        let shell = render_shell(index, &app("key-123"), &simulator());

        let app_bridge = format!(r#"<script data-api-key="key-123" src="{APP_BRIDGE}"></script>"#);
        let polaris = format!(r#"<script src="{POLARIS}"></script>"#);
        // In place of Shopify's: one App Bridge, which keeps its key, and one Polaris.
        assert!(shell.contains(&app_bridge), "{shell}");
        assert!(shell.contains(&polaris), "{shell}");
        assert_eq!(shell.matches("app-bridge.js").count(), 1, "{shell}");
        assert!(!shell.contains("cdn.shopify.com"), "{shell}");
        assert!(shell.contains(r#"<meta name="shopify-api-key" content="key-123" />"#));
        // Nothing else moves.
        let unchanged = shell
            .replace(APP_BRIDGE, CDN_APP_BRIDGE)
            .replace(POLARIS, CDN_POLARIS);
        assert_eq!(unchanged, inject_api_key(index, "key-123"));

        // Each one on its own.
        let only_app_bridge = render_shell(index, &app("key-123"), &endpoints(ShopifyEndpoints::new().app_bridge(APP_BRIDGE)));
        assert_eq!(only_app_bridge, inject_api_key(index, "key-123").replace(CDN_APP_BRIDGE, APP_BRIDGE));
        let only_polaris = render_shell(index, &app("key-123"), &endpoints(ShopifyEndpoints::new().polaris(POLARIS)));
        assert_eq!(only_polaris, inject_api_key(index, "key-123").replace(CDN_POLARIS, POLARIS));
        // The admin origin alone moves no script.
        let only_origin = endpoints(ShopifyEndpoints::new().admin_origin("http://localhost:9393"));
        assert_eq!(render_shell(index, &app("key-123"), &only_origin), inject_api_key(index, "key-123"));
    }

    #[test]
    fn with_shopify_endpoints_the_shell_does_not_change() {
        let shopify = Endpoints::default();
        let index = include_str!("../../ui-dist/index.html");
        assert_eq!(render_shell(index, &app("key-123"), &shopify), inject_api_key(index, "key-123"));
        let minified = format!(r#"<meta name=shopify-api-key content=""><script src={CDN_APP_BRIDGE}></script>"#);
        assert_eq!(render_shell(&minified, &app("k"), &shopify), inject_api_key(&minified, "k"));
    }

    #[test]
    fn the_endpoints_follow_minified_markup_and_other_attributes() {
        let html = format!(
            r#"<meta name=shopify-api-key content=""><script async type=module src={CDN_APP_BRIDGE}></script><script defer src={CDN_POLARIS}></script>"#
        );
        let endpoints = endpoints(
            ShopifyEndpoints::new()
                .app_bridge("http://[::1]:9393/app-bridge.js")
                .polaris("https://polaris.example.com/p.js"),
        );
        assert_eq!(
            render_shell(&html, &app("k"), &endpoints),
            r#"<meta name="shopify-api-key" content="k" /><script async type=module data-api-key="k" src="http://[::1]:9393/app-bridge.js"></script><script defer src="https://polaris.example.com/p.js"></script>"#
        );
        // Once only, however many times it is applied.
        let once = render_shell(&html, &app("k"), &endpoints);
        assert_eq!(render_shell(&once, &app("k"), &endpoints), once);
        // A page without them gets none.
        let page = "<head><title>t</title></head>";
        assert_eq!(render_shell(page, &app("k"), &endpoints), page);
    }

    #[test]
    fn the_script_urls_are_escaped_where_they_are_written() {
        let html = format!(r#"<script src="{CDN_APP_BRIDGE}"></script><script src="{CDN_POLARIS}"></script>"#);
        let hostile = r#"http://x"><script>alert('&')</script>"#;
        let written = use_scripts(html, Some(hostile), Some(hostile));
        assert!(!written.contains("<script>alert"), "{written}");
        let escaped = r#"<script src="http://x&quot;&gt;&lt;script&gt;alert(&#39;&amp;&#39;)&lt;/script&gt;"></script>"#;
        assert_eq!(written, format!("{escaped}{escaped}"));
        // A query string, as a valid URL may have one.
        let query = endpoints(ShopifyEndpoints::new().polaris("https://polaris.example.com/p.js?v=2&x=1"));
        assert_eq!(
            render_shell(&format!(r#"<script src="{CDN_POLARIS}"></script>"#), &app("k"), &query),
            r#"<script src="https://polaris.example.com/p.js?v=2&amp;x=1"></script>"#
        );
    }

    #[test]
    fn the_welcome_page_allows_its_polaris_where_polaris_needs_it() {
        assert_eq!(welcome_policy(None), WELCOME_POLICY);
        // The App Bridge and the admin origin change nothing of it: the page loads neither.
        let no_polaris = endpoints(ShopifyEndpoints::new().admin_origin("http://localhost:9393").app_bridge(APP_BRIDGE));
        assert_eq!(welcome_policy(no_polaris.polaris.as_ref()), WELCOME_POLICY);

        let simulator = simulator();
        assert_eq!(
            welcome_policy(simulator.polaris.as_ref()),
            "default-src 'none'; script-src https://cdn.shopify.com/shopifycloud/polaris.js http://localhost:9393; \
             style-src 'unsafe-inline' https://cdn.shopify.com/static/fonts/ http://localhost:9393; \
             font-src https://cdn.shopify.com/static/fonts/ http://localhost:9393; \
             connect-src https://cdn.shopify.com/shopifycloud/ http://localhost:9393; \
             form-action 'none'; base-uri 'none'"
        );
    }

    #[tokio::test]
    async fn a_large_asset_asked_for_twice_is_compressed_once() {
        let data: Vec<u8> = (0..200_000u32).flat_map(|value| (value % 1000).to_le_bytes()).collect();
        let accepted = HeaderMap::from_iter([(
            axum::http::header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, br"),
        )]);
        let mut bodies = Vec::new();
        for _ in 0..2 {
            let response = asset_response("app-0123456789abcdef_bg.wasm", Cow::Owned(data.clone()), &accepted).await;
            assert_eq!(response.headers()[CONTENT_ENCODING], "br");
            assert!(response.headers()[ETAG].as_bytes().starts_with(b"W/\""));
            bodies.push(axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap());
        }
        // The same bytes, not equal ones: the second answer reuses the first compression.
        assert_eq!(bodies[0].as_ptr(), bodies[1].as_ptr());
        assert!(bodies[0].len() < data.len() / 10);
    }

    #[tokio::test]
    async fn refuses_path_traversal() {
        let dir = UiSource::Dir(std::env::temp_dir());
        assert!(dir.read("../etc/passwd").await.is_none());
        assert!(dir.read("a//b").await.is_none());
        assert!(dir.read("").await.is_none());
        assert!(dir.read("/etc/hosts").await.is_none());
        assert!(dir.read("..\\secret.txt").await.is_none());
        assert!(dir.read("assets\\..\\..\\secret.txt").await.is_none());
        assert!(UiSource::None.read("index.html").await.is_none());
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
