use corescope_lib::inventory::{
    DeviceCategory, DeviceKind, DeviceLayer, IdentityConfidence, RawInventory, normalize_inventory,
};

const INSTALL_SALT: &[u8] = b"simulated-install-salt";

fn fixture(name: &str) -> RawInventory {
    let json = match name {
        "desktop_nvidia_ssd_hdd" => include_str!("fixtures/desktop_nvidia_ssd_hdd.json"),
        "igpu_only" => include_str!("fixtures/igpu_only.json"),
        "two_identical_ssd" => include_str!("fixtures/two_identical_ssd.json"),
        _ => panic!("unknown fixture"),
    };
    let value: serde_json::Value = serde_json::from_str(json).unwrap();
    assert_eq!(
        value["simulated"], true,
        "fixture data must be labeled simulated"
    );
    serde_json::from_value(value["inventory"].clone()).unwrap()
}

#[test]
fn desktop_fixture_separates_entities_from_pnp_nodes_and_preserves_proven_edges() {
    let inventory = fixture("desktop_nvidia_ssd_hdd");
    let snapshot = normalize_inventory(&inventory, INSTALL_SALT);

    assert_eq!(snapshot.count_kind(DeviceKind::Cpu), 1);
    assert_eq!(snapshot.count_kind(DeviceKind::GpuAdapter), 2);
    assert_eq!(snapshot.count_kind(DeviceKind::MemoryModule), 4);
    assert_eq!(snapshot.count_kind(DeviceKind::PhysicalDisk), 3);
    assert_eq!(snapshot.count_kind(DeviceKind::Partition), 5);
    assert_eq!(snapshot.count_kind(DeviceKind::Volume), 3);
    assert_eq!(snapshot.count_kind(DeviceKind::NetworkAdapter), 2);
    assert_eq!(snapshot.count_layer(DeviceLayer::PnpNode), 3);
    assert_eq!(
        snapshot
            .devices
            .iter()
            .filter(|device| device.layer == DeviceLayer::PhysicalDevice)
            .count(),
        11
    );

    assert_eq!(snapshot.count_edge("physical_disk_contains_partition"), 5);
    assert_eq!(snapshot.count_edge("partition_maps_to_volume"), 3);
    assert!(
        snapshot
            .edges
            .iter()
            .all(|edge| edge.evidence_status == "OS_REPORTED")
    );
    assert!(
        snapshot
            .devices
            .iter()
            .filter(|device| device.layer == DeviceLayer::PnpNode)
            .all(|device| device.kind == DeviceKind::PnpNode)
    );
    assert!(snapshot.devices.iter().any(|device| {
        device.category == DeviceCategory::Gpu
            && device.display_name.contains("Oray")
            && device.is_virtual == Some(true)
    }));

    let serialized = serde_json::to_string(&snapshot).unwrap();
    assert!(!serialized.contains("SIM-RTX4060"));
    assert!(!serialized.contains("SIM-SN580"));
    assert!(!serialized.contains("PCI\\VEN_10DE"));
}

#[test]
fn gpu_pci_vendor_and_product_ids_are_stored_without_raw_pnp_instance_paths() {
    let snapshot = normalize_inventory(&fixture("desktop_nvidia_ssd_hdd"), INSTALL_SALT);
    let nvidia = snapshot
        .devices
        .iter()
        .find(|device| device.display_name == "NVIDIA GeForce RTX 4060")
        .unwrap();

    assert_eq!(nvidia.attributes["pciVendorId"], 0x10de);
    assert_eq!(nvidia.attributes["pciDeviceId"], 0x2882);
    assert!(nvidia.attributes.get("pnpId").is_none());
}

#[test]
fn igpu_fixture_does_not_invent_a_discrete_gpu_or_battery() {
    let snapshot = normalize_inventory(&fixture("igpu_only"), INSTALL_SALT);

    assert_eq!(snapshot.count_kind(DeviceKind::GpuAdapter), 1);
    assert_eq!(snapshot.count_kind(DeviceKind::Battery), 0);
    assert!(snapshot.devices.iter().any(|device| {
        device.kind == DeviceKind::GpuAdapter
            && device.display_name.contains("Intel")
            && device.is_virtual == Some(false)
    }));
}

#[test]
fn identical_ssd_models_keep_separate_stable_ids_using_local_identity_material() {
    let raw = fixture("two_identical_ssd");
    let first = normalize_inventory(&raw, INSTALL_SALT);
    let second = normalize_inventory(&raw, INSTALL_SALT);

    let first_ids: Vec<_> = first
        .devices
        .iter()
        .filter(|device| device.kind == DeviceKind::PhysicalDisk)
        .map(|device| {
            (
                &device.id,
                &device.identity_hash,
                device.identity_confidence,
            )
        })
        .collect();
    let second_ids: Vec<_> = second
        .devices
        .iter()
        .filter(|device| device.kind == DeviceKind::PhysicalDisk)
        .map(|device| {
            (
                &device.id,
                &device.identity_hash,
                device.identity_confidence,
            )
        })
        .collect();

    assert_eq!(first_ids, second_ids);
    assert_ne!(first_ids[0].0, first_ids[1].0);
    assert!(
        first_ids
            .iter()
            .all(|(_, _, confidence)| *confidence == IdentityConfidence::Strong)
    );
}

#[test]
fn pnp_fallback_nodes_are_not_promoted_to_physical_hardware() {
    let snapshot = normalize_inventory(&fixture("desktop_nvidia_ssd_hdd"), INSTALL_SALT);
    let pnp_count = snapshot.count_layer(DeviceLayer::PnpNode);
    let physical_count = snapshot
        .devices
        .iter()
        .filter(|device| device.layer == DeviceLayer::PhysicalDevice)
        .count();

    assert_eq!(pnp_count, 3);
    assert_eq!(physical_count, 11);
    assert!(pnp_count < physical_count);
}

#[test]
fn duplicate_memory_serials_are_disambiguated_by_stable_slot_location() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "memory_modules": [
            {"name":"Same DIMM Model","manufacturer":"Vendor","capacity_bytes":17_179_869_184u64,"identity_material":"DUPLICATE-SERIAL","slot_locator":"DIMM_A1"},
            {"name":"Same DIMM Model","manufacturer":"Vendor","capacity_bytes":17_179_869_184u64,"identity_material":"DUPLICATE-SERIAL","slot_locator":"DIMM_B1"}
        ]
    })).unwrap();
    let first = normalize_inventory(&raw, INSTALL_SALT);
    let second = normalize_inventory(&raw, INSTALL_SALT);
    let ids: Vec<_> = first.devices.iter().map(|device| &device.id).collect();

    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert_eq!(first.devices[0].id, second.devices[0].id);
    assert_eq!(first.devices[1].id, second.devices[1].id);
}

#[test]
fn historical_pnp_nodes_remain_historical_and_direct_hardware_is_not_duplicated() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "gpus": [{
            "name": "Example GPU",
            "pnp_id": "PCI/ven_1234&dev_5678",
            "identity_material": "SIM-GPU"
        }],
        "pnp_nodes": [
            {"instance_id":"PCI\\VEN_1234&DEV_5678", "name":"GPU PnP entry", "present":true},
            {"instance_id":"USB\\VID_0001&PID_0002", "name":"Removed device", "present":false, "source":"SetupAPI"}
        ]
    })).unwrap();
    let snapshot = normalize_inventory(&raw, INSTALL_SALT);
    let pnp: Vec<_> = snapshot
        .devices
        .iter()
        .filter(|device| device.kind == DeviceKind::PnpNode)
        .collect();

    assert_eq!(snapshot.count_kind(DeviceKind::GpuAdapter), 1);
    assert_eq!(
        pnp.len(),
        1,
        "direct GPU and its PnP entry should be represented once"
    );
    assert_eq!(pnp[0].is_present, Some(false));
    assert_eq!(pnp[0].source, "SetupAPI");
}

#[test]
fn network_adapter_with_unreported_physicality_stays_unclassified() {
    let raw: RawInventory = serde_json::from_value(serde_json::json!({
        "network_adapters": [{"name":"Adapter","physical_adapter":null}]
    }))
    .unwrap();
    let snapshot = normalize_inventory(&raw, INSTALL_SALT);
    let adapter = snapshot.devices.first().unwrap();

    assert_eq!(adapter.kind, DeviceKind::NetworkAdapter);
    assert_eq!(adapter.layer, DeviceLayer::Unknown);
    assert_eq!(adapter.is_virtual, None);
}
