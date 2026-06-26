-- Initial schema: api_keys table
CREATE TABLE IF NOT EXISTS api_keys (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    key VARCHAR(255) NOT NULL UNIQUE,
    scopes JSONB NOT NULL DEFAULT '[]',
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_used_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    usage_count BIGINT NOT NULL DEFAULT 0,
    metadata JSONB,
    org_id VARCHAR(255),
    team_id VARCHAR(255),
    user_id VARCHAR(255),
    models JSONB,
    rate_limit_override JSONB,
    token_budget JSONB
);

CREATE INDEX IF NOT EXISTS idx_api_keys_key ON api_keys(key);
CREATE INDEX IF NOT EXISTS idx_api_keys_enabled ON api_keys(enabled);
CREATE INDEX IF NOT EXISTS idx_api_keys_org_id ON api_keys(org_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_team_id ON api_keys(team_id);

-- config_history table
CREATE TABLE IF NOT EXISTS config_history (
    version SERIAL PRIMARY KEY,
    config JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    rolled_back_from INTEGER
);

-- request_logs table
CREATE TABLE IF NOT EXISTS request_logs (
    id SERIAL PRIMARY KEY,
    trace_id VARCHAR(255) NOT NULL,
    stage VARCHAR(50) NOT NULL,
    model VARCHAR(255) NOT NULL,
    provider VARCHAR(255) NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    error_message TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_request_logs_trace_id ON request_logs(trace_id);
CREATE INDEX IF NOT EXISTS idx_request_logs_model ON request_logs(model);
CREATE INDEX IF NOT EXISTS idx_request_logs_provider ON request_logs(provider);
CREATE INDEX IF NOT EXISTS idx_request_logs_created_at ON request_logs(created_at);

-- usage_records table
CREATE TABLE IF NOT EXISTS usage_records (
    id SERIAL PRIMARY KEY,
    request_id VARCHAR(255) NOT NULL,
    api_key_id VARCHAR(255),
    model VARCHAR(255) NOT NULL,
    provider VARCHAR(255) NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0,
    latency_ms BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    success BOOLEAN NOT NULL DEFAULT true,
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_usage_records_api_key_id ON usage_records(api_key_id);
CREATE INDEX IF NOT EXISTS idx_usage_records_model ON usage_records(model);
CREATE INDEX IF NOT EXISTS idx_usage_records_created_at ON usage_records(created_at);
