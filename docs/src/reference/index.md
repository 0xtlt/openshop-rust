# Reference

Use reference pages when you know which feature you need and want its exact
contract. For a complete procedure, start with the [how-to guides](../guides/index.md).

## Project and configuration

- [Project structure](project-structure.md) — app-owned and generated files.
- [Configuration](configuration.md) — the configuration builder, Shopify apps, crons, workers, and admin visibility.
- [Environment variables](environment-variables.md) — defaults, requirements, and resolution order.
- [CLI commands](cli.md) — commands, flags, and process behavior.
- [Public SDK](sdk.md) — crate entry points, exports, and the Shopify GraphQL client.

## Background work and storage

- [Flows](flows.md) — inputs, runtime context, checkpoints, retries, and cancellation.
- [Providers](providers.md) — fields, validation, connectors, and credentials.
- [Database and migrations](database.md) — connection, models, framework tables, and schema ownership.
- [Logging](logging.md) — structured logs, the log query syntax, and export.
- [Errors](errors.md) — framework error types and failure contracts.
- [Testing](testing.md) — test contexts, fakes, signed requests, and factories.

## Shopify and HTTP

- [Authentication](authentication.md) — credentials and identity for each HTTP surface.
- [Admin API](admin-api.md) — authenticated endpoint contracts.
- [Proxy routes](proxy-routes.md) — storefront and Customer Account endpoints.
- [Public server routes](server-routes.md) — explicit authentication and `Response` handlers.
- [Webhooks](webhooks.md) — topic matching, context, and acknowledgment behavior.
- [Shopify Functions](shopify-functions.md) — Function definitions and configuration.
- [Custom admin pages](custom-admin-pages.md) — experimental pages, loaders, actions, and policies.
- [MCP](mcp.md) — tokens, permissions, tools, resources, and JSON-RPC errors.

## Security and compatibility

- [Security](security.md) — secret handling and trust boundaries.
- [Versioning](versioning.md) — beta compatibility and generated project updates.
- [Differences from the TypeScript version](differences.md) — where this implementation intentionally differs from OpenShop for TypeScript.
