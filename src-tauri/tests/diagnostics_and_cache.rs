use corescope_lib::inventory::{RawInventory, normalize_inventory};
use corescope_lib::persistence::Database;
use rusqlite::{Connection, params};
use std::path::Path;
use tempfile::tempdir;

fn fixture() -> RawInventory {
    let value: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/desktop_nvidia_ssd_hdd.json")).unwrap();
    assert_eq!(value["simulated"], true);
    serde_json::from_value(value["inventory"].clone()).unwrap()
}

#[test]
fn sanitized_report_omits_device_ids_and_untrusted_details() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("diagnostic-fixture.sqlite3");
    let database = Database::open_at(&path).unwrap();
    let connection = Connection::open(&path).unwrap();
    connection.execute(
        "INSERT INTO diagnostic_events(id, error_code, device_id, adapter, method, occurred_at, native_code, request_id, sanitized_detail) \
         VALUES (?1, 'WMI_QUERY_FAILED', ?2, 'WMI', 'scan_devices', '2026-10-14T10:00:00Z', 'HRESULT_0x80041003', '9e30d4f3-2aa1-4742-9e58-8fc804a65a81', ?3)",
        params!["event-1", "SERIAL-PRIVATE-001", "MAC=00-11-22-33-44-55; C:\\Users\\Alice"],
    ).unwrap();
    drop(connection);

    let report = database.sanitized_diagnostic_report("0.1.0").unwrap();
    let encoded = serde_json::to_string(&report).unwrap();

    assert!(encoded.contains("WMI_QUERY_FAILED"));
    assert!(encoded.contains("scan_devices"));
    assert!(encoded.contains("0x80041003"));
    assert!(!encoded.contains("SERIAL-PRIVATE-001"));
    assert!(!encoded.contains("00-11-22-33-44-55"));
    assert!(!encoded.contains("C:\\\\Users"));
    assert!(!encoded.contains("sanitized_detail"));
}

#[test]
fn clearing_ai_cache_preserves_inventory_identity_and_unrelated_settings() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope-cache.sqlite3");
    let database = Database::open_at(&path).unwrap();
    let salt = database.identity_salt().unwrap();
    let raw = fixture();
    let snapshot = normalize_inventory(&raw, &salt);
    database.persist_inventory(&snapshot, "0.1.0").unwrap();
    let cpu_id = snapshot
        .devices
        .iter()
        .find(|device| device.kind.as_str() == "cpu")
        .unwrap()
        .id
        .clone();
    let connection = Connection::open(&path).unwrap();
    connection.execute(
        "INSERT INTO ai_analyses(id, scope, target_id, input_fingerprint, evidence_snapshot_json, response_json, model_slug, created_at, status, is_historical) VALUES ('analysis-1', 'system', NULL, 'fingerprint', '{}', '{}', 'model', '2026-10-14T10:00:00Z', 'COMPLETED', 0)",
        [],
    ).unwrap();
    connection.execute("INSERT INTO chat_threads(id, scope, target_id, created_at, updated_at) VALUES ('thread-1', 'system', NULL, '2026-10-14T10:00:00Z', '2026-10-14T10:00:00Z')", []).unwrap();
    connection.execute("INSERT INTO chat_messages(id, thread_id, role, body, created_at, status) VALUES ('message-1', 'thread-1', 'user', 'hello', '2026-10-14T10:00:00Z', 'COMPLETED')", []).unwrap();
    connection.execute("INSERT INTO model_knowledge(id, vendor, model, evidence_json, updated_at) VALUES ('knowledge-1', 'Vendor', 'Model', '{}', '2026-10-14T10:00:00Z')", []).unwrap();
    connection.execute("INSERT INTO price_references(id, product_match_key, region, condition, currency, source_url, quoted_at, price_json, match_confidence, status) VALUES ('price-1', 'sku', 'CN', 'NEW', 'CNY', 'https://example.invalid', '2026-10-14T10:00:00Z', '{}', 1.0, 'VALID')", []).unwrap();
    connection.execute("INSERT INTO app_settings(key, value_json) VALUES ('ai.initial_analysis_attempted', 'true'), ('app.theme', '\"graphite\"')", []).unwrap();
    drop(connection);

    let before = database.cache_summary().unwrap();
    assert_eq!(before.ai_analysis_count, 1);
    assert_eq!(before.chat_message_count, 1);
    assert!(before.database_bytes > 0);
    assert!(database.clear_ai_cache().unwrap() >= 5);

    let after = database.cache_summary().unwrap();
    assert_eq!(after.ai_analysis_count, 0);
    assert_eq!(after.chat_message_count, 0);
    assert_eq!(after.model_knowledge_count, 0);
    assert_eq!(after.price_reference_count, 0);
    assert_eq!(database.identity_salt().unwrap(), salt);
    assert!(database.load_device_profile(&cpu_id).unwrap().is_some());
    assert_eq!(
        database.inventory_summary().unwrap().current_devices,
        snapshot.devices.len() as i64
    );
    let connection = Connection::open(Path::new(&path)).unwrap();
    let marker: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key = 'ai.initial_analysis_attempted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let unrelated: String = connection
        .query_row(
            "SELECT value_json FROM app_settings WHERE key = 'app.theme'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        marker, 1,
        "AI-only cache clearing must not re-enable automatic first-run analysis"
    );
    assert_eq!(unrelated, "\"graphite\"");
}
