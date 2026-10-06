//! A running application for tests.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use indexmap::IndexMap;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgPool;
use tokio::net::TcpListener;
use uuid::Uuid;

use super::db::TestDb;
use super::factory::{Factory, FactoryScope};
use super::fake::{FakeProviders, create_fake_providers};
use super::transport::MockTransport;
use super::{create_session_token, to_query_string};
use crate::app::{OpenShop, OpenShopConfig};
use crate::engine::dispatch::DispatchFlowParams;
use crate::engine::runner::{RunFlowOptions, RunFlowResult, run_flow};
use crate::runtime::RuntimeEnv;
use crate::server::hmac::proxy_signature;
use crate::server::{ServerHandle, ServerOptions, create_router, serve};

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

#[derive(Clone)]
pub struct TestOptions {
    pub shop: String,
    /// `SHOPIFY_API_SECRET` of the single-app setup.
    pub secret: String,
    /// `SHOPIFY_API_KEY` of the single-app setup.
    pub api_key: String,
    /// Marks the shop as installed with this access token.
    pub access_token: Option<String>,
    pub encryption_key: Option<String>,
    pub host: Option<String>,
    pub production: bool,
    /// Project root used to resolve Shopify TOML files. Defaults to a directory without any.
    pub cwd: Option<PathBuf>,
    pub server: ServerOptions,
    pub transport: Option<Arc<MockTransport>>,
    /// Send Shopify requests over the network instead of to the mock transport,
    /// for tests that create real resources with factories.
    pub live_shopify: bool,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            shop: "test.myshopify.com".to_string(),
            secret: "test-secret".to_string(),
            api_key: "test-app".to_string(),
            access_token: None,
            encryption_key: None,
            host: None,
            production: false,
            cwd: None,
            server: ServerOptions::default(),
            transport: None,
            live_shopify: false,
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
    /// HTTP client that does not follow redirects.
    pub http: reqwest::Client,
    factory_scope: FactoryScope,
    test_db: TestDb,
    server: ServerHandle,
}

impl TestContext {
    pub async fn new(config: OpenShopConfig) -> Result<Self> {
        Self::with_options(config, TestOptions::default()).await
    }

    pub async fn with_options(config: OpenShopConfig, options: TestOptions) -> Result<Self> {
        let test_db = TestDb::create_with_models(&config.models).await?;
        let db = test_db.pool.clone();
        let transport = options.transport.clone().unwrap_or_default();
        let env = RuntimeEnv {
            shopify_api_key: Some(options.api_key.clone()),
            shopify_api_secret: Some(options.secret.clone()),
            host: options.host.clone(),
            shopify_app_url: None,
            encryption_key: options.encryption_key.clone(),
            production: options.production,
            cwd: options
                .cwd
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("openshop-test-no-toml")),
        };
        let app = if options.live_shopify {
            OpenShop::new(config, db.clone(), env)?
        } else {
            OpenShop::with_transport(config, db.clone(), env, transport.clone())?
        };

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
        let fakes = create_fake_providers(&app.config().providers);
        let shopify = app.shopify_client(&options.shop, "default").await?;

        Ok(Self {
            url: format!("http://127.0.0.1:{port}"),
            port,
            fakes,
            transport,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            factory_scope: FactoryScope::new(shopify),
            shop: options.shop,
            secret: options.secret,
            api_key: options.api_key,
            app,
            db,
            test_db,
            server,
        })
    }

    /// Run a flow with faked providers
    pub async fn run_flow(&self, flow_name: &str, input: Value) -> Result<RunFlowResult> {
        let shop = self.shop.clone();
        self.run_flow_for(flow_name, input, &shop).await
    }

    pub async fn run_flow_for(&self, flow_name: &str, input: Value, shop: &str) -> Result<RunFlowResult> {
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
        let result = self
            .app
            .dispatch_flow(DispatchFlowParams::new(flow_name, self.shop.clone()).input(input))
            .await?;
        Ok(result.run_id)
    }

    /// Proxy test client — auto-signs HMAC
    pub fn proxy(&self) -> ProxyClient {
        ProxyClient {
            http: self.http.clone(),
            base_url: self.url.clone(),
            shop: self.shop.clone(),
            secret: self.secret.clone(),
        }
    }

    /// Create a signed Shopify session token for authenticated API requests.
    pub fn session_token(&self) -> String {
        self.session_token_for(&self.shop, "1")
    }

    pub fn session_token_for(&self, shop: &str, sub: &str) -> String {
        create_session_token(shop, &self.secret, &self.api_key, sub)
    }

    /// Authorization header value for authenticated API requests.
    pub fn authorization_header(&self) -> String {
        format!("Bearer {}", self.session_token())
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
        self.http
            .request(method, format!("{}{path}", self.url))
            .header("Authorization", self.authorization_header())
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

    /// Reset all fakes call history
    pub fn reset_fakes(&self) {
        self.fakes.reset();
    }

    /// Create a resource via a factory, tracked for auto-cleanup
    pub async fn create<T: Clone + Send + 'static>(&mut self, factory: &Factory<T>, overrides: Value) -> Result<T> {
        if !self.is_installed().await? {
            bail!("No Shopify client available. Is the test shop installed?");
        }
        self.factory_scope.create(factory, overrides).await
    }

    async fn is_installed(&self) -> Result<bool> {
        let installed: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM installations WHERE shop = $1 AND access_token IS NOT NULL LIMIT 1")
                .bind(&self.shop)
                .fetch_optional(&self.db)
                .await?;
        Ok(installed.is_some())
    }

    /// Cleanup all factory-created resources
    pub async fn cleanup(&mut self) {
        self.factory_scope.cleanup().await;
    }

    /// Shut down the test server and drop the isolated database.
    pub async fn shutdown(mut self) -> Result<()> {
        self.factory_scope.cleanup().await;
        let _ = self.server.shutdown().await;
        self.test_db.cleanup().await
    }
}
