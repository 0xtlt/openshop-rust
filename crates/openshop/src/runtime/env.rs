//! Environment-backed settings, captured once so that requests never read
//! process-global state.

use std::path::PathBuf;

/// Database used when `DATABASE_URL` is not set.
pub const DEFAULT_DATABASE_URL: &str = "postgresql://openshop:openshop@localhost:5432/openshop";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// `SHOPIFY_CLIENT_CREDENTIALS_SHOP`: the one `*.myshopify.com` shop that
    /// work no Shopify request vouches for (crons, flows, routes, the MCP
    /// tool) may get an access token for with the client credentials grant,
    /// as it is written. [`OpenShop`](crate::OpenShop) refuses to start when
    /// it is not a shop domain.
    pub client_credentials_shop: Option<String>,
    /// `OPENSHOP_ENV=production` (or `NODE_ENV=production`).
    pub production: bool,
    /// Project root used to resolve Shopify TOML files and migrations.
    pub cwd: PathBuf,
}

fn non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

impl RuntimeEnv {
    pub fn from_env() -> Self {
        let production = ["OPENSHOP_ENV", "NODE_ENV"]
            .iter()
            .any(|name| std::env::var(name).is_ok_and(|value| value == "production"));

        Self {
            shopify_api_key: non_empty("SHOPIFY_API_KEY"),
            shopify_api_secret: non_empty("SHOPIFY_API_SECRET"),
            host: non_empty("HOST"),
            shopify_app_url: non_empty("SHOPIFY_APP_URL"),
            encryption_key: non_empty("ENCRYPTION_KEY"),
            client_credentials_shop: non_empty("SHOPIFY_CLIENT_CREDENTIALS_SHOP"),
            production,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
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
