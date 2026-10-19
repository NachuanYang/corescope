use crate::persistence::Database;
use crate::telemetry::{MonitoringStatus, TelemetryFrame, TelemetryManager};
use tauri::State;

#[tauri::command]
pub fn start_monitoring(
    device_ids: Vec<String>,
    database: State<'_, Database>,
    manager: State<'_, TelemetryManager>,
) -> Result<MonitoringStatus, String> {
    let snapshot = database
        .load_inventory()?
        .ok_or_else(|| "TELEMETRY_INVENTORY_NOT_AVAILABLE".to_owned())?;
    manager.configure_inventory(snapshot)?;
    manager.start(device_ids)
}

#[tauri::command]
pub fn stop_monitoring(manager: State<'_, TelemetryManager>) -> Result<MonitoringStatus, String> {
    manager.stop()
}

#[tauri::command]
pub fn load_telemetry(
    device_ids: Option<Vec<String>>,
    manager: State<'_, TelemetryManager>,
) -> Result<TelemetryFrame, String> {
    manager.frame(device_ids)
}
