ALTER TABLE chat_messages
    ADD COLUMN citations_json TEXT NOT NULL DEFAULT '[]';

ALTER TABLE chat_messages
    ADD COLUMN error_code TEXT;

ALTER TABLE chat_messages
    ADD COLUMN model_slug TEXT;

ALTER TABLE chat_messages
    ADD COLUMN source TEXT;

CREATE TABLE IF NOT EXISTS chat_evidence_snapshots (
    snapshot_id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES chat_threads(id) ON DELETE CASCADE,
    target_id TEXT,
    input_fingerprint TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    captured_at_utc TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_chat_evidence_thread_created
    ON chat_evidence_snapshots(thread_id, created_at);

CREATE INDEX IF NOT EXISTS idx_chat_messages_thread_status_created
    ON chat_messages(thread_id, status, created_at);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (6, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
