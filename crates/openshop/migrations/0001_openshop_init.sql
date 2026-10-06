CREATE TABLE IF NOT EXISTS installations (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text DEFAULT 'default' NOT NULL,
  shop text NOT NULL,
  access_token text,
  scopes text,
  nonce text,
  installed_at timestamp with time zone DEFAULT now() NOT NULL,
  uninstalled_at timestamp with time zone
);
CREATE UNIQUE INDEX IF NOT EXISTS installations_app_shop_unique ON installations (app_handle, shop);

CREATE TABLE IF NOT EXISTS flow_runs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text DEFAULT 'default' NOT NULL,
  shop text NOT NULL,
  flow_name text NOT NULL,
  status text DEFAULT 'pending' NOT NULL,
  input json,
  error text,
  deadline_at timestamp with time zone,
  parent_run_id text,
  attempts integer DEFAULT 0 NOT NULL,
  available_at timestamp with time zone,
  worker_id text,
  retry_policy json,
  started_at timestamp with time zone,
  completed_at timestamp with time zone,
  created_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS flow_runs_status_available_created_idx ON flow_runs (status, available_at, created_at);
CREATE INDEX IF NOT EXISTS flow_runs_app_shop_created_idx ON flow_runs (app_handle, shop, created_at);
CREATE INDEX IF NOT EXISTS flow_runs_app_shop_flow_status_idx ON flow_runs (app_handle, shop, flow_name, status);

CREATE TABLE IF NOT EXISTS step_results (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  step_name text NOT NULL,
  attempt integer DEFAULT 1 NOT NULL,
  status text DEFAULT 'pending' NOT NULL,
  output json,
  error text,
  duration_ms integer,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  flow_run_id text NOT NULL
);
CREATE INDEX IF NOT EXISTS step_results_flow_run_idx ON step_results (flow_run_id);
CREATE UNIQUE INDEX IF NOT EXISTS step_results_flow_run_step_attempt_unique ON step_results (flow_run_id, step_name, attempt);

CREATE TABLE IF NOT EXISTS provider_configs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text DEFAULT 'default' NOT NULL,
  shop text NOT NULL,
  provider_name text NOT NULL,
  config json DEFAULT '{}'::json,
  last_checked_at timestamp with time zone,
  last_check_ok boolean,
  updated_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS provider_configs_app_shop_provider_unique ON provider_configs (app_handle, shop, provider_name);

CREATE TABLE IF NOT EXISTS cron_overrides (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text DEFAULT 'default' NOT NULL,
  shop text NOT NULL,
  cron_key text NOT NULL,
  enabled boolean DEFAULT true NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS cron_overrides_app_shop_cron_unique ON cron_overrides (app_handle, shop, cron_key);

CREATE TABLE IF NOT EXISTS logs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  flow_run_id text,
  level text DEFAULT 'info' NOT NULL,
  message text,
  payload json,
  created_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS logs_flow_run_created_idx ON logs (flow_run_id, created_at);

CREATE TABLE IF NOT EXISTS mcp_tokens (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text DEFAULT 'default' NOT NULL,
  shop text NOT NULL,
  name text NOT NULL,
  token_id text NOT NULL,
  token_hash text NOT NULL,
  token_fingerprint text NOT NULL,
  status text DEFAULT 'active' NOT NULL,
  expires_at timestamp with time zone,
  revoked_at timestamp with time zone,
  last_used_at timestamp with time zone,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS mcp_tokens_token_id_unique ON mcp_tokens (token_id);
CREATE INDEX IF NOT EXISTS mcp_tokens_app_shop_idx ON mcp_tokens (app_handle, shop);

CREATE TABLE IF NOT EXISTS mcp_permission_grants (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text DEFAULT 'default' NOT NULL,
  shop text NOT NULL,
  token_id text NOT NULL,
  permission_key text NOT NULL,
  granted boolean DEFAULT true NOT NULL,
  created_at timestamp with time zone DEFAULT now() NOT NULL,
  updated_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS mcp_permission_grants_token_permission_unique ON mcp_permission_grants (app_handle, shop, token_id, permission_key);
CREATE INDEX IF NOT EXISTS mcp_permission_grants_app_shop_token_idx ON mcp_permission_grants (app_handle, shop, token_id);

CREATE TABLE IF NOT EXISTS mcp_audit_logs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid() NOT NULL,
  app_handle text DEFAULT 'default' NOT NULL,
  shop text NOT NULL,
  token_id text,
  capability_type text,
  capability_name text,
  permission_keys json,
  target_shop text,
  status text NOT NULL,
  error text,
  duration_ms integer,
  request_id text,
  created_at timestamp with time zone DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS mcp_audit_logs_app_shop_created_idx ON mcp_audit_logs (app_handle, shop, created_at);
CREATE INDEX IF NOT EXISTS mcp_audit_logs_token_created_idx ON mcp_audit_logs (token_id, created_at);
