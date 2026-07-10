-- Per-model routing weight. Each (provider, model) becomes its own routing
-- target, so weight now lives on the model rather than only the provider.
-- Existing rows default to 1.0 (equal weighting), matching prior behavior.
ALTER TABLE models ADD COLUMN weight REAL NOT NULL DEFAULT 1.0;
