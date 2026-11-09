//! What the previews of the templates share: an application served in a regular
//! browser, without a Shopify store.
//!
//! The application runs on `http://localhost:<port>` against a throwaway
//! database schema, with Shopify answered by fixtures. It answers a browser as
//! it does in production: without a shop, the admin is the page that says
//! where the app opens from, and the Admin API answers `401`. The operator
//! dashboard is at `/ops`. Its password is `preview`, or the one the
//! application gives.
//!
//! Set `PREVIEW_AS_SHOP=1` to look at the admin itself, as the staff of a
//! fake shop: App Bridge is then replaced by a stub that signs session tokens
//! for it, adds them to the requests of the page as App Bridge's `fetch`
//! does, and shows the toasts. Development only: never expose it.

use std::sync::Arc;

use openshop::builder::Route;
use openshop::engine::{Scheduler, Worker};
use openshop::runtime::RuntimeEnv;
use openshop::server::ServerOptions;
use openshop::server::{router, serve};
use openshop::testing::{MockTransport, TestDb, session_token};
use openshop::{DispatchOptions, OpenShop, OpenShopConfig, Result, UiSource, Value, WorkerConfig, json, response};
use response::{IntoResponse, header};

/// The shop the previewed application is installed on.
pub const SHOP: &str = "preview.myshopify.com";
const API_KEY: &str = "preview-key";
const API_SECRET: &str = "preview-secret";

/// App Bridge as far as the admin needs it, for a page opened in a regular
/// browser: a session token for the fake shop, added to the requests of the
/// page as App Bridge's `fetch` adds it; the bounce page loads the page again
/// with one; a toast shows at the bottom of the page.
const APP_BRIDGE_STUB: &str = r#"(() => {
  const native = window.fetch.bind(window)
  const idToken = () => native('/routes/preview-token').then((response) => response.text())
  const toast = (message, options = {}) => {
    const shown = Object.assign(document.createElement('div'), { textContent: message })
    shown.style.cssText = `position:fixed;bottom:16px;left:50%;transform:translateX(-50%);z-index:9999;padding:8px 16px;border-radius:8px;color:#fff;font:14px system-ui;background:${options.isError ? '#c70a24' : '#303030'}`
    document.body.append(shown)
    setTimeout(() => shown.remove(), 3000)
    return 'toast'
  }
  window.shopify = { idToken, toast: { show: toast }, loading: () => {} }
  window.fetch = async (input, init = {}) => {
    const url = new URL(input instanceof Request ? input.url : input, location.href)
    if (url.origin !== location.origin) return native(input, init)
    const headers = new Headers(init.headers ?? (input instanceof Request ? input.headers : undefined))
    headers.set('Authorization', `Bearer ${await idToken()}`)
    return native(input, { ...init, headers })
  }
  const reload = new URLSearchParams(location.search).get('shopify-reload')
  if (reload) idToken().then((token) => {
    const url = new URL(reload, location.origin)
    url.searchParams.set('id_token', token)
    location.replace(url)
  })
})()
"#;

/// The preview of an application.
pub struct Preview {
    /// The application, as `APP.config()` gives it.
    pub config: OpenShopConfig,
    /// Where it is served: `http://localhost:<port>`.
    pub port: u16,
    /// What Shopify answers a GraphQL query with.
    pub shopify: fn(&str) -> Value,
    /// Some history to look at: runs queued for [`SHOP`] when the preview
    /// starts, each a flow and its input.
    pub runs: Vec<(&'static str, Value)>,
    /// What the application left behind in other shops, as SQL run once on
    /// the throwaway schema.
    pub history: &'static str,
}

impl Preview {
    /// Serves the application until `SIGINT` or `SIGTERM`.
    pub fn run(self) -> Result<()> {
        tokio::runtime::Runtime::new()?.block_on(self.serve())
    }

    async fn serve(self) -> Result<()> {
        openshop::cli::init_logging();

        let Self {
            mut config,
            port,
            shopify,
            runs,
            history,
        } = self;
        // A preview reports to Sentry like the commands do, when the application gives a DSN.
        let _sentry = config.sentry.as_ref().map(|sentry| sentry.init(false));
        // The operator dashboard, with a password of its own when the application gives none.
        let ops_password = config.ops_password.get_or_insert_with(|| "preview".to_string()).clone();
        // Only on request does the preview act as a shop: it then signs session
        // tokens for anyone who asks.
        let as_shop = std::env::var("PREVIEW_AS_SHOP").is_ok_and(|value| !matches!(value.as_str(), "" | "0" | "false"));
        if as_shop {
            config.routes.insert(
                "/preview-token".to_string(),
                Route::public().get(|_ctx| async move {
                    Ok(response::text(session_token(SHOP, API_SECRET, API_KEY, "preview-user")))
                }),
            );
            config.routes.insert(
                "/preview-app-bridge.js".to_string(),
                Route::public().get(|_ctx| async move {
                    Ok((
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        APP_BRIDGE_STUB,
                    )
                        .into_response())
                }),
            );
            config.shopify_endpoints = std::mem::take(&mut config.shopify_endpoints)
                .app_bridge(format!("http://localhost:{port}/routes/preview-app-bridge.js"));
        }

        let database = TestDb::create_with_models(&config.models).await?;
        let transport = MockTransport::new();
        transport.respond_with(move |request| {
            let query = request.body["query"].as_str().unwrap_or_default();
            Ok(MockTransport::json(200, json!({ "data": shopify(query) })))
        });

        let env = RuntimeEnv {
            shopify_api_key: Some(API_KEY.to_string()),
            shopify_api_secret: Some(API_SECRET.to_string()),
            host: Some(format!("http://localhost:{port}")),
            development: true,
            cwd: std::env::temp_dir(),
            ..Default::default()
        };
        let app = OpenShop::with_transport(config, database.pool.clone(), env, Arc::clone(&transport) as _)?;

        openshop::sqlx::query("INSERT INTO installations (shop, access_token, scopes) VALUES ($1, 'preview-token', 'read_products,read_orders')")
            .bind(SHOP)
            .execute(app.db())
            .await?;
        for (flow, input) in runs {
            app.dispatch_flow(DispatchOptions::new(flow, SHOP).input(input)).await?;
        }
        openshop::sqlx::raw_sql(history).execute(app.db()).await?;

        // As a shop, the page reloads when the preview starts again, like
        // `dev`. Otherwise the admin answers as in production: not without a shop.
        let options = ServerOptions {
            ui: UiSource::Server,
            dev: as_shop,
        };
        // Loopback only: as a shop, this server hands out session tokens to anyone who asks.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let server = serve(listener, router(&app, options)?).await?;
        let worker = Worker::new(app.clone(), WorkerConfig::default());
        worker.start().await;
        let mut scheduler = Scheduler::start(&app)?;

        if as_shop {
            println!("[preview] The admin, as the staff of {SHOP}: http://localhost:{port}/?shop={SHOP}&embedded=1");
        } else {
            println!("[preview] Without a shop, as in production: http://localhost:{port}/");
            println!("[preview] Set PREVIEW_AS_SHOP=1 to open the admin as the staff of a fake shop.");
        }
        println!("[preview] Operator dashboard: http://localhost:{port}/ops (password: {ops_password})");
        println!("[preview] Press Ctrl-C to stop.");
        // `openshop dev` stops the preview to start it again: the schema is dropped then too.
        openshop::cli::shutdown_signal().await;

        scheduler.stop();
        worker.stop().await;
        let _ = server.shutdown().await;
        database.cleanup().await
    }
}
