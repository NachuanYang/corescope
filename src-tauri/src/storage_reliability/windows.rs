use super::{
    MediaKind, ProbeFailure, classify_media_type, classify_probe_failure,
    reliability_query_for_disk_number,
};
use crate::capabilities::{CapabilityKind, CapabilityRecord, CapabilityState, EvidenceLevel};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use wmi::{WMIConnection, WMIError};

const STORAGE_NAMESPACE: &str = "ROOT\\Microsoft\\Windows\\Storage";
const STORAGE_RELIABILITY_SOURCE: &str = "MSFT_StorageReliabilityCounter";

#[derive(Clone, Debug)]
pub struct ProbeError {
    pub failure: ProbeFailure,
    pub reason_code: &'static str,
    pub native_error_code: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PhysicalDiskMediaRow {
    device_id: Option<String>,
    media_type: Option<u16>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ReliabilityCounterRow {
    device_id: Option<String>,
    temperature: Option<u8>,
    temperature_max: Option<u8>,
    wear: Option<u8>,
    power_on_hours: Option<u16>,
    start_stop_cycle_count: Option<u32>,
    load_unload_cycle_count: Option<u32>,
    read_errors_total: Option<u64>,
    read_errors_uncorrected: Option<u64>,
    write_errors_total: Option<u64>,
    write_errors_uncorrected: Option<u64>,
}

#[derive(Clone, Copy)]
struct MetricDefinition {
    metric_key: &'static str,
    unit: &'static str,
    field: &'static str,
    value: Option<u64>,
}

pub fn media_types_by_disk_number() -> Result<HashMap<u32, MediaKind>, String> {
    let connection = WMIConnection::with_namespace_path(STORAGE_NAMESPACE)
        .map_err(|error| safe_error_code(&error))?;
    let rows: Vec<PhysicalDiskMediaRow> = connection
        .raw_query("SELECT DeviceId, MediaType FROM MSFT_PhysicalDisk")
        .map_err(|error| safe_error_code(&error))?;

    let mut media_types = HashMap::new();
    let mut ambiguous = std::collections::HashSet::new();
    for row in rows {
        let Some(number) = row.device_id.as_deref().and_then(parse_disk_number) else {
            continue;
        };
        let media_kind = classify_media_type(row.media_type);
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

pub fn probe_disk_number(disk_number: u32) -> Result<Vec<CapabilityRecord>, ProbeError> {
    let connection = WMIConnection::with_namespace_path(STORAGE_NAMESPACE)
        .map_err(|error| classify_wmi_error(&error))?;
    let query = reliability_query_for_disk_number(disk_number);
    let rows: Vec<ReliabilityCounterRow> = connection
        .raw_query(query)
        .map_err(|error| classify_wmi_error(&error))?;
    if rows.is_empty() {
        return Err(ProbeError {
            failure: ProbeFailure::TempUnavailable,
            reason_code: "SOURCE_RETURNED_NO_COUNTERS",
            native_error_code: None,
        });
    }
    if rows.len() != 1 {
        return Err(ProbeError {
            failure: ProbeFailure::Unknown,
            reason_code: "STORAGE_RELIABILITY_RESULT_AMBIGUOUS",
            native_error_code: None,
        });
    }

    let row = rows
        .into_iter()
        .next()
        .expect("one counter row was validated");
    if row.device_id.as_deref() != Some(&disk_number.to_string()) {
        return Err(ProbeError {
            failure: ProbeFailure::Unknown,
            reason_code: "STORAGE_RELIABILITY_DEVICE_ID_MISMATCH",
            native_error_code: None,
        });
    }
    Ok(capabilities_from_row(disk_number, row))
}

fn capabilities_from_row(disk_number: u32, row: ReliabilityCounterRow) -> Vec<CapabilityRecord> {
    let observed_at_utc = chrono::Utc::now().to_rfc3339();
    let definitions = [
        MetricDefinition {
            metric_key: "disk.temperature.celsius",
            unit: "°C",
            field: "Temperature",
            value: row.temperature.map(u64::from),
        },
        MetricDefinition {
            metric_key: "disk.temperature.max.celsius",
            unit: "°C",
            field: "TemperatureMax",
            value: row.temperature_max.map(u64::from),
        },
        MetricDefinition {
            metric_key: "disk.wear.provider_value",
            unit: "percent",
            field: "Wear",
            value: row.wear.map(u64::from),
        },
        MetricDefinition {
            metric_key: "disk.power_on_hours",
            unit: "hour",
            field: "PowerOnHours",
            value: row.power_on_hours.map(u64::from),
        },
        MetricDefinition {
            metric_key: "disk.start_stop_cycle_count",
            unit: "count",
            field: "StartStopCycleCount",
            value: row.start_stop_cycle_count.map(u64::from),
        },
        MetricDefinition {
            metric_key: "disk.start_stop_count",
            unit: "count",
            field: "StartStopCycleCount",
            value: row.start_stop_cycle_count.map(u64::from),
        },
        MetricDefinition {
            metric_key: "disk.load_unload_cycle_count",
            unit: "count",
            field: "LoadUnloadCycleCount",
            value: row.load_unload_cycle_count.map(u64::from),
        },
        MetricDefinition {
            metric_key: "disk.read_errors.total",
            unit: "count",
            field: "ReadErrorsTotal",
            value: row.read_errors_total,
        },
        MetricDefinition {
            metric_key: "disk.read_errors.uncorrected",
            unit: "count",
            field: "ReadErrorsUncorrected",
            value: row.read_errors_uncorrected,
        },
        MetricDefinition {
            metric_key: "disk.write_errors.total",
            unit: "count",
            field: "WriteErrorsTotal",
            value: row.write_errors_total,
        },
        MetricDefinition {
            metric_key: "disk.write_errors.uncorrected",
            unit: "count",
            field: "WriteErrorsUncorrected",
            value: row.write_errors_uncorrected,
        },
    ];

    definitions
        .into_iter()
        .map(|definition| {
            let value = definition.value.map(|value| json!(value));
            CapabilityRecord {
                device_id: String::new(),
                metric_key: definition.metric_key.to_owned(),
                kind: CapabilityKind::Dynamic,
                state: if value.is_some() {
                    CapabilityState::Available
                } else {
                    CapabilityState::TempUnavailable
                },
                value,
                unit: Some(definition.unit.to_owned()),
                source: STORAGE_RELIABILITY_SOURCE.to_owned(),
                evidence_level: Some(EvidenceLevel::SourceReported),
                observed_at_utc: Some(observed_at_utc.clone()),
                reason_code: None,
                native_error_code: None,
                details: json!({
                    "diskNumber": disk_number,
                    "reportedField": definition.field,
                    "aliasOf": (definition.metric_key == "disk.start_stop_count")
                        .then_some("disk.start_stop_cycle_count"),
                }),
            }
        })
        .collect()
}

pub fn failure_capabilities(
    device_id: &str,
    disk_number: u32,
    error: &ProbeError,
) -> Vec<CapabilityRecord> {
    let state = match error.failure {
        ProbeFailure::PermissionRequired => CapabilityState::PermissionRequired,
        ProbeFailure::TempUnavailable => CapabilityState::TempUnavailable,
        ProbeFailure::Unknown => CapabilityState::Unknown,
    };
    reliability_metric_keys()
        .into_iter()
        .map(|(metric_key, unit)| CapabilityRecord {
            device_id: device_id.to_owned(),
            metric_key: metric_key.to_owned(),
            kind: CapabilityKind::Dynamic,
            state,
            value: None,
            unit: Some(unit.to_owned()),
            source: STORAGE_RELIABILITY_SOURCE.to_owned(),
            evidence_level: None,
            observed_at_utc: Some(chrono::Utc::now().to_rfc3339()),
            reason_code: Some(error.reason_code.to_owned()),
            native_error_code: error.native_error_code.clone(),
            details: json!({"diskNumber":disk_number}),
        })
        .collect()
}

pub fn bind_device_id(
    mut capabilities: Vec<CapabilityRecord>,
    device_id: &str,
) -> Vec<CapabilityRecord> {
    for capability in &mut capabilities {
        capability.device_id = device_id.to_owned();
    }
    capabilities
}

pub fn mark_missing_values(mut capabilities: Vec<CapabilityRecord>) -> Vec<CapabilityRecord> {
    for capability in &mut capabilities {
        if capability.state == CapabilityState::TempUnavailable {
            capability.reason_code = Some("SOURCE_RETURNED_NO_VALUE".to_owned());
        }
    }
    capabilities
}

fn reliability_metric_keys() -> [(&'static str, &'static str); 11] {
    [
        ("disk.temperature.celsius", "°C"),
        ("disk.temperature.max.celsius", "°C"),
        ("disk.wear.provider_value", "percent"),
        ("disk.power_on_hours", "hour"),
        ("disk.start_stop_cycle_count", "count"),
        ("disk.start_stop_count", "count"),
        ("disk.load_unload_cycle_count", "count"),
        ("disk.read_errors.total", "count"),
        ("disk.read_errors.uncorrected", "count"),
        ("disk.write_errors.total", "count"),
        ("disk.write_errors.uncorrected", "count"),
    ]
}

fn classify_wmi_error(error: &WMIError) -> ProbeError {
    let hresult = match error {
        WMIError::HResultError { hres } => Some(*hres as u32),
        _ => None,
    };
    let failure = hresult
        .map(classify_probe_failure)
        .unwrap_or(ProbeFailure::TempUnavailable);
    ProbeError {
        failure,
        reason_code: match failure {
            ProbeFailure::PermissionRequired => "STANDARD_USER_ACCESS_DENIED",
            ProbeFailure::TempUnavailable => "STORAGE_RELIABILITY_QUERY_FAILED",
            ProbeFailure::Unknown => "STORAGE_RELIABILITY_STATE_UNKNOWN",
        },
        native_error_code: hresult.map(|code| format!("HRESULT_0x{code:08X}")),
    }
}

fn safe_error_code(error: &WMIError) -> String {
    match error {
        WMIError::HResultError { hres } => format!("HRESULT_0x{:08X}", *hres as u32),
        WMIError::ResultEmpty => "WMI_RESULT_EMPTY".to_owned(),
        _ => "WMI_STORAGE_QUERY_FAILED".to_owned(),
    }
}

fn parse_disk_number(value: &str) -> Option<u32> {
    let value = value.trim();
    if let Ok(number) = value.parse::<u32>() {
        return Some(number);
    }
    value
        .rsplit_once("PhysicalDisk")
        .and_then(|(_, suffix)| suffix.parse::<u32>().ok())
}
