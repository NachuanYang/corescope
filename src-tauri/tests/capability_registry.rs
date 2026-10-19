use corescope_lib::capabilities::{CapabilityState, EvidenceLevel, build_device_profile};
use corescope_lib::inventory::{DeviceKind, RawInventory, normalize_inventory};

const INSTALL_SALT: &[u8] = b"simulated-capability-salt";

fn capability<'a>(
    profile: &'a corescope_lib::capabilities::DeviceProfile,
    key: &str,
) -> &'a corescope_lib::capabilities::CapabilityRecord {
    profile
        .capabilities
        .iter()
        .find(|item| item.metric_key == key)
        .unwrap_or_else(|| panic!("missing capability {key}"))
}

#[test]
fn static_details_keep_values_units_sources_and_checked_times() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "cpus": [{"name":"Fixture CPU","manufacturer":"Example","physical_cores":8,"logical_processors":16}],
        "memory_modules": [{"name":"DIMM A1","capacity_bytes":17179869184u64,"speed_mhz":5600}],
        "physical_disks": [{"index":0,"model":"Fixture SSD","size_bytes":1000204886016u64,"media_type":"SSD"}]
    })).unwrap();
    let snapshot = normalize_inventory(&raw, INSTALL_SALT);
    let cpu = snapshot
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::Cpu)
        .unwrap();
    let profile = build_device_profile(cpu.clone(), &snapshot.scanned_at_utc);

    let cores = capability(&profile, "cpu.cores.physical");
    assert_eq!(cores.state, CapabilityState::Available);
    assert_eq!(cores.value, Some(serde_json::json!(8)));
    assert_eq!(cores.unit.as_deref(), Some("core"));
    assert_eq!(cores.source, "WMI");
    assert_eq!(cores.evidence_level, Some(EvidenceLevel::OsReported));
    assert_eq!(
        cores.observed_at_utc.as_deref(),
        Some(snapshot.scanned_at_utc.as_str())
    );

    let utilization = capability(&profile, "cpu.utilization.percent");
    assert_eq!(utilization.state, CapabilityState::NotTested);
    assert_eq!(utilization.value, None);
    assert_eq!(utilization.observed_at_utc, None);
}

#[test]
fn missing_capability_evidence_is_not_converted_to_zero_or_false() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "network_adapters": [{"name":"Adapter","physical_adapter":null}]
    }))
    .unwrap();
    let snapshot = normalize_inventory(&raw, INSTALL_SALT);
    let adapter = snapshot
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::NetworkAdapter)
        .unwrap();
    let profile = build_device_profile(adapter.clone(), &snapshot.scanned_at_utc);

    let physicality = capability(&profile, "device.virtual");
    assert_eq!(physicality.state, CapabilityState::Unknown);
    assert_eq!(physicality.value, None);
    assert_eq!(
        physicality.reason_code.as_deref(),
        Some("PHYSICALITY_NOT_ASSERTED")
    );

    let link_speed = capability(&profile, "network.link_speed");
    assert_eq!(link_speed.state, CapabilityState::TempUnavailable);
    assert_eq!(link_speed.value, None);
    assert_eq!(
        link_speed.reason_code.as_deref(),
        Some("SOURCE_RETURNED_NO_VALUE")
    );
}

#[test]
fn hdd_health_stays_not_tested_and_ssd_spindle_speed_is_not_applicable() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "physical_disks": [
            {"index":0,"model":"Fixture SSD","media_type":"SSD"},
            {"index":1,"model":"Fixture HDD","media_type":"Rotational"}
        ]
    }))
    .unwrap();
    let snapshot = normalize_inventory(&raw, INSTALL_SALT);
    let ssd = snapshot
        .devices
        .iter()
        .find(|device| device.display_name == "Fixture SSD")
        .unwrap();
    let hdd = snapshot
        .devices
        .iter()
        .find(|device| device.display_name == "Fixture HDD")
        .unwrap();
    let ssd_profile = build_device_profile(ssd.clone(), &snapshot.scanned_at_utc);
    let hdd_profile = build_device_profile(hdd.clone(), &snapshot.scanned_at_utc);

    let spindle_speed = capability(&ssd_profile, "disk.spindle_speed.rpm");
    assert_eq!(spindle_speed.state, CapabilityState::NotApplicable);
    assert_eq!(
        spindle_speed.reason_code.as_deref(),
        Some("NOT_APPLICABLE_TO_SSD")
    );

    let health = capability(&hdd_profile, "disk.temperature.celsius");
    assert_eq!(health.state, CapabilityState::NotTested);
    assert_eq!(
        health.reason_code.as_deref(),
        Some("HDD_QUERY_SKIPPED_TO_AVOID_WAKE")
    );
}

#[test]
fn capability_states_serialize_as_the_documented_wire_values() {
    let states = [
        CapabilityState::Available,
        CapabilityState::NotApplicable,
        CapabilityState::NotTested,
        CapabilityState::Unsupported,
        CapabilityState::PermissionRequired,
        CapabilityState::TempUnavailable,
        CapabilityState::Unknown,
        CapabilityState::Probing,
        CapabilityState::Stale,
    ];
    let encoded: Vec<_> = states
        .iter()
        .map(|state| serde_json::to_value(state).unwrap())
        .collect();
    assert_eq!(
        encoded,
        [
            "AVAILABLE",
            "NOT_APPLICABLE",
            "NOT_TESTED",
            "UNSUPPORTED",
            "PERMISSION_REQUIRED",
            "TEMP_UNAVAILABLE",
            "UNKNOWN",
            "PROBING",
            "STALE"
        ]
    );
}
