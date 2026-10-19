#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MediaKind {
    Hdd,
    Ssd,
    Scm,
    Unknown,
}

impl MediaKind {
    pub fn as_str(self) -> Option<&'static str> {
        match self {
            Self::Hdd => Some("HDD"),
            Self::Ssd => Some("SSD"),
            Self::Scm => Some("SCM"),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeFailure {
    PermissionRequired,
    TempUnavailable,
    Unknown,
}

pub fn classify_media_type(value: Option<u16>) -> MediaKind {
    match value {
        Some(3) => MediaKind::Hdd,
        Some(4) => MediaKind::Ssd,
        Some(5) => MediaKind::Scm,
        _ => MediaKind::Unknown,
    }
}

pub fn resolve_media_kind(
    storage_media_type: Option<u16>,
    reported_media_type: Option<&str>,
) -> MediaKind {
    let from_storage_api = classify_media_type(storage_media_type);
    if from_storage_api != MediaKind::Unknown {
        return from_storage_api;
    }

    let Some(value) = reported_media_type else {
        return MediaKind::Unknown;
    };
    let normalized = value.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "ssd" | "solid state" | "solid_state") {
        MediaKind::Ssd
    } else if normalized == "hdd"
        || normalized.contains("rotational")
        || normalized.contains("mechanical")
    {
        MediaKind::Hdd
    } else if normalized == "scm" {
        MediaKind::Scm
    } else {
        MediaKind::Unknown
    }
}

pub fn is_safe_probe_target(media_type: Option<&str>) -> bool {
    media_type.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "ssd" | "solid state" | "solid_state"
        )
    })
}

pub fn skip_probe_after_permission_denial(
    previous_permission_denial: bool,
    explicit_retry_requested: bool,
) -> bool {
    previous_permission_denial && !explicit_retry_requested
}

pub fn classify_probe_failure(hresult: u32) -> ProbeFailure {
    match hresult {
        // WBEM_E_ACCESS_DENIED, E_ACCESSDENIED, and the PermissionDenied
        // HRESULT recorded by the M0.1 Storage Reliability diagnostic.
        0x8004_1003 | 0x8007_0005 | 0x8013_1500 => ProbeFailure::PermissionRequired,
        _ => ProbeFailure::TempUnavailable,
    }
}

pub fn reliability_query_for_disk_number(disk_number: u32) -> String {
    format!("SELECT * FROM MSFT_StorageReliabilityCounter WHERE DeviceId = '{disk_number}'")
}

#[cfg(windows)]
pub mod windows;
