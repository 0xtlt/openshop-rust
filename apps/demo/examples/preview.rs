//! Previews the admin UI in a regular browser, without a Shopify store.
//!
//! ```bash
//! DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop \
//!   cargo run -p openshop-demo --example preview
//! ```
//!
//! Set `PREVIEW_UI_DIR=apps/demo/ui/dist` to preview the demo's own bundle with
//! its custom `/reviews` page (build it first with `trunk build --release`).
//!
//! The demo app runs on `http://localhost:3999` against a throwaway database
//! schema, with Shopify answered by fixtures and App Bridge replaced by a stub
//! that signs session tokens for a fake shop. Development only: never expose it.

#[path = "../tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use openshop::runtime::RuntimeEnv;
use openshop::server::serve;
use openshop::testing::{MockTransport, TestDb, create_session_token};
use openshop::{
    DispatchFlowParams, OpenShop, PartialWorkerConfig, Result, Scheduler, ServerOptions, ServerRoute, UiSource, Worker,
    create_server, json, response,
};

const SHOP: &str = "preview.myshopify.com";
const API_KEY: &str = "preview-key";
const API_SECRET: &str = "preview-secret";
const PORT: u16 = 3999;

const APP_BRIDGE_STUB: &str = r#"<script>
      window.shopify = {
        idToken: () => fetch('/routes/preview-token').then((response) => response.text()),
        toast: { show: (message) => { console.log('[toast]', message); return 'toast'; } },
      };
    </script>"#;

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// A copy of the built UI whose shell loads the App Bridge stub.
fn preview_ui_dir() -> Result<PathBuf> {
    // `PREVIEW_UI_DIR` previews another bundle, e.g. `apps/demo/ui/dist`.
    let built = match std::env::var_os("PREVIEW_UI_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/openshop/ui-dist"),
    };
    let dir = std::env::temp_dir().join(format!("openshop-preview-ui-{}", std::process::id()));
    copy_dir(&built, &dir)?;

    let index = std::fs::read_to_string(dir.join("index.html"))?;
    let app_bridge = r#"<script src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script>"#;
    if !index.contains(app_bridge) {
        openshop::bail!("ui-dist/index.html does not load App Bridge the expected way");
    }
    std::fs::write(dir.join("index.html"), index.replace(app_bridge, APP_BRIDGE_STUB))?;
    Ok(dir)
}

#[tokio::main]
async fn main() -> Result<()> {
    openshop::cli::init_logging();

    let mut config = openshop_demo::APP.config()?;
    config.routes.insert(
        "/preview-token".to_string(),
        ServerRoute::public().get(|_ctx| async move {
            Ok(response::text(create_session_token(
                SHOP,
                API_SECRET,
                API_KEY,
                "preview-user",
            )))
        }),
    );

    let database = TestDb::create_with_models(&config.models).await?;
    let transport = MockTransport::new();
    transport.respond_with(|request| {
        let query = request.body["query"].as_str().unwrap_or_default();
        Ok(MockTransport::json(
            200,
            json!({ "data": common::graphql_fixture(query) }),
        ))
    });

    let env = RuntimeEnv {
        shopify_api_key: Some(API_KEY.to_string()),
        shopify_api_secret: Some(API_SECRET.to_string()),
        host: Some(format!("http://localhost:{PORT}")),
        cwd: std::env::temp_dir(),
        ..Default::default()
    };
    let app = OpenShop::with_transport(config, database.pool.clone(), env, Arc::clone(&transport) as _)?;

    openshop::sqlx::query("INSERT INTO installations (shop, access_token, scopes) VALUES ($1, 'preview-token', 'read_products,read_orders')")
        .bind(SHOP)
        .execute(app.db())
        .await?;
    // Some history to look at: a completed run and a failed one.
    app.dispatch_flow(DispatchFlowParams::new("syncOrders", SHOP).input(json!({ "limit": 5 })))
        .await?;
    app.dispatch_flow(DispatchFlowParams::new("countVariants", SHOP).input(json!({})))
        .await?;

    let ui = preview_ui_dir()?;
    let options = ServerOptions {
        ui: UiSource::Dir(ui.clone()),
        dev: true,
    };
    // Loopback only: this server hands out session tokens to anyone who asks.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await?;
    let server = serve(listener, create_server(&app, options)?).await?;
    let worker = Worker::new(app.clone(), PartialWorkerConfig::default());
    worker.start().await;
    let mut scheduler = Scheduler::start(&app)?;

    println!("[preview] Admin UI: http://localhost:{PORT}/");
    println!("[preview] Press Ctrl-C to stop.");
    tokio::signal::ctrl_c().await?;

    scheduler.stop();
    worker.stop().await;
    let _ = server.shutdown().await;
    let _ = std::fs::remove_dir_all(ui);
    database.cleanup().await
}
