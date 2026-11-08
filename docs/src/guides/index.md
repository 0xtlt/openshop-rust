# How-to guides

These guides assume you already have an OpenShop project. If you are starting
from an empty directory, follow the [first-app tutorial](../tutorials/first-app.md).
Run app commands from the project root, the directory containing `Cargo.toml`
and `src/main.rs`.

## Build an integration

| I want to… | Guide |
| --- | --- |
| Configure one or several Shopify apps | [Configure Shopify apps](configure-shopify-apps.md) |
| Save credentials and call an external API | [Connect an external service](define-provider.md) |
| Send Shopify orders to a provider on a schedule | [Sync orders with a flow](define-flow.md) |
| Respond to Shopify events | [Handle a Shopify webhook](handle-webhooks.md) |

## Extend the app

| I want to… | Guide |
| --- | --- |
| Serve a storefront or Customer Account extension | [Add proxy routes](add-proxy-routes.md) |
| Expose an endpoint to another service | [Add a server route](add-server-route.md) |
| Add a screen inside Shopify admin | [Add a custom admin page](add-admin-page.md) |
| Charge for the app with plans of the Partner Dashboard | [Charge with managed pricing](charge-with-managed-pricing.md) |
| Verify behavior before shipping | [Test an app](test-app.md) |
| Run against a simulator of Shopify, a proxy, or an API of your own | [Use custom Shopify endpoints](custom-shopify-endpoints.md) |

## Deploy and operate

| I want to… | Guide |
| --- | --- |
| Change the database schema | [Manage migrations](manage-migrations.md) |
| Run an app in production | [Deploy to production](deploy-production.md) |
| Monitor, retry, cancel, or scale work | [Operate an app](operate-app.md) |
| Diagnose a failure | [Troubleshooting](troubleshooting.md) |
| Update the framework version | [Upgrade OpenShop](upgrade.md) |

For all options and defaults, use the [reference](../reference/index.md).
