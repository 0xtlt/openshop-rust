//! The HTTP server: OAuth, the Admin API, webhooks, app proxies, public routes,
//! the MCP endpoint, the admin it renders and the operator dashboard.

pub mod admin_pages;
pub mod api;
pub mod auth;
mod client;
mod compression;
pub mod connectors;
pub mod crypto;
pub mod file_routes;
pub mod function_mutations;
pub mod functions;
pub mod hmac;
pub mod http;
pub mod jwt;
mod launch;
mod limits;
mod linger;
pub(crate) mod locale;
pub mod log_query;
pub mod mcp;
pub mod ops;
mod pages;
pub mod provider_config;
pub mod proxy;
mod reload;
pub mod routes;
pub mod shop;
pub mod shop_domain;
pub mod shopify_apps;
pub mod webhooks;

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};

use anyhow::{Context, Result};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Router, ServiceExt};
use regex::Regex;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio_util::sync::{CancellationToken, DropGuard};
use tower::layer::layer_fn;
use tower_http::cors::{AllowHeaders, AllowOrigin, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

use self::http::{json_error, json_response, not_found};
use self::proxy::{EXTENSION_AUTH_MODES, PROXY_AUTH_MODES};
use crate::admin_paths::admin_page_from_api_path;
use crate::app::OpenShop;
use crate::runtime::logger;
use crate::runtime::time::to_iso;
use crate::types::AdminPageMode;

pub use self::limits::{DEFAULT_MAX_CONCURRENT_REQUESTS, DEFAULT_REQUEST_TIMEOUT};

const ROBOTS_HEADER: &str = "noindex, nofollow, noarchive, nosnippet";
/// Request bodies above this size are rejected with `413` unless the
/// application sets `max_request_body_bytes`.
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;
const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /\n";

/// The admin the server serves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum UiSource {
    /// API only: no admin is served, and CORS is enabled on `/api`.
    None,
    /// The admin the server renders, page by page. The default.
    #[default]
    Server,
}

impl UiSource {
    pub fn is_enabled(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    /// Admin UI to serve: the one the server renders, by default. With
    /// `UiSource::None` the server is API-only and enables CORS on `/api`.
    pub ui: UiSource,
    /// Development mode: a page of the admin may be framed by any shop and by
    /// `localhost`, and reloads when the server is started again; CORS is
    /// enabled on `/api`.
    pub dev: bool,
}

static LOCAL_ORIGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^https?://(?:(?:[^:]+\.)?localhost|127\.0\.0\.1|\[::1\])(?::\d+)?$").expect("valid regex")
});
static SHOP_ORIGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^https://[a-z0-9][a-z0-9-]*\.myshopify\.com$").expect("valid regex"));
const SHOPIFY_EXTENSION_ORIGIN: &str = "https://extensions.shopifycdn.com";

/// Origins allowed to call `/api` and `/proxy` cross-origin: Shopify admin and
/// storefronts, checkout extensions, the app's own URL and its
/// `cors_origins`, and in development the local hosts. `/ext` answers any
/// origin (see `ext_cors_layer`). In production a page on `localhost` is on the visitor's
/// own machine, not an origin of the application: it is not allowed unless
/// the application's URL is one.
pub fn is_allowed_cors_origin(origin: &str, configured_origins: &[String], production: bool) -> bool {
    !origin.is_empty()
        && ((!production && LOCAL_ORIGIN.is_match(origin))
            || origin == "https://admin.shopify.com"
            || origin == "https://shopify.com"
            || origin == SHOPIFY_EXTENSION_ORIGIN
            || SHOP_ORIGIN.is_match(origin)
            || configured_origins.iter().any(|configured| configured == origin))
}

/// The opaque origin a browser sends for a sandboxed frame, a local file or a
/// `data:` page.
pub(crate) const OPAQUE_ORIGIN: &str = "null";

/// An entry of `cors_origins` as a browser sends it: `https://shop.example`,
/// `http://localhost:3000`, or `null`. A URL with a path, a query or
/// credentials is not an origin.
pub(crate) fn parse_cors_origin(value: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value == OPAQUE_ORIGIN {
        return Ok(OPAQUE_ORIGIN.to_string());
    }
    let refused = || format!("`{value}` is not an origin: expected `https://shop.example` or `null`");
    let url = url::Url::parse(value).map_err(|_| refused())?;
    let origin_only = matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if !origin_only {
        return Err(refused());
    }
    Ok(url.origin().ascii_serialization())
}

/// Refuses `null` among the `cors_origins` of a production server: every
/// sandboxed frame and every local file sends it, whatever site it comes from.
pub(crate) fn check_cors_origins(origins: &[String], production: bool) -> Result<()> {
    if production && origins.iter().any(|origin| origin == OPAQUE_ORIGIN) {
        anyhow::bail!(
            "[openshop] Invalid config: cors_origins: `null` is refused in production, where any sandboxed page \
             or local file could send it; allow it in development only"
        );
    }
    Ok(())
}

const CORS_METHODS: [Method; 6] = [
    Method::GET,
    Method::HEAD,
    Method::PUT,
    Method::POST,
    Method::DELETE,
    Method::PATCH,
];

fn cors_layer(app: &OpenShop) -> CorsLayer {
    let mut configured_origins = app.env().configured_origins();
    configured_origins.extend(app.config().cors_origins.iter().cloned());
    let production = app.env().production;
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin: &HeaderValue, _| {
            origin
                .to_str()
                .is_ok_and(|origin| is_allowed_cors_origin(origin, &configured_origins, production))
        }))
        .allow_methods(CORS_METHODS)
        .allow_headers(AllowHeaders::mirror_request())
}

/// CORS of `/ext`: `Access-Control-Allow-Origin: *`, whatever the origin, as
/// Shopify asks of a backend that UI extensions call: the origin of their
/// worker may change without notice. No credentials: the session token is in
/// `Authorization`, and it, not the origin, authenticates the request.
fn ext_cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::any())
        .allow_methods(CORS_METHODS)
        .allow_headers(AllowHeaders::mirror_request())
}

/// The CORS layer answers every `OPTIONS` request itself; report it as `204 No Content`.
async fn preflight_status(request: Request, next: Next) -> Response {
    let preflight = request.method() == Method::OPTIONS;
    let mut response = next.run(request).await;
    if preflight {
        *response.status_mut() = StatusCode::NO_CONTENT;
    }
    response
}

fn with_cors(router: Router<OpenShop>, cors: &CorsLayer) -> Router<OpenShop> {
    router.layer(cors.clone()).layer(middleware::from_fn(preflight_status))
}

/// Blocks the Admin API of built-in pages that are disabled.
async fn page_gate(State(app): State<OpenShop>, request: Request, next: Next) -> Response {
    let config = app.config();
    if let Some(page) = admin_page_from_api_path(request.uri().path())
        && api::pages::resolved_admin_pages(config).get(page) == AdminPageMode::Disabled
    {
        return json_error(StatusCode::NOT_FOUND, "Not found");
    }
    next.run(request).await
}

async fn health() -> Response {
    json_response(
        StatusCode::OK,
        json!({ "status": "ok", "timestamp": to_iso(&chrono::Utc::now()) }),
    )
}

/// Whether a request is a form sent by a page of the admin: its body is
/// `application/x-www-form-urlencoded`.
fn is_form(request: &Request) -> bool {
    request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/x-www-form-urlencoded"))
}

/// Builds the application router.
pub fn router(app: &OpenShop, options: ServerOptions) -> Result<Router> {
    create_router(app, options, true)
}

pub(crate) fn create_router(app: &OpenShop, options: ServerOptions, log: bool) -> Result<Router> {
    let cors = cors_layer(app);

    // Admin API: session token, then the disabled-page gate. Unknown paths still require a session.
    let mut api = Router::new()
        .merge(api::create_api_routes())
        .merge(admin_pages::create_custom_admin_page_routes())
        .merge(functions::create_function_routes())
        .route("/api", axum::routing::any(|| async { not_found() }))
        .route("/api/", axum::routing::any(|| async { not_found() }))
        .route("/api/{*rest}", axum::routing::any(|| async { not_found() }))
        .method_not_allowed_fallback(|| async { not_found() })
        .layer(middleware::from_fn_with_state(app.clone(), page_gate))
        .layer(middleware::from_fn_with_state(app.clone(), shop::shop_middleware));
    // CORS when the admin UI is not served from this origin: in development,
    // and on an API-only server, production included.
    if !options.ui.is_enabled() || options.dev {
        api = with_cors(api, &cors);
    }

    // Development: a page of the admin reloads when the server is started again.
    let reload = (options.dev && options.ui.is_enabled()).then(|| Arc::new(reload::Served::new()));

    let ui_options = options.clone();
    let ui_reload = reload.clone();
    let fallback = move |state: State<OpenShop>, request: Request| {
        let options = ui_options.clone();
        let reload = ui_reload.clone();
        async move {
            match options.ui {
                UiSource::Server => pages::serve(state.0, options, reload, request).await,
                UiSource::None => not_found(),
            }
        }
    };

    // MCP uses OpenShop-issued Bearer tokens, separate from Shopify admin auth.
    // Only POST is the endpoint: `GET /mcp` is a page of the admin UI, and a
    // form of that page posts to it as a form, which an MCP client never sends.
    let page = fallback.clone();
    let mcp = post(move |state: State<OpenShop>, request: Request| {
        let page = page.clone();
        async move {
            if is_form(&request) {
                page(state, request).await
            } else {
                mcp::handle(state, request).await
            }
        }
    })
    .fallback(fallback.clone());

    // Auth and webhook routes are public: Shopify signs them with HMAC, not a session token.
    let mut router = Router::new()
        .route("/robots.txt", get(|| async { ROBOTS_TXT }))
        .route("/health", get(health))
        .route("/mcp", mcp.clone())
        .route("/mcp/", mcp)
        .merge(auth::create_auth_routes())
        .merge(webhooks::create_webhook_routes())
        .merge(api);

    // The admin rendered by the server: its script, and the bounce page
    // where App Bridge gets a session token for a page loaded without one.
    if matches!(options.ui, UiSource::Server) {
        router = router
            .route(pages::SCRIPT_PATH, get(pages::script))
            .route(pages::BOUNCE_PATH, get(pages::bounce_page));
    }

    if let Some(served) = reload {
        router = router.route(
            reload::PATH,
            get(move |closing| reload::listen(served.clone(), closing)),
        );
    }

    // Public server routes define their own explicit authentication contract.
    if !app.config().routes.is_empty() {
        router = router.merge(routes::create_server_routes(app, log)?);
    }

    // The operator dashboard has its own session, behind the password the
    // application gives. Without one, `/ops` is a path that does not exist.
    // A browser keeps a `Secure` cookie only over HTTPS: development, and a
    // server whose public URL is plain HTTP, do without the attribute.
    let secure_cookie = !options.dev && !app.env().fallback_app_url().starts_with("http://");
    if let Some(ops) = ops::create_ops_routes(app, secure_cookie) {
        router = router.merge(ops);
        if log {
            logger::info("[openshop] Operator dashboard served under /ops");
        }
    }

    if !app.config().proxies.is_empty() {
        // Extension-direct routes: same handlers, session token required. Mounted on
        // /ext so that the Shopify CLI proxy does not intercept them.
        router = router
            .merge(with_cors(
                proxy::create_proxy_routes(app, "/proxy", PROXY_AUTH_MODES, log)?,
                &cors,
            ))
            .merge(with_cors(
                proxy::create_proxy_routes(app, "/ext", EXTENSION_AUTH_MODES, false)?,
                &ext_cors_layer(),
            ));
    }

    // An unsupported method on a known path is a plain 404, like an unknown path.
    let router = router
        .method_not_allowed_fallback(|| async { not_found() })
        .fallback(fallback);

    let body_limit = app
        .config()
        .max_request_body_bytes
        .unwrap_or(DEFAULT_MAX_REQUEST_BODY_BYTES);
    let request_timeout = app.config().request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT);
    let slots = Arc::new(Semaphore::new(
        app.config()
            .max_concurrent_requests
            .unwrap_or(DEFAULT_MAX_CONCURRENT_REQUESTS)
            .max(1),
    ));
    // One stack, outermost first: each `Router::layer` call would box every
    // route once more, and a request clones every box it goes through.
    Ok(router
        .layer((
            SetResponseHeaderLayer::overriding(
                HeaderName::from_static("x-robots-tag"),
                HeaderValue::from_static(ROBOTS_HEADER),
            ),
            layer_fn(compression::CompressLarge),
            middleware::from_fn_with_state(slots, limits::shed_excess),
            middleware::from_fn_with_state(request_timeout, limits::time_out),
            DefaultBodyLimit::max(body_limit),
            #[cfg(feature = "sentry")]
            layer_fn(crate::runtime::report::IsolateRequests),
        ))
        .with_state(app.clone()))
}

/// A listening server. Keep the handle for as long as the server should run:
/// dropping it stops accepting connections and lets requests in flight finish.
/// [`ServerHandle::shutdown`] does the same, then waits for those requests, up
/// to five seconds, and returns an error if the server failed or the wait
/// timed out.
pub struct ServerHandle {
    pub addr: SocketAddr,
    closing: DropGuard,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl ServerHandle {
    /// Stops accepting connections and waits for requests in flight, up to five
    /// seconds. After that it stops waiting and returns an error: the requests
    /// still in flight keep running until they finish or the process exits.
    pub async fn shutdown(self) -> Result<()> {
        let Self { closing, mut task, .. } = self;
        drop(closing);
        match tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await {
            Ok(joined) => Ok(joined??),
            Err(_) => {
                task.abort();
                anyhow::bail!("Timed out closing HTTP server after 5000ms")
            }
        }
    }
}

/// Binds `0.0.0.0:port` (0 picks a free port) and serves the application.
pub async fn listen(app: &OpenShop, port: u16, options: ServerOptions) -> Result<ServerHandle> {
    let router = router(app, options)?;
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("[openshop] Failed to listen on port {port}"))?;
    serve(listener, router).await
}

/// Serves a router built with [`router`] on an existing listener.
pub async fn serve(listener: TcpListener, router: Router) -> Result<ServerHandle> {
    let addr = listener.local_addr()?;
    let closing = CancellationToken::new();
    // In development a page of the admin UI listens for as long as it is open.
    let service = reload::WithClosing {
        inner: router,
        closing: reload::Closing(Arc::new(closing.clone())),
    };
    let stopped = closing.clone().cancelled_owned();
    let task = tokio::spawn(async move {
        // Served as is, a router copies its whole route table for each
        // connection. The make-service hands every connection the same one.
        // Handlers that want the address of the peer read it as `ConnectInfo<Peer>`.
        axum::serve(
            linger::LingeringListener(listener),
            ServiceExt::<Request>::into_make_service_with_connect_info::<linger::Peer>(service),
        )
        .with_graceful_shutdown(stopped)
        .await
    });
    logger::info(format!(
        "[openshop] API server running on http://localhost:{}",
        addr.port()
    ));
    Ok(ServerHandle {
        addr,
        closing: closing.drop_guard(),
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_only_known_cors_origins() {
        let configured = vec!["https://app.example.com".to_string()];
        let local = [
            "http://localhost:3000",
            "https://app.localhost",
            "http://127.0.0.1:8080",
            "http://[::1]:3000",
        ];
        for origin in local {
            assert!(is_allowed_cors_origin(origin, &configured, false), "{origin}");
            assert!(!is_allowed_cors_origin(origin, &configured, true), "{origin}");
        }
        // The application's own URL is allowed, even a local one.
        let local_app = vec!["http://localhost:3000".to_string()];
        assert!(is_allowed_cors_origin("http://localhost:3000", &local_app, true));
        assert!(!is_allowed_cors_origin("http://localhost:3001", &local_app, true));
        for origin in [
            "https://admin.shopify.com",
            "https://shopify.com",
            "https://extensions.shopifycdn.com",
            "https://Test-Shop.myshopify.com",
            "https://app.example.com",
        ] {
            for production in [false, true] {
                assert!(is_allowed_cors_origin(origin, &configured, production), "{origin}");
            }
        }
        for origin in [
            "",
            "https://evil.example.com",
            "https://shop.myshopify.com.evil.com",
            "http://shop.myshopify.com",
            "https://localhost.evil.com",
            "https://app.example.com.evil.com",
            "null",
        ] {
            for production in [false, true] {
                assert!(!is_allowed_cors_origin(origin, &configured, production), "{origin}");
            }
        }
        // `null` only when the application lists it.
        assert!(is_allowed_cors_origin("null", &["null".to_string()], false));
    }

    #[test]
    fn a_cors_origin_is_an_origin_or_null() {
        for (value, origin) in [
            ("https://shop.example", "https://shop.example"),
            (" https://Shop.Example/ ", "https://shop.example"),
            ("http://localhost:9000", "http://localhost:9000"),
            ("https://shop.example:443", "https://shop.example"),
            ("null", "null"),
        ] {
            assert_eq!(parse_cors_origin(value).as_deref(), Ok(origin), "{value}");
        }
        for value in [
            "shop.example",
            "https://shop.example/path",
            "https://shop.example/?a=1",
            "https://shop.example/#a",
            "https://user@shop.example",
            "ftp://shop.example",
            "file:///tmp",
            "*",
            "Null",
        ] {
            assert!(parse_cors_origin(value).is_err(), "{value}");
        }
    }

    #[test]
    fn null_is_refused_in_production() {
        let null = ["https://shop.example".to_string(), "null".to_string()];
        assert!(check_cors_origins(&null, false).is_ok());
        assert!(check_cors_origins(&null[..1], true).is_ok());
        let error = check_cors_origins(&null, true).unwrap_err().to_string();
        assert!(error.contains("`null` is refused in production"), "{error}");
    }
}
