-- The concurrency key of a run (`DispatchOptions::concurrency_key`, or the
-- `concurrency_key` of its flow): a run is refused while another active run of
-- the same flow, for the same shop, has the same key.
-- Rust only: the TypeScript runtime neither reads nor writes it.
ALTER TABLE flow_runs ADD COLUMN IF NOT EXISTS concurrency_key text;

-- What a dispatch with a key looks up, under the lock of the flow for the shop.
CREATE INDEX IF NOT EXISTS flow_runs_concurrency_key_idx ON flow_runs (app_handle, shop, flow_name, concurrency_key)
  WHERE status IN ('pending', 'sleeping', 'running') AND concurrency_key IS NOT NULL;
