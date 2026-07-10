-- Model-first routing: a model owns one or more provider endpoints. Each
-- endpoint carries its own provider type, base URL, credentials, and weight
-- (denormalized). A model with no enabled endpoint is inactive (unroutable).
CREATE TABLE IF NOT EXISTS model_endpoints (
    id UUID PRIMARY KEY,
    model_id UUID NOT NULL REFERENCES models(id) ON DELETE CASCADE,
    provider_type VARCHAR(255) NOT NULL,
    base_url TEXT,
    api_key TEXT,
    weight DOUBLE PRECISION NOT NULL DEFAULT 1.0,
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_model_endpoints_model_id ON model_endpoints(model_id);

-- Backfill: turn each existing (model -> provider) into one endpoint, carrying
-- the provider's type/base_url/api_key (the api_key blob is copied verbatim;
-- the same cipher decrypts it) and the model's weight/enabled flag. Runs while
-- models still has provider_id/weight.
INSERT INTO model_endpoints (id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at)
SELECT gen_random_uuid(), m.id, p.name, p.base_url, p.api_key, m.weight, m.enabled, m.created_at, m.updated_at
FROM models m
JOIN providers p ON m.provider_id = p.id;

-- Make models first-party: drop the legacy provider_id (and its index) and the
-- model-level weight, which now live on endpoints.
DROP INDEX IF EXISTS idx_models_provider_id;
ALTER TABLE models DROP COLUMN IF EXISTS provider_id;
ALTER TABLE models DROP COLUMN IF EXISTS weight;
