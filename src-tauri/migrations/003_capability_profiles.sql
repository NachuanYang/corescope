ALTER TABLE capabilities ADD COLUMN capability_kind TEXT NOT NULL DEFAULT 'STATIC';
ALTER TABLE capabilities ADD COLUMN value_json TEXT;
ALTER TABLE capabilities ADD COLUMN unit TEXT;
ALTER TABLE capabilities ADD COLUMN evidence_level TEXT;
ALTER TABLE capabilities ADD COLUMN native_error_code TEXT;

CREATE INDEX IF NOT EXISTS idx_capabilities_state
    ON capabilities(state, capability_kind);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
