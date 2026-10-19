use corescope_lib::{
    analysis::{AiAssessment, AnalysisScope, EvidenceDevice, EvidenceSnapshot, StoredAnalysis},
    persistence::Database,
};
use uuid::Uuid;

fn snapshot() -> EvidenceSnapshot {
    EvidenceSnapshot {
        snapshot_id: Uuid::new_v4().to_string(),
        scope: AnalysisScope::System,
        captured_at_utc: "2026-10-14T12:00:00Z".to_owned(),
        scan_at_utc: "2026-10-14T11:59:00Z".to_owned(),
        devices: vec![EvidenceDevice {
            evidence_key: "hardware-1.kind".to_owned(),
            device_key: "hardware-1".to_owned(),
            category: "cpu".to_owned(),
            kind: "cpu".to_owned(),
            name: Some("Test CPU".to_owned()),
            manufacturer: Some("Test Vendor".to_owned()),
            virtual_device: Some(false),
            source: "Windows inventory".to_owned(),
            attributes: Default::default(),
        }],
        metrics: Vec::new(),
    }
}

fn assessment(snapshot: &EvidenceSnapshot) -> AiAssessment {
    AiAssessment {
        scope: AnalysisScope::System,
        target_device_id: None,
        summary: "系统报告识别到处理器。当前没有提供长期负载窗口。".to_owned(),
        priority_sections: Vec::new(),
        strengths: Vec::new(),
        concerns: Vec::new(),
        suggestions: Vec::new(),
        limitations: vec!["没有提供长期负载窗口".to_owned()],
        evidence_snapshot_id: snapshot.snapshot_id.clone(),
        generated_at_utc: "2026-10-14T12:01:00Z".to_owned(),
        model_slug: "fixture-model".to_owned(),
        provenance: "AI_INTERPRETATION".to_owned(),
    }
}

fn begin(database: &Database, snapshot: &EvidenceSnapshot, fingerprint: &str) -> String {
    database
        .begin_ai_analysis(
            AnalysisScope::System,
            None,
            fingerprint,
            snapshot,
            "fixture-model",
        )
        .unwrap()
}

#[test]
fn consent_and_one_time_initial_attempt_are_persistent_and_idempotent() {
    let database = Database::open_in_memory().unwrap();
    assert!(!database.ai_data_consent_granted().unwrap());
    assert!(!database.ai_data_consent_decided().unwrap());
    assert!(!database.initial_system_analysis_attempted().unwrap());

    database.set_ai_data_consent(false).unwrap();
    assert!(!database.ai_data_consent_granted().unwrap());
    assert!(database.ai_data_consent_decided().unwrap());
    database.set_ai_data_consent(true).unwrap();
    assert!(database.ai_data_consent_granted().unwrap());
    assert!(database.ai_data_consent_decided().unwrap());
    assert!(database.claim_initial_system_analysis().unwrap());
    assert!(!database.claim_initial_system_analysis().unwrap());
    assert!(database.initial_system_analysis_attempted().unwrap());

    database.clear_ai_cache().unwrap();
    assert!(database.ai_data_consent_granted().unwrap());
    assert!(database.initial_system_analysis_attempted().unwrap());
}

#[test]
fn initial_device_analysis_claims_are_persistent_and_scoped_per_device() {
    let database = Database::open_in_memory().unwrap();

    assert!(!database.initial_device_analysis_attempted("gpu-1").unwrap());
    assert!(database.claim_initial_device_analysis("gpu-1").unwrap());
    assert!(!database.claim_initial_device_analysis("gpu-1").unwrap());
    assert!(database.initial_device_analysis_attempted("gpu-1").unwrap());
    assert!(!database.initial_device_analysis_attempted("gpu-2").unwrap());
    assert!(
        database
            .analysis_target_state(AnalysisScope::Device, Some("gpu-1"))
            .unwrap()
            .initial_attempted
    );
}

#[test]
fn failed_attempt_does_not_replace_last_success_and_history_tracks_fingerprint() {
    let database = Database::open_in_memory().unwrap();
    let evidence = snapshot();
    let first = begin(&database, &evidence, &"a".repeat(64));
    database
        .fail_ai_analysis(&first, "FAILED", "AUTH_NETWORK_UNAVAILABLE")
        .unwrap();
    assert_eq!(
        database
            .latest_ai_analysis_attempt_status(AnalysisScope::System, None)
            .unwrap()
            .as_deref(),
        Some("FAILED")
    );
    assert!(
        database
            .latest_successful_ai_analysis(AnalysisScope::System, None)
            .unwrap()
            .is_none()
    );

    let second = begin(&database, &evidence, &"b".repeat(64));
    database
        .complete_ai_analysis(&second, &assessment(&evidence))
        .unwrap();
    let cached = database
        .latest_successful_ai_analysis(AnalysisScope::System, None)
        .unwrap()
        .unwrap();
    assert_eq!(cached.id, second);
    assert_eq!(
        cached.assessment.unwrap().evidence_snapshot_id,
        evidence.snapshot_id
    );

    database
        .mark_ai_analyses_current(AnalysisScope::System, None, &"b".repeat(64))
        .unwrap();
    assert!(
        !database
            .latest_successful_ai_analysis(AnalysisScope::System, None)
            .unwrap()
            .unwrap()
            .is_historical
    );
    database
        .mark_ai_analyses_current(AnalysisScope::System, None, &"c".repeat(64))
        .unwrap();
    assert!(
        database
            .latest_successful_ai_analysis(AnalysisScope::System, None)
            .unwrap()
            .unwrap()
            .is_historical
    );
}

#[test]
fn pending_analysis_is_recovered_as_interrupted_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("corescope.sqlite3");
    let pending_id;
    {
        let database = Database::open_at(&path).unwrap();
        let evidence = snapshot();
        pending_id = begin(&database, &evidence, &"d".repeat(64));
    }

    let reopened = Database::open_at(&path).unwrap();
    assert_eq!(
        reopened
            .latest_ai_analysis_attempt_status(AnalysisScope::System, None)
            .unwrap()
            .as_deref(),
        Some("INTERRUPTED")
    );
    assert!(
        reopened
            .latest_successful_ai_analysis(AnalysisScope::System, None)
            .unwrap()
            .is_none()
    );

    let state = reopened
        .analysis_target_state(AnalysisScope::System, None)
        .unwrap();
    assert_eq!(state.latest_attempt_status.as_deref(), Some("INTERRUPTED"));
    assert!(reopened.has_table("ai_analyses").unwrap());
    assert!(!pending_id.is_empty());
}

#[test]
fn completed_record_round_trips_snapshot_model_time_and_source() {
    let database = Database::open_in_memory().unwrap();
    let evidence = snapshot();
    let id = begin(&database, &evidence, &"e".repeat(64));
    database
        .complete_ai_analysis(&id, &assessment(&evidence))
        .unwrap();

    let StoredAnalysis {
        assessment: Some(assessment),
        evidence_snapshot: saved_snapshot,
        model_slug,
        status,
        ..
    } = database
        .latest_successful_ai_analysis(AnalysisScope::System, None)
        .unwrap()
        .unwrap()
    else {
        panic!("completed assessment should be restored")
    };
    assert_eq!(saved_snapshot.snapshot_id, evidence.snapshot_id);
    assert_eq!(assessment.evidence_snapshot_id, evidence.snapshot_id);
    assert_eq!(assessment.provenance, "AI_INTERPRETATION");
    assert_eq!(model_slug, "fixture-model");
    assert_eq!(status, "COMPLETED");
}
