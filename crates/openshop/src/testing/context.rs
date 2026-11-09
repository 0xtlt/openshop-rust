//! A running application for tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

use super::db::TestDb;
use super::fake::FakeProviders;
use super::proxy_signature;
use super::shopify::FakeShopify;
use super::transport::MockTransport;
use super::{session_token, to_query_string};
use crate::app::{OpenShop, OpenShopConfig};
use crate::declare::App;
use crate::engine::dispatch::DispatchOptions;
use crate::engine::runner::{RunFlowOptions, RunOutcome, run_flow};
use crate::runtime::RuntimeEnv;
use crate::server::shopify_apps::{DEFAULT_SHOPIFY_APP_HANDLE, client_id_in_place_of};
use crate::server::{ServerHandle, ServerOptions, create_router, serve};
use crate::shopify::{PartnerApi, ShopifyEndpoints};
use crate::types::ShopScope;

#[derive(Debug, Clone)]
pub struct ProxyResponse {
    pub status: u16,
    pub headers: IndexMap<String, String>,
    /// Parsed JSON for JSON responses, the raw text otherwise.
    pub body: Value,
    pub text: String,
    pub content_type: String,
}

impl ProxyResponse {
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_value(self.body.clone())?)
    }
}

/// Builds an app proxy request signed like Shopify signs them.
pub struct ProxyRequestBuilder {
    http: reqwest::Client,
    base_url: String,
    shop: String,
    secret: String,
    method: reqwest::Method,
    path: String,
    customer_id: String,
    query: IndexMap<String, String>,
    body: Option<Value>,
    headers: Vec<(String, String)>,
}

impl ProxyRequestBuilder {
    pub fn as_customer(mut self, customer_id: impl Into<String>) -> Self {
        self.customer_id = customer_id.into();
        self
    }

    pub fn qs(mut self, params: &[(&str, &str)]) -> Self {
        for (key, value) in params {
            self.query.insert(key.to_string(), value.to_string());
        }
        self
    }

    pub fn json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }

    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    pub async fn send(self) -> Result<ProxyResponse> {
        let mut params: IndexMap<String, String> = IndexMap::from([
            ("shop".to_string(), self.shop.clone()),
            ("timestamp".to_string(), chrono::Utc::now().timestamp().to_string()),
            ("path_prefix".to_string(), "/apps/openshop".to_string()),
            ("logged_in_customer_id".to_string(), self.customer_id.clone()),
        ]);
        params.extend(self.query);
        params.insert("signature".to_string(), proxy_signature(&params, &self.secret));

        let url = format!("{}/proxy{}?{}", self.base_url, self.path, to_query_string(&params));
        let mut request = self.http.request(self.method, url);
        for (key, value) in &self.headers {
            request = request.header(key, value);
        }
        if let Some(body) = &self.body {
            request = request.json(body);
        }

        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers: IndexMap<String, String> = response
            .headers()
            .iter()
            .filter_map(|(name, value)| Some((name.to_string(), value.to_str().ok()?.to_string())))
            .collect();
        let content_type = headers.get("content-type").cloned().unwrap_or_default();
        let text = response.text().await?;
        let body = if content_type.contains("json") {
            serde_json::from_str(&text).unwrap_or_else(|_| Value::from(text.clone()))
        } else {
            Value::from(text.clone())
        };

        Ok(ProxyResponse {
            status,
            headers,
            body,
            text,
            content_type,
        })
    }
}

/// Proxy test client — signs every request with the app secret.
#[derive(Clone)]
pub struct ProxyClient {
    http: reqwest::Client,
    base_url: String,
    shop: String,
    secret: String,
}

impl ProxyClient {
    fn request(&self, method: reqwest::Method, path: &str) -> ProxyRequestBuilder {
        ProxyRequestBuilder {
            http: self.http.clone(),
            base_url: self.base_url.clone(),
            shop: self.shop.clone(),
            secret: self.secret.clone(),
            method,
            path: path.to_string(),
            customer_id: String::new(),
            query: IndexMap::new(),
            body: None,
            headers: Vec::new(),
        }
    }

    pub fn get(&self, path: &str) -> ProxyRequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    pub fn post(&self, path: &str) -> ProxyRequestBuilder {
        self.request(reqwest::Method::POST, path)
    }

    pub fn put(&self, path: &str) -> ProxyRequestBuilder {
        self.request(reqwest::Method::PUT, path)
    }

    pub fn delete(&self, path: &str) -> ProxyRequestBuilder {
        self.request(reqwest::Method::DELETE, path)
    }

    pub fn patch(&self, path: &str) -> ProxyRequestBuilder {
        self.request(reqwest::Method::PATCH, path)
    }
}

/// The API key of the options of a test that sets none, [`TestOptions::api_key`].
pub const TEST_API_KEY: &str = "test-app";

/// The Partner ID of a test whose Partner API has a token and no Partner ID.
pub const TEST_PARTNER_ID: &str = "1234567";

/// The token of a test whose Partner API has a Partner ID and no token.
pub const TEST_PARTNER_API_TOKEN: &str = "prtapi_test";

tokio::task_local! {
    /// The directory of the manifest of the crate whose test runs, which
    /// `#[openshop::test]` sets.
    static MANIFEST_DIR: PathBuf;
}

/// Runs `test` with `manifest_dir`, the `CARGO_MANIFEST_DIR` of the crate of
/// the test, as the directory of the contexts it starts without a `cwd`:
/// what `#[openshop::test]` wraps a test in.
#[doc(hidden)]
pub async fn in_crate<F: Future>(manifest_dir: &'static str, test: F) -> F::Output {
    MANIFEST_DIR.scope(PathBuf::from(manifest_dir), test).await
}

/// The retries of `policy`, without waiting between them: the same number
/// of attempts, so that a test still runs the code that retries.
fn without_waits(policy: &shopify_api::RetryPolicy) -> shopify_api::RetryPolicy {
    shopify_api::RetryPolicy::new(policy.max_retries())
        .with_base_delay(Duration::ZERO)
        .with_max_delay(Duration::ZERO)
        .with_max_retry_after(policy.max_retry_after())
}

#[derive(Clone)]
pub struct TestOptions {
    pub shop: String,
    /// `SHOPIFY_API_SECRET` of the single-app setup.
    pub secret: String,
    /// `SHOPIFY_API_KEY` of the single-app setup. With [`TEST_API_KEY`], the
    /// default, and a `shopify.app.toml` in [`cwd`](Self::cwd) whose
    /// `client_id` is another, the test is that app: its `client_id` is the
    /// API key.
    pub api_key: String,
    /// Marks the shop as installed with this access token.
    pub access_token: Option<String>,
    pub encryption_key: Option<String>,
    /// The `client_credentials_shop` of the application, in place of the one
    /// its configuration gives: the shop background work may ask a token for
    /// with the client credentials grant, from the mock transport.
    pub client_credentials_shop: Option<String>,
    pub host: Option<String>,
    pub production: bool,
    /// A development environment, where secrets are stored in plaintext without
    /// an `encryption_key`. `true` by default; with `false` and no key the
    /// application refuses to start.
    pub development: bool,
    /// Project root used to resolve Shopify TOML files: the `handle` and the
    /// `name` of the app, its scopes. Defaults, under `#[openshop::test]`, to
    /// the directory of the crate of the test (its `CARGO_MANIFEST_DIR`), and
    /// to a directory without any otherwise.
    pub cwd: Option<PathBuf>,
    /// How the Shopify client retries what Shopify did not run. `None`, the
    /// default: the policy of the application (its `shopify_retry`, else
    /// `shopify_api`'s four retries) without waiting between the attempts,
    /// whose number is the same. `Some(policy)` is that policy as it is,
    /// waits included.
    pub shopify_retry: Option<shopify_api::RetryPolicy>,
    pub server: ServerOptions,
    pub transport: Option<Arc<MockTransport>>,
    /// The Shopify endpoints of the application, field by field over the ones
    /// its configuration sets: the Admin API and OAuth at another origin, the
    /// admin UI with another App Bridge or Polaris. The requests still go to
    /// the mock transport. The environment variables of the endpoints are not
    /// read in a test.
    pub shopify_endpoints: ShopifyEndpoints,
    /// The Partner API of the application, field by field over the one its
    /// configuration sets: with a Partner ID or a token, billing reads the
    /// plans from the Partner API, which the mock transport answers
    /// (`ctx.shopify.active_subscription(…)`). The one that is missing is
    /// [`TEST_PARTNER_ID`] or [`TEST_PARTNER_API_TOKEN`], so that a
    /// configuration that names its Partner ID and reads its token from the
    /// environment runs in a test. The environment variables are not read.
    pub partner_api: PartnerApi,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            shop: "test.myshopify.com".to_string(),
            secret: "test-secret".to_string(),
            api_key: TEST_API_KEY.to_string(),
            access_token: None,
            encryption_key: None,
            client_credentials_shop: None,
            host: None,
            production: false,
            development: true,
            cwd: None,
            shopify_retry: None,
            server: ServerOptions::default(),
            transport: None,
            shopify_endpoints: ShopifyEndpoints::new(),
            partner_api: PartnerApi::new(),
        }
    }
}

pub struct TestContext {
    pub url: String,
    pub port: u16,
    pub app: OpenShop,
    /// The isolated database of this context.
    pub db: PgPool,
    pub shop: String,
    pub secret: String,
    pub api_key: String,
    /// Auto-generated provider fakes, used by [`TestContext::run_flow`].
    pub fakes: FakeProviders,
    /// Scripted Shopify HTTP responses and the recorded requests.
    pub transport: Arc<MockTransport>,
    /// The Admin API of the test: answers the GraphQL operations the
    /// application sends by their name, `ctx.shopify.on("Name", data)`, and
    /// records them. The one of [`transport`](Self::transport).
    pub shopify: FakeShopify,
    /// HTTP client that does not follow redirects.
    pub http: reqwest::Client,
    test_db: TestDb,
    server: ServerHandle,
}

/// What `#[openshop::test(…)]` starts a test from: a declared application,
/// or anything that resolves to a test context.
///
/// - `#[openshop::test(APP)]`: an [`App`], started with its shop installed;
/// - `#[openshop::test(TestContext::new(config()))]`, or a function of the
///   test file that returns a context, `#[openshop::test(context())]`: a
///   future of a `TestContext`, or of a `Result` of one.
pub trait IntoTestContext {
    fn into_test_context(self) -> impl Future<Output = Result<TestContext>>;
}

impl IntoTestContext for &App {
    async fn into_test_context(self) -> Result<TestContext> {
        TestContext::for_app(self).await
    }
}

impl<F> IntoTestContext for F
where
    F: Future,
    F::Output: StartedTestContext,
{
    async fn into_test_context(self) -> Result<TestContext> {
        self.await.started()
    }
}

/// What a future resolves to for a test to start from it.
pub trait StartedTestContext {
    fn started(self) -> Result<TestContext>;
}

impl StartedTestContext for TestContext {
    fn started(self) -> Result<TestContext> {
        Ok(self)
    }
}

impl<E: Into<anyhow::Error>> StartedTestContext for std::result::Result<TestContext, E> {
    fn started(self) -> Result<TestContext> {
        self.map_err(Into::into)
    }
}

/// A test stands for its shop, in the single-app setup: what takes a context
/// takes the test context, `Review::of(ctx)` or `sync_orders::dispatch(ctx, …)`.
impl ShopScope for TestContext {
    fn shop(&self) -> &str {
        &self.shop
    }

    fn shopify_app(&self) -> &str {
        DEFAULT_SHOPIFY_APP_HANDLE
    }

    fn app(&self) -> &OpenShop {
        &self.app
    }
}

/// `partner_api` with the Partner ID or the token of a test in place of the
/// one that is missing, when the other is set.
fn with_test_credentials(partner_api: PartnerApi) -> PartnerApi {
    if partner_api.partner_id.is_none() && partner_api.token.is_none() {
        return partner_api;
    }
    partner_api.or(PartnerApi::new()
        .partner_id(TEST_PARTNER_ID)
        .token(TEST_PARTNER_API_TOKEN))
}

/// The API key a test runs with: the one of the options, or with
/// [`TEST_API_KEY`] the `client_id` of the `shopify.app.toml` of `cwd` when
/// no `shopify.app*.toml` there is the one of `test-app`, so that the file
/// is the one of the app.
fn api_key_of(api_key: &str, cwd: &Path) -> String {
    match api_key {
        TEST_API_KEY => client_id_in_place_of(cwd, api_key).unwrap_or_else(|| api_key.to_string()),
        _ => api_key.to_string(),
    }
}

impl TestOptions {
    /// The options of a test whose shop has installed the application: the
    /// flows, the proxies and the admin find an access token for it.
    pub fn installed() -> Self {
        Self {
            access_token: Some("test-access-token".to_string()),
            ..Self::default()
        }
    }
}

impl TestContext {
    pub async fn new(config: OpenShopConfig) -> Result<Self> {
        Self::with_options(config, TestOptions::default()).await
    }

    /// Starts a declared application for a test, with its shop installed.
    /// What `#[openshop::test(APP)]` gives a test.
    pub async fn for_app(app: &App) -> Result<Self> {
        Self::with_options(app.config()?, TestOptions::installed()).await
    }

    pub async fn with_options(mut config: OpenShopConfig, options: TestOptions) -> Result<Self> {
        if options.client_credentials_shop.is_some() {
            config.client_credentials_shop = options.client_credentials_shop.clone();
        }
        config.shopify_endpoints = options.shopify_endpoints.clone().or(config.shopify_endpoints);
        config.partner_api = with_test_credentials(options.partner_api.clone().or(config.partner_api));
        config.shopify_retry = Some(match &options.shopify_retry {
            Some(policy) => policy.clone(),
            None => without_waits(&config.shopify_retry.clone().unwrap_or_default()),
        });
        let cwd = options
            .cwd
            .clone()
            .or_else(|| MANIFEST_DIR.try_with(Clone::clone).ok())
            .unwrap_or_else(|| std::env::temp_dir().join("openshop-test-no-toml"));
        let api_key = api_key_of(&options.api_key, &cwd);
        let test_db = TestDb::create_with_models(&config.models).await?;
        let db = test_db.pool.clone();
        let transport = options.transport.clone().unwrap_or_default();
        let env = RuntimeEnv {
            shopify_api_key: Some(api_key.clone()),
            shopify_api_secret: Some(options.secret.clone()),
            host: options.host.clone(),
            shopify_app_url: None,
            encryption_key: options.encryption_key.clone(),
            production: options.production,
            development: options.development,
            cwd,
            shopify_endpoints: ShopifyEndpoints::new(),
            partner_api: PartnerApi::new(),
        };
        let app = OpenShop::with_transport(config, db.clone(), env, transport.clone())?;

        if let Some(access_token) = &options.access_token {
            sqlx::query(
                "INSERT INTO installations (shop, access_token, scopes) VALUES ($1, $2, 'read_products,read_orders') \
                 ON CONFLICT (app_handle, shop) DO UPDATE SET \
                   access_token = EXCLUDED.access_token, scopes = EXCLUDED.scopes, uninstalled_at = NULL",
            )
            .bind(&options.shop)
            .bind(access_token)
            .execute(&db)
            .await?;
        }

        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let server = serve(listener, create_router(&app, options.server.clone(), false)?).await?;
        let port = server.addr.port();
        let fakes = FakeProviders::new(&app.config().providers);

        Ok(Self {
            url: format!("http://127.0.0.1:{port}"),
            port,
            fakes,
            shopify: transport.shopify(),
            transport,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            shop: options.shop,
            secret: options.secret,
            api_key,
            app,
            db,
            test_db,
            server,
        })
    }

    /// Run a flow with faked providers
    pub async fn run_flow(&self, flow_name: &str, input: Value) -> Result<RunOutcome> {
        let shop = self.shop.clone();
        self.run_flow_for(flow_name, input, &shop).await
    }

    pub async fn run_flow_for(&self, flow_name: &str, input: Value, shop: &str) -> Result<RunOutcome> {
        let (run_id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO flow_runs (shop, flow_name, status, input, available_at) \
             VALUES ($1, $2, 'running', $3::json, now()) RETURNING id",
        )
        .bind(shop)
        .bind(flow_name)
        .bind(input.to_string())
        .fetch_one(&self.db)
        .await?;

        run_flow(
            &self.app,
            RunFlowOptions::new(run_id, flow_name, shop)
                .input(input)
                .connectors(self.fakes.connectors()),
        )
        .await
    }

    /// Dispatch a flow to the queue
    pub async fn dispatch_flow(&self, flow_name: &str, input: Value) -> Result<Uuid> {
        let shop = self.shop.clone();
        self.dispatch_flow_for(flow_name, input, &shop).await
    }

    pub async fn dispatch_flow_for(&self, flow_name: &str, input: Value, shop: &str) -> Result<Uuid> {
        let result = self
            .app
            .dispatch_flow(DispatchOptions::new(flow_name, shop).input(input))
            .await?;
        Ok(result.run_id)
    }

    /// Proxy test client — auto-signs HMAC
    pub fn proxy(&self) -> ProxyClient {
        self.proxy_for(&self.shop, &self.secret)
    }

    /// Create a signed Shopify session token for authenticated API requests.
    pub fn session_token(&self) -> String {
        self.session_token_for(&self.shop, "1")
    }

    pub fn session_token_for(&self, shop: &str, sub: &str) -> String {
        session_token(shop, &self.secret, &self.api_key, sub)
    }

    /// Authorization header value for authenticated API requests.
    pub fn authorization_header(&self) -> String {
        format!("Bearer {}", self.session_token())
    }

    pub fn authorization_header_for(&self, shop: &str, sub: &str) -> String {
        format!("Bearer {}", self.session_token_for(shop, sub))
    }

    /// The absolute URL of a path of the application.
    pub fn url_for(&self, path: &str) -> String {
        format!("{}{path}", self.url)
    }

    /// A request authenticated as staff user `sub` of `shop`.
    pub fn request_as(&self, method: reqwest::Method, path: &str, shop: &str, sub: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url_for(path))
            .bearer_auth(self.session_token_for(shop, sub))
    }

    /// A proxy client signing for another shop or with another app's secret.
    pub fn proxy_for(&self, shop: &str, secret: &str) -> ProxyClient {
        ProxyClient {
            http: self.http.clone(),
            base_url: self.url.clone(),
            shop: shop.to_string(),
            secret: secret.to_string(),
        }
    }

    /// An authenticated request to the application, e.g. `ctx.request(Method::GET, "/api/flows")`.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.request_as(method, path, &self.shop, "1")
    }

    pub fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    pub fn post(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::POST, path)
    }

    pub fn put(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::PUT, path)
    }

    pub fn patch(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::PATCH, path)
    }

    pub fn delete(&self, path: &str) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::DELETE, path)
    }

    /// The page of the admin at `path`, as staff user `1` opens it from the
    /// menu: `ctx.page("/reviews").send().await?.text().await?` is its HTML,
    /// the whole document the server renders: the `<ui-nav-menu>` of the
    /// admin, then the page, rendered by its view, in
    /// `<main id="openshop-page">`.
    pub fn page(&self, path: &str) -> reqwest::RequestBuilder {
        self.get(path).header("x-openshop-fetch", "1")
    }

    /// A form of the page of the admin at `path`, sent as the admin sends it:
    /// `fields` with the name of its action in `_action`. The answer is:
    ///
    /// - `200` and the page rendered again (the whole document, as
    ///   [`page`](Self::page) gives it), with the toast of the action in
    ///   `x-openshop-toast`, or its error toast in `x-openshop-toast-error`;
    /// - `204` with `x-openshop-location` when the action goes to another
    ///   page, `Outcome::redirect(…)`;
    /// - `422` (or the status of the `AdminPublicError` that refused it) and
    ///   the page with the error of each field when the form is refused and
    ///   the view reads `form`;
    /// - the status of the error and `x-openshop-toast-error`, without a body,
    ///   when the action fails, or when the form is refused and the view does
    ///   not read `form`.
    ///
    /// The toast headers are percent-encoded (`Saved%20Great`):
    /// [`toast`](super::toast) and [`toast_error`](super::toast_error) read
    /// them decoded.
    ///
    /// ```no_run
    /// # async fn example(ctx: &openshop::testing::TestContext) -> openshop::Result<()> {
    /// use openshop::testing::{toast, toast_error};
    ///
    /// let saved = ctx.submit("/settings", &[("_action", "saveSettings"), ("threshold", "8")]).send().await?;
    /// assert_eq!(saved.status(), 200);
    /// assert_eq!(toast(&saved).as_deref(), Some("Threshold saved"));
    /// assert_eq!(toast_error(&saved), None);
    /// # Ok(())
    /// # }
    /// ```
    pub fn submit(&self, path: &str, fields: &[(&str, &str)]) -> reqwest::RequestBuilder {
        self.post(path)
            .header("x-openshop-fetch", "1")
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(serde_urlencoded::to_string(fields).unwrap_or_default())
    }

    /// Reset all fakes call history
    pub fn reset_fakes(&self) {
        self.fakes.reset();
    }

    /// Shut down the test server and drop the isolated database.
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.server.shutdown().await;
        self.test_db.cleanup().await
    }
}
