#![cfg(windows)]

use corescope_lib::inventory::{DeviceKind, DeviceLayer, normalize_inventory, scan_local};
use corescope_lib::persistence::Database;
use std::collections::HashSet;
use tempfile::tempdir;

#[test]
fn discovers_real_local_devices_and_round_trips_them_through_sqlite() {
    let raw = scan_local().expect("standard-user WMI inventory scan should start");
    assert!(
        !raw.cpus.is_empty(),
        "Windows should report at least one processor"
    );
    let indexed_network_adapters = raw
        .network_adapters
        .iter()
        .filter(|adapter| adapter.interface_index.is_some())
        .count();
    assert!(
        indexed_network_adapters > 0,
        "Win32_NetworkAdapter should report interface indices for at least one adapter"
    );
    assert!(
        raw.pnp_nodes.len() >= 200,
        "PnP enumeration should include historical devices; observed {} nodes",
        raw.pnp_nodes.len()
    );

    let directory = tempdir().unwrap();
    let database_path = directory.path().join("live-inventory.sqlite3");
    let database = Database::open_at(&database_path).unwrap();
    let identity_salt = database.identity_salt().unwrap();
    let snapshot = normalize_inventory(&raw, &identity_salt);
    assert!(snapshot.count_kind(DeviceKind::Cpu) > 0);
    assert!(
        !snapshot
            .warnings
            .iter()
            .any(|warning| warning.source == "Win32_Battery"),
        "an absent battery should be a valid empty result, not a source error"
    );
    assert!(
        snapshot
            .devices
            .iter()
            .filter(|device| device.kind != DeviceKind::PnpNode)
            .all(|device| device.is_present == Some(true))
    );
    let mut seen_ids = HashSet::new();
    let duplicates: Vec<_> = snapshot
        .devices
        .iter()
        .filter(|device| !seen_ids.insert(device.id.clone()))
        .map(|device| {
            (
                device.id.clone(),
                device.kind,
                device.layer,
                device.display_name.clone(),
            )
        })
        .collect();
    assert!(
        duplicates.is_empty(),
        "normalized devices must have unique IDs: {duplicates:?}"
    );

    let serialized = serde_json::to_string(&snapshot).unwrap();
    for secret in raw
        .physical_disks
        .iter()
        .filter_map(|disk| disk.serial_number.as_deref())
    {
        if !secret.trim().is_empty() {
            assert!(
                !serialized.contains(secret.trim()),
                "disk serial must remain local identity material"
            );
        }
    }

    database
        .persist_inventory(&snapshot, env!("CARGO_PKG_VERSION"))
        .unwrap();
    let restored = database.load_inventory().unwrap().unwrap();
    assert_eq!(restored.devices.len(), snapshot.devices.len());
    assert_eq!(restored.edges.len(), snapshot.edges.len());
    assert_eq!(
        database.inventory_summary().unwrap().current_devices as usize,
        snapshot
            .devices
            .iter()
            .filter(|device| device.is_present == Some(true))
            .count()
    );

    let expected_device_ids: HashSet<_> = snapshot
        .devices
        .iter()
        .map(|device| device.id.clone())
        .collect();
    let expected_edges: HashSet<_> = snapshot
        .edges
        .iter()
        .map(|edge| {
            (
                edge.parent_id.clone(),
                edge.child_id.clone(),
                edge.relationship.clone(),
            )
        })
        .collect();

    drop(database);
    let reopened_database = Database::open_at(&database_path).unwrap();
    assert_eq!(reopened_database.identity_salt().unwrap(), identity_salt);
    let after_reopen = reopened_database.load_inventory().unwrap().unwrap();
    let restored_device_ids: HashSet<_> = after_reopen
        .devices
        .iter()
        .map(|device| device.id.clone())
        .collect();
    let restored_edges: HashSet<_> = after_reopen
        .edges
        .iter()
        .map(|edge| {
            (
                edge.parent_id.clone(),
                edge.child_id.clone(),
                edge.relationship.clone(),
            )
        })
        .collect();
    assert_eq!(restored_device_ids, expected_device_ids);
    assert_eq!(restored_edges, expected_edges);
    assert_eq!(after_reopen.scanned_at_utc, snapshot.scanned_at_utc);
    assert_eq!(
        reopened_database
            .inventory_summary()
            .unwrap()
            .current_devices as usize,
        snapshot
            .devices
            .iter()
            .filter(|device| device.is_present == Some(true))
            .count()
    );

    let physical = snapshot.count_layer(DeviceLayer::PhysicalDevice);
    let logical = snapshot.count_layer(DeviceLayer::LogicalDevice);
    let pnp = snapshot.count_layer(DeviceLayer::PnpNode);
    let current_warnings = snapshot.warnings.len();
    println!(
        "M2 live inventory: devices={} physical={} logical={} pnp={} edges={} source_warnings={}",
        snapshot.devices.len(),
        physical,
        logical,
        pnp,
        snapshot.edges.len(),
        current_warnings
    );
    println!(
        "M4 adapter mapping: network_adapters={} with_interface_index={indexed_network_adapters}",
        raw.network_adapters.len()
    );
    for warning in &snapshot.warnings {
        println!(
            "M2 source warning: source={} reason={} native={}",
            warning.source,
            warning.reason_code,
            warning.native_code.as_deref().unwrap_or("none")
        );
    }
    for kind in [
        DeviceKind::Cpu,
        DeviceKind::GpuAdapter,
        DeviceKind::MemoryModule,
        DeviceKind::PhysicalDisk,
        DeviceKind::Partition,
        DeviceKind::Volume,
        DeviceKind::NetworkAdapter,
        DeviceKind::Battery,
        DeviceKind::PnpNode,
    ] {
        println!("M2 live count: {:?}={}", kind, snapshot.count_kind(kind));
    }
}
