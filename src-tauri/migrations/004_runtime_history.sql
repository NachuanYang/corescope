ALTER TABLE metric_snapshots ADD COLUMN evidence_level TEXT;
ALTER TABLE metric_snapshots ADD COLUMN reason_code TEXT;
ALTER TABLE metric_snapshots ADD COLUMN details_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE metric_snapshots ADD COLUMN scan_session_id TEXT REFERENCES scan_sessions(id);

UPDATE devices SET identity_confidence = UPPER(identity_confidence)
WHERE identity_confidence IN ('strong', 'moderate', 'weak');

CREATE INDEX IF NOT EXISTS idx_metric_snapshots_device_metric_time
    ON metric_snapshots(device_id, metric_key, observed_at DESC);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
