//! What the previews of the demos share: an application served in a regular
//! browser, without a Shopify store.
//!
//! The application runs on `http://localhost:<port>` against a throwaway
//! database schema, with Shopify answered by fixtures. It answers a browser as
//! it does in production: without a shop, the admin is the page that says
//! where the app opens from, and the Admin API answers `401`. The operator
//! dashboard is at `/ops`. Its password is `preview`, or the one the
//! application gives.
//!
//! Set `PREVIEW_AS_SHOP=1` to look at the admin UI itself, as the staff of a
//! fake shop: App Bridge is then replaced by a stub that signs session tokens
//! for it. Development only: never expose it.
//!
//! Set `PREVIEW_UI_DIR` to the `dist` of a demo, e.g. `apps/demo-custom/dist`,
//! to preview its own bundle with its custom pages (build it first with
//! `mise run demo:ui`). `openshop dev` names the one it builds with
//! `OPENSHOP_UI_DIR`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openshop::builder::Route;
use openshop::engine::{Scheduler, Worker};
use openshop::runtime::RuntimeEnv;
use openshop::server::ServerOptions;
use openshop::server::{router, serve};
use openshop::testing::{MockTransport, TestDb, session_token};
use openshop::{DispatchOptions, OpenShop, OpenShopConfig, Result, UiSource, Value, WorkerConfig, json, response};

/// The shop the previewed application is installed on.
pub const SHOP: &str = "preview.myshopify.com";
const API_KEY: &str = "preview-key";
const API_SECRET: &str = "preview-secret";

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

/// The built UI. `PREVIEW_UI_DIR` previews another bundle, e.g. `apps/demo-custom/dist`,
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
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let server = serve(listener, router(&app, options)?).await?;
        let worker = Worker::new(app.clone(), WorkerConfig::default());
        worker.start().await;
        let mut scheduler = Scheduler::start(&app)?;

        if as_shop {
            println!("[preview] Admin UI, as the staff of {SHOP}: http://localhost:{port}/");
        } else {
            println!("[preview] Without a shop, as in production: http://localhost:{port}/");
            println!("[preview] Set PREVIEW_AS_SHOP=1 to open the admin UI as the staff of a fake shop.");
        }
        println!("[preview] Operator dashboard: http://localhost:{port}/ops (password: {ops_password})");
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
}
