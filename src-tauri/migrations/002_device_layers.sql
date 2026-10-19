ALTER TABLE devices ADD COLUMN layer TEXT NOT NULL DEFAULT 'physical_device';
ALTER TABLE devices ADD COLUMN identity_confidence TEXT NOT NULL DEFAULT 'weak';
ALTER TABLE devices ADD COLUMN source TEXT NOT NULL DEFAULT 'WMI';

CREATE INDEX IF NOT EXISTS idx_devices_layer_present ON devices(layer, is_present);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
