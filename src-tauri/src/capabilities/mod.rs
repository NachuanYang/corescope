use crate::inventory::{DeviceKind, DeviceRecord};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapabilityState {
    Available,
    NotApplicable,
    NotTested,
    Unsupported,
    PermissionRequired,
    TempUnavailable,
    Unknown,
    Probing,
    Stale,
}

impl CapabilityState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "AVAILABLE",
            Self::NotApplicable => "NOT_APPLICABLE",
            Self::NotTested => "NOT_TESTED",
            Self::Unsupported => "UNSUPPORTED",
            Self::PermissionRequired => "PERMISSION_REQUIRED",
            Self::TempUnavailable => "TEMP_UNAVAILABLE",
            Self::Unknown => "UNKNOWN",
            Self::Probing => "PROBING",
            Self::Stale => "STALE",
        }
    }
}

impl FromStr for CapabilityState {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "AVAILABLE" => Ok(Self::Available),
            "NOT_APPLICABLE" => Ok(Self::NotApplicable),
            "NOT_TESTED" => Ok(Self::NotTested),
            "UNSUPPORTED" => Ok(Self::Unsupported),
            "PERMISSION_REQUIRED" => Ok(Self::PermissionRequired),
            "TEMP_UNAVAILABLE" => Ok(Self::TempUnavailable),
            "UNKNOWN" => Ok(Self::Unknown),
            "PROBING" => Ok(Self::Probing),
            "STALE" => Ok(Self::Stale),
            _ => Err("CAPABILITY_STATE_UNKNOWN".to_owned()),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceLevel {
    Measured,
    OsReported,
    SourceReported,
}

impl EvidenceLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "MEASURED",
            Self::OsReported => "OS_REPORTED",
            Self::SourceReported => "SOURCE_REPORTED",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CapabilityKind {
    Static,
    Dynamic,
}

impl CapabilityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Static => "STATIC",
            Self::Dynamic => "DYNAMIC",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityRecord {
    pub device_id: String,
    pub metric_key: String,
    pub kind: CapabilityKind,
    pub state: CapabilityState,
    pub value: Option<Value>,
    pub unit: Option<String>,
    pub source: String,
    pub evidence_level: Option<EvidenceLevel>,
    pub observed_at_utc: Option<String>,
    pub reason_code: Option<String>,
    pub native_error_code: Option<String>,
    pub details: Value,
}

impl CapabilityRecord {
    pub fn validate(&self) -> Result<(), String> {
        if self.state == CapabilityState::Available
            && (self.value.is_none()
                || self.source.trim().is_empty()
                || self.observed_at_utc.is_none())
        {
            return Err("CAPABILITY_AVAILABLE_EVIDENCE_INCOMPLETE".to_owned());
        }
        if self.state != CapabilityState::Available && self.value.is_some() {
            return Err("CAPABILITY_NON_AVAILABLE_HAS_VALUE".to_owned());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProfile {
    pub device: DeviceRecord,
    pub capabilities: Vec<CapabilityRecord>,
}

pub fn build_device_profile(device: DeviceRecord, scanned_at_utc: &str) -> DeviceProfile {
    let mut capabilities = Vec::new();
    add_static(
        &mut capabilities,
        &device,
        "device.name",
        None,
        Some(json!(device.display_name)),
        scanned_at_utc,
    );
    add_static(
        &mut capabilities,
        &device,
        "device.manufacturer",
        None,
        device.manufacturer.clone().map(Value::String),
        scanned_at_utc,
    );
    add_optional_state(
        &mut capabilities,
        &device,
        "device.present",
        "boolean",
        device.is_present,
        "SOURCE_RETURNED_NO_VALUE",
        scanned_at_utc,
    );
    add_optional_state(
        &mut capabilities,
        &device,
        "device.virtual",
        "boolean",
        device.is_virtual,
        "PHYSICALITY_NOT_ASSERTED",
        scanned_at_utc,
    );

    let static_fields: &[(&str, &str, Option<&str>)] = match device.kind {
        DeviceKind::Cpu => &[
            ("cpu.cores.physical", "physicalCores", Some("core")),
            (
                "cpu.processors.logical",
                "logicalProcessors",
                Some("thread"),
            ),
            ("cpu.clock.max.mhz", "maxClockMhz", Some("MHz")),
        ],
        DeviceKind::GpuAdapter => &[
            ("gpu.driver.version", "driverVersion", None),
            ("gpu.driver.date", "driverDate", None),
            ("gpu.video_processor", "videoProcessor", None),
            (
                "gpu.adapter_ram_reported.bytes",
                "memoryBytes",
                Some("byte"),
            ),
        ],
        DeviceKind::MemoryModule => &[
            (
                "memory.module.capacity.bytes",
                "capacityBytes",
                Some("byte"),
            ),
            ("memory.module.speed.mhz", "speedMhz", Some("MHz")),
            (
                "memory.module.configured_clock.mhz",
                "configuredClockMhz",
                Some("MHz"),
            ),
            ("memory.module.form_factor", "formFactor", None),
            ("memory.module.locator", "slotLocator", None),
            ("memory.module.part_number", "partNumber", None),
        ],
        DeviceKind::PhysicalDisk => &[
            ("disk.capacity.bytes", "sizeBytes", Some("byte")),
            ("disk.bus_type", "interfaceType", None),
            ("disk.media_type", "mediaType", None),
            ("disk.index", "index", None),
        ],
        DeviceKind::Partition => &[
            ("partition.capacity.bytes", "sizeBytes", Some("byte")),
            ("partition.partition_number", "partitionNumber", None),
            ("partition.boot", "bootPartition", Some("boolean")),
            ("partition.role", "role", None),
        ],
        DeviceKind::Volume => &[
            ("volume.drive_letter", "driveLetter", None),
            ("volume.label", "label", None),
            ("volume.file_system", "filesystem", None),
            ("volume.capacity.bytes", "sizeBytes", Some("byte")),
            ("volume.free.bytes", "freeBytes", Some("byte")),
        ],
        DeviceKind::NetworkAdapter => &[
            ("network.status", "connectionStatus", None),
            ("network.link_speed", "speedBps", Some("bit/s")),
            ("network.interface_index", "interfaceIndex", None),
        ],
        DeviceKind::Battery => &[(
            "battery.design_capacity.mwh",
            "designCapacityMwh",
            Some("mWh"),
        )],
        DeviceKind::PnpNode => &[
            ("pnp.node.class", "className", None),
            ("pnp.node.status", "status", None),
        ],
    };
    for (key, attribute, unit) in static_fields {
        add_attribute(
            &mut capabilities,
            &device,
            key,
            attribute,
            *unit,
            scanned_at_utc,
        );
    }

    add_device_templates(&mut capabilities, &device, scanned_at_utc);
    capabilities.sort_by(|left, right| left.metric_key.cmp(&right.metric_key));
    DeviceProfile {
        device,
        capabilities,
    }
}

fn add_static(
    output: &mut Vec<CapabilityRecord>,
    device: &DeviceRecord,
    metric_key: &str,
    unit: Option<&str>,
    value: Option<Value>,
    observed_at_utc: &str,
) {
    let (state, evidence_level, reason_code) = if value.is_some() {
        (
            CapabilityState::Available,
            Some(EvidenceLevel::OsReported),
            None,
        )
    } else {
        (
            CapabilityState::TempUnavailable,
            Some(EvidenceLevel::SourceReported),
            Some("SOURCE_RETURNED_NO_VALUE".to_owned()),
        )
    };
    output.push(CapabilityRecord {
        device_id: device.id.clone(),
        metric_key: metric_key.to_owned(),
        kind: CapabilityKind::Static,
        state,
        value: value.clone(),
        unit: unit.map(ToOwned::to_owned),
        source: device.source.clone(),
        evidence_level,
        observed_at_utc: Some(observed_at_utc.to_owned()),
        reason_code,
        native_error_code: None,
        details: value
            .map(|value| json!({"value": value}))
            .unwrap_or_else(|| json!({})),
    });
}

fn add_optional_state<T>(
    output: &mut Vec<CapabilityRecord>,
    device: &DeviceRecord,
    metric_key: &str,
    unit: &str,
    value: Option<T>,
    unknown_reason: &str,
    scanned_at_utc: &str,
) where
    T: Serialize,
{
    match value {
        Some(value) => add_static(
            output,
            device,
            metric_key,
            Some(unit),
            serde_json::to_value(value).ok(),
            scanned_at_utc,
        ),
        None => output.push(CapabilityRecord {
            device_id: device.id.clone(),
            metric_key: metric_key.to_owned(),
            kind: CapabilityKind::Static,
            state: CapabilityState::Unknown,
            value: None,
            unit: Some(unit.to_owned()),
            source: device.source.clone(),
            evidence_level: Some(EvidenceLevel::OsReported),
            observed_at_utc: Some(scanned_at_utc.to_owned()),
            reason_code: Some(unknown_reason.to_owned()),
            native_error_code: None,
            details: json!({}),
        }),
    }
}

fn add_attribute(
    output: &mut Vec<CapabilityRecord>,
    device: &DeviceRecord,
    metric_key: &str,
    attribute: &str,
    unit: Option<&str>,
    scanned_at_utc: &str,
) {
    let value = device
        .attributes
        .get(attribute)
        .filter(|value| !value.is_null())
        .cloned();
    if device.attributes.get(attribute).is_none() {
        output.push(template(
            device,
            metric_key,
            unit,
            CapabilityState::NotTested,
            device.source.as_str(),
            None,
            Some("SOURCE_FIELD_NOT_COLLECTED"),
            json!({"attribute": attribute}),
            CapabilityKind::Static,
        ));
    } else {
        add_static(output, device, metric_key, unit, value, scanned_at_utc);
    }
}

fn add_device_templates(
    output: &mut Vec<CapabilityRecord>,
    device: &DeviceRecord,
    scanned_at_utc: &str,
) {
    match device.kind {
        DeviceKind::Cpu => push_not_tested(
            output,
            device,
            "cpu.utilization.percent",
            Some("percent"),
            "PDH",
            "TELEMETRY_NOT_STARTED",
        ),
        DeviceKind::GpuAdapter => {
            let nvidia = is_nvidia(device);
            for (key, unit) in [
                ("gpu.utilization.percent", Some("percent")),
                ("gpu.memory_controller.utilization.percent", Some("percent")),
                ("gpu.memory.total.bytes", Some("byte")),
                ("gpu.memory.used.bytes", Some("byte")),
                ("gpu.temperature.celsius", Some("°C")),
                ("gpu.power.watts", Some("W")),
                ("gpu.fan.percent", Some("percent")),
            ] {
                if nvidia {
                    push_not_tested(output, device, key, unit, "NVML", "LIVE_PROBE_NOT_RUN");
                } else {
                    output.push(template(
                        device,
                        key,
                        unit,
                        CapabilityState::NotApplicable,
                        "NVML",
                        Some(EvidenceLevel::SourceReported),
                        Some("NOT_APPLICABLE_TO_NON_NVIDIA_ADAPTER"),
                        json!({}),
                        CapabilityKind::Dynamic,
                    ));
                }
            }
        }
        DeviceKind::MemoryModule => output.push(template(
            device,
            "memory.module.health.percent",
            Some("percent"),
            CapabilityState::Unknown,
            "CoreScope.CapabilityRegistry",
            Some(EvidenceLevel::SourceReported),
            Some("NO_UNIFORM_MEMORY_HEALTH_INTERFACE"),
            json!({}),
            CapabilityKind::Dynamic,
        )),
        DeviceKind::PhysicalDisk => {
            let rotational_speed = template(
                device,
                "disk.spindle_speed.rpm",
                Some("rpm"),
                CapabilityState::NotTested,
                "CoreScope.CapabilityRegistry",
                None,
                Some("DISK_MEDIA_TYPE_UNCONFIRMED"),
                json!({}),
                CapabilityKind::Dynamic,
            );
            let rotational_speed = if is_ssd(device) {
                with_state(
                    rotational_speed,
                    CapabilityState::NotApplicable,
                    Some("NOT_APPLICABLE_TO_SSD"),
                    Some(EvidenceLevel::OsReported),
                    scanned_at_utc,
                )
            } else {
                rotational_speed
            };
            output.push(rotational_speed);

            for key in [
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
            ] {
                let reason = if is_rotational_disk(device) {
                    "HDD_QUERY_SKIPPED_TO_AVOID_WAKE"
                } else {
                    "RELIABILITY_PROBE_NOT_RUN"
                };
                push_not_tested(output, device, key, None, "StorageReliability", reason);
            }
            push_not_tested(
                output,
                device,
                "disk.read.bytes_per_second",
                Some("byte/s"),
                "PDH",
                "TELEMETRY_NOT_STARTED",
            );
            push_not_tested(
                output,
                device,
                "disk.write.bytes_per_second",
                Some("byte/s"),
                "PDH",
                "TELEMETRY_NOT_STARTED",
            );
        }
        DeviceKind::NetworkAdapter => {
            push_not_tested(
                output,
                device,
                "network.receive.bytes_per_second",
                Some("byte/s"),
                "PDH",
                "TELEMETRY_NOT_STARTED",
            );
            push_not_tested(
                output,
                device,
                "network.send.bytes_per_second",
                Some("byte/s"),
                "PDH",
                "TELEMETRY_NOT_STARTED",
            );
            push_not_tested(
                output,
                device,
                "network.bytes_received.total",
                Some("byte"),
                device.source.as_str(),
                "STATISTICS_SOURCE_NOT_STARTED",
            );
            push_not_tested(
                output,
                device,
                "network.bytes_sent.total",
                Some("byte"),
                device.source.as_str(),
                "STATISTICS_SOURCE_NOT_STARTED",
            );
            if device.is_virtual.is_none() {
                output.push(template(
                    device,
                    "network.virtual_classification_status",
                    None,
                    CapabilityState::Unknown,
                    device.source.as_str(),
                    Some(EvidenceLevel::SourceReported),
                    Some("SOURCE_FIELD_NOT_RECORDED"),
                    json!({}),
                    CapabilityKind::Static,
                ));
            }
        }
        DeviceKind::Battery | DeviceKind::Partition | DeviceKind::Volume | DeviceKind::PnpNode => {}
    }
}

fn push_not_tested(
    output: &mut Vec<CapabilityRecord>,
    device: &DeviceRecord,
    metric_key: &str,
    unit: Option<&str>,
    source: &str,
    reason: &str,
) {
    output.push(template(
        device,
        metric_key,
        unit,
        CapabilityState::NotTested,
        source,
        None,
        Some(reason),
        json!({}),
        CapabilityKind::Dynamic,
    ));
}

fn template(
    device: &DeviceRecord,
    metric_key: &str,
    unit: Option<&str>,
    state: CapabilityState,
    source: &str,
    evidence_level: Option<EvidenceLevel>,
    reason_code: Option<&str>,
    details: Value,
    kind: CapabilityKind,
) -> CapabilityRecord {
    CapabilityRecord {
        device_id: device.id.clone(),
        metric_key: metric_key.to_owned(),
        kind,
        state,
        value: None,
        unit: unit.map(ToOwned::to_owned),
        source: source.to_owned(),
        evidence_level,
        observed_at_utc: None,
        reason_code: reason_code.map(ToOwned::to_owned),
        native_error_code: None,
        details,
    }
}

fn with_state(
    mut capability: CapabilityRecord,
    state: CapabilityState,
    reason_code: Option<&str>,
    evidence_level: Option<EvidenceLevel>,
    observed_at_utc: &str,
) -> CapabilityRecord {
    capability.state = state;
    capability.reason_code = reason_code.map(ToOwned::to_owned);
    capability.evidence_level = evidence_level;
    capability.observed_at_utc = Some(observed_at_utc.to_owned());
    capability
}

fn is_nvidia(device: &DeviceRecord) -> bool {
    device
        .manufacturer
        .as_deref()
        .is_some_and(|manufacturer| manufacturer.to_ascii_lowercase().contains("nvidia"))
        || device.display_name.to_ascii_lowercase().contains("nvidia")
}

fn is_ssd(device: &DeviceRecord) -> bool {
    let media = device
        .attributes
        .get("mediaType")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    media.contains("ssd") || media.contains("solid state") || media.contains("solid_state")
}

fn is_rotational_disk(device: &DeviceRecord) -> bool {
    let media = device
        .attributes
        .get("mediaType")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    media == "hdd" || media.contains("rotational") || media.contains("mechanical")
}
