-- The runs a run started (`parent_run_id`), which the page of a run lists.
CREATE INDEX IF NOT EXISTS flow_runs_parent_run_idx ON flow_runs (parent_run_id)
  WHERE parent_run_id IS NOT NULL;
