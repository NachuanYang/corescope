use crate::persistence::{Database, LATEST_SCHEMA_VERSION};
use serde::Serialize;
use tauri::State;

pub const PRODUCT_NAME: &str = "机枢";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    product_name: &'static str,
    version: &'static str,
    database_ready: bool,
}

#[tauri::command]
pub fn app_info(database: State<'_, Database>) -> Result<AppInfo, String> {
    let schema_version = database.schema_version()?;
    Ok(AppInfo {
        product_name: PRODUCT_NAME,
        version: env!("CARGO_PKG_VERSION"),
        database_ready: schema_version == LATEST_SCHEMA_VERSION,
    })
}
