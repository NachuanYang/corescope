use crate::persistence::Database;
use crate::persistence::SanitizedDiagnosticReport;
use std::path::Path;
use tauri::State;

const MAX_EXPORT_BYTES: usize = 1_048_576;

#[tauri::command]
pub fn get_cache_summary(
    database: State<'_, Database>,
) -> Result<crate::persistence::CacheSummary, String> {
    database.cache_summary()
}

#[tauri::command]
pub fn clear_ai_cache(database: State<'_, Database>) -> Result<usize, String> {
    database.clear_ai_cache()
}

#[tauri::command]
pub fn export_sanitized_diagnostics(
    path: String,
    database: State<'_, Database>,
) -> Result<(), String> {
    let destination = Path::new(&path);
    let is_json = destination
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    if !is_json {
        return Err("DIAGNOSTIC_EXPORT_JSON_EXTENSION_REQUIRED".to_owned());
    }

    let report = database.sanitized_diagnostic_report(env!("CARGO_PKG_VERSION"))?;
    write_sanitized_report(destination, &report)
}

fn write_sanitized_report(path: &Path, report: &SanitizedDiagnosticReport) -> Result<(), String> {
    let is_json = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"));
    if !is_json {
        return Err("DIAGNOSTIC_EXPORT_JSON_EXTENSION_REQUIRED".to_owned());
    }
    let encoded = serde_json::to_vec_pretty(report)
        .map_err(|_| "DIAGNOSTIC_EXPORT_SERIALIZE_FAILED".to_owned())?;
    if encoded.len() > MAX_EXPORT_BYTES {
        return Err("DIAGNOSTIC_EXPORT_SIZE_LIMIT_EXCEEDED".to_owned());
    }
    std::fs::write(path, encoded).map_err(|error| {
        tracing::error!(error_code = "DIAGNOSTIC_EXPORT_WRITE_FAILED", error = %error);
        "DIAGNOSTIC_EXPORT_WRITE_FAILED".to_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::{Database, write_sanitized_report};
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn writes_json_report_and_rejects_non_json_destinations() {
        let database = Database::open_in_memory().unwrap();
        let report = database.sanitized_diagnostic_report("0.1.0").unwrap();
        let directory = tempdir().unwrap();
        let json_path = directory.path().join("diagnostics.json");
        write_sanitized_report(&json_path, &report).unwrap();
        let output = std::fs::read_to_string(&json_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["schemaVersion"],
            crate::persistence::LATEST_SCHEMA_VERSION
        );
        assert!(parsed["diagnostics"].is_array());

        let invalid_path = Path::new("diagnostics.txt");
        assert_eq!(
            write_sanitized_report(invalid_path, &report).unwrap_err(),
            "DIAGNOSTIC_EXPORT_JSON_EXTENSION_REQUIRED"
        );
    }
}
