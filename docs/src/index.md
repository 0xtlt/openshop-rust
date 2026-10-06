# OpenShop documentation

OpenShop is a Rust framework for Shopify integrations. Write the code that
moves data between Shopify and your services; OpenShop provides the embedded
admin UI, saved provider credentials, background workers, schedules, and run logs.

> **Beta.** APIs, generated files, and workflows may change before `1.0`. These
> docs describe the current repository. Check [versioning](reference/versioning.md)
> when working with an older version.

## Find the help you need

| Section | What you will find |
| --- | --- |
| [Tutorials](tutorials/index.md) | Learn by building an app and observing a checkpointed flow. |
| [How-to guides](guides/index.md) | Connect services, add endpoints, test, deploy, and recover failed runs. |
| [Reference](reference/index.md) | Look up configuration, CLI commands, APIs, defaults, and constraints. |
| [Explanation](concepts/index.md) | Understand the runtime, retries, and shop isolation. |

## New to OpenShop?

[Build your first app](tutorials/first-app.md) to create a project, install it on a
Shopify development store, and run its sample integration. You will need Rust
1.88 or newer, PostgreSQL 17, and Shopify CLI.

A typical integration has three parts:

| Part | What you write | What OpenShop provides |
| --- | --- | --- |
| **Provider** | Methods for calling an external service, such as a warehouse | A configuration form and saved credentials for each shop |
| **Flow** | A background job, split into named steps | Queueing, checkpoints, retries, and execution logs |
| **App configuration** | The providers, flows, and schedules to enable | An embedded Shopify admin to configure and operate them |

For example, an order-sync flow reads orders from Shopify and calls a warehouse
provider. A merchant configures the warehouse credentials in **Providers** and
inspects each execution in **Flows**. See [the building blocks](concepts/building-blocks.md)
for how the source files and runtime objects connect.

## Common tasks

- **Connect an external service:** [Add a provider](guides/define-provider.md).
- **Run background work:** [Sync orders with a flow](guides/define-flow.md).
- **Extend the app:** [Add a storefront endpoint](guides/add-proxy-routes.md),
  [a server endpoint](guides/add-server-route.md), or
  [a custom admin page](guides/add-admin-page.md).
- **Ship an app:** [Deploy to production](guides/deploy-production.md), then
  [monitor and recover runs](guides/operate-app.md).
- **Fix a problem:** Start with [troubleshooting](guides/troubleshooting.md).

Coming from the TypeScript version of OpenShop? Read
[Differences from the TypeScript version](reference/differences.md).

Contributing to the framework or these docs? See the
[contributor guide](https://github.com/0xtlt/openshop-rust/blob/main/CONTRIBUTING.md).
