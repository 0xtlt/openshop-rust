//! Resolution of the Shopify app(s) served by this OpenShop instance.

use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Result, anyhow, bail};
use indexmap::IndexMap;
use regex::Regex;

use super::hmac::{verify_query_hmac, verify_webhook_hmac};
use crate::runtime::RuntimeEnv;
use crate::types::{ShopifyAppConfig, ShopifyConfig};

pub const DEFAULT_SHOPIFY_APP_HANDLE: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShopifyAppSource {
    Env,
    Toml,
    Config,
}

/// `Debug` does not show `api_secret`.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedShopifyApp {
    pub handle: String,
    pub api_key: String,
    pub api_secret: String,
    pub app_url: String,
    pub scopes: String,
    pub source: ShopifyAppSource,
    pub toml: Option<String>,
}

impl std::fmt::Debug for ResolvedShopifyApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedShopifyApp")
            .field("handle", &self.handle)
            .field("api_key", &self.api_key)
            .field("api_secret", &crate::runtime::redact::redacted(&self.api_secret))
            .field("app_url", &self.app_url)
            .field("scopes", &self.scopes)
            .field("source", &self.source)
            .field("toml", &self.toml)
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShopifyTomlData {
    pub api_key: Option<String>,
    pub app_url: Option<String>,
    pub scopes: Option<String>,
}

fn read_toml_value(content: &str, key: &str) -> Option<String> {
    let pattern = format!(r#"(?m)^\s*{key}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s#]+))"#);
    let captures = Regex::new(&pattern).ok()?.captures(content)?;
    captures
        .get(1)
        .or_else(|| captures.get(2))
        .or_else(|| captures.get(3))
        .map(|value| value.as_str().to_string())
}

pub fn parse_shopify_toml(content: &str) -> ShopifyTomlData {
    ShopifyTomlData {
        api_key: read_toml_value(content, "client_id"),
        app_url: read_toml_value(content, "application_url"),
        scopes: read_toml_value(content, "scopes"),
    }
}

pub fn read_shopify_toml(path: &Path) -> Result<ShopifyTomlData> {
    Ok(parse_shopify_toml(&std::fs::read_to_string(path)?))
}

static TOML_CANDIDATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^shopify\.app(?:\..+)?\.toml$").expect("valid regex"));

/// Reads OAuth scopes from `shopify.app.<name>.toml` files, then `shopify.app.toml`.
pub fn read_scopes_from_toml(cwd: &Path) -> String {
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
        .filter_map(|file| read_shopify_toml(&cwd.join(file)).ok())
        .find_map(|toml| toml.scopes.filter(|scopes| !scopes.is_empty()))
        .unwrap_or_default()
}

fn global_scopes(config: Option<&ShopifyConfig>) -> Option<String> {
    config.and_then(|config| config.scopes.clone())
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
            let data = read_shopify_toml(&path)?;
            let Some(api_key) = data.api_key.filter(|key| !key.is_empty()) else {
                bail!("[openshop] Shopify app \"{handle}\" TOML is missing client_id");
            };
            Ok(ResolvedShopifyApp {
                handle: handle.to_string(),
                api_key,
                api_secret: api_secret.clone(),
                app_url: app_url
                    .clone()
                    .or(data.app_url)
                    .unwrap_or_else(|| env.fallback_app_url()),
                scopes: config.scopes.clone().or(data.scopes).unwrap_or_default(),
                source: ShopifyAppSource::Toml,
                toml: Some(toml.clone()),
            })
        }
        ShopifyAppConfig::Keys {
            api_key,
            api_secret,
            app_url,
        } => Ok(ResolvedShopifyApp {
            handle: handle.to_string(),
            api_key: api_key.clone(),
            api_secret: api_secret.clone(),
            app_url: app_url.clone().unwrap_or_else(|| env.fallback_app_url()),
            scopes: config.scopes.clone().unwrap_or_else(|| read_scopes_from_toml(&env.cwd)),
            source: ShopifyAppSource::Config,
            toml: None,
        }),
    }
}

fn normalize_legacy_app(config: Option<&ShopifyConfig>, env: &RuntimeEnv) -> ResolvedShopifyApp {
    ResolvedShopifyApp {
        handle: DEFAULT_SHOPIFY_APP_HANDLE.to_string(),
        api_key: env.shopify_api_key.clone().unwrap_or_default(),
        api_secret: env.shopify_api_secret.clone().unwrap_or_default(),
        app_url: env.fallback_app_url(),
        scopes: global_scopes(config).unwrap_or_else(|| read_scopes_from_toml(&env.cwd)),
        source: ShopifyAppSource::Env,
        toml: None,
    }
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

pub fn resolve_shopify_apps(config: Option<&ShopifyConfig>, env: &RuntimeEnv) -> Result<Vec<ResolvedShopifyApp>> {
    let Some((config, app_configs)) = config.and_then(|config| config.apps.as_ref().map(|apps| (config, apps))) else {
        return Ok(vec![normalize_legacy_app(config, env)]);
    };

    let apps = app_configs
        .iter()
        .map(|(handle, app)| normalize_app(handle, app, config, env))
        .collect::<Result<Vec<_>>>()?;

    if apps.is_empty() {
        bail!("[openshop] shopify.apps must contain at least one app");
    }
    assert_consistent_scopes(&apps, config.scopes.as_deref().is_some_and(|scopes| !scopes.is_empty()))?;
    Ok(apps)
}

/// The resolved apps of a running instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShopifyApps {
    apps: Vec<ResolvedShopifyApp>,
}

impl ShopifyApps {
    pub fn new(apps: Vec<ResolvedShopifyApp>) -> Self {
        Self { apps }
    }

    pub fn resolve(config: Option<&ShopifyConfig>, env: &RuntimeEnv) -> Result<Self> {
        Ok(Self::new(resolve_shopify_apps(config, env)?))
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

    pub fn by_signed_query(&self, query: &IndexMap<String, String>) -> Result<&ResolvedShopifyApp> {
        let matches: Vec<&ResolvedShopifyApp> = self
            .apps
            .iter()
            .filter(|app| verify_query_hmac(query, &app.api_secret))
            .collect();
        match matches[..] {
            [app] => Ok(app),
            [] => bail!("[openshop] No Shopify app matched the signed request"),
            _ => bail!("[openshop] Multiple Shopify apps matched the signed request"),
        }
    }

    pub fn by_webhook_hmac(&self, body: &[u8], hmac: &str) -> Result<&ResolvedShopifyApp> {
        let matches: Vec<&ResolvedShopifyApp> = self
            .apps
            .iter()
            .filter(|app| verify_webhook_hmac(body, hmac, &app.api_secret))
            .collect();
        match matches[..] {
            [app] => Ok(app),
            [] => bail!("[openshop] No Shopify app matched the webhook signature"),
            _ => bail!("[openshop] Multiple Shopify apps matched the webhook signature"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::hmac::tests::{sign_body, sign_query};

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
        let apps = resolve_shopify_apps(None, &env).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].handle, DEFAULT_SHOPIFY_APP_HANDLE);
        assert_eq!(apps[0].api_key, "legacy-key");
        assert_eq!(apps[0].api_secret, "legacy-secret");
        assert_eq!(apps[0].source, ShopifyAppSource::Env);
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

        let error = resolve_shopify_apps(Some(&toml_app("nope.toml")), &env).unwrap_err();
        assert!(error.to_string().contains("TOML not found: nope.toml"));

        std::fs::write(dir.join("empty.toml"), "name = \"x\"\n").unwrap();
        let error = resolve_shopify_apps(Some(&toml_app("empty.toml")), &env).unwrap_err();
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
        let error = resolve_shopify_apps(Some(&config), &env).unwrap_err();
        assert!(error.to_string().contains("scopes must be identical"));

        config.scopes = Some("read_products".into());
        assert_eq!(resolve_shopify_apps(Some(&config), &env).unwrap().len(), 2);
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

        let query = sign_query(&[("shop", "x.myshopify.com"), ("timestamp", "1780000000")], "secret-b");
        assert_eq!(apps.by_signed_query(&query).unwrap().handle, "clientB");

        let body = r#"{"id":1}"#;
        let hmac = sign_body(body, "secret-a");
        assert_eq!(apps.by_webhook_hmac(body.as_bytes(), &hmac).unwrap().handle, "clientA");
    }

    #[test]
    fn fails_explicitly_when_no_app_or_multiple_apps_match() {
        let apps = two_apps("shared-secret", "shared-secret", "duplicate-key", "duplicate-key");

        let error = apps.by_api_key("missing-key").unwrap_err().to_string();
        assert!(error.contains("No Shopify app matches"));
        let error = apps.by_api_key("duplicate-key").unwrap_err().to_string();
        assert!(error.contains("Multiple Shopify apps use apiKey"));

        let query = sign_query(
            &[("shop", "x.myshopify.com"), ("timestamp", "1780000000")],
            "shared-secret",
        );
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
    fn does_not_resolve_signed_requests_when_the_secret_is_missing() {
        let apps = ShopifyApps::resolve(None, &RuntimeEnv::default()).unwrap();
        assert!(!apps.has_configured_secret());

        let query = sign_query(&[("shop", "x.myshopify.com"), ("timestamp", "1780000000")], "");
        let error = apps.by_signed_query(&query).unwrap_err().to_string();
        assert!(error.contains("No Shopify app matched"));

        let body = r#"{"id":1}"#;
        let error = apps
            .by_webhook_hmac(body.as_bytes(), &sign_body(body, ""))
            .unwrap_err()
            .to_string();
        assert!(error.contains("No Shopify app matched"));
    }
}
