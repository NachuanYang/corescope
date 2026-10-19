#![cfg(windows)]

use corescope_lib::capabilities::{CapabilityState, build_device_profile};
use corescope_lib::inventory::{DeviceKind, normalize_inventory, scan_local};
use corescope_lib::persistence::Database;
use corescope_lib::storage_reliability::is_safe_probe_target;
use corescope_lib::storage_reliability::windows::{bind_device_id, mark_missing_values};
use std::collections::BTreeMap;

#[test]
#[ignore = "real read-only SSD reliability probe; runs once per explicitly confirmed SSD"]
fn standard_user_probe_reads_only_confirmed_ssds_and_keeps_hdd_unqueried() {
    let database = Database::open_in_memory().unwrap();
    let raw = scan_local().expect("real Windows inventory scan should complete");
    let snapshot = normalize_inventory(&raw, &database.identity_salt().unwrap());
    database
        .persist_inventory(&snapshot, env!("CARGO_PKG_VERSION"))
        .unwrap();

    let disks = snapshot
        .devices
        .iter()
        .filter(|device| device.kind == DeviceKind::PhysicalDisk)
        .collect::<Vec<_>>();
    let ssds = disks
        .iter()
        .copied()
        .filter(|device| {
            is_safe_probe_target(
                device
                    .attributes
                    .get("mediaType")
                    .and_then(serde_json::Value::as_str),
            )
        })
        .collect::<Vec<_>>();
    let hdds = disks
        .iter()
        .copied()
        .filter(|device| {
            device
                .attributes
                .get("mediaType")
                .and_then(serde_json::Value::as_str)
                == Some("HDD")
        })
        .collect::<Vec<_>>();
    assert!(
        !ssds.is_empty(),
        "Storage API should identify at least one SSD"
    );
    assert!(
        !hdds.is_empty(),
        "Storage API should identify the known HDD"
    );

    let hdd_profile = build_device_profile(hdds[0].clone(), &snapshot.scanned_at_utc);
    for metric in [
        "disk.temperature.celsius",
        "disk.wear.provider_value",
        "disk.power_on_hours",
    ] {
        let capability = hdd_profile
            .capabilities
            .iter()
            .find(|capability| capability.metric_key == metric)
            .unwrap();
        assert_eq!(capability.state, CapabilityState::NotTested);
        assert_eq!(
            capability.reason_code.as_deref(),
            Some("HDD_QUERY_SKIPPED_TO_AVOID_WAKE")
        );
    }

    let mut tested_indexes = Vec::new();
    for ssd in ssds {
        let disk_number = ssd
            .attributes
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .unwrap() as u32;
        tested_indexes.push(disk_number);
        let capabilities =
            match corescope_lib::storage_reliability::windows::probe_disk_number(disk_number) {
                Ok(capabilities) => mark_missing_values(bind_device_id(capabilities, &ssd.id)),
                Err(error) => bind_device_id(
                    corescope_lib::storage_reliability::windows::failure_capabilities(
                        &ssd.id,
                        disk_number,
                        &error,
                    ),
                    &ssd.id,
                ),
            };
        assert_eq!(capabilities.len(), 11);
        for capability in &capabilities {
            capability.validate().unwrap();
        }
        database.save_capabilities(&capabilities).unwrap();
        let loaded = database.load_device_profile(&ssd.id).unwrap().unwrap();
        let mut states = BTreeMap::new();
        for capability in loaded
            .capabilities
            .iter()
            .filter(|capability| capability.source == "MSFT_StorageReliabilityCounter")
        {
            *states.entry(capability.state.as_str()).or_insert(0usize) += 1;
        }
        let native_codes = loaded
            .capabilities
            .iter()
            .filter(|capability| capability.source == "MSFT_StorageReliabilityCounter")
            .filter_map(|capability| capability.native_error_code.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        println!("M5 SSD disk={disk_number} states={states:?} native_codes={native_codes:?}");
        if states.contains_key("PERMISSION_REQUIRED") {
            assert_eq!(states.get("PERMISSION_REQUIRED"), Some(&11));
            for capability in loaded
                .capabilities
                .iter()
                .filter(|capability| capability.source == "MSFT_StorageReliabilityCounter")
            {
                assert_eq!(capability.state, CapabilityState::PermissionRequired);
                assert!(capability.value.is_none());
                assert_eq!(
                    capability.reason_code.as_deref(),
                    Some("STANDARD_USER_ACCESS_DENIED")
                );
                assert!(
                    capability
                        .native_error_code
                        .as_deref()
                        .is_some_and(|code| code.starts_with("HRESULT_0x"))
                );
            }
        }
    }
    let hdd_indexes = hdds
        .iter()
        .filter_map(|device| {
            device
                .attributes
                .get("index")
                .and_then(serde_json::Value::as_u64)
        })
        .map(|number| number as u32)
        .collect::<Vec<_>>();
    assert!(
        tested_indexes
            .iter()
            .all(|index| !hdd_indexes.contains(index))
    );
}
