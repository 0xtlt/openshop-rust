# Configure Shopify apps

Use this guide after creating an OpenShop project. For local linking and your
first installation, follow the [first-app tutorial](../tutorials/first-app.md).

## 1. Choose the deployment shape

Use single-app mode for one Shopify client ID. Use explicit multi-app mode when
one deployment must serve several Shopify apps with the same scopes and app code.
See [Shop and app isolation](../concepts/shop-isolation.md) for the data boundary.

### One Shopify app

Leave `ShopifyConfig::apps` unset, which is the default. Supply these through the
production process environment:

```dotenv
SHOPIFY_API_KEY=your-client-id
SHOPIFY_API_SECRET=your-client-secret
HOST=https://your-app.example.com
```

Set the requested scopes in the linked `shopify.app.toml` or in
`ShopifyConfig::scopes`. During local development, Shopify CLI supplies the app
credentials and tunnel URL when you run `shopify app dev`.

### Several Shopify apps

Link a TOML file for each Shopify app with Shopify CLI, then register the files
under stable handles where the application is declared. Add the `use` line and
the `shopify` function, and set the `shopify` setting of `app!`:

```rust,ignore
use openshop::{Context, IndexMap, Result, ShopifyAppConfig, ShopifyConfig};

fn shopify() -> Result<ShopifyConfig> {
    let secret = |name: &str| std::env::var(name).with_context(|| format!("{name} is not set"));

    Ok(ShopifyConfig {
        scopes: Some("read_products,read_orders".to_string()),
        apps: Some(IndexMap::from([
            (
                "retail".to_string(),
                ShopifyAppConfig::Toml {
                    toml: "shopify.app.retail.toml".to_string(),
                    api_secret: secret("SHOPIFY_RETAIL_API_SECRET")?,
                    app_url: None,
                },
            ),
            (
                "wholesale".to_string(),
                ShopifyAppConfig::Toml {
                    toml: "shopify.app.wholesale.toml".to_string(),
                    api_secret: secret("SHOPIFY_WHOLESALE_API_SECRET")?,
                    app_url: None,
                },
            ),
        ])),
    })
}

pub static APP: App = app! {
    shopify: shopify()?,
    // ... providers, flows and other entries ...
};
```

Provide each secret through the process environment. A setting of `app!` can
fail with `?`: when a secret is missing, the command stops at startup with
`SHOPIFY_RETAIL_API_SECRET is not set`.

During development the secrets can live in `.env`: the settings of `app!` are
evaluated when `APP.run()` builds the configuration, and `dev` loads `.env`
before that. The other commands never load `.env`: export the secrets in the
environment of `start`, `worker`, and `migrate`.

Keep API keys unique and use each app's own secret. Include the TOML files in
the deployed project and run the binary from the directory that contains them;
OpenShop reads their client IDs and URLs at startup, and a missing file or
`client_id` stops the process. Each `application_url` should point to the
public OpenShop origin.

If your platform manages the client IDs directly, replace a `Toml` entry with
`Keys`:

```rust,ignore
ShopifyAppConfig::Keys {
    api_key: secret("SHOPIFY_RETAIL_API_KEY")?,
    api_secret: secret("SHOPIFY_RETAIL_API_SECRET")?,
    app_url: Some("https://your-app.example.com".to_string()),
}
```

Use either `Toml` or `Keys` per entry. Keep global scopes identical for all apps.
The [configuration reference](../reference/configuration.md#shopify) lists every
field and its resolution rules.

## 2. Deploy the Shopify configuration

Deploying the OpenShop server does not update Shopify's app configuration.
Release each relevant TOML with Shopify CLI:

```bash
shopify app deploy --config shopify.app.retail.toml
shopify app deploy --config shopify.app.wholesale.toml
```

For single-app mode, use the linked single-app config instead. Verify application
and callback URLs, webhook subscriptions, and app proxy settings. Requested scope
changes may need Shopify approval and renewed authorization by the merchant.

## 3. Verify each installation

For multiple apps, start installation with an explicit app handle:

```text
https://your-app.example.com/auth?shop=example.myshopify.com&app=retail
```

For each configured app, complete installation, open its embedded admin, save its
provider configuration, and run a harmless flow. Confirm the credentials and runs
appear only in the intended app/shop context.

If authentication fails, compare the selected client ID, secret, public URL, and
handle with [Authentication](../reference/authentication.md) and
[Troubleshooting](troubleshooting.md).
