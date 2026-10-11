//! Previews the demo app in a regular browser, without a Shopify store.
//!
//! ```bash
//! DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop \
//!   cargo run -p openshop-demo --example preview
//! ```
//!
//! The demo app runs on `http://localhost:3999` against a throwaway database
//! schema, with Shopify answered by fixtures. It answers a browser as it does
//! in production: without a shop, the admin is the page that says where the
//! app opens from, and the Admin API answers `401`. The operator dashboard is
//! at `/ops`. Its password is `preview`, or `OPS_PASSWORD` when it is set.
//!
//! Set `PREVIEW_AS_SHOP=1` to look at the admin UI itself, as the staff of a
//! fake shop: App Bridge is then replaced by a stub that signs session tokens
//! for it. Development only: never expose it.
//!
//! Set `PREVIEW_UI_DIR=apps/demo/dist` to preview the demo's own bundle with its
//! custom `/reviews` page (build it first with `mise run demo:ui`).
//!
//! `mise run dev` is the preview as a shop, run by `openshop dev`: started again
//! when the workspace changes, with the demo's own bundle built again by trunk,
//! and a page that reloads by itself.

#[path = "../tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openshop::builder::Route;
use openshop::engine::{Scheduler, Worker};
use openshop::runtime::RuntimeEnv;
use openshop::server::ServerOptions;
use openshop::server::{router, serve};
use openshop::testing::{MockTransport, TestDb, session_token};
use openshop::{DispatchOptions, OpenShop, Result, UiSource, WorkerConfig, json, response};

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

/// Copies the files of a bundle, without its shell.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if entry.file_name() != "index.html" {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// The built UI. `PREVIEW_UI_DIR` previews another bundle, e.g. `apps/demo/dist`,
/// and `openshop dev` names the one it builds with `OPENSHOP_UI_DIR`.
fn built_ui_dir() -> PathBuf {
    let named = ["PREVIEW_UI_DIR", "OPENSHOP_UI_DIR"]
        .into_iter()
        .find_map(std::env::var_os);
    match named {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/openshop/ui-dist"),
    }
}

/// Copies the built UI, with a shell that loads the App Bridge stub.
fn stub_ui(shell: &str, built: &Path, stubbed: &Path) -> Result<()> {
    let app_bridge = r#"<script src="https://cdn.shopify.com/shopifycloud/app-bridge.js"></script>"#;
    if !shell.contains(app_bridge) {
        openshop::bail!(
            "{} does not load App Bridge the expected way",
            built.join("index.html").display()
        );
    }
    copy_dir(built, stubbed)?;
    // The shell last: it names the files of the build.
    std::fs::write(stubbed.join("index.html"), shell.replace(app_bridge, APP_BRIDGE_STUB))?;
    Ok(())
}

/// A copy of the built UI whose shell loads the App Bridge stub, kept up to
/// date: `openshop dev` builds the bundle again when a page changes, and may
/// still be building the first one.
fn stubbed_ui_dir(built: PathBuf) -> Result<PathBuf> {
    let stubbed = std::env::temp_dir().join(format!("openshop-preview-ui-{}", std::process::id()));
    std::fs::create_dir_all(&stubbed)?;
    let shell = |built: &Path| std::fs::read_to_string(built.join("index.html")).ok();

    let mut copied = shell(&built);
    if let Some(shell) = &copied {
        stub_ui(shell, &built, &stubbed)?;
    }
    let directory = stubbed.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let built_shell = shell(&built);
            if built_shell.is_none() || built_shell == copied {
                continue;
            }
            match stub_ui(built_shell.as_deref().unwrap_or_default(), &built, &directory) {
                Ok(()) => copied = built_shell,
                // The files of a build are moved into place one by one.
                Err(error) => eprintln!("[preview] {error:#}"),
            }
        }
    });
    Ok(stubbed)
}

/// A week of what an application with several shops leaves behind: fictitious
/// installations, runs with their steps and their log lines, a provider whose
/// check fails, a cron a shop turned off. Enough for the operator dashboard to
/// have something to show.
const HISTORY: &str = r#"
INSERT INTO installations (shop, access_token, scopes, installed_at, uninstalled_at) VALUES
  ('atelier-nord.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '40 days', NULL),
  ('maison-lune.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '21 days', NULL),
  ('kiosque-vert.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '9 days', NULL),
  ('la-fabrique.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '3 days', NULL),
  ('ancien-comptoir.myshopify.com', NULL, 'read_products', now() - interval '60 days', now() - interval '12 days'),
  ('en-cours.myshopify.com', NULL, NULL, now() - interval '2 hours', NULL);

INSERT INTO flow_runs (shop, flow_name, status, input, error, attempts, started_at, completed_at, created_at)
SELECT shop, flow,
       CASE WHEN failed THEN 'failed' ELSE 'completed' END,
       CASE WHEN flow = 'syncOrders' THEN '{"limit":50}'::json ELSE '{"productId":"gid://shopify/Product/1"}'::json END,
       CASE WHEN NOT failed THEN NULL
            WHEN flow = 'syncOrders' THEN (ARRAY[
              'Warehouse API answered 502 Bad Gateway',
              'Warehouse API timed out after 30 s',
              'Shopify throttled the request'
            ])[1 + floor(pick * 3)::int]
            ELSE 'Product not found' END,
       CASE WHEN failed THEN 3 ELSE 1 END,
       ts, ts + make_interval(secs => 0.4 + took * 6), ts
FROM (
  SELECT ts,
         (ARRAY[
           'atelier-nord.myshopify.com', 'atelier-nord.myshopify.com', 'atelier-nord.myshopify.com',
           'maison-lune.myshopify.com', 'maison-lune.myshopify.com',
           'kiosque-vert.myshopify.com', 'la-fabrique.myshopify.com'
         ])[1 + floor(random() * 7)::int] AS shop,
         CASE WHEN random() < 0.8 THEN 'syncOrders' ELSE 'countVariants' END AS flow,
         random() AS pick, random() AS took, random() AS chance, random() AS keep
  FROM generate_series(now() - interval '7 days', now() - interval '2 minutes', interval '90 seconds') AS ts
) candidates
CROSS JOIN LATERAL (
  SELECT chance < CASE
    WHEN ts BETWEEN now() - interval '30 hours' AND now() - interval '27 hours' THEN 0.55
    WHEN shop = 'kiosque-vert.myshopify.com' THEN 0.18
    ELSE 0.04
  END AS failed
) outcome
WHERE keep < 0.2 + 0.8 * greatest(0, sin((extract(hour FROM ts) - 5) * pi() / 14));

INSERT INTO step_results (flow_run_id, step_name, attempt, status, output, error, duration_ms, created_at)
SELECT id::text, 'fetch-orders', 1, 'completed', '{"count":12}'::json, NULL, 180, created_at
  FROM flow_runs WHERE flow_name = 'syncOrders' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'send-to-warehouse', attempts, status,
       CASE WHEN status = 'completed' THEN '{"accepted":12}'::json END, error, 420, created_at + interval '1 second'
  FROM flow_runs WHERE flow_name = 'syncOrders' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'count', attempts, status,
       CASE WHEN status = 'completed' THEN '{"variants":4}'::json END, error, 90, created_at
  FROM flow_runs WHERE flow_name = 'countVariants' AND shop <> 'preview.myshopify.com';

INSERT INTO logs (flow_run_id, level, message, payload, created_at)
SELECT id::text, 'info', 'Flow "' || flow_name || '" started', NULL::json, created_at
  FROM flow_runs WHERE shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'info', 'Fetched 12 orders', '{"count":12}'::json, created_at + interval '300 milliseconds'
  FROM flow_runs WHERE flow_name = 'syncOrders' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'warn', 'Retrying: ' || error, NULL::json, created_at + interval '1 second'
  FROM flow_runs WHERE status = 'failed' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'error', 'Flow "' || flow_name || '" failed: ' || error, NULL::json, completed_at
  FROM flow_runs WHERE status = 'failed' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'info', 'Flow "' || flow_name || '" completed', NULL::json, completed_at
  FROM flow_runs WHERE status = 'completed' AND shop <> 'preview.myshopify.com';

INSERT INTO provider_configs (shop, provider_name, config, last_checked_at, last_check_ok) VALUES
  ('atelier-nord.myshopify.com', 'warehouse', '{}', now() - interval '3 hours', true),
  ('kiosque-vert.myshopify.com', 'warehouse', '{}', now() - interval '1 hour', false),
  ('maison-lune.myshopify.com', 'warehouse', '{}', NULL, NULL);

INSERT INTO cron_overrides (shop, cron_key, enabled) VALUES
  ('la-fabrique.myshopify.com', 'syncOrders:0 2 * * *', false);
"#;

#[tokio::main]
async fn main() -> Result<()> {
    openshop::cli::init_logging();

    let mut config = openshop_demo::APP.config()?;
    // The operator dashboard, with a password of its own when `OPS_PASSWORD` gives none.
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
    }

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
    app.dispatch_flow(DispatchOptions::new("syncOrders", SHOP).input(json!({ "limit": 5 })))
        .await?;
    app.dispatch_flow(DispatchOptions::new("countVariants", SHOP).input(json!({})))
        .await?;
    openshop::sqlx::raw_sql(HISTORY).execute(app.db()).await?;

    let built = built_ui_dir();
    let stubbed = if as_shop {
        Some(stubbed_ui_dir(built.clone())?)
    } else {
        None
    };
    // As a shop, the shell is served without a signed launch, like `dev`.
    // Otherwise the admin answers as in production: not without a shop.
    let options = ServerOptions {
        ui: UiSource::Dir(stubbed.clone().unwrap_or(built)),
        dev: as_shop,
    };
    // Loopback only: as a shop, this server hands out session tokens to anyone who asks.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await?;
    let server = serve(listener, router(&app, options)?).await?;
    let worker = Worker::new(app.clone(), WorkerConfig::default());
    worker.start().await;
    let mut scheduler = Scheduler::start(&app)?;

    if as_shop {
        println!("[preview] Admin UI, as the staff of {SHOP}: http://localhost:{PORT}/");
    } else {
        println!("[preview] Without a shop, as in production: http://localhost:{PORT}/");
        println!("[preview] Set PREVIEW_AS_SHOP=1 to open the admin UI as the staff of a fake shop.");
    }
    println!("[preview] Operator dashboard: http://localhost:{PORT}/ops (password: {ops_password})");
    println!("[preview] Press Ctrl-C to stop.");
    // `openshop dev` stops the preview to start it again: the schema is dropped then too.
    openshop::cli::shutdown_signal().await;

    scheduler.stop();
    worker.stop().await;
    let _ = server.shutdown().await;
    if let Some(stubbed) = stubbed {
        let _ = std::fs::remove_dir_all(stubbed);
    }
    database.cleanup().await
}
