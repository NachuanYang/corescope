pub mod analysis;
pub mod api;
pub mod auth;
pub mod capabilities;
pub mod chat;
pub mod inventory;
pub mod persistence;
pub mod storage_reliability;
pub mod telemetry;

use std::fs::{self, OpenOptions};
use std::io;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

fn initialize_logging(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let log_dir = app.path().app_log_dir()?;
    fs::create_dir_all(&log_dir)?;
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("corescope.log"))?;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tauri=warn,wgpu=warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(log_file)
        .try_init()
        .map_err(io::Error::other)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            initialize_logging(app.handle())?;
            let app_data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&app_data_dir)?;
            let auth_directory = app.path().app_local_data_dir()?.join("auth");
            let auth = auth::service::AuthService::open(auth_directory)
                .map_err(io::Error::other)?;
            let database = persistence::Database::open_at(&app_data_dir.join("corescope.sqlite3"))
                .map_err(io::Error::other)?;
            let cached_inventory = database.load_inventory().map_err(io::Error::other)?;
            app.manage(telemetry::TelemetryManager::new(cached_inventory));
            app.manage(inventory::ScanCoordinator::default());
            app.manage(analysis::AnalysisCoordinator::default());
            app.manage(chat::ChatCoordinator::default());
            app.manage(database);
            app.manage(auth);
            tracing::info!("CoreScope desktop shell initialized");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            api::app::app_info,
            api::analysis::get_analysis_target_state,
            api::analysis::create_analysis_preview,
            api::analysis::set_ai_data_consent,
            api::analysis::cancel_ai_request,
            api::analysis::start_initial_system_analysis,
            api::analysis::start_initial_device_analysis,
            api::analysis::refresh_system_analysis,
            api::analysis::refresh_device_analysis,
            api::chat::list_chat_threads,
            api::chat::create_chat_thread,
            api::chat::load_chat_messages,
            api::chat::clear_chat_thread,
            api::chat::create_chat_preview,
            api::chat::send_chat_message,
            api::auth::get_chatgpt_auth_state,
            api::auth::connect_chatgpt,
            api::auth::cancel_chatgpt_sign_in,
            api::auth::list_chatgpt_models,
            api::auth::verify_chatgpt_inference,
            api::auth::disconnect_chatgpt,
            api::diagnostics::get_cache_summary,
            api::diagnostics::clear_ai_cache,
            api::diagnostics::export_sanitized_diagnostics,
            api::hardware::scan_devices,
            api::hardware::cancel_scan,
            api::hardware::load_inventory,
            api::hardware::load_device_profile,
            api::hardware::load_metric_history,
            api::storage::probe_storage_reliability,
            api::telemetry::start_monitoring,
            api::telemetry::stop_monitoring,
            api::telemetry::load_telemetry
        ])
        .on_window_event(|window, event| {
            use tauri::WindowEvent;
            if let Some(manager) = window.try_state::<telemetry::TelemetryManager>() {
                match event {
                    WindowEvent::Focused(foreground) => {
                        if let Err(error) = manager.set_foreground(*foreground) {
                            tracing::warn!(error_code = %error, "Could not update monitoring foreground state");
                        }
                    }
                    WindowEvent::Destroyed => {
                        if let Err(error) = manager.stop() {
                            tracing::warn!(error_code = %error, "Could not stop monitoring during window shutdown");
                        }
                    }
                    _ => {}
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to run CoreScope desktop application");
}

#[cfg(test)]
mod tests {
    #[test]
    fn product_name_is_stable_for_native_and_ui_shells() {
        assert_eq!(super::api::PRODUCT_NAME, "机枢");
    }
}
