//! Where an application reaches Shopify: the Admin API and OAuth, the
//! Partner API, and the scripts its admin UI loads. Shopify's by default; a
//! simulator, a proxy or an API of one's own when set.

use anyhow::{Result, anyhow};
use shopify_api::AdminOrigin;

/// The variable that sets [`ShopifyEndpoints::admin_origin`] when the code does not.
pub const ADMIN_ORIGIN_VAR: &str = "SHOPIFY_ADMIN_ORIGIN";
/// The variable that sets [`ShopifyEndpoints::app_bridge`] when the code does not.
pub const APP_BRIDGE_VAR: &str = "SHOPIFY_APP_BRIDGE_URL";
/// The variable that sets [`ShopifyEndpoints::polaris`] when the code does not.
pub const POLARIS_VAR: &str = "SHOPIFY_POLARIS_URL";
/// The variable that sets [`ShopifyEndpoints::partner_origin`] when the code does not.
pub const PARTNER_ORIGIN_VAR: &str = "SHOPIFY_PARTNER_ORIGIN";

/// Shopify's Partner API: where its requests go when `partner_origin` is not set.
pub const SHOPIFY_PARTNER_ORIGIN: &str = "https://partners.shopify.com";

/// Shopify's App Bridge: what the admin UI loads when `app_bridge` is not set.
pub const SHOPIFY_APP_BRIDGE: &str = "https://cdn.shopify.com/shopifycloud/app-bridge.js";
/// Shopify's Polaris: what the admin UI loads when `polaris` is not set.
pub const SHOPIFY_POLARIS: &str = "https://cdn.shopify.com/shopifycloud/polaris.js";

/// Where an application reaches Shopify, field by field: a field that is
/// `None` is Shopify's. [`ShopifyEndpoints::new`] is Shopify for all of them.
///
/// Each field moves one thing, independently of the others:
///
/// - [`admin_origin`](Self::admin_origin): where the Admin API, the token
///   requests (token exchange, code exchange, client credentials, refresh) and
///   the install link of every shop go, in place of `https://{shop}`. The
///   shop is still the shop: the requests name it in `X-Forwarded-Host`, and
///   the install link in its `shop` parameter. After OAuth, the browser goes
///   back to `{admin_origin}/admin/apps/{client_id}?shop={shop}`, and the
///   admin UI may be framed by this origin.
/// - [`app_bridge`](Self::app_bridge): the App Bridge script the admin UI
///   loads, in place of [`SHOPIFY_APP_BRIDGE`]. The tag keeps its
///   `data-api-key`, and the page its `shopify-api-key` meta.
/// - [`polaris`](Self::polaris): the Polaris script the admin UI and the pages
///   the server renders load, in place of [`SHOPIFY_POLARIS`].
/// - [`partner_origin`](Self::partner_origin): where the requests to the
///   Partner API go, in place of [`SHOPIFY_PARTNER_ORIGIN`]: billing reads the
///   plans of the shops there when the [Partner API](crate::PartnerApi) is set.
///   The path stays Shopify's, `/{partner_id}/api/{version}/graphql.json`.
///
/// An application sets them in [`app!`](crate::app!) (`shopify_endpoints:
/// …`), or at startup on the builder ([`App::main_with`](crate::App::main_with)).
/// A field the code does not set is read from its variable,
/// `SHOPIFY_ADMIN_ORIGIN`, `SHOPIFY_APP_BRIDGE_URL`, `SHOPIFY_POLARIS_URL` or
/// `SHOPIFY_PARTNER_ORIGIN`, then is Shopify's.
///
/// An origin (the admin's, the Partner API's) is `https://host[:port]`, or `http://` to `localhost`,
/// `127.0.0.1` or `[::1]`, with no path, query string, fragment or user. A
/// script is an absolute `https://` URL, or `http://` to the same hosts, with
/// no user or fragment. Anything else stops the application at startup, so
/// that an access token never travels in clear text off the machine.
///
/// ```
/// use openshop::ShopifyEndpoints;
///
/// // local-storefront, a local simulator of Shopify, on port 9393.
/// let simulator = ShopifyEndpoints::new()
///     .admin_origin("http://localhost:9393")
///     .app_bridge("http://localhost:9393/__lsf/admin/app-bridge.js")
///     .polaris("http://localhost:9393/__lsf/cdn/shopifycloud/polaris.js")
///     .partner_origin("http://localhost:9393");
/// assert!(!simulator.is_shopify());
/// assert!(ShopifyEndpoints::new().is_shopify());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShopifyEndpoints {
    /// Where the Admin API and OAuth requests go, in place of `https://{shop}`.
    pub admin_origin: Option<String>,
    /// The URL of the App Bridge script, in place of [`SHOPIFY_APP_BRIDGE`].
    pub app_bridge: Option<String>,
    /// The URL of the Polaris script, in place of [`SHOPIFY_POLARIS`].
    pub polaris: Option<String>,
    /// Where the Partner API requests go, in place of [`SHOPIFY_PARTNER_ORIGIN`].
    pub partner_origin: Option<String>,
}

impl ShopifyEndpoints {
    /// Shopify's, for every field.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sends the Admin API, OAuth and the token requests of every shop to
    /// `origin`, such as `http://localhost:9393`, in place of `https://{shop}`.
    pub fn admin_origin(mut self, origin: impl Into<String>) -> Self {
        self.admin_origin = Some(origin.into());
        self
    }

    /// Loads App Bridge from `url` in place of Shopify's CDN.
    pub fn app_bridge(mut self, url: impl Into<String>) -> Self {
        self.app_bridge = Some(url.into());
        self
    }

    /// Loads Polaris from `url` in place of Shopify's CDN.
    pub fn polaris(mut self, url: impl Into<String>) -> Self {
        self.polaris = Some(url.into());
        self
    }

    /// Sends the Partner API requests to `origin`, such as
    /// `http://localhost:9393`, in place of `https://partners.shopify.com`.
    pub fn partner_origin(mut self, origin: impl Into<String>) -> Self {
        self.partner_origin = Some(origin.into());
        self
    }

    /// Each field of `self`, or of `fallback` where `self` sets none.
    pub fn or(self, fallback: Self) -> Self {
        Self {
            admin_origin: self.admin_origin.or(fallback.admin_origin),
            app_bridge: self.app_bridge.or(fallback.app_bridge),
            polaris: self.polaris.or(fallback.polaris),
            partner_origin: self.partner_origin.or(fallback.partner_origin),
        }
    }

    /// Whether every field is Shopify's: none is set.
    pub fn is_shopify(&self) -> bool {
        *self == Self::default()
    }

    /// The fields the environment sets: `SHOPIFY_ADMIN_ORIGIN`,
    /// `SHOPIFY_APP_BRIDGE_URL`, `SHOPIFY_POLARIS_URL` and
    /// `SHOPIFY_PARTNER_ORIGIN`. An empty value is not set.
    pub fn from_env() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Self {
            admin_origin: var(ADMIN_ORIGIN_VAR),
            app_bridge: var(APP_BRIDGE_VAR),
            polaris: var(POLARIS_VAR),
            partner_origin: var(PARTNER_ORIGIN_VAR),
        }
    }

    /// A page of the Shopify admin of `shop`, where a merchant is sent:
    /// `path` is what follows the admin of the store, such as
    /// `settings/payments`, `apps/my-app` or `""` for its home, and may carry
    /// a query string.
    ///
    /// On Shopify, `https://admin.shopify.com/store/{store}/{path}`, `store`
    /// being the shop without `.myshopify.com`; with an
    /// [`admin_origin`](Self::admin_origin), which serves every shop on one
    /// origin, `{admin_origin}/admin/{path}?shop={shop}`.
    ///
    /// ```
    /// use openshop::ShopifyEndpoints;
    ///
    /// assert_eq!(
    ///     ShopifyEndpoints::new().admin_url("demo.myshopify.com", "settings/payments"),
    ///     "https://admin.shopify.com/store/demo/settings/payments"
    /// );
    /// assert_eq!(
    ///     ShopifyEndpoints::new()
    ///         .admin_origin("http://localhost:9393")
    ///         .admin_url("demo.myshopify.com", "settings/payments"),
    ///     "http://localhost:9393/admin/settings/payments?shop=demo.myshopify.com"
    /// );
    /// ```
    pub fn admin_url(&self, shop: &str, path: &str) -> String {
        let path = path.trim_start_matches('/');
        let (path, fragment) = match path.split_once('#') {
            Some((path, fragment)) => (path, format!("#{fragment}")),
            None => (path, String::new()),
        };
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let path = path.trim_end_matches('/');
        let page = |base: String| {
            if path.is_empty() {
                base
            } else {
                format!("{base}/{path}")
            }
        };
        match &self.admin_origin {
            Some(origin) => {
                let shop: String = url::form_urlencoded::byte_serialize(shop.as_bytes()).collect();
                let query = if query.is_empty() {
                    String::new()
                } else {
                    format!("{query}&")
                };
                format!(
                    "{}?{query}shop={shop}{fragment}",
                    page(format!("{}/admin", origin.trim_end_matches('/')))
                )
            }
            None => {
                let store = shop.strip_suffix(".myshopify.com").unwrap_or(shop);
                let query = if query.is_empty() {
                    String::new()
                } else {
                    format!("?{query}")
                };
                format!(
                    "{}{query}{fragment}",
                    page(format!("https://admin.shopify.com/store/{store}"))
                )
            }
        }
    }

    /// The plan selection page of an app with Shopify's managed pricing, for
    /// `shop`: `charges/{app_handle}/pricing_plans` of its admin, as
    /// [`admin_url`](Self::admin_url) writes it. `app_handle` is the handle of
    /// the app on Shopify, which [`ShopScope::app_handle`](crate::ShopScope::app_handle)
    /// gives.
    ///
    /// ```
    /// use openshop::ShopifyEndpoints;
    ///
    /// assert_eq!(
    ///     ShopifyEndpoints::new().pricing_plans_url("demo.myshopify.com", "hide-paypal"),
    ///     "https://admin.shopify.com/store/demo/charges/hide-paypal/pricing_plans"
    /// );
    /// ```
    pub fn pricing_plans_url(&self, shop: &str, app_handle: &str) -> String {
        self.admin_url(shop, &format!("charges/{app_handle}/pricing_plans"))
    }

    /// Checks the fields the code sets, as `build()` does.
    pub(crate) fn check(&self) -> Result<()> {
        Endpoints::resolve(self, &Self::default()).map(drop)
    }
}

/// The address of a script, checked: absolute, `https://`, or `http://` to
/// this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScriptUrl {
    url: String,
    origin: String,
}

impl ScriptUrl {
    /// Checks `url` and writes it in full. An error does not show `url`,
    /// which may hold a password.
    fn new(url: &str) -> Result<Self, String> {
        let invalid = |reason: &str| {
            format!("invalid script URL, expected an absolute `https://` URL (or `http://` to localhost): {reason}")
        };
        let parsed = url::Url::parse(url.trim()).map_err(|_| invalid("not an absolute URL"))?;
        let secure = match parsed.scheme() {
            "https" => true,
            "http" => false,
            _ => return Err(invalid("the scheme is not `http` or `https`")),
        };
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(invalid("it has a user or a password"));
        }
        if parsed.fragment().is_some() {
            return Err(invalid("it has a fragment"));
        }
        let loopback = match parsed.host() {
            Some(url::Host::Domain(host)) => host == "localhost",
            Some(url::Host::Ipv4(address)) => address == std::net::Ipv4Addr::LOCALHOST,
            Some(url::Host::Ipv6(address)) => address == std::net::Ipv6Addr::LOCALHOST,
            None => return Err(invalid("it has no host")),
        };
        if !secure && !loopback {
            return Err(invalid(
                "`http://` is taken for `localhost`, `127.0.0.1` or `[::1]` only: use `https://`",
            ));
        }
        Ok(Self {
            origin: parsed.origin().ascii_serialization(),
            url: parsed.into(),
        })
    }

    /// The URL, written in full.
    pub(crate) fn as_str(&self) -> &str {
        &self.url
    }

    /// `scheme://host[:port]`: what a policy allows it by.
    pub(crate) fn origin(&self) -> &str {
        &self.origin
    }
}

/// Where a value comes from: the code, or a variable of the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Code(&'static str),
    Env(&'static str),
}

impl Source {
    /// What an error or the startup line calls it by.
    fn name(self) -> String {
        match self {
            Self::Code(field) => format!("shopify_endpoints.{field}"),
            Self::Env(var) => var.to_string(),
        }
    }

    /// The value of the field from the code, else from the environment.
    fn pick<'a>(
        code: &'a Option<String>,
        env: &'a Option<String>,
        field: &'static str,
        var: &'static str,
    ) -> Option<(&'a str, Self)> {
        match (code, env) {
            (Some(value), _) => Some((value, Self::Code(field))),
            (None, Some(value)) => Some((value, Self::Env(var))),
            (None, None) => None,
        }
    }

    /// The error of a value that is not valid, naming where it comes from.
    fn invalid(self, reason: impl std::fmt::Display) -> anyhow::Error {
        match self {
            Self::Code(_) => anyhow!("[openshop] Invalid config: {} is not valid: {reason}", self.name()),
            Self::Env(_) => anyhow!("[openshop] {} is not valid: {reason}", self.name()),
        }
    }
}

/// The endpoints a running application uses: each field of the code, else of
/// the environment, checked.
#[derive(Debug, Clone, Default)]
pub(crate) struct Endpoints {
    pub(crate) admin_origin: Option<AdminOrigin>,
    pub(crate) app_bridge: Option<ScriptUrl>,
    pub(crate) polaris: Option<ScriptUrl>,
    /// `scheme://host[:port]`, without a trailing `/`.
    pub(crate) partner_origin: Option<String>,
    /// The same, as [`OpenShop::shopify_endpoints`](crate::OpenShop::shopify_endpoints) shows them.
    pub(crate) shown: ShopifyEndpoints,
    /// What the startup line says of the ones that are not Shopify's.
    replaced: Vec<String>,
}

impl Endpoints {
    /// Each field of `code`, else of `env`, checked. Fails on the first one
    /// that is not valid, naming the setting or the variable it comes from.
    pub(crate) fn resolve(code: &ShopifyEndpoints, env: &ShopifyEndpoints) -> Result<Self> {
        let mut endpoints = Self::default();
        if let Some((value, source)) =
            Source::pick(&code.admin_origin, &env.admin_origin, "admin_origin", ADMIN_ORIGIN_VAR)
        {
            let origin = AdminOrigin::new(value).map_err(|error| match error {
                shopify_api::Error::Config(reason) => source.invalid(reason),
                other => source.invalid(other),
            })?;
            endpoints.shown.admin_origin = Some(origin.as_str().to_string());
            endpoints
                .replaced
                .push(format!("Admin API and OAuth at {origin} ({})", source.name()));
            endpoints.admin_origin = Some(origin);
        }
        let script = |code: &Option<String>, env: &Option<String>, field, var| -> Result<Option<(ScriptUrl, Source)>> {
            Source::pick(code, env, field, var)
                .map(|(value, source)| {
                    ScriptUrl::new(value)
                        .map(|url| (url, source))
                        .map_err(|reason| source.invalid(reason))
                })
                .transpose()
        };
        if let Some((url, source)) = script(&code.app_bridge, &env.app_bridge, "app_bridge", APP_BRIDGE_VAR)? {
            endpoints.shown.app_bridge = Some(url.as_str().to_string());
            endpoints
                .replaced
                .push(format!("App Bridge from {} ({})", url.as_str(), source.name()));
            endpoints.app_bridge = Some(url);
        }
        if let Some((url, source)) = script(&code.polaris, &env.polaris, "polaris", POLARIS_VAR)? {
            endpoints.shown.polaris = Some(url.as_str().to_string());
            endpoints
                .replaced
                .push(format!("Polaris from {} ({})", url.as_str(), source.name()));
            endpoints.polaris = Some(url);
        }
        if let Some((value, source)) = Source::pick(
            &code.partner_origin,
            &env.partner_origin,
            "partner_origin",
            PARTNER_ORIGIN_VAR,
        ) {
            // The rules of the admin origin: an origin, `https://`, or `http://` to this machine.
            let origin = AdminOrigin::new(value).map_err(|error| match error {
                shopify_api::Error::Config(reason) => {
                    source.invalid(reason.replacen("invalid admin origin", "invalid Partner API origin", 1))
                }
                other => source.invalid(other),
            })?;
            let origin = origin.as_str().to_string();
            endpoints.shown.partner_origin = Some(origin.clone());
            endpoints
                .replaced
                .push(format!("Partner API at {origin} ({})", source.name()));
            endpoints.partner_origin = Some(origin);
        }
        Ok(endpoints)
    }

    /// Where the Partner API is: the `partner_origin`, else Shopify's.
    pub(crate) fn partner_origin(&self) -> &str {
        self.partner_origin.as_deref().unwrap_or(SHOPIFY_PARTNER_ORIGIN)
    }

    /// The line the application logs at startup when an endpoint is not
    /// Shopify's: which ones, where, and from what setting.
    pub(crate) fn startup_line(&self) -> Option<String> {
        (!self.replaced.is_empty())
            .then(|| format!("[openshop] Shopify endpoints replaced: {}", self.replaced.join("; ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIMULATOR: &str = "http://localhost:9393";

    fn env(admin_origin: Option<&str>, app_bridge: Option<&str>, polaris: Option<&str>) -> ShopifyEndpoints {
        ShopifyEndpoints {
            admin_origin: admin_origin.map(Into::into),
            app_bridge: app_bridge.map(Into::into),
            polaris: polaris.map(Into::into),
            partner_origin: None,
        }
    }

    #[test]
    fn nothing_set_is_shopify() {
        let endpoints = Endpoints::resolve(&ShopifyEndpoints::new(), &ShopifyEndpoints::new()).unwrap();
        assert!(endpoints.admin_origin.is_none() && endpoints.app_bridge.is_none() && endpoints.polaris.is_none());
        assert!(endpoints.shown.is_shopify());
        assert_eq!(endpoints.partner_origin(), "https://partners.shopify.com");
        assert_eq!(endpoints.startup_line(), None);
    }

    #[test]
    fn the_partner_api_is_an_origin_of_the_code_else_of_the_environment() {
        let env = ShopifyEndpoints::new().partner_origin("https://partners-proxy.example.com");
        let endpoints = Endpoints::resolve(&ShopifyEndpoints::new(), &env).unwrap();
        assert_eq!(endpoints.partner_origin(), "https://partners-proxy.example.com");
        assert_eq!(
            endpoints.startup_line().unwrap(),
            "[openshop] Shopify endpoints replaced: Partner API at https://partners-proxy.example.com \
             (SHOPIFY_PARTNER_ORIGIN)"
        );

        let code = ShopifyEndpoints::new().partner_origin("HTTP://LOCALHOST:9393/");
        let endpoints = Endpoints::resolve(&code, &env).unwrap();
        // Written as an origin: lower case, no trailing `/`.
        assert_eq!(endpoints.partner_origin(), "http://localhost:9393");
        assert_eq!(
            endpoints.shown,
            ShopifyEndpoints::new().partner_origin("http://localhost:9393")
        );
        assert!(
            endpoints
                .startup_line()
                .unwrap()
                .contains("(shopify_endpoints.partner_origin)")
        );

        let error =
            |code: ShopifyEndpoints, env: ShopifyEndpoints| Endpoints::resolve(&code, &env).unwrap_err().to_string();
        assert_eq!(
            error(
                ShopifyEndpoints::new().partner_origin("http://partners.example.com"),
                ShopifyEndpoints::new()
            ),
            "[openshop] Invalid config: shopify_endpoints.partner_origin is not valid: invalid Partner API \
             origin, expected `https://host[:port]` (or `http://` to localhost): `http://` is taken for \
             `localhost`, `127.0.0.1` or `[::1]` only: use `https://`"
        );
        let from_env = error(
            ShopifyEndpoints::new(),
            ShopifyEndpoints::new().partner_origin("https://user:prtapi_secret@partners.shopify.com/1234567"),
        );
        assert!(
            from_env.starts_with("[openshop] SHOPIFY_PARTNER_ORIGIN is not valid: invalid Partner API origin"),
            "{from_env}"
        );
        assert!(!from_env.contains("prtapi_secret"), "{from_env}");
    }

    #[test]
    fn each_field_comes_from_the_code_else_from_the_environment() {
        let code = ShopifyEndpoints::new()
            .admin_origin(format!("{SIMULATOR}/"))
            .polaris("https://polaris.example.com/polaris.js?v=2&x=1");
        let env = env(
            Some("https://proxy.example.com"),
            Some("http://127.0.0.1:9393/app-bridge.js"),
            None,
        );
        let endpoints = Endpoints::resolve(&code, &env).unwrap();
        // The code's admin origin, written without its `/`.
        assert_eq!(
            endpoints.admin_origin.as_ref().map(AdminOrigin::as_str),
            Some(SIMULATOR)
        );
        // No App Bridge in the code: the environment's.
        assert_eq!(
            endpoints.app_bridge.as_ref().map(ScriptUrl::as_str),
            Some("http://127.0.0.1:9393/app-bridge.js")
        );
        assert_eq!(
            endpoints.app_bridge.as_ref().map(ScriptUrl::origin),
            Some("http://127.0.0.1:9393")
        );
        assert_eq!(
            endpoints.polaris.as_ref().map(ScriptUrl::origin),
            Some("https://polaris.example.com")
        );
        assert_eq!(
            endpoints.shown,
            ShopifyEndpoints::new()
                .admin_origin(SIMULATOR)
                .app_bridge("http://127.0.0.1:9393/app-bridge.js")
                .polaris("https://polaris.example.com/polaris.js?v=2&x=1")
        );
        assert_eq!(
            endpoints.startup_line().unwrap(),
            "[openshop] Shopify endpoints replaced: Admin API and OAuth at http://localhost:9393 \
             (shopify_endpoints.admin_origin); App Bridge from http://127.0.0.1:9393/app-bridge.js \
             (SHOPIFY_APP_BRIDGE_URL); Polaris from https://polaris.example.com/polaris.js?v=2&x=1 \
             (shopify_endpoints.polaris)"
        );

        // A field of the code wins over the environment for that field only.
        let env_only = Endpoints::resolve(&ShopifyEndpoints::new(), &env.clone()).unwrap();
        assert_eq!(env_only.shown, env.clone().admin_origin("https://proxy.example.com"));
    }

    #[test]
    fn admin_pages_are_shopify_s_or_at_the_admin_origin() {
        let shopify = ShopifyEndpoints::new();
        let local = ShopifyEndpoints::new().admin_origin("http://localhost:9393/");
        let shop = "demo-shop.myshopify.com";

        assert_eq!(shopify.admin_url(shop, ""), "https://admin.shopify.com/store/demo-shop");
        assert_eq!(
            shopify.admin_url(shop, "/apps/hide-paypal/"),
            "https://admin.shopify.com/store/demo-shop/apps/hide-paypal"
        );
        assert_eq!(
            shopify.admin_url(shop, "orders?query=open#top"),
            "https://admin.shopify.com/store/demo-shop/orders?query=open#top"
        );
        assert_eq!(
            local.admin_url(shop, ""),
            "http://localhost:9393/admin?shop=demo-shop.myshopify.com"
        );
        assert_eq!(
            local.admin_url(shop, "orders?query=open#top"),
            "http://localhost:9393/admin/orders?query=open&shop=demo-shop.myshopify.com#top"
        );

        assert_eq!(
            shopify.pricing_plans_url(shop, "hide-paypal"),
            "https://admin.shopify.com/store/demo-shop/charges/hide-paypal/pricing_plans"
        );
        assert_eq!(
            local.pricing_plans_url(shop, "hide-paypal"),
            "http://localhost:9393/admin/charges/hide-paypal/pricing_plans?shop=demo-shop.myshopify.com"
        );
    }

    #[test]
    fn or_keeps_the_fields_that_are_set() {
        let declared = ShopifyEndpoints::new()
            .admin_origin("https://a.example")
            .polaris("https://a.example/p.js");
        let startup = ShopifyEndpoints::new().admin_origin(SIMULATOR);
        assert_eq!(
            startup.or(declared),
            ShopifyEndpoints::new()
                .admin_origin(SIMULATOR)
                .polaris("https://a.example/p.js")
        );
    }

    #[test]
    fn scripts_are_https_or_on_this_machine() {
        for (url, written) in [
            (
                "https://cdn.example.com/app-bridge.js",
                "https://cdn.example.com/app-bridge.js",
            ),
            (
                "http://localhost:9393/__lsf/admin/app-bridge.js",
                "http://localhost:9393/__lsf/admin/app-bridge.js",
            ),
            ("http://127.0.0.1:9393/a.js", "http://127.0.0.1:9393/a.js"),
            ("http://[::1]:9393/a.js", "http://[::1]:9393/a.js"),
            ("HTTPS://Example.COM/a b.js?q=1", "https://example.com/a%20b.js?q=1"),
        ] {
            assert_eq!(ScriptUrl::new(url).unwrap().as_str(), written, "{url}");
        }
        for (url, reason) in [
            ("/__lsf/admin/app-bridge.js", "not an absolute URL"),
            ("localhost:9393/a.js", "the scheme is not `http` or `https`"),
            ("ftp://example.com/a.js", "the scheme is not `http` or `https`"),
            ("http://example.com/a.js", "`http://` is taken for"),
            ("http://127.0.0.2/a.js", "`http://` is taken for"),
            ("https://user:hunter2@example.com/a.js", "it has a user or a password"),
            ("https://example.com/a.js#x", "it has a fragment"),
        ] {
            let error = ScriptUrl::new(url).unwrap_err();
            assert!(error.contains(reason), "{url}: {error}");
            assert!(!error.contains("hunter2"), "{error}");
        }
    }

    #[test]
    fn an_invalid_value_names_its_setting_or_its_variable() {
        let error =
            |code: ShopifyEndpoints, env: ShopifyEndpoints| Endpoints::resolve(&code, &env).unwrap_err().to_string();
        assert_eq!(
            error(
                ShopifyEndpoints::new().admin_origin("http://localhost:9393/admin"),
                ShopifyEndpoints::new()
            ),
            "[openshop] Invalid config: shopify_endpoints.admin_origin is not valid: invalid admin origin, \
             expected `https://host[:port]` (or `http://` to localhost): it has a path: give the scheme, the \
             host and the port only"
        );
        let from_env = error(
            ShopifyEndpoints::new(),
            env(Some("http://user:shpat_secret@localhost:9393"), None, None),
        );
        assert!(
            from_env.starts_with("[openshop] SHOPIFY_ADMIN_ORIGIN is not valid: invalid admin origin"),
            "{from_env}"
        );
        assert!(!from_env.contains("shpat_secret"), "{from_env}");
        assert!(
            error(
                ShopifyEndpoints::new(),
                env(None, Some("http://example.com/a.js"), None)
            )
            .starts_with("[openshop] SHOPIFY_APP_BRIDGE_URL is not valid: invalid script URL")
        );
        assert!(
            error(ShopifyEndpoints::new().polaris("polaris.js"), ShopifyEndpoints::new())
                .starts_with("[openshop] Invalid config: shopify_endpoints.polaris is not valid: invalid script URL")
        );
        // A value of the code that is valid hides the environment's that is not.
        assert!(
            Endpoints::resolve(
                &ShopifyEndpoints::new().admin_origin(SIMULATOR),
                &env(Some("not an origin"), None, None)
            )
            .is_ok()
        );
    }
}
