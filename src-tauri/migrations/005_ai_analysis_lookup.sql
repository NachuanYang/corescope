CREATE INDEX IF NOT EXISTS idx_ai_analyses_fingerprint
    ON ai_analyses(scope, input_fingerprint, status, created_at DESC);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
