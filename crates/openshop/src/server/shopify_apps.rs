//! Resolution of the Shopify app(s) served by this OpenShop instance.

use std::path::Path;
use std::sync::{Arc, LazyLock};

use anyhow::{Result, anyhow, bail};
use regex::Regex;

use axum::http::HeaderMap;
use shopify_api::AdminOrigin;
use shopify_api::oauth::{ShopifyApp, SignedQuery};
use shopify_api::webhooks::Webhook;

use crate::runtime::transport::{SharedTransport, USER_AGENT};
use crate::runtime::{RuntimeEnv, Transport};
use crate::types::{ShopifyAppConfig, ShopifyConfig};

pub const DEFAULT_SHOPIFY_APP_HANDLE: &str = "default";

/// The name of an app that nothing names: no `name` in a `shopify.app*.toml`,
/// and the app of the environment.
pub const DEFAULT_APP_NAME: &str = "OpenShop";

/// `Debug` does not show `api_secret`.
#[derive(Clone)]
pub struct ResolvedShopifyApp {
    /// The name OpenShop gives the app: `default` for the app of the
    /// environment, the key of `shopify.apps` otherwise. What `shopify_app`
    /// of a context is.
    pub handle: String,
    /// The handle of the app on Shopify, which the URLs of the Shopify admin
    /// name it by (`/charges/{app_handle}/pricing_plans`): the `handle` of its
    /// `shopify.app*.toml`. `None` without one: the client ID does not take
    /// its place, Shopify refuses it there. [`app_handle()`](Self::app_handle)
    /// says what to do.
    pub app_handle: Option<String>,
    /// The name of the app, which the admin shows: the `name` of the
    /// `shopify.app*.toml` its `app_handle` is read from, else its
    /// [`handle`](Self::handle) when it is one of `shopify.apps`, else
    /// [`DEFAULT_APP_NAME`].
    pub name: String,
    pub api_key: String,
    pub api_secret: String,
    pub app_url: String,
    pub scopes: String,
    /// The `shopify_api` app: what verifies the requests Shopify signs for
    /// this app, and exchanges its codes and session tokens.
    pub oauth: ShopifyApp,
}

/// The secret that verifies what Shopify signs for an app: none when it is
/// blank, which anybody could sign with.
fn signing_secret(api_secret: &str) -> &str {
    if api_secret.trim().is_empty() { "" } else { api_secret }
}

impl ResolvedShopifyApp {
    /// `toml` is the text of the `shopify.app*.toml` of the app, when it has one.
    fn new(
        handle: &str,
        toml: Option<&str>,
        api_key: String,
        api_secret: String,
        app_url: String,
        scopes: String,
    ) -> Self {
        let oauth = ShopifyApp::new(&api_key, signing_secret(&api_secret)).with_user_agent(USER_AGENT);
        let app_handle = toml.and_then(|toml| read_top_level_value(toml, "handle"));
        let name = toml
            .and_then(|toml| read_top_level_value(toml, "name"))
            .or_else(|| (handle != DEFAULT_SHOPIFY_APP_HANDLE).then(|| handle.to_string()))
            .unwrap_or_else(|| DEFAULT_APP_NAME.to_string());
        Self {
            handle: handle.to_string(),
            app_handle,
            name,
            api_key,
            api_secret,
            app_url,
            scopes,
            oauth,
        }
    }
}

impl ResolvedShopifyApp {
    /// The handle of the app on Shopify, which the pages of the Shopify admin
    /// name it by, such as the plan selection page. Fails when its
    /// `shopify.app*.toml` has no `handle`, saying to add it: Shopify does not
    /// take the client ID there.
    pub fn app_handle(&self) -> Result<&str> {
        self.app_handle.as_deref().ok_or_else(|| {
            let file = if self.api_key.trim().is_empty() {
                "shopify.app.toml".to_string()
            } else {
                format!("the shopify.app*.toml whose client_id is \"{}\"", self.api_key)
            };
            anyhow!(
                "[openshop] The Shopify app \"{}\" has no handle: add `handle = \"<your-app-handle>\"` at the \
                 top of {file}, next to client_id (`shopify app config link` writes it). The pages of the Shopify \
                 admin, such as the plan selection page, name the app by its handle, and Shopify does not take \
                 the client ID there.",
                self.handle
            )
        })
    }
}

impl std::fmt::Debug for ResolvedShopifyApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedShopifyApp")
            .field("handle", &self.handle)
            .field("app_handle", &self.app_handle)
            .field("name", &self.name)
            .field("api_key", &self.api_key)
            .field("api_secret", &crate::runtime::redact::redacted(&self.api_secret))
            .field("app_url", &self.app_url)
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// The value of `key` in the text of a `shopify.app.toml` file.
fn read_toml_value(content: &str, key: &str) -> Option<String> {
    let pattern = format!(r#"(?m)^\s*{key}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s#]+))"#);
    let captures = Regex::new(&pattern).ok()?.captures(content)?;
    captures
        .get(1)
        .or_else(|| captures.get(2))
        .or_else(|| captures.get(3))
        .map(|value| value.as_str().to_string())
}

static TOML_CANDIDATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^shopify\.app(?:\..+)?\.toml$").expect("valid regex"));

/// The text of a TOML file before its first table: its top-level keys.
fn top_level(content: &str) -> &str {
    let mut end = 0;
    for line in content.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            break;
        }
        end += line.len();
    }
    &content[..end]
}

/// A top-level key of a `shopify.app.toml`, not one of a table, that is not
/// blank, trimmed: the `handle` and the `name` Shopify CLI writes when it
/// links the app.
fn read_top_level_value(content: &str, key: &str) -> Option<String> {
    read_toml_value(top_level(content), key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The text of the `shopify.app*.toml` of `cwd` of the app whose API key is
/// `api_key`: the one whose `client_id` it is. Without an API key,
/// `shopify.app.toml`.
fn read_app_toml_of(cwd: &Path, api_key: &str) -> Option<String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return std::fs::read_to_string(cwd.join("shopify.app.toml")).ok();
    }
    let mut files: Vec<String> = std::fs::read_dir(cwd)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| TOML_CANDIDATE_RE.is_match(name))
        .collect();
    files.sort();
    files
        .iter()
        .filter_map(|file| std::fs::read_to_string(cwd.join(file)).ok())
        .find(|content| read_toml_value(top_level(content), "client_id").as_deref() == Some(api_key))
}

/// The `client_id` of the `shopify.app.toml` of `cwd`, when no
/// `shopify.app*.toml` there has `api_key` for its own: the app a test runs
/// as, when it gives the placeholder API key.
#[cfg(feature = "testing")]
pub(crate) fn client_id_in_place_of(cwd: &Path, api_key: &str) -> Option<String> {
    if read_app_toml_of(cwd, api_key).is_some() {
        return None;
    }
    let toml = std::fs::read_to_string(cwd.join("shopify.app.toml")).ok()?;
    read_top_level_value(&toml, "client_id")
}

/// Reads OAuth scopes from `shopify.app.<name>.toml` files, then `shopify.app.toml`.
fn read_scopes_from_toml(cwd: &Path) -> String {
    let mut candidates: Vec<String> = std::fs::read_dir(cwd)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| TOML_CANDIDATE_RE.is_match(name) && name != "shopify.app.toml")
                .collect()
        })
        .unwrap_or_default();
    candidates.sort();
    candidates.push("shopify.app.toml".to_string());

    candidates
        .iter()
        .filter_map(|file| std::fs::read_to_string(cwd.join(file)).ok())
        .find_map(|content| read_toml_value(&content, "scopes").filter(|scopes| !scopes.is_empty()))
        .unwrap_or_default()
}

fn normalize_app(
    handle: &str,
    app: &ShopifyAppConfig,
    config: &ShopifyConfig,
    env: &RuntimeEnv,
) -> Result<ResolvedShopifyApp> {
    match app {
        ShopifyAppConfig::Toml {
            toml,
            api_secret,
            app_url,
        } => {
            let path = env.cwd.join(toml);
            if !path.exists() {
                bail!("[openshop] Shopify app \"{handle}\" TOML not found: {toml}");
            }
            let content = std::fs::read_to_string(&path)?;
            let Some(api_key) = read_toml_value(&content, "client_id").filter(|key| !key.is_empty()) else {
                bail!("[openshop] Shopify app \"{handle}\" TOML is missing client_id");
            };
            Ok(ResolvedShopifyApp::new(
                handle,
                Some(&content),
                api_key,
                api_secret.clone(),
                app_url
                    .clone()
                    .or_else(|| read_toml_value(&content, "application_url"))
                    .unwrap_or_else(|| env.fallback_app_url()),
                config
                    .scopes
                    .clone()
                    .or_else(|| read_toml_value(&content, "scopes"))
                    .unwrap_or_default(),
            ))
        }
        ShopifyAppConfig::Keys {
            api_key,
            api_secret,
            app_url,
        } => Ok(ResolvedShopifyApp::new(
            handle,
            // An app of several has a file of its own: one with its client id.
            (!api_key.trim().is_empty())
                .then(|| read_app_toml_of(&env.cwd, api_key))
                .flatten()
                .as_deref(),
            api_key.clone(),
            api_secret.clone(),
            app_url.clone().unwrap_or_else(|| env.fallback_app_url()),
            config.scopes.clone().unwrap_or_else(|| read_scopes_from_toml(&env.cwd)),
        )),
    }
}

fn normalize_legacy_app(config: Option<&ShopifyConfig>, env: &RuntimeEnv) -> ResolvedShopifyApp {
    let api_key = env.shopify_api_key.clone().unwrap_or_default();
    ResolvedShopifyApp::new(
        DEFAULT_SHOPIFY_APP_HANDLE,
        read_app_toml_of(&env.cwd, &api_key).as_deref(),
        api_key,
        env.shopify_api_secret.clone().unwrap_or_default(),
        env.fallback_app_url(),
        config
            .and_then(|config| config.scopes.clone())
            .unwrap_or_else(|| read_scopes_from_toml(&env.cwd)),
    )
}

fn assert_consistent_scopes(apps: &[ResolvedShopifyApp], has_global_scopes: bool) -> Result<()> {
    if has_global_scopes {
        return Ok(());
    }
    let mut scopes: Vec<&str> = apps
        .iter()
        .map(|app| app.scopes.as_str())
        .filter(|scopes| !scopes.is_empty())
        .collect();
    scopes.sort_unstable();
    scopes.dedup();
    if scopes.len() <= 1 {
        return Ok(());
    }
    bail!("[openshop] Shopify app scopes must be identical across all configured apps")
}

/// The resolved apps of a running instance.
#[derive(Debug, Clone)]
pub struct ShopifyApps {
    apps: Vec<ResolvedShopifyApp>,
}

impl ShopifyApps {
    pub fn resolve(config: Option<&ShopifyConfig>, env: &RuntimeEnv) -> Result<Self> {
        let Some((config, app_configs)) = config.and_then(|config| config.apps.as_ref().map(|apps| (config, apps)))
        else {
            return Ok(Self {
                apps: vec![normalize_legacy_app(config, env)],
            });
        };

        let apps = app_configs
            .iter()
            .map(|(handle, app)| normalize_app(handle, app, config, env))
            .collect::<Result<Vec<_>>>()?;

        if apps.is_empty() {
            bail!("[openshop] shopify.apps must contain at least one app");
        }
        assert_consistent_scopes(&apps, config.scopes.as_deref().is_some_and(|scopes| !scopes.is_empty()))?;
        Ok(Self { apps })
    }

    /// The same apps, asking Shopify for tokens through `transport`.
    pub(crate) fn with_transport(mut self, transport: Arc<dyn Transport>) -> Self {
        for app in &mut self.apps {
            app.oauth = app.oauth.clone().with_transport(SharedTransport(transport.clone()));
        }
        self
    }

    /// The same apps, with the Admin API, OAuth and the token exchange of
    /// every shop at `origin` instead of Shopify (the `admin_origin` of the
    /// Shopify endpoints): the clients they build send their requests there
    /// too.
    pub(crate) fn with_admin_origin(mut self, origin: Option<&AdminOrigin>) -> Result<Self> {
        if let Some(origin) = origin {
            for app in &mut self.apps {
                app.oauth = app.oauth.clone().with_admin_origin(origin)?;
            }
        }
        Ok(self)
    }

    pub fn all(&self) -> &[ResolvedShopifyApp] {
        &self.apps
    }

    pub fn has_configured_secret(&self) -> bool {
        self.apps.iter().any(|app| !app.api_secret.trim().is_empty())
    }

    /// With a single app the handle may be omitted.
    pub fn by_handle(&self, handle: Option<&str>) -> Result<&ResolvedShopifyApp> {
        let handle = match handle {
            Some(handle) => handle,
            None if self.apps.len() == 1 => self.apps[0].handle.as_str(),
            None => "",
        };
        // An explicitly empty handle (`?app=`) is missing, not unknown.
        if handle.is_empty() {
            bail!("[openshop] Missing Shopify app handle");
        }
        self.apps
            .iter()
            .find(|app| app.handle == handle)
            .ok_or_else(|| anyhow!("[openshop] Unknown Shopify app \"{handle}\""))
    }

    pub fn by_api_key(&self, api_key: &str) -> Result<&ResolvedShopifyApp> {
        let matches: Vec<&ResolvedShopifyApp> = self.apps.iter().filter(|app| app.api_key == api_key).collect();
        match matches[..] {
            [app] => Ok(app),
            [] => bail!("[openshop] No Shopify app matches the session token audience"),
            _ => bail!("[openshop] Multiple Shopify apps use apiKey \"{api_key}\""),
        }
    }

    /// The one app `verify` accepts what was signed for, and what it verified.
    /// `signed` names it in the errors.
    fn only_signer<T>(
        &self,
        signed: &str,
        verify: impl Fn(&ResolvedShopifyApp) -> Option<T>,
    ) -> Result<(&ResolvedShopifyApp, T)> {
        let mut matches = self.apps.iter().filter_map(|app| Some((app, verify(app)?)));
        match (matches.next(), matches.next()) {
            (Some(verified), None) => Ok(verified),
            (None, _) => bail!("[openshop] No Shopify app matched the {signed}"),
            (Some(_), Some(_)) => bail!("[openshop] Multiple Shopify apps matched the {signed}"),
        }
    }

    /// The one app whose secret signed `query`, a launch from Shopify admin or
    /// an OAuth callback (`hmac`, and a `timestamp` of the last 90 seconds), and
    /// the query as it verified.
    pub fn by_signed_query(&self, query: &str) -> Result<(&ResolvedShopifyApp, SignedQuery)> {
        self.only_signer("signed request", |app| app.oauth.verify_query(query).ok())
    }

    /// The one app whose secret signed the app proxy request of `query`
    /// (`signature`, and a `timestamp` of the last 90 seconds).
    pub fn by_proxy_signature(&self, query: &str) -> Result<(&ResolvedShopifyApp, SignedQuery)> {
        self.only_signer("proxy signature", |app| app.oauth.verify_app_proxy(query).ok())
    }

    /// The one app whose secret signed a webhook, and the webhook as it verified.
    pub fn by_webhook(&self, headers: &HeaderMap, body: &[u8]) -> Result<(&ResolvedShopifyApp, Webhook)> {
        self.only_signer("webhook signature", |app| {
            Webhook::verify(headers, body.to_vec(), signing_secret(&app.api_secret)).ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `params` as a query string, signed with `secret` as Shopify admin signs a launch.
    fn sign_query(params: &[(&str, &str)], secret: &str) -> String {
        let query = serde_urlencoded::to_string(params).unwrap();
        let hmac = ShopifyApp::new("", secret).sign_query(&query).unwrap();
        format!("{query}&hmac={hmac}")
    }

    /// The headers Shopify sends with a webhook whose body is `body`.
    fn webhook_headers(body: &str, secret: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let signature = shopify_api::webhooks::sign(secret, body.as_bytes());
        headers.insert("x-shopify-hmac-sha256", signature.parse().unwrap());
        headers.insert("x-shopify-topic", "orders/create".parse().unwrap());
        headers.insert("x-shopify-shop-domain", "x.myshopify.com".parse().unwrap());
        headers
    }

    fn now() -> String {
        chrono::Utc::now().timestamp().to_string()
    }

    fn keys(api_key: &str, api_secret: &str, app_url: &str) -> ShopifyAppConfig {
        ShopifyAppConfig::Keys {
            api_key: api_key.into(),
            api_secret: api_secret.into(),
            app_url: Some(app_url.into()),
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("openshop-apps-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_env_based_app_when_shopify_apps_is_absent() {
        let env = RuntimeEnv {
            shopify_api_key: Some("legacy-key".into()),
            shopify_api_secret: Some("legacy-secret".into()),
            cwd: temp_dir("legacy"),
            ..Default::default()
        };
        let apps = ShopifyApps::resolve(None, &env).unwrap();
        let apps = apps.all();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].handle, DEFAULT_SHOPIFY_APP_HANDLE);
        assert_eq!(apps[0].api_key, "legacy-key");
        assert_eq!(apps[0].api_secret, "legacy-secret");
    }

    #[test]
    fn resolves_toml_and_config_only_apps() {
        let dir = temp_dir("toml");
        std::fs::write(
            dir.join("shopify.app.client-a.toml"),
            r#"
client_id = "client-a-key"
application_url = "https://client-a.example.com"

[access_scopes]
scopes = "read_products,read_orders"
"#,
        )
        .unwrap();

        let config = ShopifyConfig {
            scopes: Some("read_products,read_orders".into()),
            apps: Some(
                [
                    (
                        "clientA".to_string(),
                        ShopifyAppConfig::Toml {
                            toml: "shopify.app.client-a.toml".into(),
                            api_secret: "secret-a".into(),
                            app_url: None,
                        },
                    ),
                    (
                        "clientB".to_string(),
                        keys("client-b-key", "secret-b", "https://client-b.example.com"),
                    ),
                ]
                .into(),
            ),
        };
        let env = RuntimeEnv {
            cwd: dir.clone(),
            ..Default::default()
        };

        let apps = ShopifyApps::resolve(Some(&config), &env).unwrap();
        assert_eq!(apps.all().len(), 2);
        let client_a = apps.by_handle(Some("clientA")).unwrap();
        assert_eq!(client_a.api_key, "client-a-key");
        assert_eq!(client_a.app_url, "https://client-a.example.com");
        assert_eq!(apps.by_api_key("client-b-key").unwrap().handle, "clientB");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reports_missing_toml_files_and_client_ids() {
        let dir = temp_dir("missing");
        let env = RuntimeEnv {
            cwd: dir.clone(),
            ..Default::default()
        };
        let toml_app = |file: &str| ShopifyConfig {
            scopes: None,
            apps: Some(
                [(
                    "main".to_string(),
                    ShopifyAppConfig::Toml {
                        toml: file.into(),
                        api_secret: "secret".into(),
                        app_url: None,
                    },
                )]
                .into(),
            ),
        };

        let error = ShopifyApps::resolve(Some(&toml_app("nope.toml")), &env).unwrap_err();
        assert!(error.to_string().contains("TOML not found: nope.toml"));

        std::fs::write(dir.join("empty.toml"), "name = \"x\"\n").unwrap();
        let error = ShopifyApps::resolve(Some(&toml_app("empty.toml")), &env).unwrap_err();
        assert!(error.to_string().contains("TOML is missing client_id"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_scopes_from_linked_toml_before_the_template() {
        let dir = temp_dir("scopes");
        std::fs::write(dir.join("shopify.app.toml"), "scopes = \"read_products\"\n").unwrap();
        assert_eq!(read_scopes_from_toml(&dir), "read_products");
        std::fs::write(
            dir.join("shopify.app.dev.toml"),
            "[access_scopes]\nscopes = 'read_orders' # linked\n",
        )
        .unwrap();
        assert_eq!(read_scopes_from_toml(&dir), "read_orders");
        assert_eq!(read_scopes_from_toml(&dir.join("missing")), "");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reads_the_handle_of_the_app_on_shopify_from_its_toml() {
        let dir = temp_dir("handle");
        let env = |api_key: Option<&str>| RuntimeEnv {
            shopify_api_key: api_key.map(str::to_string),
            cwd: dir.clone(),
            ..Default::default()
        };
        let app_handle = |api_key: Option<&str>| {
            let apps = ShopifyApps::resolve(None, &env(api_key)).unwrap();
            let app = &apps.all()[0];
            app.app_handle()
                .map(str::to_string)
                .unwrap_or_else(|error| error.to_string())
        };
        // No file: no handle, and the error says where to write it.
        assert_eq!(
            app_handle(Some("prod-key")),
            "[openshop] The Shopify app \"default\" has no handle: add `handle = \"<your-app-handle>\"` at the \
             top of the shopify.app*.toml whose client_id is \"prod-key\", next to client_id (`shopify app config \
             link` writes it). The pages of the Shopify admin, such as the plan selection page, name the app by \
             its handle, and Shopify does not take the client ID there."
        );
        assert!(app_handle(None).contains("at the top of shopify.app.toml, next to client_id"));

        std::fs::write(
            dir.join("shopify.app.toml"),
            "client_id = \"prod-key\"\nname = \"Demo App\"\nhandle = \"demo-app\"\n\n[webhooks]\napi_version = \"2026-10\"\n",
        )
        .unwrap();
        // A handle of a table is not the app's.
        std::fs::write(
            dir.join("shopify.app.dev.toml"),
            "client_id = 'dev-key'\n[[extensions]]\nhandle = \"not-the-app\"\n",
        )
        .unwrap();
        assert_eq!(app_handle(Some("prod-key")), "demo-app");
        assert_eq!(app_handle(None), "demo-app");
        // The file of the app is the one of its client id.
        assert!(app_handle(Some("dev-key")).contains("has no handle"));
        std::fs::write(
            dir.join("shopify.app.dev.toml"),
            "client_id = 'dev-key'\nhandle = 'demo-app-dev' # linked\n",
        )
        .unwrap();
        assert_eq!(app_handle(Some("dev-key")), "demo-app-dev");
        assert!(app_handle(Some("other-key")).contains("whose client_id is \"other-key\""));

        // An app of `shopify.apps`: the handle of its own file, else as above.
        let config = ShopifyConfig {
            scopes: None,
            apps: Some(
                [
                    (
                        "main".to_string(),
                        ShopifyAppConfig::Toml {
                            toml: "shopify.app.dev.toml".into(),
                            api_secret: "secret".into(),
                            app_url: None,
                        },
                    ),
                    ("keys".to_string(), keys("prod-key", "secret", "https://a.example.com")),
                    ("other".to_string(), keys("", "secret", "https://b.example.com")),
                ]
                .into(),
            ),
        };
        let apps = ShopifyApps::resolve(Some(&config), &env(None)).unwrap();
        let handle_of = |name: &str| apps.by_handle(Some(name)).unwrap().app_handle.clone();
        assert_eq!(handle_of("main").as_deref(), Some("demo-app-dev"));
        assert_eq!(handle_of("keys").as_deref(), Some("demo-app"));
        assert_eq!(handle_of("other"), None);
        assert!(
            apps.by_handle(Some("other"))
                .unwrap()
                .app_handle()
                .unwrap_err()
                .to_string()
                .starts_with("[openshop] The Shopify app \"other\" has no handle")
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn names_the_app_after_the_name_of_its_toml() {
        let dir = temp_dir("name");
        let env = |api_key: Option<&str>| RuntimeEnv {
            shopify_api_key: api_key.map(str::to_string),
            cwd: dir.clone(),
            ..Default::default()
        };
        let name = |api_key: Option<&str>| ShopifyApps::resolve(None, &env(api_key)).unwrap().all()[0].name.clone();
        // No file: nothing names the app of the environment.
        assert_eq!(name(Some("prod-key")), DEFAULT_APP_NAME);
        assert_eq!(name(None), DEFAULT_APP_NAME);

        std::fs::write(
            dir.join("shopify.app.toml"),
            "client_id = \"prod-key\"\nname = \"My Demo App\"\nhandle = \"demo-app\"\n",
        )
        .unwrap();
        // The name of a table is not the app's, and a blank one names nothing.
        std::fs::write(
            dir.join("shopify.app.dev.toml"),
            "client_id = 'dev-key'\nname = '  '\n[[extensions]]\nname = \"Not the app\"\n",
        )
        .unwrap();
        assert_eq!(name(Some("prod-key")), "My Demo App");
        assert_eq!(name(None), "My Demo App");
        assert_eq!(name(Some("dev-key")), DEFAULT_APP_NAME);
        assert_eq!(name(Some("other-key")), DEFAULT_APP_NAME);
        std::fs::write(
            dir.join("shopify.app.dev.toml"),
            "client_id = 'dev-key'\nname = 'Demo App (dev)' # linked\n",
        )
        .unwrap();
        assert_eq!(name(Some("dev-key")), "Demo App (dev)");

        // An app of `shopify.apps`: the name of its own file, else its key.
        let config = ShopifyConfig {
            scopes: None,
            apps: Some(
                [
                    (
                        "main".to_string(),
                        ShopifyAppConfig::Toml {
                            toml: "shopify.app.dev.toml".into(),
                            api_secret: "secret".into(),
                            app_url: None,
                        },
                    ),
                    ("keys".to_string(), keys("prod-key", "secret", "https://a.example.com")),
                    (
                        "other".to_string(),
                        keys("other-key", "secret", "https://b.example.com"),
                    ),
                ]
                .into(),
            ),
        };
        let apps = ShopifyApps::resolve(Some(&config), &env(None)).unwrap();
        let name_of = |handle: &str| apps.by_handle(Some(handle)).unwrap().name.clone();
        assert_eq!(name_of("main"), "Demo App (dev)");
        assert_eq!(name_of("keys"), "My Demo App");
        assert_eq!(name_of("other"), "other");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejects_inconsistent_scopes_without_global_scopes() {
        let dir = temp_dir("inconsistent");
        std::fs::write(dir.join("a.toml"), "client_id = \"a\"\nscopes = \"read_products\"\n").unwrap();
        std::fs::write(dir.join("b.toml"), "client_id = \"b\"\nscopes = \"read_orders\"\n").unwrap();
        let app = |file: &str| ShopifyAppConfig::Toml {
            toml: file.into(),
            api_secret: "secret".into(),
            app_url: None,
        };
        let mut config = ShopifyConfig {
            scopes: None,
            apps: Some([("a".to_string(), app("a.toml")), ("b".to_string(), app("b.toml"))].into()),
        };
        let env = RuntimeEnv {
            cwd: dir.clone(),
            ..Default::default()
        };
        let error = ShopifyApps::resolve(Some(&config), &env).unwrap_err();
        assert!(error.to_string().contains("scopes must be identical"));

        config.scopes = Some("read_products".into());
        assert_eq!(ShopifyApps::resolve(Some(&config), &env).unwrap().all().len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    fn two_apps(secret_a: &str, secret_b: &str, key_a: &str, key_b: &str) -> ShopifyApps {
        let config = ShopifyConfig {
            scopes: Some("read_products".into()),
            apps: Some(
                [
                    (
                        "clientA".to_string(),
                        keys(key_a, secret_a, "https://client-a.example.com"),
                    ),
                    (
                        "clientB".to_string(),
                        keys(key_b, secret_b, "https://client-b.example.com"),
                    ),
                ]
                .into(),
            ),
        };
        ShopifyApps::resolve(Some(&config), &RuntimeEnv::default()).unwrap()
    }

    #[test]
    fn resolves_signed_oauth_and_webhook_requests_against_the_matching_app_secret() {
        let apps = two_apps("secret-a", "secret-b", "client-a-key", "client-b-key");

        let query = sign_query(&[("shop", "x.myshopify.com"), ("timestamp", &now())], "secret-b");
        assert_eq!(apps.by_signed_query(&query).unwrap().0.handle, "clientB");

        let body = r#"{"id":1}"#;
        let (app, webhook) = apps
            .by_webhook(&webhook_headers(body, "secret-a"), body.as_bytes())
            .unwrap();
        assert_eq!(app.handle, "clientA");
        assert_eq!(webhook.topic(), "orders/create");
    }

    #[test]
    fn fails_explicitly_when_no_app_or_multiple_apps_match() {
        let apps = two_apps("shared-secret", "shared-secret", "duplicate-key", "duplicate-key");

        let error = apps.by_api_key("missing-key").unwrap_err().to_string();
        assert!(error.contains("No Shopify app matches"));
        let error = apps.by_api_key("duplicate-key").unwrap_err().to_string();
        assert!(error.contains("Multiple Shopify apps use apiKey"));

        let query = sign_query(&[("shop", "x.myshopify.com"), ("timestamp", &now())], "shared-secret");
        let error = apps.by_signed_query(&query).unwrap_err().to_string();
        assert!(error.contains("Multiple Shopify apps matched"));

        let error = apps.by_handle(None).unwrap_err().to_string();
        assert!(error.contains("Missing Shopify app handle"));
        let error = apps.by_handle(Some("")).unwrap_err().to_string();
        assert!(error.contains("Missing Shopify app handle"));
        let error = apps.by_handle(Some("nope")).unwrap_err().to_string();
        assert!(error.contains("Unknown Shopify app \"nope\""));
    }

    #[test]
    fn an_admin_origin_sends_every_app_and_client_there() {
        let apps = two_apps("secret-a", "secret-b", "client-a-key", "client-b-key")
            .with_transport(crate::testing::MockTransport::new());
        let origin = AdminOrigin::new("http://localhost:9292").unwrap();
        let shop = shopify_api::ShopDomain::new("local-supply-co.myshopify.com").unwrap();
        let graphql_url = |apps: &ShopifyApps| -> Vec<String> {
            apps.all()
                .iter()
                .map(|app| {
                    app.oauth
                        .builder(&shop)
                        .access_token("shpat_x")
                        .build()
                        .unwrap()
                        .graphql_url(shopify_api::ApiVersion::V2026_10)
                })
                .collect()
        };

        let unchanged = apps.clone().with_admin_origin(None).unwrap();
        assert!(unchanged.all().iter().all(|app| app.oauth.admin_origin().is_none()));
        assert_eq!(
            graphql_url(&unchanged),
            vec!["https://local-supply-co.myshopify.com/admin/api/2026-10/graphql.json"; 2]
        );

        let local = apps.with_admin_origin(Some(&origin)).unwrap();
        assert!(local.all().iter().all(|app| app.oauth.admin_origin() == Some(&origin)));
        assert_eq!(
            graphql_url(&local),
            vec!["http://localhost:9292/admin/api/2026-10/graphql.json"; 2]
        );
        // OAuth too, for the same shop.
        let link = local.all()[0].oauth.authorization_url(
            &shop,
            &shopify_api::oauth::AuthorizationRequest::new("http://localhost:3000/auth/callback", "state"),
        );
        assert!(
            link.starts_with("http://localhost:9292/admin/oauth/authorize?client_id=client-a-key&"),
            "{link}"
        );
        assert!(link.ends_with("&shop=local-supply-co.myshopify.com"), "{link}");
    }

    #[test]
    fn does_not_resolve_signed_requests_when_the_secret_is_missing() {
        let apps = ShopifyApps::resolve(None, &RuntimeEnv::default()).unwrap();
        assert!(!apps.has_configured_secret());

        let query = sign_query(&[("shop", "x.myshopify.com"), ("timestamp", &now())], "");
        let error = apps.by_signed_query(&query).unwrap_err().to_string();
        assert!(error.contains("No Shopify app matched"));

        let body = r#"{"id":1}"#;
        let error = apps
            .by_webhook(&webhook_headers(body, ""), body.as_bytes())
            .unwrap_err()
            .to_string();
        assert!(error.contains("No Shopify app matched"));
    }
}
