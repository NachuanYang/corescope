use corescope_lib::capabilities::{
    CapabilityKind, CapabilityRecord, CapabilityState, EvidenceLevel,
};
use corescope_lib::inventory::{RawInventory, normalize_inventory};
use corescope_lib::persistence::Database;
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;
use tempfile::tempdir;

fn fixture(name: &str) -> RawInventory {
    let json = match name {
        "desktop_nvidia_ssd_hdd" => include_str!("fixtures/desktop_nvidia_ssd_hdd.json"),
        "two_identical_ssd" => include_str!("fixtures/two_identical_ssd.json"),
        _ => panic!("unknown fixture"),
    };
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(value["simulated"], true);
    serde_json::from_value(value["inventory"].clone()).unwrap()
}

#[test]
fn persists_current_devices_topology_and_scan_metadata_without_raw_identifiers() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope.sqlite3");
    let database = Database::open_at(&path).unwrap();
    let salt = database.identity_salt().unwrap();
    let snapshot = normalize_inventory(&fixture("desktop_nvidia_ssd_hdd"), &salt);

    database.persist_inventory(&snapshot, "0.1.0").unwrap();
    let summary = database.inventory_summary().unwrap();

    assert_eq!(summary.current_devices, 23);
    assert_eq!(summary.historical_devices, 0);
    assert_eq!(summary.current_edges, 8);
    assert_eq!(summary.completed_scans, 1);
    assert_eq!(database.schema_version().unwrap(), 6);
    let loaded = database.load_inventory().unwrap().unwrap();
    assert_eq!(loaded.devices.len(), 23);
    assert_eq!(loaded.edges.len(), 8);
    assert_eq!(loaded.warnings.len(), 0);

    let cpu = snapshot
        .devices
        .iter()
        .find(|device| device.kind == corescope_lib::inventory::DeviceKind::Cpu)
        .unwrap();
    let profile = database.load_device_profile(&cpu.id).unwrap().unwrap();
    let physical_cores = profile
        .capabilities
        .iter()
        .find(|capability| capability.metric_key == "cpu.cores.physical")
        .unwrap();
    assert_eq!(
        physical_cores.state,
        corescope_lib::capabilities::CapabilityState::Available
    );
    assert!(physical_cores.value.is_some());
    assert_eq!(physical_cores.unit.as_deref(), Some("core"));
    assert!(physical_cores.observed_at_utc.is_some());

    let connection = Connection::open(&path).unwrap();
    let leaked: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM devices WHERE display_name LIKE '%SIM-SN%' OR attributes_json LIKE '%SIM-SN%' OR identity_hash LIKE '%SIM-SN%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        leaked, 0,
        "serial material must never be stored in normalized data"
    );
}

#[test]
fn reconstructs_missing_capability_profiles_from_the_cached_inventory_on_open() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope.sqlite3");
    {
        let database = Database::open_at(&path).unwrap();
        let snapshot = normalize_inventory(
            &fixture("desktop_nvidia_ssd_hdd"),
            &database.identity_salt().unwrap(),
        );
        database.persist_inventory(&snapshot, "0.1.0").unwrap();
    }

    {
        let connection = Connection::open(&path).unwrap();
        connection.execute("DELETE FROM capabilities", []).unwrap();
    }

    let database = Database::open_at(&path).unwrap();
    let snapshot = database.load_inventory().unwrap().unwrap();
    let cpu = snapshot
        .devices
        .iter()
        .find(|device| device.kind == corescope_lib::inventory::DeviceKind::Cpu)
        .unwrap();
    let profile = database.load_device_profile(&cpu.id).unwrap().unwrap();
    let physical_cores = profile
        .capabilities
        .iter()
        .find(|capability| capability.metric_key == "cpu.cores.physical")
        .unwrap();
    assert_eq!(
        physical_cores.state,
        corescope_lib::capabilities::CapabilityState::Available
    );
    assert!(physical_cores.value.is_some());
}

#[test]
fn keeps_removed_devices_as_history_and_reuses_stable_install_salt() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope.sqlite3");
    let database = Database::open_at(&path).unwrap();
    let salt = database.identity_salt().unwrap();
    let mut raw = fixture("two_identical_ssd");

    database
        .persist_inventory(&normalize_inventory(&raw, &salt), "0.1.0")
        .unwrap();
    raw.physical_disks.pop();
    database
        .persist_inventory(&normalize_inventory(&raw, &salt), "0.1.0")
        .unwrap();

    let summary = database.inventory_summary().unwrap();
    assert_eq!(summary.current_devices, 1);
    assert_eq!(summary.historical_devices, 1);
    assert_eq!(summary.completed_scans, 2);
    let connection = Connection::open(&path).unwrap();
    let stale_values: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM capabilities WHERE state = 'STALE' AND value_json IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stale_values, 0,
        "stale capability rows must not retain current values"
    );
    assert_eq!(
        Database::open_at(Path::new(&path))
            .unwrap()
            .identity_salt()
            .unwrap(),
        salt
    );
}

#[test]
fn a_rescan_marks_current_live_values_pending_without_claiming_the_device_was_removed() {
    let database = Database::open_in_memory().unwrap();
    let salt = database.identity_salt().unwrap();
    let snapshot = normalize_inventory(&fixture("desktop_nvidia_ssd_hdd"), &salt);
    database.persist_inventory(&snapshot, "0.1.0").unwrap();
    let cpu = snapshot
        .devices
        .iter()
        .find(|device| device.kind == corescope_lib::inventory::DeviceKind::Cpu)
        .unwrap();
    database
        .save_capabilities(&[CapabilityRecord {
            device_id: cpu.id.clone(),
            metric_key: "cpu.utilization.percent".to_owned(),
            kind: CapabilityKind::Dynamic,
            state: CapabilityState::Available,
            value: Some(json!(6.25)),
            unit: Some("percent".to_owned()),
            source: "PDH".to_owned(),
            evidence_level: Some(EvidenceLevel::Measured),
            observed_at_utc: Some("2026-10-15T09:00:00Z".to_owned()),
            reason_code: None,
            native_error_code: None,
            details: json!({}),
        }])
        .unwrap();

    database.persist_inventory(&snapshot, "0.1.0").unwrap();

    let profile = database.load_device_profile(&cpu.id).unwrap().unwrap();
    let utilization = profile
        .capabilities
        .iter()
        .find(|capability| capability.metric_key == "cpu.utilization.percent")
        .unwrap();
    assert_eq!(utilization.state, CapabilityState::Stale);
    assert_eq!(utilization.value, None);
    assert_eq!(
        utilization.reason_code.as_deref(),
        Some("SCAN_REFRESH_PENDING_NEW_SAMPLE")
    );
    assert_eq!(profile.device.is_present, Some(true));

    let history = database.load_metric_history(&cpu.id).unwrap();
    let archived = history
        .iter()
        .find(|item| item.metric_key == "cpu.utilization.percent")
        .unwrap();
    assert_eq!(archived.value, Some(json!(6.25)));
    assert_eq!(archived.source, "PDH");
}

#[test]
fn persists_known_historical_and_unknown_presence_without_collapsing_them() {
    let database = Database::open_in_memory().unwrap();
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "network_adapters": [{"name":"Adapter","physical_adapter":null}],
        "pnp_nodes": [
            {"instance_id":"USB\\OLD", "name":"Removed device", "present":false, "source":"SetupAPI"},
            {"instance_id":"USB\\UNKNOWN", "name":"Unverified device", "source":"WMI"}
        ]
    })).unwrap();
    let snapshot = normalize_inventory(&raw, &database.identity_salt().unwrap());

    database.persist_inventory(&snapshot, "0.1.0").unwrap();
    let summary = database.inventory_summary().unwrap();
    let loaded = database.load_inventory().unwrap().unwrap();

    assert_eq!(summary.current_devices, 1);
    assert_eq!(summary.historical_devices, 1);
    assert_eq!(summary.unknown_presence_devices, 1);
    assert_eq!(loaded.devices.len(), 3);
    assert!(
        loaded
            .devices
            .iter()
            .any(|device| device.is_present == Some(false))
    );
    assert!(
        loaded
            .devices
            .iter()
            .any(|device| device.is_present.is_none())
    );
}

#[test]
fn explicit_storage_probe_can_replace_cached_permission_state_with_new_evidence() {
    let database = Database::open_in_memory().unwrap();
    let snapshot = normalize_inventory(
        &fixture("desktop_nvidia_ssd_hdd"),
        &database.identity_salt().unwrap(),
    );
    database.persist_inventory(&snapshot, "0.1.0").unwrap();
    let disk = snapshot
        .devices
        .iter()
        .find(|device| device.display_name == "WD_BLACK SN770 1TB")
        .unwrap();

    let denied = CapabilityRecord {
        device_id: disk.id.clone(),
        metric_key: "disk.temperature.celsius".to_owned(),
        kind: CapabilityKind::Dynamic,
        state: CapabilityState::PermissionRequired,
        value: None,
        unit: Some("°C".to_owned()),
        source: "StorageReliability".to_owned(),
        evidence_level: None,
        observed_at_utc: Some("2026-10-14T10:00:00Z".to_owned()),
        reason_code: Some("STANDARD_USER_ACCESS_DENIED".to_owned()),
        native_error_code: Some("HRESULT_0x80041003".to_owned()),
        details: json!({}),
    };
    database.save_capabilities(&[denied]).unwrap();

    let measured = CapabilityRecord {
        device_id: disk.id.clone(),
        metric_key: "disk.temperature.celsius".to_owned(),
        kind: CapabilityKind::Dynamic,
        state: CapabilityState::Available,
        value: Some(json!(36)),
        unit: Some("°C".to_owned()),
        source: "StorageReliability".to_owned(),
        evidence_level: Some(EvidenceLevel::SourceReported),
        observed_at_utc: Some("2026-10-14T10:05:00Z".to_owned()),
        reason_code: None,
        native_error_code: None,
        details: json!({"reportedField":"Temperature"}),
    };
    database.save_capabilities(&[measured]).unwrap();

    let profile = database.load_device_profile(&disk.id).unwrap().unwrap();
    let temperature = profile
        .capabilities
        .iter()
        .find(|capability| capability.metric_key == "disk.temperature.celsius")
        .unwrap();
    assert_eq!(temperature.state, CapabilityState::Available);
    assert_eq!(temperature.value, Some(json!(36)));
    assert_eq!(temperature.native_error_code, None);
}

#[test]
fn restart_marks_persisted_dynamic_value_stale_and_keeps_it_only_in_history() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope-restart.sqlite3");
    let raw = fixture("desktop_nvidia_ssd_hdd");
    let (disk_id, salt);
    {
        let database = Database::open_at(&path).unwrap();
        salt = database.identity_salt().unwrap();
        let snapshot = normalize_inventory(&raw, &salt);
        database.persist_inventory(&snapshot, "0.1.0").unwrap();
        let disk = snapshot
            .devices
            .iter()
            .find(|device| device.display_name == "WD_BLACK SN770 1TB")
            .unwrap();
        disk_id = disk.id.clone();
        database
            .save_capabilities(&[CapabilityRecord {
                device_id: disk_id.clone(),
                metric_key: "disk.temperature.celsius".to_owned(),
                kind: CapabilityKind::Dynamic,
                state: CapabilityState::Available,
                value: Some(json!(37)),
                unit: Some("°C".to_owned()),
                source: "StorageReliability".to_owned(),
                evidence_level: Some(EvidenceLevel::SourceReported),
                observed_at_utc: Some("2026-10-14T10:05:00Z".to_owned()),
                reason_code: None,
                native_error_code: None,
                details: json!({"reportedField":"Temperature"}),
            }])
            .unwrap();
    }

    let database = Database::open_at(&path).unwrap();
    assert_eq!(database.identity_salt().unwrap(), salt);
    let profile = database.load_device_profile(&disk_id).unwrap().unwrap();
    let current = profile
        .capabilities
        .iter()
        .find(|item| item.metric_key == "disk.temperature.celsius")
        .unwrap();
    assert_eq!(current.state, CapabilityState::Stale);
    assert_eq!(current.value, None);
    assert_eq!(
        current.reason_code.as_deref(),
        Some("APP_RESTARTED_LIVE_DATA_NOT_CURRENT")
    );

    let history = database.load_metric_history(&disk_id).unwrap();
    let archived = history
        .iter()
        .find(|item| item.metric_key == "disk.temperature.celsius")
        .unwrap();
    assert_eq!(archived.value, Some(json!(37)));
    assert_eq!(archived.source, "StorageReliability");
    assert_eq!(
        archived.observed_at_utc.as_deref(),
        Some("2026-10-14T10:05:00Z")
    );
}

#[test]
fn keeps_only_the_twenty_most_recent_history_rows_per_metric() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope-history-limit.sqlite3");
    let database = Database::open_at(&path).unwrap();
    let snapshot = normalize_inventory(
        &fixture("desktop_nvidia_ssd_hdd"),
        &database.identity_salt().unwrap(),
    );
    database.persist_inventory(&snapshot, "0.1.0").unwrap();
    let disk_id = snapshot
        .devices
        .iter()
        .find(|device| device.display_name == "WD_BLACK SN770 1TB")
        .unwrap()
        .id
        .clone();
    let connection = Connection::open(&path).unwrap();
    for sample in 0..22 {
        connection.execute(
            "INSERT INTO metric_snapshots(id, device_id, metric_key, value_json, unit, state, observed_at, source, evidence_level, reason_code, details_json) \
             VALUES (?1, ?2, 'disk.temperature.celsius', ?3, '°C', 'AVAILABLE', ?4, 'fixture', 'SOURCE_REPORTED', NULL, '{}')",
            rusqlite::params![
                format!("history-{sample:02}"), disk_id, serde_json::json!(sample).to_string(),
                format!("2026-10-14T10:{:02}:00Z", sample),
            ],
        ).unwrap();
    }
    drop(connection);
    drop(database);

    let database = Database::open_at(&path).unwrap();
    let history = database.load_metric_history(&disk_id).unwrap();
    assert_eq!(history.len(), 20);
    assert_eq!(history[0].value, Some(json!(21)));
    assert_eq!(history[19].value, Some(json!(2)));
}

#[test]
fn loads_twenty_history_rows_for_each_metric_without_a_device_wide_truncation() {
    let directory = tempdir().unwrap();
    let path = directory
        .path()
        .join("corescope-history-multiple-metrics.sqlite3");
    let database = Database::open_at(&path).unwrap();
    let snapshot = normalize_inventory(
        &fixture("desktop_nvidia_ssd_hdd"),
        &database.identity_salt().unwrap(),
    );
    database.persist_inventory(&snapshot, "0.1.0").unwrap();
    let disk_id = snapshot
        .devices
        .iter()
        .find(|device| device.display_name == "WD_BLACK SN770 1TB")
        .unwrap()
        .id
        .clone();
    let connection = Connection::open(&path).unwrap();
    for metric in 0..6 {
        for sample in 0..20 {
            connection.execute(
                "INSERT INTO metric_snapshots(id, device_id, metric_key, value_json, unit, state, observed_at, source, evidence_level, reason_code, details_json) \
                 VALUES (?1, ?2, ?3, ?4, 'unit', 'AVAILABLE', ?5, 'fixture', 'SOURCE_REPORTED', NULL, '{}')",
                rusqlite::params![
                    format!("history-{metric:02}-{sample:02}"), disk_id.clone(), format!("fixture.metric.{metric}"),
                    serde_json::json!(sample).to_string(), format!("2026-10-14T10:{sample:02}:00Z"),
                ],
            ).unwrap();
        }
    }
    drop(connection);
    drop(database);

    let database = Database::open_at(&path).unwrap();
    let history = database.load_metric_history(&disk_id).unwrap();
    let fixture_history: Vec<_> = history
        .iter()
        .filter(|item| item.metric_key.starts_with("fixture.metric."))
        .collect();

    assert_eq!(fixture_history.len(), 120);
    assert_eq!(
        fixture_history
            .iter()
            .map(|item| item.metric_key.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        6
    );
}
