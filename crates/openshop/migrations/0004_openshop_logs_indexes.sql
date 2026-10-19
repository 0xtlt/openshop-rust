-- The operator dashboard reads the latest log lines of every run, and the
-- retention purge removes the oldest. Both go through these indexes. They
-- only add: the columns and their types stay as the TypeScript runtime
-- reads and writes them. A migration runs in a transaction, which excludes
-- CREATE INDEX CONCURRENTLY; on a large table, create the indexes by hand
-- that way first (the IF NOT EXISTS below then leaves them alone).

-- The latest lines over a period, and the lines the purge removes.
CREATE INDEX IF NOT EXISTS logs_created_at_idx ON logs (created_at);

-- The same for one level.
CREATE INDEX IF NOT EXISTS logs_level_created_at_idx ON logs (level, created_at);

-- A line names its run by the text of its id (`logs.flow_run_id` is text), so
-- the dashboard joins on `flow_runs.id::text`: this index lets that join
-- find the run of a line.
CREATE INDEX IF NOT EXISTS flow_runs_id_text_idx ON flow_runs ((id::text));
