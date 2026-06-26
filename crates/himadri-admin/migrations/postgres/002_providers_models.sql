-- Providers table
CREATE TABLE IF NOT EXISTS providers (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL UNIQUE,
    enabled BOOLEAN NOT NULL DEFAULT true,
    api_key TEXT,
    base_url TEXT,
    weight DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Models table
CREATE TABLE IF NOT EXISTS models (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    provider_id UUID NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    display_name VARCHAR(255),
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_models_provider_id ON models(provider_id);
