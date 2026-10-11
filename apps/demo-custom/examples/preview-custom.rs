//! Previews the custom app demo in a regular browser, without a Shopify store.
//!
//! ```bash
//! DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop \
//!   cargo run -p openshop-demo-custom --example preview-custom
//! ```
//!
//! The demo runs on `http://localhost:3999`. `apps/preview` is what serves it:
//! see there what a browser gets, and `PREVIEW_AS_SHOP` and `PREVIEW_UI_DIR`.
//!
//! `mise run dev:custom` is the preview as a shop, run by `openshop dev`:
//! started again when the workspace changes, with the demo's own bundle built
//! again by trunk, and a page that reloads by itself.

#[path = "../tests/common/mod.rs"]
mod common;

use openshop::{Result, json};
use openshop_demo_preview::Preview;

/// A week of what an application with several shops leaves behind: fictitious
/// installations, runs with their steps and their log lines, a provider whose
/// check fails, a cron a shop turned off. Enough for the operator dashboard to
/// have something to show.
const HISTORY: &str = r#"
INSERT INTO installations (shop, access_token, scopes, installed_at, uninstalled_at) VALUES
  ('atelier-nord.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '40 days', NULL),
  ('maison-lune.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '21 days', NULL),
  ('kiosque-vert.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '9 days', NULL),
  ('la-fabrique.myshopify.com', 'preview-token', 'read_products,read_orders', now() - interval '3 days', NULL),
  ('ancien-comptoir.myshopify.com', NULL, 'read_products', now() - interval '60 days', now() - interval '12 days'),
  ('en-cours.myshopify.com', NULL, NULL, now() - interval '2 hours', NULL);

INSERT INTO flow_runs (shop, flow_name, status, input, error, attempts, started_at, completed_at, created_at)
SELECT shop, flow,
       CASE WHEN failed THEN 'failed' ELSE 'completed' END,
       CASE WHEN flow = 'syncOrders' THEN '{"limit":50}'::json ELSE '{"productId":"gid://shopify/Product/1"}'::json END,
       CASE WHEN NOT failed THEN NULL
            WHEN flow = 'syncOrders' THEN (ARRAY[
              'Warehouse API answered 502 Bad Gateway',
              'Warehouse API timed out after 30 s',
              'Shopify throttled the request'
            ])[1 + floor(pick * 3)::int]
            ELSE 'Product not found' END,
       CASE WHEN failed THEN 3 ELSE 1 END,
       ts, ts + make_interval(secs => 0.4 + took * 6), ts
FROM (
  SELECT ts,
         (ARRAY[
           'atelier-nord.myshopify.com', 'atelier-nord.myshopify.com', 'atelier-nord.myshopify.com',
           'maison-lune.myshopify.com', 'maison-lune.myshopify.com',
           'kiosque-vert.myshopify.com', 'la-fabrique.myshopify.com'
         ])[1 + floor(random() * 7)::int] AS shop,
         CASE WHEN random() < 0.8 THEN 'syncOrders' ELSE 'countVariants' END AS flow,
         random() AS pick, random() AS took, random() AS chance, random() AS keep
  FROM generate_series(now() - interval '7 days', now() - interval '2 minutes', interval '90 seconds') AS ts
) candidates
CROSS JOIN LATERAL (
  SELECT chance < CASE
    WHEN ts BETWEEN now() - interval '30 hours' AND now() - interval '27 hours' THEN 0.55
    WHEN shop = 'kiosque-vert.myshopify.com' THEN 0.18
    ELSE 0.04
  END AS failed
) outcome
WHERE keep < 0.2 + 0.8 * greatest(0, sin((extract(hour FROM ts) - 5) * pi() / 14));

INSERT INTO step_results (flow_run_id, step_name, attempt, status, output, error, duration_ms, created_at)
SELECT id::text, 'fetch-orders', 1, 'completed', '{"count":12}'::json, NULL, 180, created_at
  FROM flow_runs WHERE flow_name = 'syncOrders' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'send-to-warehouse', attempts, status,
       CASE WHEN status = 'completed' THEN '{"accepted":12}'::json END, error, 420, created_at + interval '1 second'
  FROM flow_runs WHERE flow_name = 'syncOrders' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'count', attempts, status,
       CASE WHEN status = 'completed' THEN '{"variants":4}'::json END, error, 90, created_at
  FROM flow_runs WHERE flow_name = 'countVariants' AND shop <> 'preview.myshopify.com';

INSERT INTO logs (flow_run_id, level, message, payload, created_at)
SELECT id::text, 'info', 'Flow "' || flow_name || '" started', NULL::json, created_at
  FROM flow_runs WHERE shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'info', 'Fetched 12 orders', '{"count":12}'::json, created_at + interval '300 milliseconds'
  FROM flow_runs WHERE flow_name = 'syncOrders' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'warn', 'Retrying: ' || error, NULL::json, created_at + interval '1 second'
  FROM flow_runs WHERE status = 'failed' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'error', 'Flow "' || flow_name || '" failed: ' || error, NULL::json, completed_at
  FROM flow_runs WHERE status = 'failed' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'info', 'Flow "' || flow_name || '" completed', NULL::json, completed_at
  FROM flow_runs WHERE status = 'completed' AND shop <> 'preview.myshopify.com';

INSERT INTO provider_configs (shop, provider_name, config, last_checked_at, last_check_ok) VALUES
  ('atelier-nord.myshopify.com', 'warehouse', '{}', now() - interval '3 hours', true),
  ('kiosque-vert.myshopify.com', 'warehouse', '{}', now() - interval '1 hour', false),
  ('maison-lune.myshopify.com', 'warehouse', '{}', NULL, NULL);

INSERT INTO cron_overrides (shop, cron_key, enabled) VALUES
  ('la-fabrique.myshopify.com', 'syncOrders:0 2 * * *', false);
"#;

fn main() -> Result<()> {
    Preview {
        config: openshop_demo_custom::APP.config()?,
        port: 3999,
        shopify: common::graphql_fixture,
        // Two runs to look at, with their steps and their logs.
        runs: vec![("syncOrders", json!({ "limit": 5 })), ("countVariants", json!({}))],
        history: HISTORY,
    }
    .run()
}
