PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS install (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    install_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    local_salt_ref TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scan_sessions (
    id TEXT PRIMARY KEY,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    status TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    app_version TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS devices (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    class TEXT NOT NULL,
    display_name TEXT NOT NULL,
    is_virtual INTEGER,
    is_present INTEGER,
    identity_hash TEXT NOT NULL,
    first_seen TEXT NOT NULL,
    last_seen TEXT NOT NULL,
    version TEXT,
    attributes_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_devices_kind_present ON devices(kind, is_present);
CREATE INDEX IF NOT EXISTS idx_devices_identity_hash ON devices(identity_hash);

CREATE TABLE IF NOT EXISTS device_edges (
    parent_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    child_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    relationship TEXT NOT NULL,
    evidence_source TEXT NOT NULL,
    evidence_status TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    PRIMARY KEY(parent_id, child_id, relationship)
);

CREATE TABLE IF NOT EXISTS capabilities (
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    metric_key TEXT NOT NULL,
    state TEXT NOT NULL,
    source TEXT NOT NULL,
    reason_code TEXT,
    last_checked_at TEXT,
    details_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY(device_id, metric_key)
);

CREATE TABLE IF NOT EXISTS metric_snapshots (
    id TEXT PRIMARY KEY,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    metric_key TEXT NOT NULL,
    value_json TEXT,
    unit TEXT,
    state TEXT NOT NULL,
    observed_at TEXT,
    source TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_metric_snapshots_device_time
    ON metric_snapshots(device_id, observed_at DESC);

CREATE TABLE IF NOT EXISTS ai_analyses (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    target_id TEXT,
    input_fingerprint TEXT NOT NULL,
    evidence_snapshot_json TEXT NOT NULL,
    response_json TEXT NOT NULL,
    model_slug TEXT NOT NULL,
    created_at TEXT NOT NULL,
    status TEXT NOT NULL,
    is_historical INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_ai_analyses_target_created
    ON ai_analyses(scope, target_id, created_at DESC);

CREATE TABLE IF NOT EXISTS model_knowledge (
    id TEXT PRIMARY KEY,
    vendor TEXT NOT NULL,
    model TEXT NOT NULL,
    sku_variant TEXT,
    evidence_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS price_references (
    id TEXT PRIMARY KEY,
    product_match_key TEXT NOT NULL,
    region TEXT NOT NULL,
    condition TEXT NOT NULL,
    currency TEXT NOT NULL,
    source_url TEXT NOT NULL,
    quoted_at TEXT NOT NULL,
    price_json TEXT NOT NULL,
    match_confidence REAL NOT NULL,
    status TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chat_threads (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    target_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS chat_messages (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES chat_threads(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    body TEXT NOT NULL,
    evidence_snapshot_ref TEXT,
    created_at TEXT NOT NULL,
    status TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chat_messages_thread_created
    ON chat_messages(thread_id, created_at);

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS diagnostic_events (
    id TEXT PRIMARY KEY,
    error_code TEXT NOT NULL,
    device_id TEXT,
    adapter TEXT,
    method TEXT,
    occurred_at TEXT NOT NULL,
    duration_ms INTEGER,
    permission_state TEXT,
    native_code TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    request_id TEXT,
    sanitized_detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_diagnostic_events_occurred
    ON diagnostic_events(occurred_at DESC);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
