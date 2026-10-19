use corescope_lib::storage_reliability::{
    MediaKind, ProbeFailure, classify_media_type, classify_probe_failure, is_safe_probe_target,
    reliability_query_for_disk_number, resolve_media_kind, skip_probe_after_permission_denial,
};

#[test]
fn windows_storage_media_codes_are_classified_without_guessing() {
    assert_eq!(classify_media_type(Some(3)), MediaKind::Hdd);
    assert_eq!(classify_media_type(Some(4)), MediaKind::Ssd);
    assert_eq!(classify_media_type(Some(5)), MediaKind::Scm);
    assert_eq!(classify_media_type(Some(0)), MediaKind::Unknown);
    assert_eq!(classify_media_type(None), MediaKind::Unknown);
}

#[test]
fn reliability_probe_only_targets_explicitly_confirmed_ssds() {
    assert!(is_safe_probe_target(Some("SSD")));
    assert!(is_safe_probe_target(Some("Solid State")));
    assert!(!is_safe_probe_target(Some("HDD")));
    assert!(!is_safe_probe_target(Some("Fixed hard disk media")));
    assert!(!is_safe_probe_target(None));
}

#[test]
fn storage_api_media_type_overrides_generic_win32_disk_media_labels() {
    assert_eq!(
        resolve_media_kind(Some(4), Some("Fixed hard disk media")),
        MediaKind::Ssd
    );
    assert_eq!(
        resolve_media_kind(None, Some("Fixed hard disk media")),
        MediaKind::Unknown
    );
    assert_eq!(resolve_media_kind(None, Some("Rotational")), MediaKind::Hdd);
}

#[test]
fn storage_permission_denials_keep_their_state_and_unknown_errors_stay_temporary() {
    for code in [0x8004_1003, 0x8007_0005, 0x8013_1500] {
        assert_eq!(
            classify_probe_failure(code),
            ProbeFailure::PermissionRequired
        );
    }
    assert_eq!(
        classify_probe_failure(0x8000_4001),
        ProbeFailure::TempUnavailable
    );
}

#[test]
fn cached_permission_denial_is_not_retried_without_an_explicit_user_request() {
    assert!(skip_probe_after_permission_denial(true, false));
    assert!(!skip_probe_after_permission_denial(true, true));
    assert!(!skip_probe_after_permission_denial(false, false));
}

#[test]
fn reliability_query_is_read_only_and_targets_one_disk_number() {
    let query = reliability_query_for_disk_number(2);
    assert!(query.contains("SELECT"));
    assert!(query.contains("MSFT_StorageReliabilityCounter"));
    assert!(query.contains("DeviceId = '2'"));
    assert!(!query.contains("RESET"));
    assert!(!query.contains("UPDATE"));
}
