use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

mod scan;
#[cfg(windows)]
mod windows;

pub use scan::{ScanAbort, ScanCompletion, ScanContext, ScanCoordinator};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCategory {
    Cpu,
    Gpu,
    Memory,
    Storage,
    Network,
    Battery,
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Cpu,
    GpuAdapter,
    MemoryModule,
    PhysicalDisk,
    Partition,
    Volume,
    NetworkAdapter,
    Battery,
    PnpNode,
}

impl DeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::GpuAdapter => "gpu_adapter",
            Self::MemoryModule => "memory_module",
            Self::PhysicalDisk => "physical_disk",
            Self::Partition => "partition",
            Self::Volume => "volume",
            Self::NetworkAdapter => "network_adapter",
            Self::Battery => "battery",
            Self::PnpNode => "pnp_node",
        }
    }
}

impl DeviceCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Memory => "memory",
            Self::Storage => "storage",
            Self::Network => "network",
            Self::Battery => "battery",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceLayer {
    PhysicalDevice,
    LogicalDevice,
    Unknown,
    PnpNode,
}

impl DeviceLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PhysicalDevice => "physical_device",
            Self::LogicalDevice => "logical_device",
            Self::Unknown => "unknown",
            Self::PnpNode => "pnp_node",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IdentityConfidence {
    Strong,
    Moderate,
    Weak,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawInventory {
    pub cpus: Vec<RawCpu>,
    pub gpus: Vec<RawGpu>,
    pub memory_modules: Vec<RawMemoryModule>,
    pub physical_disks: Vec<RawPhysicalDisk>,
    pub partitions: Vec<RawPartition>,
    pub volumes: Vec<RawVolume>,
    pub disk_partition_links: Vec<RawDiskPartitionLink>,
    pub partition_volume_links: Vec<RawPartitionVolumeLink>,
    pub network_adapters: Vec<RawNetworkAdapter>,
    pub pnp_nodes: Vec<RawPnpNode>,
    pub batteries: Vec<RawBattery>,
    pub warnings: Vec<InventorySourceWarning>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct InventorySourceWarning {
    pub source: String,
    pub reason_code: String,
    pub native_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawCpu {
    pub name: String,
    pub manufacturer: Option<String>,
    pub identity_material: Option<String>,
    pub pnp_id: Option<String>,
    pub physical_cores: Option<u32>,
    pub logical_processors: Option<u32>,
    pub max_clock_mhz: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawGpu {
    pub name: String,
    pub manufacturer: Option<String>,
    pub identity_material: Option<String>,
    pub pnp_id: Option<String>,
    pub driver_version: Option<String>,
    pub driver_date: Option<String>,
    pub video_processor: Option<String>,
    pub memory_bytes: Option<u64>,
    pub virtual_hint: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawMemoryModule {
    pub name: String,
    pub manufacturer: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub identity_material: Option<String>,
    pub pnp_id: Option<String>,
    pub slot_locator: Option<String>,
    pub part_number: Option<String>,
    pub speed_mhz: Option<u32>,
    pub configured_clock_mhz: Option<u32>,
    pub form_factor: Option<u16>,
    pub memory_type_code: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawPhysicalDisk {
    pub index: u32,
    pub model: String,
    pub manufacturer: Option<String>,
    pub size_bytes: Option<u64>,
    pub serial_number: Option<String>,
    pub pnp_id: Option<String>,
    pub interface_type: Option<String>,
    pub media_type: Option<String>,
    pub reported_media_type: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawPartition {
    pub id: String,
    pub disk_index: u32,
    pub partition_number: u32,
    pub size_bytes: Option<u64>,
    pub name: Option<String>,
    pub boot_partition: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawVolume {
    pub id: String,
    pub partition_id: Option<String>,
    pub drive_letter: Option<String>,
    pub label: Option<String>,
    pub filesystem: Option<String>,
    pub size_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawDiskPartitionLink {
    pub disk_index: u32,
    pub partition_id: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawPartitionVolumeLink {
    pub partition_id: String,
    pub volume_id: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawNetworkAdapter {
    pub name: String,
    pub manufacturer: Option<String>,
    pub identity_material: Option<String>,
    pub pnp_id: Option<String>,
    pub physical_adapter: Option<bool>,
    pub connection_status: Option<u16>,
    pub speed_bps: Option<u64>,
    pub interface_index: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawPnpNode {
    pub instance_id: String,
    pub name: String,
    pub class_name: Option<String>,
    pub status: Option<String>,
    pub present: Option<bool>,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct RawBattery {
    pub name: String,
    pub manufacturer: Option<String>,
    pub identity_material: Option<String>,
    pub design_capacity_mwh: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceRecord {
    pub id: String,
    pub identity_hash: String,
    pub identity_confidence: IdentityConfidence,
    pub category: DeviceCategory,
    pub kind: DeviceKind,
    pub layer: DeviceLayer,
    pub display_name: String,
    pub manufacturer: Option<String>,
    pub is_virtual: Option<bool>,
    pub is_present: Option<bool>,
    pub source: String,
    pub attributes: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceEdge {
    pub parent_id: String,
    pub child_id: String,
    pub relationship: String,
    pub evidence_source: String,
    pub evidence_status: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventorySnapshot {
    pub scanned_at_utc: String,
    pub devices: Vec<DeviceRecord>,
    pub edges: Vec<DeviceEdge>,
    pub warnings: Vec<InventorySourceWarning>,
}

impl InventorySnapshot {
    pub fn count_kind(&self, expected: DeviceKind) -> usize {
        self.devices
            .iter()
            .filter(|device| device.kind == expected)
            .count()
    }

    pub fn count_layer(&self, expected: DeviceLayer) -> usize {
        self.devices
            .iter()
            .filter(|device| device.layer == expected)
            .count()
    }

    pub fn count_edge(&self, expected: &str) -> usize {
        self.edges
            .iter()
            .filter(|edge| edge.relationship == expected)
            .count()
    }
}

pub fn normalize_inventory(raw: &RawInventory, install_salt: &[u8]) -> InventorySnapshot {
    let mut devices = Vec::new();
    let mut edges = Vec::new();

    for cpu in &raw.cpus {
        push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Cpu,
            DeviceKind::Cpu,
            DeviceLayer::PhysicalDevice,
            &cpu.name,
            cpu.manufacturer.as_deref(),
            first_identity(&[cpu.identity_material.as_deref(), cpu.pnp_id.as_deref()]),
            Some(false),
            json!({
                "physicalCores": cpu.physical_cores,
                "logicalProcessors": cpu.logical_processors,
                "maxClockMhz": cpu.max_clock_mhz
            }),
        );
    }

    for gpu in &raw.gpus {
        let virtual_gpu = gpu.virtual_hint;
        let layer = if virtual_gpu == Some(true) {
            DeviceLayer::LogicalDevice
        } else {
            DeviceLayer::PhysicalDevice
        };
        push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Gpu,
            DeviceKind::GpuAdapter,
            layer,
            &gpu.name,
            gpu.manufacturer.as_deref(),
            first_identity(&[gpu.identity_material.as_deref(), gpu.pnp_id.as_deref()]),
            virtual_gpu,
            json!({
                "driverVersion": gpu.driver_version,
                "driverDate": gpu.driver_date,
                "videoProcessor": gpu.video_processor,
                "memoryBytes": gpu.memory_bytes,
                "pciVendorId": pci_component(gpu.pnp_id.as_deref(), "VEN_"),
                "pciDeviceId": pci_component(gpu.pnp_id.as_deref(), "DEV_")
            }),
        );
    }

    let mut memory_identity_counts = HashMap::<String, usize>::new();
    for module in &raw.memory_modules {
        if let Some(identity) = first_identity(&[
            module.identity_material.as_deref(),
            module.pnp_id.as_deref(),
        ]) {
            *memory_identity_counts
                .entry(identity.trim().to_ascii_lowercase())
                .or_default() += 1;
        }
    }
    let mut used_memory_identities = HashMap::<String, usize>::new();
    for (index, module) in raw.memory_modules.iter().enumerate() {
        let primary_identity = first_identity(&[
            module.identity_material.as_deref(),
            module.pnp_id.as_deref(),
        ]);
        let duplicated_primary = primary_identity.is_some_and(|identity| {
            memory_identity_counts
                .get(&identity.trim().to_ascii_lowercase())
                .copied()
                .unwrap_or_default()
                > 1
        });
        let slot_locator = module
            .slot_locator
            .as_deref()
            .filter(|value| !value.trim().is_empty());
        let (mut identity, mut confidence) = if duplicated_primary {
            if let (Some(primary), Some(slot)) = (primary_identity, slot_locator) {
                (
                    format!("{}|slot:{}", primary.trim(), slot.trim()),
                    IdentityConfidence::Moderate,
                )
            } else {
                (
                    format!(
                        "{}|{}|{}|instance:{index}",
                        primary_identity.unwrap_or_default().trim(),
                        module.name.trim(),
                        module.capacity_bytes.unwrap_or_default()
                    ),
                    IdentityConfidence::Weak,
                )
            }
        } else if let Some(primary) = primary_identity {
            (primary.trim().to_owned(), IdentityConfidence::Strong)
        } else if let Some(slot) = slot_locator {
            (
                format!(
                    "slot:{}|{}",
                    slot.trim(),
                    module.manufacturer.as_deref().unwrap_or_default().trim()
                ),
                IdentityConfidence::Moderate,
            )
        } else {
            (
                format!(
                    "{}|{}|{}|instance:{index}",
                    module.name.trim(),
                    module.manufacturer.as_deref().unwrap_or_default().trim(),
                    module.capacity_bytes.unwrap_or_default()
                ),
                IdentityConfidence::Weak,
            )
        };
        let duplicate_index = used_memory_identities.entry(identity.clone()).or_default();
        if *duplicate_index > 0 {
            identity.push_str(&format!("|instance:{index}"));
            confidence = IdentityConfidence::Weak;
        }
        *duplicate_index += 1;
        push_device_with_confidence(
            &mut devices,
            install_salt,
            DeviceCategory::Memory,
            DeviceKind::MemoryModule,
            DeviceLayer::PhysicalDevice,
            &module.name,
            module.manufacturer.as_deref(),
            Some(&identity),
            Some(confidence),
            Some(false),
            json!({
                "capacityBytes": module.capacity_bytes,
                "partNumber": module.part_number,
                "slotLocator": module.slot_locator,
                "speedMhz": module.speed_mhz,
                "configuredClockMhz": module.configured_clock_mhz,
                "formFactor": module.form_factor,
                "memoryTypeCode": module.memory_type_code
            }),
        );
    }

    let mut disk_ids = HashMap::new();
    for disk in &raw.physical_disks {
        let id = push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Storage,
            DeviceKind::PhysicalDisk,
            DeviceLayer::PhysicalDevice,
            &disk.model,
            disk.manufacturer.as_deref(),
            first_identity(&[
                disk.serial_number.as_deref(),
                disk.pnp_id.as_deref(),
                Some(&format!(
                    "{}|{}|{}",
                    disk.model,
                    disk.size_bytes.unwrap_or_default(),
                    disk.index
                )),
            ]),
            Some(false),
            json!({
                "index": disk.index,
                "sizeBytes": disk.size_bytes,
                "interfaceType": disk.interface_type,
                "mediaType": disk.media_type,
                "reportedMediaType": disk.reported_media_type,
                "status": disk.status
            }),
        );
        disk_ids.insert(disk.index, id);
    }

    let disk_identity: HashMap<u32, String> = raw
        .physical_disks
        .iter()
        .filter_map(|disk| disk_ids.get(&disk.index).map(|id| (disk.index, id.clone())))
        .collect();
    let mut partition_ids = HashMap::new();
    for partition in &raw.partitions {
        let parent_identity = disk_identity.get(&partition.disk_index).map(String::as_str);
        let identity = parent_identity
            .map(|parent| format!("{parent}|partition:{}", partition.partition_number))
            .unwrap_or_else(|| {
                format!(
                    "partition:{}:{}",
                    partition.disk_index, partition.partition_number
                )
            });
        let id = push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Storage,
            DeviceKind::Partition,
            DeviceLayer::LogicalDevice,
            partition.name.as_deref().unwrap_or("分区"),
            None,
            Some(&identity),
            Some(false),
            json!({
                "sizeBytes": partition.size_bytes,
                "partitionNumber": partition.partition_number,
                "bootPartition": partition.boot_partition,
                "role": partition_role(partition.name.as_deref(), partition.boot_partition)
            }),
        );
        partition_ids.insert(partition.id.as_str(), id);
    }

    let mut volume_ids = HashMap::new();
    for volume in &raw.volumes {
        let parent_identity = volume
            .partition_id
            .as_deref()
            .and_then(|partition_id| partition_ids.get(partition_id))
            .map(String::as_str);
        let identity = parent_identity
            .map(|parent| format!("{parent}|volume:{}", volume.id))
            .unwrap_or_else(|| format!("volume:{}", volume.id));
        let display_name = volume
            .label
            .as_deref()
            .filter(|label| !label.trim().is_empty())
            .map(|label| {
                format!(
                    "{} ({label})",
                    volume.drive_letter.as_deref().unwrap_or("卷")
                )
            })
            .unwrap_or_else(|| {
                volume
                    .drive_letter
                    .clone()
                    .unwrap_or_else(|| "卷".to_owned())
            });
        let id = push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Storage,
            DeviceKind::Volume,
            DeviceLayer::LogicalDevice,
            &display_name,
            None,
            Some(&identity),
            Some(false),
            json!({
                "driveLetter": volume.drive_letter,
                "label": volume.label,
                "filesystem": volume.filesystem,
                "sizeBytes": volume.size_bytes,
                "freeBytes": volume.free_bytes
            }),
        );
        volume_ids.insert(volume.id.as_str(), id);
    }

    for link in &raw.disk_partition_links {
        if let (Some(parent_id), Some(child_id)) = (
            disk_ids.get(&link.disk_index),
            partition_ids.get(link.partition_id.as_str()),
        ) {
            edges.push(DeviceEdge {
                parent_id: parent_id.clone(),
                child_id: child_id.clone(),
                relationship: "physical_disk_contains_partition".to_owned(),
                evidence_source: "WMI:Win32_DiskDriveToDiskPartition".to_owned(),
                evidence_status: "OS_REPORTED".to_owned(),
            });
        }
    }
    for link in &raw.partition_volume_links {
        if let (Some(parent_id), Some(child_id)) = (
            partition_ids.get(link.partition_id.as_str()),
            volume_ids.get(link.volume_id.as_str()),
        ) {
            edges.push(DeviceEdge {
                parent_id: parent_id.clone(),
                child_id: child_id.clone(),
                relationship: "partition_maps_to_volume".to_owned(),
                evidence_source: "WMI:Win32_LogicalDiskToPartition".to_owned(),
                evidence_status: "OS_REPORTED".to_owned(),
            });
        }
    }

    for adapter in &raw.network_adapters {
        let is_virtual = adapter.physical_adapter.map(|physical| !physical);
        push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Network,
            DeviceKind::NetworkAdapter,
            match adapter.physical_adapter {
                Some(true) => DeviceLayer::PhysicalDevice,
                Some(false) => DeviceLayer::LogicalDevice,
                None => DeviceLayer::Unknown,
            },
            &adapter.name,
            adapter.manufacturer.as_deref(),
            first_identity(&[
                adapter.identity_material.as_deref(),
                adapter.pnp_id.as_deref(),
            ]),
            is_virtual,
            json!({
                "connectionStatus": adapter.connection_status,
                "speedBps": adapter.speed_bps,
                "interfaceIndex": adapter.interface_index
            }),
        );
    }

    for battery in &raw.batteries {
        push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Battery,
            DeviceKind::Battery,
            DeviceLayer::PhysicalDevice,
            &battery.name,
            battery.manufacturer.as_deref(),
            first_identity(&[battery.identity_material.as_deref()]),
            Some(false),
            json!({"designCapacityMwh": battery.design_capacity_mwh}),
        );
    }

    let represented_pnp_ids: std::collections::HashSet<String> = raw
        .gpus
        .iter()
        .filter_map(|device| device.pnp_id.as_deref())
        .chain(
            raw.physical_disks
                .iter()
                .filter_map(|device| device.pnp_id.as_deref()),
        )
        .chain(
            raw.network_adapters
                .iter()
                .filter_map(|device| device.pnp_id.as_deref()),
        )
        .map(normalize_pnp_id)
        .collect();
    for node in &raw.pnp_nodes {
        if represented_pnp_ids.contains(&normalize_pnp_id(&node.instance_id)) {
            continue;
        }
        push_device(
            &mut devices,
            install_salt,
            DeviceCategory::Other,
            DeviceKind::PnpNode,
            DeviceLayer::PnpNode,
            &node.name,
            None,
            first_identity(&[Some(node.instance_id.as_str())]),
            None,
            json!({"className": node.class_name, "status": node.status, "present": node.present}),
        );
        if let Some(device) = devices.last_mut() {
            device.is_present = node.present;
            device.source = if node.source.is_empty() {
                "PnP".to_owned()
            } else {
                node.source.clone()
            };
        }
    }

    InventorySnapshot {
        scanned_at_utc: chrono::Utc::now().to_rfc3339(),
        devices,
        edges,
        warnings: raw.warnings.clone(),
    }
}

fn push_device(
    devices: &mut Vec<DeviceRecord>,
    install_salt: &[u8],
    category: DeviceCategory,
    kind: DeviceKind,
    layer: DeviceLayer,
    display_name: &str,
    manufacturer: Option<&str>,
    identity_material: Option<&str>,
    is_virtual: Option<bool>,
    attributes: Value,
) -> String {
    push_device_with_confidence(
        devices,
        install_salt,
        category,
        kind,
        layer,
        display_name,
        manufacturer,
        identity_material,
        None,
        is_virtual,
        attributes,
    )
}

fn pci_component(pnp_id: Option<&str>, marker: &str) -> Option<u16> {
    let pnp_id = pnp_id?;
    let upper = pnp_id.to_ascii_uppercase();
    let start = upper.find(marker)?.checked_add(marker.len())?;
    let digits = upper.get(start..start.checked_add(4)?)?;
    u16::from_str_radix(digits, 16).ok()
}

#[allow(clippy::too_many_arguments)]
fn push_device_with_confidence(
    devices: &mut Vec<DeviceRecord>,
    install_salt: &[u8],
    category: DeviceCategory,
    kind: DeviceKind,
    layer: DeviceLayer,
    display_name: &str,
    manufacturer: Option<&str>,
    identity_material: Option<&str>,
    identity_confidence: Option<IdentityConfidence>,
    is_virtual: Option<bool>,
    attributes: Value,
) -> String {
    let material = identity_material
        .filter(|value| !value.trim().is_empty())
        .map(|value| (value.trim(), IdentityConfidence::Strong))
        .unwrap_or_else(|| (display_name.trim(), IdentityConfidence::Weak));
    let identity_hash = hash_identity(install_salt, kind.as_str(), material.0);
    let id = format!("dev_{}", &identity_hash[..32]);
    devices.push(DeviceRecord {
        id: id.clone(),
        identity_hash,
        identity_confidence: identity_confidence.unwrap_or(material.1),
        category,
        kind,
        layer,
        display_name: display_name.trim().to_owned(),
        manufacturer: manufacturer
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        is_virtual,
        is_present: Some(true),
        source: "WMI".to_owned(),
        attributes,
    });
    id
}

fn normalize_pnp_id(id: &str) -> String {
    id.trim().replace('/', "\\").to_ascii_uppercase()
}

fn first_identity<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values
        .iter()
        .copied()
        .flatten()
        .find(|value| !value.trim().is_empty())
}

fn hash_identity(salt: &[u8], kind: &str, material: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(salt);
    digest.update([0]);
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(material.trim().to_ascii_lowercase().as_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn partition_role(name: Option<&str>, boot_partition: Option<bool>) -> &'static str {
    let lower = name.unwrap_or_default().to_ascii_lowercase();
    if lower.contains("system") || lower.contains("reserved") || boot_partition == Some(true) {
        "system_or_reserved"
    } else {
        "ordinary"
    }
}

#[cfg(windows)]
pub fn scan_local() -> Result<RawInventory, String> {
    let coordinator = ScanCoordinator::default();
    let (context, completion) = coordinator.begin()?;
    let result = scan_local_with_context(context);
    drop(completion);
    result
}

#[cfg(not(windows))]
pub fn scan_local() -> Result<RawInventory, String> {
    Err("INVENTORY_WINDOWS_ONLY".to_owned())
}

#[cfg(windows)]
pub fn scan_local_with_context(context: ScanContext) -> Result<RawInventory, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|_| "SCAN_RUNTIME_UNAVAILABLE".to_owned())?;
    runtime.block_on(windows::scan_local_with_context(context))
}

#[cfg(not(windows))]
pub fn scan_local_with_context(_context: ScanContext) -> Result<RawInventory, String> {
    Err("INVENTORY_WINDOWS_ONLY".to_owned())
}
