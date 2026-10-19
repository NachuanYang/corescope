use crate::capabilities::{CapabilityState, DeviceProfile};
use crate::inventory::DeviceKind;
use crate::persistence::Database;
#[cfg(windows)]
use crate::storage_reliability::windows::{bind_device_id, mark_missing_values};
use crate::storage_reliability::{is_safe_probe_target, skip_probe_after_permission_denial};
use tauri::State;

const RELIABILITY_METRICS: [&str; 11] = [
    "disk.temperature.celsius",
    "disk.temperature.max.celsius",
    "disk.wear.provider_value",
    "disk.power_on_hours",
    "disk.start_stop_cycle_count",
    "disk.start_stop_count",
    "disk.load_unload_cycle_count",
    "disk.read_errors.total",
    "disk.read_errors.uncorrected",
    "disk.write_errors.total",
    "disk.write_errors.uncorrected",
];

#[tauri::command]
pub fn probe_storage_reliability(
    device_id: String,
    retry_after_permissions_changed: Option<bool>,
    database: State<'_, Database>,
) -> Result<DeviceProfile, String> {
    let profile = database
        .load_device_profile(&device_id)?
        .ok_or_else(|| "STORAGE_RELIABILITY_DEVICE_NOT_FOUND".to_owned())?;
    if profile.device.kind != DeviceKind::PhysicalDisk {
        return Err("STORAGE_RELIABILITY_REQUIRES_PHYSICAL_DISK".to_owned());
    }
    let media_type = profile
        .device
        .attributes
        .get("mediaType")
        .and_then(serde_json::Value::as_str);
    if !is_safe_probe_target(media_type) {
        return Err("STORAGE_RELIABILITY_REQUIRES_CONFIRMED_SSD".to_owned());
    }
    let disk_number = profile
        .device
        .attributes
        .get("index")
        .and_then(serde_json::Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| "STORAGE_RELIABILITY_DISK_NUMBER_UNAVAILABLE".to_owned())?;

    let previous_permission_denial = RELIABILITY_METRICS.iter().any(|metric| {
        profile.capabilities.iter().any(|capability| {
            capability.metric_key == *metric
                && capability.state == CapabilityState::PermissionRequired
                && capability.source == "MSFT_StorageReliabilityCounter"
        })
    });
    if skip_probe_after_permission_denial(
        previous_permission_denial,
        retry_after_permissions_changed == Some(true),
    ) {
        tracing::info!(
            device_id = %device_id,
            reason_code = "PREVIOUS_PERMISSION_DENIAL_REQUIRES_EXPLICIT_RETRY",
            "Skipped storage reliability probe"
        );
        return Ok(profile);
    }

    #[cfg(windows)]
    let capabilities = match crate::storage_reliability::windows::probe_disk_number(disk_number) {
        Ok(capabilities) => mark_missing_values(bind_device_id(capabilities, &device_id)),
        Err(error) => bind_device_id(
            crate::storage_reliability::windows::failure_capabilities(
                &device_id,
                disk_number,
                &error,
            ),
            &device_id,
        ),
    };

    #[cfg(not(windows))]
    let capabilities = {
        let _ = (disk_number, retry_after_permissions_changed);
        return Err("STORAGE_RELIABILITY_WINDOWS_ONLY".to_owned());
    };

    for capability in &capabilities {
        capability.validate()?;
    }
    database.save_capabilities(&capabilities)?;
    database
        .load_device_profile(&device_id)?
        .ok_or_else(|| "STORAGE_RELIABILITY_PROFILE_RELOAD_FAILED".to_owned())
}
