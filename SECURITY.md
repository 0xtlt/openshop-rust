# Security policy

OpenShop handles Shopify access tokens, provider secrets encrypted with
`ENCRYPTION_KEY`, OAuth and session tokens. A flaw in any of them can expose a
merchant's store, so please report it privately.

## Reporting a vulnerability

Open a private advisory from the **Security** tab of the repository
(**Report a vulnerability**). Do not open a public issue, pull request or
discussion for it.

Include what you can of:

- the version or commit, and the template the app was made from;
- what an attacker can do, and what they need first (a session, a shop, network
  access);
- the steps or a test that reproduces it.

You get an answer within 7 days. Once a fix is ready, it is released and the
advisory is published, crediting you unless you prefer otherwise.

## Supported versions

OpenShop is a v0 (see `docs/src/reference/versioning.md`): only the latest
commit of `main` gets fixes.
