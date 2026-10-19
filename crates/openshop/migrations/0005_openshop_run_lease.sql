-- Names the claim of a run: a worker sets a new value every time it takes one,
-- and only the execution that holds it can record the outcome of the run.
-- Rust only: the TypeScript runtime neither reads nor writes it.
ALTER TABLE flow_runs ADD COLUMN IF NOT EXISTS lease_id uuid;
