use corescope_lib::persistence::Database;
use rusqlite::Connection;
use tempfile::tempdir;

#[test]
fn initial_migration_creates_the_local_hardware_and_ai_schema() {
    let database = Database::open_in_memory().unwrap();
    assert_eq!(database.schema_version().unwrap(), 6);

    for table in [
        "schema_migrations",
        "install",
        "scan_sessions",
        "devices",
        "device_edges",
        "capabilities",
        "metric_snapshots",
        "ai_analyses",
        "model_knowledge",
        "price_references",
        "chat_threads",
        "chat_messages",
        "app_settings",
    ] {
        assert!(database.has_table(table).unwrap(), "missing table {table}");
    }

    assert!(!database.has_table("oauth_tokens").unwrap());
}

#[test]
fn upgrades_existing_schema_v1_database_to_chat_schema_v6() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope-v1.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(include_str!("../migrations/001_initial.sql"))
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);

    let database = Database::open_at(&path).unwrap();
    assert_eq!(database.schema_version().unwrap(), 6);
    assert!(database.has_table("devices").unwrap());

    let connection = Connection::open(path).unwrap();
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(devices)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(columns.contains(&"layer".to_owned()));
    assert!(columns.contains(&"identity_confidence".to_owned()));
    assert!(columns.contains(&"source".to_owned()));

    let capability_columns: Vec<String> = connection
        .prepare("PRAGMA table_info(capabilities)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for column in [
        "capability_kind",
        "value_json",
        "unit",
        "evidence_level",
        "native_error_code",
    ] {
        assert!(capability_columns.contains(&column.to_owned()));
    }
}

#[test]
fn upgrades_v3_without_dropping_scan_inventory_or_capability_rows() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope-v3.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(concat!(
            include_str!("../migrations/001_initial.sql"),
            include_str!("../migrations/002_device_layers.sql"),
            include_str!("../migrations/003_capability_profiles.sql")
        ))
        .unwrap();
    connection.pragma_update(None, "user_version", 3).unwrap();
    connection.execute(
        "INSERT INTO scan_sessions(id, started_at, completed_at, status, schema_version, app_version) VALUES ('scan-v3', '2026-10-14T09:00:00Z', '2026-10-14T09:00:00Z', 'COMPLETED', 3, '0.1.0')",
        [],
    ).unwrap();
    connection.execute(
        "INSERT INTO devices(id, kind, class, display_name, is_virtual, is_present, identity_hash, first_seen, last_seen, attributes_json, layer, identity_confidence, source) VALUES ('dev-cpu', 'cpu', 'cpu', 'CPU fixture', 0, 1, 'hash-v3', '2026-10-14T09:00:00Z', '2026-10-14T09:00:00Z', '{}', 'physical_device', 'strong', 'fixture')",
        [],
    ).unwrap();
    connection.execute(
        "INSERT INTO capabilities(device_id, metric_key, state, source, reason_code, last_checked_at, details_json, capability_kind, value_json, unit, evidence_level, native_error_code) VALUES ('dev-cpu', 'cpu.cores.physical', 'AVAILABLE', 'fixture', NULL, '2026-10-14T09:00:00Z', '{}', 'STATIC', '8', 'core', 'OS_REPORTED', NULL)",
        [],
    ).unwrap();
    connection.execute(
        "INSERT INTO ai_analyses(id, scope, target_id, input_fingerprint, evidence_snapshot_json, response_json, model_slug, created_at, status, is_historical) VALUES ('analysis-v3', 'system', NULL, 'fingerprint-v3', '{}', '{\"summary\":\"kept\"}', 'model', '2026-10-14T09:01:00Z', 'COMPLETED', 0)",
        [],
    ).unwrap();
    connection
        .execute(
            "INSERT INTO app_settings(key, value_json) VALUES ('app.theme', '\"graphite\"')",
            [],
        )
        .unwrap();
    drop(connection);

    let database = Database::open_at(&path).unwrap();
    assert_eq!(database.schema_version().unwrap(), 6);
    assert_eq!(database.inventory_summary().unwrap().completed_scans, 1);
    let profile = database.load_device_profile("dev-cpu").unwrap().unwrap();
    let cores = profile
        .capabilities
        .iter()
        .find(|item| item.metric_key == "cpu.cores.physical")
        .unwrap();
    assert_eq!(
        cores.state,
        corescope_lib::capabilities::CapabilityState::Available
    );
    assert_eq!(cores.value, Some(serde_json::json!(8)));

    let connection = Connection::open(path).unwrap();
    let history_columns: Vec<String> = connection
        .prepare("PRAGMA table_info(metric_snapshots)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for column in [
        "evidence_level",
        "reason_code",
        "details_json",
        "scan_session_id",
    ] {
        assert!(history_columns.contains(&column.to_owned()));
    }
    let retained_analysis: String = connection
        .query_row(
            "SELECT response_json FROM ai_analyses WHERE id = 'analysis-v3'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let retained_setting: String = connection
        .query_row(
            "SELECT value_json FROM app_settings WHERE key = 'app.theme'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained_analysis, "{\"summary\":\"kept\"}");
    assert_eq!(retained_setting, "\"graphite\"");
}
