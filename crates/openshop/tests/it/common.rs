//! Helpers shared by the modules of the integration tests.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use openshop::builder::{Field, Flow, Provider};
use openshop::db::schema::{FlowRun, StepResult};
use openshop::runtime::{RuntimeEnv, RuntimeLogger};
use openshop::testing::{MockTransport, OutboundRequest, TestContext, TestDb};
use openshop::{FlowContext, OpenShop, OpenShopConfig, PgPool, Result, ShopifyAppConfig, ShopifyConfig, Value, json};
use reqwest::{RequestBuilder, Response};
use uuid::Uuid;

pub const TEST_SHOP: &str = "test-integration.myshopify.com";
pub const TEST_SECRET: &str = "test-secret";
pub const TEST_API_KEY: &str = "test-app";

/// An application bound to an isolated database, without an HTTP server.
pub struct TestApp {
    pub app: OpenShop,
    pub db: PgPool,
    pub transport: Arc<MockTransport>,
    test_db: TestDb,
}

pub fn test_env() -> RuntimeEnv {
    RuntimeEnv {
        shopify_api_key: Some(TEST_API_KEY.to_string()),
        shopify_api_secret: Some(TEST_SECRET.to_string()),
        development: true,
        cwd: std::env::temp_dir().join("openshop-test-no-toml"),
        ..Default::default()
    }
}

impl TestApp {
    pub async fn new(config: OpenShopConfig) -> Self {
        let test_db = TestDb::create().await.expect("test database");
        let transport = MockTransport::new();
        let app = OpenShop::with_transport(config, test_db.pool.clone(), test_env(), transport.clone())
            .expect("valid application");
        Self {
            db: test_db.pool.clone(),
            app,
            transport,
            test_db,
        }
    }

    /// Another application definition sharing this database.
    pub fn sibling(&self, config: OpenShopConfig) -> OpenShop {
        OpenShop::with_transport(config, self.db.clone(), test_env(), self.transport.clone())
            .expect("valid application")
    }

    pub async fn cleanup(self) {
        self.test_db.cleanup().await.expect("drop test schema");
    }
}

/// A configuration containing only the given flows.
pub fn create_config(flows: impl IntoIterator<Item = Flow>) -> OpenShopConfig {
    flows
        .into_iter()
        .fold(OpenShopConfig::builder(), |builder, flow| builder.flow(flow))
        .build()
        .expect("valid config")
}

/// A flow that does nothing.
pub fn noop_flow(name: &str) -> Flow {
    Flow::new(name, |_ctx: FlowContext| async move { Result::Ok(()) })
}

/// The directory of a project, with its `migrations` folder, removed when the test ends.
pub struct TempProject {
    dir: std::path::PathBuf,
}

impl TempProject {
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("openshop-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn folder(&self) -> std::path::PathBuf {
        self.dir.join(openshop::db::migrate::MIGRATIONS_DIR)
    }

    /// Writes the migration `<tag>.sql`.
    pub fn write(&self, tag: &str, sql: &str) {
        self.write_file(&format!("{tag}.sql"), sql);
    }

    pub fn write_file(&self, name: &str, content: &str) {
        let path = self.folder().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A database without any table, as before the first deployment.
pub async fn empty_database() -> TestDb {
    let db = TestDb::create().await.unwrap();
    sqlx::raw_sql(&format!(
        "DROP SCHEMA \"{0}\" CASCADE; CREATE SCHEMA \"{0}\"",
        db.schema
    ))
    .execute(&db.pool)
    .await
    .unwrap();
    db
}

/// Saves the configuration of a provider for the shop of the context, through the Admin API.
pub async fn save_provider_config(ctx: &TestContext, provider: &str, config: Value) {
    let response = ctx
        .put(&format!("/api/providers/{provider}"))
        .json(&json!({ "config": config }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(status, 200, "{}", response.text().await.unwrap());
}

/// Runs the connection check of a provider and returns the answer of the Admin API.
pub async fn check_provider(ctx: &TestContext, provider: &str) -> Value {
    let response = ctx
        .post(&format!("/api/providers/{provider}/check"))
        .send()
        .await
        .unwrap();
    response.json().await.unwrap()
}

/// The connectors of the shop of the context, with its saved configurations.
pub async fn shop_connectors(ctx: &TestContext) -> openshop::Connectors {
    ctx.app.connectors(&ctx.shop, "default")
}

/// Makes the updates of `table` that match `when` fail the way a lost
/// connection does (SQLSTATE 08006): the first `failures` of them, or all of
/// them until [`heal_write_fault`]. No code of the framework is involved.
pub async fn inject_write_fault(db: &PgPool, table: &str, when: &str, failures: i64) {
    sqlx::raw_sql(&format!(
        "CREATE SEQUENCE fault_{table}_hits; \
         CREATE FUNCTION fault_{table}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN \
           IF nextval('fault_{table}_hits') <= {failures} THEN \
             RAISE EXCEPTION 'injected fault' USING ERRCODE = '08006'; \
           END IF; \
           RETURN NEW; \
         END $$; \
         CREATE TRIGGER fault BEFORE UPDATE ON {table} FOR EACH ROW WHEN ({when}) \
           EXECUTE FUNCTION fault_{table}()"
    ))
    .execute(db)
    .await
    .expect("fault trigger");
}

/// Removes the fault of [`inject_write_fault`].
pub async fn heal_write_fault(db: &PgPool, table: &str) {
    sqlx::raw_sql(&format!("DROP TRIGGER fault ON {table}"))
        .execute(db)
        .await
        .expect("drop fault trigger");
}

/// How many updates the fault of [`inject_write_fault`] has seen.
pub async fn write_fault_hits(db: &PgPool, table: &str) -> i64 {
    let (hits,): (i64,) = sqlx::query_as(&format!("SELECT last_value FROM fault_{table}_hits"))
        .fetch_one(db)
        .await
        .unwrap();
    hits
}

/// An application on the database of `test` whose clock, for `now()`, is an
/// hour ahead of the one of this process: a replica with a clock that drifted.
pub async fn app_with_a_clock_ahead(test: &TestApp, config: OpenShopConfig) -> (OpenShop, PgPool) {
    let (schema,): (String,) = sqlx::query_as("SELECT current_schema()")
        .fetch_one(&test.db)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE FUNCTION now() RETURNS timestamptz LANGUAGE sql VOLATILE AS \
         $$ SELECT clock_timestamp() + interval '1 hour' $$",
    )
    .execute(&test.db)
    .await
    .unwrap();
    // The options of the test pool, whose URL passed the checks of
    // `test_database_url`. The schema comes before `pg_catalog`, so its
    // `now()` is the one called: the last `search_path` given wins.
    let options = (*test.db.connect_options())
        .clone()
        .options([("search_path", format!("{schema},pg_catalog"))]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap();
    let app = OpenShop::with_transport(config, pool.clone(), test_env(), test.transport.clone()).unwrap();
    (app, pool)
}

// ─── HTTP ────────────────────────────────────────────────────────────

/// Status and body of a response; a body that is not JSON is returned as a string.
pub async fn send(request: RequestBuilder) -> (u16, Value) {
    let response = request.send().await.expect("response");
    let status = response.status().as_u16();
    let text = response.text().await.expect("response body");
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    (status, body)
}

pub async fn json_body(response: Response) -> Value {
    response.json().await.expect("JSON body")
}

/// Checks the status of a response and its body, `{ "error": error }`.
pub async fn assert_error(response: Response, status: u16, error: &str) {
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(json_body(response).await, json!({ "error": error }));
}

/// The Admin GraphQL calls the application sent to Shopify.
pub fn graphql_requests(ctx: &TestContext) -> Vec<OutboundRequest> {
    ctx.transport
        .requests()
        .into_iter()
        .filter(|request| request.url.ends_with("/graphql.json"))
        .collect()
}

// ─── Time ────────────────────────────────────────────────────────────

/// Now, in seconds since the Unix epoch.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Lets `duration` pass for the process, as a timer sees it.
pub async fn wait(duration: Duration) {
    tokio::time::pause();
    tokio::time::advance(duration).await;
    tokio::time::resume();
}

/// Waits for `future`, ten seconds at most.
pub async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("condition not reached in time")
}

// ─── Runs ────────────────────────────────────────────────────────────

/// The row of a run that exists.
pub async fn find_run(db: &PgPool, run_id: Uuid) -> FlowRun {
    sqlx::query_as("SELECT * FROM flow_runs WHERE id = $1")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .unwrap()
        .unwrap_or_else(|| panic!("no run {run_id}"))
}

/// Whether the run is still in the database.
pub async fn run_exists(db: &PgPool, run_id: Uuid) -> bool {
    let (exists,): (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM flow_runs WHERE id = $1)")
        .bind(run_id)
        .fetch_one(db)
        .await
        .unwrap();
    exists
}

/// The steps of a run, in the order they were stored.
pub async fn find_steps(db: &PgPool, run_id: Uuid) -> Vec<StepResult> {
    sqlx::query_as("SELECT * FROM step_results WHERE flow_run_id = $1 ORDER BY created_at, attempt")
        .bind(run_id.to_string())
        .fetch_all(db)
        .await
        .unwrap()
}

/// A flow that panics before it returns its future.
pub fn explode(_ctx: FlowContext) -> std::future::Ready<Result<()>> {
    panic!("exploded before the first await")
}

// ─── Shops and providers ─────────────────────────────────────────────

/// A Shopify app named after `slug`: its key is `{slug}-key`, its secret
/// `{slug}-secret` and its URL `https://{slug}.example.test`.
pub fn shopify_app(slug: &str) -> ShopifyAppConfig {
    ShopifyAppConfig::Keys {
        api_key: format!("{slug}-key"),
        api_secret: format!("{slug}-secret"),
        app_url: Some(format!("https://{slug}.example.test")),
    }
}

/// Two Shopify apps served by one instance: `clientA`, the app `client-a`
/// of [`shopify_app`], and `clientB`, the app `client-b`.
pub fn two_shopify_apps() -> ShopifyConfig {
    ShopifyConfig {
        scopes: Some("read_products".to_string()),
        apps: Some(
            [
                ("clientA".to_string(), shopify_app("client-a")),
                ("clientB".to_string(), shopify_app("client-b")),
            ]
            .into(),
        ),
    }
}

/// [`two_shopify_apps`], both signing with the secret `shared-secret`: a
/// signature does not tell which app it comes from.
pub fn two_shopify_apps_with_one_secret() -> ShopifyConfig {
    let mut shopify = two_shopify_apps();
    for app in shopify.apps.iter_mut().flat_map(|apps| apps.values_mut()) {
        if let ShopifyAppConfig::Keys { api_secret, .. } = app {
            *api_secret = "shared-secret".to_string();
        }
    }
    shopify
}

/// Installs `shop` for the Shopify app `app_handle`, with the scopes
/// `read_orders`. The token is stored as given: encrypt it first to store it
/// as the framework does.
pub async fn install(db: &PgPool, app_handle: &str, shop: &str, token: Option<&str>, uninstalled: bool) {
    sqlx::query(
        "INSERT INTO installations (app_handle, shop, access_token, scopes, uninstalled_at) \
         VALUES ($1, $2, $3, 'read_orders', CASE WHEN $4 THEN now() END)",
    )
    .bind(app_handle)
    .bind(shop)
    .bind(token)
    .bind(uninstalled)
    .execute(db)
    .await
    .unwrap();
}

/// A provider with one secret, whose method `reveal` returns it after a prefix.
pub fn vault_provider() -> Provider {
    Provider::new("Vault")
        .field("secret", Field::password("Secret"))
        .typed_method("reveal", |config, prefix: String| async move {
            let secret = config.get("secret").and_then(Value::as_str).unwrap_or_default();
            Ok(format!("{prefix}:{secret}"))
        })
}

// ─── The process logger ──────────────────────────────────────────────

/// A line given to the process logger.
#[derive(Clone, Debug)]
pub struct LoggedLine {
    pub level: &'static str,
    pub message: String,
    pub context: Option<Value>,
}

impl std::fmt::Display for LoggedLine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}", self.level, self.message)?;
        match &self.context {
            Some(context) => write!(formatter, " {context}"),
            None => Ok(()),
        }
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<LoggedLine>>);

impl Recorder {
    fn record(&self, level: &'static str, message: &str, context: Option<&Value>) {
        self.0.lock().unwrap().push(LoggedLine {
            level,
            message: message.to_string(),
            context: context.cloned(),
        });
    }
}

impl RuntimeLogger for Recorder {
    fn info(&self, message: &str, context: Option<&Value>) {
        self.record("info", message, context);
    }
    fn warn(&self, message: &str, context: Option<&Value>) {
        self.record("warn", message, context);
    }
    fn error(&self, message: &str, context: Option<&Value>) {
        self.record("error", message, context);
    }
}

/// The process logger is one for every test of the binary: the tests that
/// capture it take turns.
static LOGGER_TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Keeps what the process logger is given, from when it starts to when it is
/// dropped, which puts the previous logger back. The lines of the tests that
/// run meanwhile are kept too: look for the ones of the test.
pub struct LogCapture {
    lines: Arc<Recorder>,
    previous: Option<Arc<dyn RuntimeLogger>>,
    _turn: tokio::sync::MutexGuard<'static, ()>,
}

impl LogCapture {
    pub async fn start() -> Self {
        let turn = LOGGER_TURN.lock().await;
        let lines = Arc::new(Recorder::default());
        let previous = openshop::runtime::set_logger(lines.clone());
        Self {
            lines,
            previous: Some(previous),
            _turn: turn,
        }
    }

    /// Every line so far.
    pub fn lines(&self) -> Vec<LoggedLine> {
        self.lines.0.lock().unwrap().clone()
    }

    /// The lines of one level, `info`, `warn` or `error`.
    pub fn at(&self, level: &str) -> Vec<LoggedLine> {
        self.lines().into_iter().filter(|line| line.level == level).collect()
    }

    /// Every line so far, one per line of text.
    pub fn text(&self) -> String {
        self.lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            openshop::runtime::set_logger(previous);
        }
    }
}

// ─── A database connection that drops ────────────────────────────────

/// A TCP proxy in front of the test database. Armed with a statement, it
/// cuts the first connection that sends it once the server has answered and
/// committed it, without passing the answer on: the client sees a lost
/// connection for a statement that went through.
pub struct DropProxy {
    port: u16,
    armed: Arc<std::sync::Mutex<Vec<String>>>,
    cuts: Arc<std::sync::atomic::AtomicUsize>,
}

impl DropProxy {
    pub async fn start(db: &PgPool) -> Self {
        let options = db.connect_options();
        let upstream = (options.get_host().to_string(), options.get_port());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("proxy port");
        let port = listener.local_addr().unwrap().port();
        let armed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let cuts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        tokio::spawn({
            let (armed, cuts) = (armed.clone(), cuts.clone());
            async move {
                while let Ok((client, _)) = listener.accept().await {
                    let Ok(server) = tokio::net::TcpStream::connect(upstream.clone()).await else {
                        continue;
                    };
                    tokio::spawn(pipe(client, server, armed.clone(), cuts.clone()));
                }
            }
        });
        Self { port, armed, cuts }
    }

    /// Cuts the connection that sends a statement containing `statement`, after
    /// the server committed it. Armed more than once, it cuts as many
    /// connections, one per statement.
    pub fn drop_answer_of(&self, statement: &str) {
        self.armed.lock().unwrap().push(statement.to_string());
    }

    /// How many connections were cut.
    pub fn cuts(&self) -> usize {
        self.cuts.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A pool of the test database that goes through the proxy.
    pub async fn pool(&self, db: &PgPool) -> PgPool {
        let options = (*db.connect_options())
            .clone()
            .host("127.0.0.1")
            .port(self.port)
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("pool through the proxy")
    }
}

/// Where a connection stands: a statement is prepared in a round trip of its
/// own, then executed in the next one, which is the one that gets cut.
const IDLE: u8 = 0;
const PREPARING: u8 = 1;
const PREPARED: u8 = 2;
const EXECUTING: u8 = 3;

async fn pipe(
    client: tokio::net::TcpStream,
    server: tokio::net::TcpStream,
    armed: Arc<std::sync::Mutex<Vec<String>>>,
    cuts: Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::{AtomicU8, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let phase = Arc::new(AtomicU8::new(IDLE));
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let upstream = {
        let phase = phase.clone();
        async move {
            let mut buffer = vec![0u8; 16 * 1024];
            while let Ok(read) = client_read.read(&mut buffer).await {
                if read == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                {
                    let mut armed = armed.lock().unwrap();
                    if phase.load(Ordering::SeqCst) == PREPARED {
                        phase.store(EXECUTING, Ordering::SeqCst);
                    } else if let Some(at) = armed.iter().position(|statement| text.contains(statement.as_str())) {
                        armed.remove(at);
                        phase.store(PREPARING, Ordering::SeqCst);
                    }
                }
                if server_write.write_all(&buffer[..read]).await.is_err() {
                    break;
                }
            }
        }
    };
    let downstream = async move {
        // ReadyForQuery, idle: the server finished the round trip.
        let ready = b"Z\0\0\0\x05I";
        let mut buffer = vec![0u8; 16 * 1024];
        while let Ok(read) = server_read.read(&mut buffer).await {
            if read == 0 {
                break;
            }
            if buffer[..read].windows(ready.len()).any(|window| window == ready) {
                match phase.load(Ordering::SeqCst) {
                    PREPARING => phase.store(PREPARED, Ordering::SeqCst),
                    EXECUTING => {
                        cuts.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
            if client_write.write_all(&buffer[..read]).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = upstream => {}
        _ = downstream => {}
    }
}
