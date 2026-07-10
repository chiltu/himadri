-- Model-first routing: a model owns one or more provider endpoints. Each
-- endpoint carries its own provider type, base URL, credentials, and weight
-- (denormalized). A model with no enabled endpoint is inactive (unroutable).
--
-- No FK to models: SQLite cannot DROP a table that is the target of a foreign
-- key, and we rebuild `models` below to drop its legacy `provider_id`. Model
-- deletion cascades to endpoints in application code instead (ModelStore).
CREATE TABLE IF NOT EXISTS model_endpoints (
    id TEXT PRIMARY KEY,
    model_id TEXT NOT NULL,
    provider_type TEXT NOT NULL,
    base_url TEXT,
    api_key TEXT,
    weight REAL NOT NULL DEFAULT 1.0,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_model_endpoints_model_id ON model_endpoints(model_id);

-- Backfill: turn each existing (model -> provider) into one endpoint, carrying
-- the provider's type/base_url/api_key (the api_key blob is copied verbatim;
-- the same cipher decrypts it) and the model's weight/enabled flag. Runs while
-- models still has provider_id/weight.
INSERT INTO model_endpoints (id, model_id, provider_type, base_url, api_key, weight, enabled, created_at, updated_at)
SELECT lower(hex(randomblob(16))), m.id, p.name, p.base_url, p.api_key, m.weight, m.enabled, m.created_at, m.updated_at
FROM models m
JOIN providers p ON m.provider_id = p.id;

-- Make models first-party: drop the legacy provider_id (and its FK to
-- providers) and the model-level weight, which now live on endpoints. SQLite
-- can't DROP a column bound in a foreign key, so rebuild the table.
CREATE TABLE models_new (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    display_name TEXT,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT INTO models_new (id, name, display_name, enabled, created_at, updated_at)
SELECT id, name, display_name, enabled, created_at, updated_at FROM models;
DROP TABLE models;
ALTER TABLE models_new RENAME TO models;
