-- One row per firing of a cron for a shop. The process that inserts the row
-- queues the run; the others, which compute the same occurrence, find it taken.
-- Rows older than a day are removed by the scheduler.
CREATE TABLE IF NOT EXISTS cron_ticks (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text NOT NULL,
  shop text NOT NULL,
  cron_key text NOT NULL,
  scheduled_at timestamp with time zone NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS cron_ticks_app_shop_cron_tick_unique ON cron_ticks (app_handle, shop, cron_key, scheduled_at);
CREATE INDEX IF NOT EXISTS cron_ticks_scheduled_at_idx ON cron_ticks (scheduled_at);
