use crate::capabilities::DeviceProfile;
use crate::inventory::ScanCoordinator;
use crate::inventory::{self, InventorySnapshot, normalize_inventory};
use crate::persistence::Database;
use crate::telemetry::TelemetryManager;
use tauri::State;

#[tauri::command]
pub async fn scan_devices(
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    coordinator: State<'_, ScanCoordinator>,
) -> Result<InventorySnapshot, String> {
    let (context, completion) = coordinator.begin()?;
    let worker_context = context.clone();
    let worker_completion = completion.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let _completion = worker_completion;
        inventory::scan_local_with_context(worker_context)
    });
    let raw = match context.race(worker).await {
        Err(abort) => {
            tracing::warn!(
                error_code = abort.code(),
                "Hardware inventory scan was interrupted"
            );
            return Err(abort.code().to_owned());
        }
        Ok(Ok(Ok(raw))) => raw,
        Ok(Ok(Err(error))) => {
            tracing::error!(error_code = %error, "Device inventory scan could not complete");
            return Err(error);
        }
        Ok(Err(_error)) => {
            tracing::error!(
                error_code = "SCAN_WORKER_FAILED",
                "Hardware inventory worker failed"
            );
            return Err("SCAN_WORKER_FAILED".to_owned());
        }
    };
    context.check().map_err(|abort| abort.code().to_owned())?;
    let install_salt = database.identity_salt()?;
    let snapshot = normalize_inventory(&raw, &install_salt);
    context.check().map_err(|abort| abort.code().to_owned())?;
    database.persist_inventory(&snapshot, env!("CARGO_PKG_VERSION"))?;
    telemetry.configure_inventory(snapshot.clone())?;
    drop(completion);
    tracing::info!(
        device_count = snapshot.devices.len(),
        edge_count = snapshot.edges.len(),
        warning_count = snapshot.warnings.len(),
        "Read-only hardware inventory scan completed"
    );
    Ok(snapshot)
}

#[tauri::command]
pub fn cancel_scan(coordinator: State<'_, ScanCoordinator>) -> Result<(), String> {
    coordinator.cancel()
}

#[tauri::command]
pub fn load_inventory(database: State<'_, Database>) -> Result<Option<InventorySnapshot>, String> {
    database.load_inventory()
}

#[tauri::command]
pub fn load_device_profile(
    device_id: String,
    database: State<'_, Database>,
) -> Result<Option<DeviceProfile>, String> {
    database.load_device_profile(&device_id)
}

#[tauri::command]
pub fn load_metric_history(
    device_id: String,
    database: State<'_, Database>,
) -> Result<Vec<crate::persistence::MetricHistoryRecord>, String> {
    database.load_metric_history(&device_id)
}
