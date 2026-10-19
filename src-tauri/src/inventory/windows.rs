use super::{
    InventorySourceWarning, RawBattery, RawCpu, RawDiskPartitionLink, RawGpu, RawInventory,
    RawMemoryModule, RawNetworkAdapter, RawPartition, RawPartitionVolumeLink, RawPhysicalDisk,
    RawPnpNode, RawVolume, ScanAbort, ScanContext,
};
use crate::storage_reliability::{MediaKind, resolve_media_kind};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::ptr;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA, SPDRP_CLASS, SPDRP_DEVICEDESC,
    SPDRP_FRIENDLYNAME, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW,
};
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_ITEMS, GetLastError,
};
use wmi::{WMIConnection, WMIError};

pub async fn scan_local_with_context(context: ScanContext) -> Result<RawInventory, String> {
    context.check().map_err(|abort| abort.code().to_owned())?;
    let connection = WMIConnection::new()
        .map_err(|error| format!("WMI_CONNECT_FAILED: {}", safe_error_code(&error)))?;
    context.check().map_err(|abort| abort.code().to_owned())?;
    let mut inventory = RawInventory::default();

    macro_rules! wmi_query {
        ($statement:literal, $source:literal) => {
            query(
                &connection,
                $statement,
                $source,
                &mut inventory.warnings,
                &context,
            )
            .await
            .map_err(|abort| abort.code().to_owned())?
        };
    }

    let cpus: Vec<CpuRow> = wmi_query!(
        "SELECT Name, Manufacturer, ProcessorId, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed FROM Win32_Processor",
        "Win32_Processor"
    );
    inventory.cpus = cpus
        .into_iter()
        .map(|row| RawCpu {
            name: row.name.unwrap_or_else(|| "处理器".to_owned()),
            manufacturer: row.manufacturer,
            identity_material: row.processor_id,
            pnp_id: None,
            physical_cores: row.number_of_cores,
            logical_processors: row.number_of_logical_processors,
            max_clock_mhz: row.max_clock_speed,
        })
        .collect();

    let gpus: Vec<GpuRow> = wmi_query!(
        "SELECT Name, AdapterCompatibility, PNPDeviceID, DriverVersion, DriverDate, AdapterRAM, VideoProcessor FROM Win32_VideoController",
        "Win32_VideoController"
    );
    inventory.gpus = gpus
        .into_iter()
        .map(|row| {
            let pnp_id = row.pnp_device_id;
            let virtual_hint = pnp_id.as_deref().map(is_virtual_pnp_id);
            RawGpu {
                name: row.name.unwrap_or_else(|| "显示适配器".to_owned()),
                manufacturer: row.adapter_compatibility,
                identity_material: pnp_id.clone(),
                pnp_id,
                driver_version: row.driver_version,
                driver_date: row.driver_date,
                video_processor: row.video_processor,
                memory_bytes: row.adapter_ram.map(u64::from),
                virtual_hint,
            }
        })
        .collect();

    let memory: Vec<MemoryRow> = wmi_query!(
        "SELECT Manufacturer, Capacity, SerialNumber, PartNumber, DeviceLocator, Speed, ConfiguredClockSpeed, FormFactor, SMBIOSMemoryType, Tag FROM Win32_PhysicalMemory",
        "Win32_PhysicalMemory"
    );
    inventory.memory_modules = memory
        .into_iter()
        .map(|row| {
            let slot_locator = row.device_locator;
            let part_number = row.part_number;
            let name = part_number
                .clone()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| slot_locator.clone())
                .unwrap_or_else(|| "内存模组".to_owned());
            RawMemoryModule {
                name,
                manufacturer: row.manufacturer,
                capacity_bytes: row.capacity,
                identity_material: row
                    .serial_number
                    .filter(|value| !value.trim().is_empty())
                    .or(row.tag),
                pnp_id: None,
                slot_locator,
                part_number,
                speed_mhz: row.speed,
                configured_clock_mhz: row.configured_clock_speed,
                form_factor: row.form_factor,
                memory_type_code: row.smbios_memory_type,
            }
        })
        .collect();

    let disks: Vec<DiskRow> = wmi_query!(
        "SELECT Index, Model, Manufacturer, Size, SerialNumber, PNPDeviceID, InterfaceType, MediaType, Status FROM Win32_DiskDrive",
        "Win32_DiskDrive"
    );
    let storage_media_types = storage_media_types_by_disk_number(&context, &mut inventory.warnings)
        .await
        .map_err(|abort| abort.code().to_owned())?;
    inventory.physical_disks = disks
        .into_iter()
        .map(|row| {
            let storage_media_type = row
                .index
                .and_then(|index| storage_media_types.get(&index).copied());
            let reported_media_type = row.media_type;
            let media_code = storage_media_type.map(|kind| match kind {
                MediaKind::Hdd => 3,
                MediaKind::Ssd => 4,
                MediaKind::Scm => 5,
                MediaKind::Unknown => 0,
            });
            let media_kind = resolve_media_kind(media_code, reported_media_type.as_deref());
            RawPhysicalDisk {
                index: row.index.unwrap_or_default(),
                model: row.model.unwrap_or_else(|| "物理磁盘".to_owned()),
                manufacturer: row.manufacturer,
                size_bytes: row.size,
                serial_number: row.serial_number,
                pnp_id: row.pnp_device_id,
                interface_type: row.interface_type,
                media_type: media_kind.as_str().map(ToOwned::to_owned),
                reported_media_type,
                status: row.status,
            }
        })
        .collect();

    let partitions: Vec<PartitionRow> = wmi_query!(
        "SELECT DeviceID, DiskIndex, Index, Size, Name, BootPartition FROM Win32_DiskPartition",
        "Win32_DiskPartition"
    );
    inventory.partitions = partitions
        .into_iter()
        .map(|row| RawPartition {
            id: row.device_id.clone().unwrap_or_default(),
            disk_index: row.disk_index.unwrap_or_default(),
            partition_number: row.index.unwrap_or_default(),
            size_bytes: row.size,
            name: row.name,
            boot_partition: row.boot_partition,
        })
        .collect();

    let volumes: Vec<VolumeRow> = wmi_query!(
        "SELECT DeviceID, VolumeName, FileSystem, Size, FreeSpace FROM Win32_LogicalDisk WHERE DriveType = 3",
        "Win32_LogicalDisk"
    );
    inventory.volumes = volumes
        .into_iter()
        .map(|row| RawVolume {
            id: row.device_id.clone().unwrap_or_default(),
            partition_id: None,
            drive_letter: row.device_id,
            label: row.volume_name,
            filesystem: row.file_system,
            size_bytes: row.size,
            free_bytes: row.free_space,
        })
        .collect();

    let disk_partition_links: Vec<AssociationRow> = wmi_query!(
        "SELECT Antecedent, Dependent FROM Win32_DiskDriveToDiskPartition",
        "Win32_DiskDriveToDiskPartition"
    );
    for association in disk_partition_links {
        let Some(index) = association
            .antecedent
            .as_deref()
            .and_then(physical_drive_index)
        else {
            continue;
        };
        let Some(partition_id) = association.dependent.as_deref().and_then(reference_value) else {
            continue;
        };
        if inventory
            .partitions
            .iter()
            .any(|partition| partition.id.eq_ignore_ascii_case(partition_id))
        {
            inventory.disk_partition_links.push(RawDiskPartitionLink {
                disk_index: index,
                partition_id: partition_id.to_owned(),
            });
        }
    }

    let partition_volume_links: Vec<AssociationRow> = wmi_query!(
        "SELECT Antecedent, Dependent FROM Win32_LogicalDiskToPartition",
        "Win32_LogicalDiskToPartition"
    );
    for association in partition_volume_links {
        let Some(partition_id) = association.antecedent.as_deref().and_then(reference_value) else {
            continue;
        };
        let Some(volume_id) = association.dependent.as_deref().and_then(reference_value) else {
            continue;
        };
        if inventory
            .partitions
            .iter()
            .any(|partition| partition.id.eq_ignore_ascii_case(partition_id))
            && inventory
                .volumes
                .iter()
                .any(|volume| volume.id.eq_ignore_ascii_case(volume_id))
        {
            inventory
                .partition_volume_links
                .push(RawPartitionVolumeLink {
                    partition_id: partition_id.to_owned(),
                    volume_id: volume_id.to_owned(),
                });
        }
    }
    for link in &inventory.partition_volume_links {
        if let Some(volume) = inventory
            .volumes
            .iter_mut()
            .find(|volume| volume.id.eq_ignore_ascii_case(&link.volume_id))
        {
            volume.partition_id = Some(link.partition_id.clone());
        }
    }

    let adapters: Vec<NetworkRow> = wmi_query!(
        "SELECT Name, Manufacturer, PNPDeviceID, PhysicalAdapter, NetConnectionStatus, Speed, InterfaceIndex, GUID FROM Win32_NetworkAdapter",
        "Win32_NetworkAdapter"
    );
    inventory.network_adapters = adapters
        .into_iter()
        .map(|row| {
            let identity_material = row.pnp_device_id.clone().or(row.guid.clone());
            RawNetworkAdapter {
                name: row.name.unwrap_or_else(|| "网络适配器".to_owned()),
                manufacturer: row.manufacturer,
                identity_material,
                pnp_id: row.pnp_device_id,
                physical_adapter: row.physical_adapter,
                connection_status: row.net_connection_status,
                speed_bps: row.speed,
                interface_index: row.interface_index,
            }
        })
        .collect();

    let batteries: Vec<BatteryRow> = wmi_query!(
        "SELECT DeviceID, Name, DesignCapacity FROM Win32_Battery",
        "Win32_Battery"
    );
    inventory.batteries = batteries
        .into_iter()
        .map(|row| RawBattery {
            name: row.name.unwrap_or_else(|| "电池".to_owned()),
            manufacturer: row.manufacturer,
            identity_material: row.device_id,
            design_capacity_mwh: row.design_capacity,
        })
        .collect();

    inventory.pnp_nodes = match setupapi_pnp_nodes(&mut inventory.warnings, &context)
        .map_err(|abort| abort.code().to_owned())?
    {
        Some(nodes) => nodes,
        None => {
            let pnp_nodes: Vec<PnpRow> = wmi_query!(
                "SELECT DeviceID, Name, PNPClass, Status, Present FROM Win32_PnPEntity",
                "Win32_PnPEntity"
            );
            pnp_nodes
                .into_iter()
                .filter_map(|row| {
                    Some(RawPnpNode {
                        instance_id: row.device_id?,
                        name: row.name.unwrap_or_else(|| "PnP 设备".to_owned()),
                        class_name: row.pnp_class,
                        status: row.status,
                        present: row.present,
                        source: "WMI:Win32_PnPEntity".to_owned(),
                    })
                })
                .collect()
        }
    };

    context.check().map_err(|abort| abort.code().to_owned())?;
    Ok(inventory)
}

async fn storage_media_types_by_disk_number(
    context: &ScanContext,
    warnings: &mut Vec<InventorySourceWarning>,
) -> Result<HashMap<u32, MediaKind>, ScanAbort> {
    const STORAGE_NAMESPACE: &str = "ROOT\\Microsoft\\Windows\\Storage";
    context.check()?;
    let connection = match WMIConnection::with_namespace_path(STORAGE_NAMESPACE) {
        Ok(connection) => connection,
        Err(error) => {
            let native_code = safe_error_code(&error);
            warnings.push(InventorySourceWarning {
                source: "MSFT_PhysicalDisk".to_owned(),
                reason_code: "STORAGE_MEDIA_TYPE_QUERY_FAILED".to_owned(),
                native_code: Some(native_code.clone()),
            });
            tracing::warn!(
                reason_code = "STORAGE_MEDIA_TYPE_QUERY_FAILED",
                native_code,
                "Storage media type source unavailable"
            );
            return Ok(HashMap::new());
        }
    };
    context.check()?;

    let rows: Result<Vec<StorageMediaRow>, WMIError> = match context
        .race(connection.async_raw_query("SELECT DeviceId, MediaType FROM MSFT_PhysicalDisk"))
        .await?
    {
        Ok(rows) => Ok(rows),
        Err(error) => Err(error),
    };
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            let native_code = safe_error_code(&error);
            warnings.push(InventorySourceWarning {
                source: "MSFT_PhysicalDisk".to_owned(),
                reason_code: "STORAGE_MEDIA_TYPE_QUERY_FAILED".to_owned(),
                native_code: Some(native_code.clone()),
            });
            tracing::warn!(
                reason_code = "STORAGE_MEDIA_TYPE_QUERY_FAILED",
                native_code,
                "Storage media type source unavailable"
            );
            return Ok(HashMap::new());
        }
    };

    let mut media_types = HashMap::new();
    let mut ambiguous = std::collections::HashSet::new();
    for row in rows {
        context.check()?;
        let Some(number) = row.device_id.as_deref().and_then(parse_storage_disk_number) else {
            continue;
        };
        let media_kind = resolve_media_kind(row.media_type, None);
        if media_kind == MediaKind::Unknown || ambiguous.contains(&number) {
            media_types.remove(&number);
            ambiguous.insert(number);
            continue;
        }
        if media_types.insert(number, media_kind).is_some() {
            media_types.remove(&number);
            ambiguous.insert(number);
        }
    }
    Ok(media_types)
}

fn parse_storage_disk_number(value: &str) -> Option<u32> {
    let value = value.trim();
    value.parse::<u32>().ok().or_else(|| {
        value
            .rsplit_once("PhysicalDisk")
            .and_then(|(_, suffix)| suffix.parse::<u32>().ok())
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StorageMediaRow {
    device_id: Option<String>,
    media_type: Option<u16>,
}

fn setupapi_pnp_nodes(
    warnings: &mut Vec<InventorySourceWarning>,
    context: &ScanContext,
) -> Result<Option<Vec<RawPnpNode>>, ScanAbort> {
    context.check()?;
    let all_devices = match DeviceInfoSet::new(DIGCF_ALLCLASSES) {
        Ok(set) => set,
        Err(code) => {
            setupapi_warning(warnings, "SETUPAPI_DEVICE_SET_FAILED", code);
            return Ok(None);
        }
    };
    context.check()?;
    let present_devices = match DeviceInfoSet::new(DIGCF_ALLCLASSES | DIGCF_PRESENT) {
        Ok(set) => set,
        Err(code) => {
            setupapi_warning(warnings, "SETUPAPI_PRESENT_SET_FAILED", code);
            return Ok(None);
        }
    };

    let Some(present_ids) = enumerate_setupapi_ids(present_devices.handle, warnings, context)?
    else {
        return Ok(None);
    };
    let mut nodes = Vec::new();
    let mut index = 0;
    loop {
        context.check()?;
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        let enumerated = unsafe {
            SetupDiEnumDeviceInfo(all_devices.handle, index, &mut info as *mut SP_DEVINFO_DATA)
        };
        if enumerated == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_NO_MORE_ITEMS {
                setupapi_warning(warnings, "SETUPAPI_ENUMERATION_FAILED", code);
            }
            break;
        }
        index += 1;

        let Some(instance_id) = setupapi_instance_id(all_devices.handle, &info) else {
            setupapi_warning(warnings, "SETUPAPI_INSTANCE_ID_UNAVAILABLE", unsafe {
                GetLastError()
            });
            continue;
        };
        let name = setupapi_string_property(all_devices.handle, &info, SPDRP_FRIENDLYNAME)
            .or_else(|| setupapi_string_property(all_devices.handle, &info, SPDRP_DEVICEDESC))
            .unwrap_or_else(|| "PnP 设备".to_owned());
        let class_name = setupapi_string_property(all_devices.handle, &info, SPDRP_CLASS);
        let present = present_ids.contains(&normalize_id(&instance_id));
        nodes.push(RawPnpNode {
            instance_id,
            name,
            class_name,
            status: None,
            present: Some(present),
            source: "SetupAPI".to_owned(),
        });
    }
    Ok(Some(nodes))
}

fn enumerate_setupapi_ids(
    handle: HDEVINFO,
    warnings: &mut Vec<InventorySourceWarning>,
    context: &ScanContext,
) -> Result<Option<HashSet<String>>, ScanAbort> {
    let mut ids = HashSet::new();
    let mut index = 0;
    loop {
        context.check()?;
        let mut info = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        let enumerated =
            unsafe { SetupDiEnumDeviceInfo(handle, index, &mut info as *mut SP_DEVINFO_DATA) };
        if enumerated == 0 {
            let code = unsafe { GetLastError() };
            if code != ERROR_NO_MORE_ITEMS {
                setupapi_warning(warnings, "SETUPAPI_PRESENT_ENUMERATION_FAILED", code);
            }
            break;
        }
        index += 1;
        if let Some(instance_id) = setupapi_instance_id(handle, &info) {
            ids.insert(normalize_id(&instance_id));
        } else {
            setupapi_warning(warnings, "SETUPAPI_PRESENT_ID_UNAVAILABLE", unsafe {
                GetLastError()
            });
        }
    }
    Ok(Some(ids))
}

fn setupapi_instance_id(handle: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
    let mut required = 0u32;
    let first =
        unsafe { SetupDiGetDeviceInstanceIdW(handle, info, ptr::null_mut(), 0, &mut required) };
    if first != 0 || required == 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let mut buffer = vec![0u16; required as usize];
    let read = unsafe {
        SetupDiGetDeviceInstanceIdW(
            handle,
            info,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut required,
        )
    };
    if read == 0 {
        return None;
    }
    Some(wide_string(&buffer))
}

fn setupapi_string_property(
    handle: HDEVINFO,
    info: &SP_DEVINFO_DATA,
    property: u32,
) -> Option<String> {
    let mut property_type = 0u32;
    let mut required = 0u32;
    let first = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            handle,
            info,
            property,
            &mut property_type,
            ptr::null_mut(),
            0,
            &mut required,
        )
    };
    if first != 0 || required == 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return None;
    }
    let word_count = (required as usize).div_ceil(size_of::<u16>());
    let mut buffer = vec![0u16; word_count];
    let read = unsafe {
        SetupDiGetDeviceRegistryPropertyW(
            handle,
            info,
            property,
            &mut property_type,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * size_of::<u16>()) as u32,
            &mut required,
        )
    };
    if read == 0 {
        return None;
    }
    let value = wide_string(&buffer);
    (!value.trim().is_empty()).then_some(value)
}

fn wide_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_owned()
}

fn setupapi_warning(warnings: &mut Vec<InventorySourceWarning>, reason: &str, code: u32) {
    warnings.push(InventorySourceWarning {
        source: "SetupAPI".to_owned(),
        reason_code: reason.to_owned(),
        native_code: Some(format!("WIN32_0x{code:08X}")),
    });
}

struct DeviceInfoSet {
    handle: HDEVINFO,
}

impl DeviceInfoSet {
    fn new(flags: u32) -> Result<Self, u32> {
        let handle =
            unsafe { SetupDiGetClassDevsW(ptr::null(), ptr::null(), ptr::null_mut(), flags) };
        if handle == -1 {
            Err(unsafe { GetLastError() })
        } else {
            Ok(Self { handle })
        }
    }
}

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.handle);
        }
    }
}

async fn query<T: for<'de> Deserialize<'de>>(
    connection: &WMIConnection,
    statement: &str,
    source: &'static str,
    warnings: &mut Vec<InventorySourceWarning>,
    context: &ScanContext,
) -> Result<Vec<T>, ScanAbort> {
    let result = context.race(connection.async_raw_query(statement)).await?;
    match result {
        Ok(rows) => Ok(rows),
        Err(error) => {
            let native_code = safe_error_code(&error);
            warnings.push(InventorySourceWarning {
                source: source.to_owned(),
                reason_code: "WMI_QUERY_FAILED".to_owned(),
                native_code: Some(native_code.clone()),
            });
            tracing::warn!(
                source,
                reason_code = "WMI_QUERY_FAILED",
                native_code,
                "Hardware inventory source unavailable"
            );
            Ok(Vec::new())
        }
    }
}

fn safe_error_code(error: &WMIError) -> String {
    match error {
        WMIError::HResultError { hres } => format!("HRESULT_0x{:08X}", *hres as u32),
        WMIError::ResultEmpty => "WMI_RESULT_EMPTY".to_owned(),
        WMIError::SerdeError(_)
        | WMIError::DeserializeValueError(_)
        | WMIError::InvalidDeserializationVariantError(_) => "WMI_DESERIALIZATION_ERROR".to_owned(),
        WMIError::ParseIntError(_)
        | WMIError::ParseFloatError(_)
        | WMIError::ConvertError(_)
        | WMIError::ConvertVariantError(_)
        | WMIError::ConvertBoolError(_)
        | WMIError::ConvertStringError(_)
        | WMIError::ConvertDatetimeError(_)
        | WMIError::ConvertLengthError(_) => "WMI_VALUE_CONVERSION_ERROR".to_owned(),
        WMIError::NullPointerResult | WMIError::UnimplementedArrayItem => {
            "WMI_RESULT_SHAPE_ERROR".to_owned()
        }
        _ => "WMI_SOURCE_ERROR".to_owned(),
    }
}

fn is_virtual_pnp_id(id: &str) -> bool {
    let normalized = normalize_id(id);
    normalized.starts_with("ROOT\\")
        || normalized.starts_with("SWD\\")
        || normalized.starts_with("VMBUS\\")
}

fn normalize_id(id: &str) -> String {
    id.trim().replace('/', "\\").to_ascii_uppercase()
}

fn physical_drive_index(reference: &str) -> Option<u32> {
    let upper = reference.to_ascii_uppercase();
    let position = upper.find("PHYSICALDRIVE")? + "PHYSICALDRIVE".len();
    let digits: String = upper[position..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn reference_value(reference: &str) -> Option<&str> {
    let marker = reference.find("DeviceID=")? + "DeviceID=".len();
    let remainder = &reference[marker..];
    let value = remainder.strip_prefix('"')?;
    let end = value.find('"')?;
    Some(&value[..end])
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CpuRow {
    name: Option<String>,
    manufacturer: Option<String>,
    processor_id: Option<String>,
    number_of_cores: Option<u32>,
    number_of_logical_processors: Option<u32>,
    max_clock_speed: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct GpuRow {
    name: Option<String>,
    adapter_compatibility: Option<String>,
    pnp_device_id: Option<String>,
    driver_version: Option<String>,
    driver_date: Option<String>,
    adapter_ram: Option<u32>,
    video_processor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct MemoryRow {
    manufacturer: Option<String>,
    capacity: Option<u64>,
    serial_number: Option<String>,
    part_number: Option<String>,
    device_locator: Option<String>,
    speed: Option<u32>,
    configured_clock_speed: Option<u32>,
    form_factor: Option<u16>,
    smbios_memory_type: Option<u16>,
    tag: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct DiskRow {
    index: Option<u32>,
    model: Option<String>,
    manufacturer: Option<String>,
    size: Option<u64>,
    serial_number: Option<String>,
    pnp_device_id: Option<String>,
    interface_type: Option<String>,
    media_type: Option<String>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PartitionRow {
    device_id: Option<String>,
    disk_index: Option<u32>,
    index: Option<u32>,
    size: Option<u64>,
    name: Option<String>,
    boot_partition: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VolumeRow {
    device_id: Option<String>,
    volume_name: Option<String>,
    file_system: Option<String>,
    size: Option<u64>,
    free_space: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AssociationRow {
    antecedent: Option<String>,
    dependent: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct NetworkRow {
    name: Option<String>,
    manufacturer: Option<String>,
    pnp_device_id: Option<String>,
    physical_adapter: Option<bool>,
    net_connection_status: Option<u16>,
    speed: Option<u64>,
    interface_index: Option<u32>,
    guid: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct BatteryRow {
    device_id: Option<String>,
    name: Option<String>,
    manufacturer: Option<String>,
    design_capacity: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PnpRow {
    device_id: Option<String>,
    name: Option<String>,
    pnp_class: Option<String>,
    status: Option<String>,
    present: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::{is_virtual_pnp_id, physical_drive_index, reference_value};

    #[test]
    fn parses_wmi_association_keys_without_guessing_names() {
        assert_eq!(
            physical_drive_index(r#"Win32_DiskDrive.DeviceID="\\.\PHYSICALDRIVE2""#),
            Some(2)
        );
        assert_eq!(
            reference_value(r#"Win32_DiskPartition.DeviceID="Disk #2, Partition #1""#),
            Some("Disk #2, Partition #1")
        );
        assert_eq!(reference_value("not a WMI reference"), None);
    }

    #[test]
    fn only_windows_software_bus_ids_mark_a_virtual_gpu() {
        assert!(is_virtual_pnp_id(r"ROOT\ORAYDISPLAY"));
        assert!(!is_virtual_pnp_id(r"PCI\VEN_10DE&DEV_2882"));
    }
}
