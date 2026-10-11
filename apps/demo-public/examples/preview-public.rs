//! Previews the public app demo in a regular browser, without a Shopify store.
//!
//! ```bash
//! DATABASE_URL=postgresql://openshop:openshop@localhost:5432/openshop \
//!   cargo run -p openshop-demo-public --example preview-public
//! ```
//!
//! The demo runs on `http://localhost:3998`. `apps/preview` is what serves it:
//! see there what a browser gets, and `PREVIEW_AS_SHOP` and `PREVIEW_UI_DIR`.
//! The admin of this demo is made of its own pages: as a shop, preview it with
//! its own bundle.
//!
//! `mise run dev:public` is the preview as a shop, run by `openshop dev`:
//! started again when the workspace changes, with the demo's own bundle built
//! again by trunk, and a page that reloads by itself.

#[path = "../tests/common/mod.rs"]
mod common;

use openshop::{Result, json};
use openshop_demo_preview::Preview;

/// A week of what a public app leaves behind: the shops that installed it,
/// one that left and one that never finished, the hourly check of each with
/// its step and its log lines, an hour when Shopify failed most of them, a
/// cron a shop turned off. What the operator dashboard is for.
const HISTORY: &str = r#"
INSERT INTO installations (shop, access_token, scopes, installed_at, uninstalled_at) VALUES
  ('atelier-nord.myshopify.com', 'preview-token', 'read_products', now() - interval '40 days', NULL),
  ('maison-lune.myshopify.com', 'preview-token', 'read_products', now() - interval '21 days', NULL),
  ('kiosque-vert.myshopify.com', 'preview-token', 'read_products', now() - interval '9 days', NULL),
  ('la-fabrique.myshopify.com', 'preview-token', 'read_products', now() - interval '3 days', NULL),
  ('ancien-comptoir.myshopify.com', NULL, 'read_products', now() - interval '60 days', now() - interval '12 days'),
  ('en-cours.myshopify.com', NULL, NULL, now() - interval '2 hours', NULL);

INSERT INTO stock_settings (shop, threshold) VALUES
  ('atelier-nord.myshopify.com', 10),
  ('kiosque-vert.myshopify.com', 3);

INSERT INTO flow_runs (shop, flow_name, status, input, error, attempts, started_at, completed_at, created_at)
SELECT shop, 'checkStock',
       CASE WHEN failed THEN 'failed' ELSE 'completed' END,
       '{}'::json,
       CASE WHEN failed THEN (ARRAY[
         'Shopify throttled the request',
         'Shopify answered 503 Service Unavailable',
         'Shopify timed out after 30 s'
       ])[1 + floor(pick * 3)::int] END,
       CASE WHEN failed THEN 3 ELSE 1 END,
       ts, ts + make_interval(secs => 0.3 + took * 4), ts
FROM (
  SELECT hour + make_interval(secs => random() * 20) AS ts, shop,
         random() AS pick, random() AS took, random() AS chance
  FROM generate_series(date_trunc('hour', now()) - interval '7 days', now() - interval '2 minutes', interval '1 hour') AS hour
  JOIN installations ON access_token IS NOT NULL AND shop <> 'preview.myshopify.com' AND installed_at <= hour
) candidates
CROSS JOIN LATERAL (
  SELECT chance < CASE
    WHEN ts BETWEEN now() - interval '30 hours' AND now() - interval '27 hours' THEN 0.55
    WHEN shop = 'kiosque-vert.myshopify.com' THEN 0.18
    ELSE 0.03
  END AS failed
) outcome;

INSERT INTO step_results (flow_run_id, step_name, attempt, status, output, error, duration_ms, created_at)
SELECT id::text, 'find-low-stock', attempts, status,
       CASE WHEN status = 'completed' THEN '[{"name":"Linen shirt - M","sku":"SHIRT-M","available":2}]'::json END,
       error, 240, created_at
  FROM flow_runs WHERE shop <> 'preview.myshopify.com';

INSERT INTO logs (flow_run_id, level, message, payload, created_at)
SELECT id::text, 'info', 'Flow "checkStock" started', NULL::json, created_at
  FROM flow_runs WHERE shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'info', 'Looking for variants that run low...', '{"threshold":5}'::json, created_at + interval '100 milliseconds'
  FROM flow_runs WHERE shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'warn', 'Retrying: ' || error, NULL::json, created_at + interval '1 second'
  FROM flow_runs WHERE status = 'failed' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'error', 'Flow "checkStock" failed: ' || error, NULL::json, completed_at
  FROM flow_runs WHERE status = 'failed' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'info', '1 variants run low', '{"count":1}'::json, completed_at
  FROM flow_runs WHERE status = 'completed' AND shop <> 'preview.myshopify.com'
UNION ALL
SELECT id::text, 'info', 'Flow "checkStock" completed', NULL::json, completed_at
  FROM flow_runs WHERE status = 'completed' AND shop <> 'preview.myshopify.com';

INSERT INTO cron_overrides (shop, cron_key, enabled) VALUES
  ('la-fabrique.myshopify.com', 'checkStock:0 * * * *', false);
"#;

fn main() -> Result<()> {
    Preview {
        config: openshop_demo_public::APP.config()?,
        port: 3998,
        shopify: common::graphql_fixture,
        // A check of the shop of the preview: what its home shows.
        runs: vec![("checkStock", json!({}))],
        history: HISTORY,
    }
    .run()
}
