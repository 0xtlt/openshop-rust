//! Environment-backed settings, captured once so that requests never read
//! process-global state.

use std::path::PathBuf;

use crate::shopify::ShopifyEndpoints;

/// Database used when `DATABASE_URL` is not set.
pub const DEFAULT_DATABASE_URL: &str = "postgresql://openshop:openshop@localhost:5432/openshop";

/// `Debug` does not show `shopify_api_secret` or `encryption_key`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RuntimeEnv {
    /// `SHOPIFY_API_KEY` (single-app mode).
    pub shopify_api_key: Option<String>,
    /// `SHOPIFY_API_SECRET` (single-app mode).
    pub shopify_api_secret: Option<String>,
    /// `HOST`: public app URL, preferred over `SHOPIFY_APP_URL`.
    pub host: Option<String>,
    /// `SHOPIFY_APP_URL`.
    pub shopify_app_url: Option<String>,
    /// `ENCRYPTION_KEY`: 64 hex characters.
    pub encryption_key: Option<String>,
    /// `OPENSHOP_ENV=production` (or `NODE_ENV=production`).
    pub production: bool,
    /// `OPENSHOP_ENV=development` or `test`, which is how an environment says it
    /// is not a deployed one. `NODE_ENV` does not. The `dev` command is
    /// one too. Unset, or any other value, is not.
    pub development: bool,
    /// Project root used to resolve Shopify TOML files and migrations.
    pub cwd: PathBuf,
    /// `SHOPIFY_ADMIN_ORIGIN`, `SHOPIFY_APP_BRIDGE_URL` and
    /// `SHOPIFY_POLARIS_URL`: the endpoints of Shopify the configuration does
    /// not set, field by field. See [`ShopifyEndpoints`].
    pub shopify_endpoints: ShopifyEndpoints,
}

impl std::fmt::Debug for RuntimeEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use super::redact::redacted_option;
        f.debug_struct("RuntimeEnv")
            .field("shopify_api_key", &self.shopify_api_key)
            .field("shopify_api_secret", &redacted_option(&self.shopify_api_secret))
            .field("host", &self.host)
            .field("shopify_app_url", &self.shopify_app_url)
            .field("encryption_key", &redacted_option(&self.encryption_key))
            .field("production", &self.production)
            .field("development", &self.development)
            .field("cwd", &self.cwd)
            .field("shopify_endpoints", &self.shopify_endpoints)
            .finish()
    }
}

/// `(production, development)` from the values of `OPENSHOP_ENV` and `NODE_ENV`.
/// Either one may say production, as before; only `OPENSHOP_ENV` can say
/// development, since `NODE_ENV` is set by tooling that has nothing to do with
/// where secrets may be stored in the clear.
fn environment(openshop_env: Option<&str>, node_env: Option<&str>) -> (bool, bool) {
    let production = [openshop_env, node_env].into_iter().flatten().any(|value| value == "production");
    (production, matches!(openshop_env, Some("development" | "test")))
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

impl RuntimeEnv {
    pub fn from_env() -> Self {
        let (production, development) = environment(
            std::env::var("OPENSHOP_ENV").ok().as_deref(),
            std::env::var("NODE_ENV").ok().as_deref(),
        );

        Self {
            shopify_api_key: non_empty("SHOPIFY_API_KEY"),
            shopify_api_secret: non_empty("SHOPIFY_API_SECRET"),
            host: non_empty("HOST"),
            shopify_app_url: non_empty("SHOPIFY_APP_URL"),
            encryption_key: non_empty("ENCRYPTION_KEY"),
            production,
            development,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            shopify_endpoints: ShopifyEndpoints::from_env(),
        }
    }

    /// Whether secrets may be stored in plaintext, without an `ENCRYPTION_KEY`:
    /// only in an environment that says it is a development one, and never in
    /// production.
    pub fn allows_plaintext_secrets(&self) -> bool {
        self.development && !self.production
    }

    /// `HOST ?? SHOPIFY_APP_URL ?? ''`
    pub fn fallback_app_url(&self) -> String {
        self.host
            .clone()
            .or_else(|| self.shopify_app_url.clone())
            .unwrap_or_default()
    }

    /// Origins of the configured public URLs, used by CORS.
    pub fn configured_origins(&self) -> Vec<String> {
        [&self.host, &self.shopify_app_url]
            .into_iter()
            .flatten()
            .map(|value| match url::Url::parse(value) {
                Ok(url) => url.origin().ascii_serialization(),
                Err(_) => value.trim_end_matches('/').to_string(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_host_over_shopify_app_url() {
        let env = RuntimeEnv {
            host: Some("https://a.example".into()),
            shopify_app_url: Some("https://b.example".into()),
            ..Default::default()
        };
        assert_eq!(env.fallback_app_url(), "https://a.example");
        assert_eq!(RuntimeEnv::default().fallback_app_url(), "");
    }

    #[test]
    fn plaintext_secrets_need_an_explicit_development_environment() {
        let env = |production, development| RuntimeEnv {
            production,
            development,
            ..Default::default()
        };
        assert!(!RuntimeEnv::default().allows_plaintext_secrets());
        assert!(env(false, true).allows_plaintext_secrets());
        assert!(!env(true, false).allows_plaintext_secrets());
        assert!(!env(true, true).allows_plaintext_secrets());
    }

    #[test]
    fn only_a_known_value_names_the_environment() {
        assert_eq!(environment(None, None), (false, false));
        assert_eq!(environment(Some("production"), None), (true, false));
        assert_eq!(environment(None, Some("production")), (true, false));
        assert_eq!(environment(Some("development"), None), (false, true));
        assert_eq!(environment(Some("test"), None), (false, true));
        // `NODE_ENV` never opts in to plaintext secrets.
        assert_eq!(environment(None, Some("development")), (false, false));
        assert_eq!(environment(None, Some("test")), (false, false));
        assert_eq!(environment(Some("development"), Some("production")), (true, true));
        // `prod`, `staging`, a typo, an empty value: not a development environment.
        for value in ["prod", "staging", "Production", "developement", ""] {
            assert!(!environment(Some(value), None).1, "{value}");
        }
    }

    #[test]
    fn derives_origins_from_configured_urls() {
        let env = RuntimeEnv {
            host: Some("https://app.example.com/some/path".into()),
            shopify_app_url: Some("not a url/".into()),
            ..Default::default()
        };
        assert_eq!(
            env.configured_origins(),
            vec!["https://app.example.com".to_string(), "not a url".to_string()]
        );
    }
}
