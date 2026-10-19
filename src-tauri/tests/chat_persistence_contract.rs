use corescope_lib::{
    analysis::{AnalysisScope, EvidenceDevice, EvidenceSnapshot},
    chat::{ChatAnswer, ChatCitation, ChatScope},
    persistence::Database,
};
use rusqlite::Connection;
use std::collections::BTreeMap;
use tempfile::tempdir;

fn snapshot(scope: AnalysisScope) -> EvidenceSnapshot {
    EvidenceSnapshot {
        snapshot_id: uuid::Uuid::new_v4().to_string(),
        scope,
        captured_at_utc: "2026-10-14T12:00:00Z".to_owned(),
        scan_at_utc: "2026-10-14T11:59:00Z".to_owned(),
        devices: vec![EvidenceDevice {
            evidence_key: "device.kind".to_owned(),
            device_key: "device".to_owned(),
            category: "gpu".to_owned(),
            kind: "gpu_adapter".to_owned(),
            name: Some("Fixture GPU".to_owned()),
            manufacturer: Some("Fixture Vendor".to_owned()),
            virtual_device: Some(false),
            source: "Windows inventory".to_owned(),
            attributes: BTreeMap::new(),
        }],
        metrics: Vec::new(),
    }
}

fn answer(snapshot: &EvidenceSnapshot) -> ChatAnswer {
    ChatAnswer {
        answer: "显卡型号来自本机设备枚举。".to_owned(),
        citations: vec![ChatCitation {
            claim: "显卡型号来自本机设备枚举。".to_owned(),
            evidence_keys: vec!["device.kind".to_owned()],
        }],
        evidence_snapshot_id: snapshot.snapshot_id.clone(),
    }
}

#[test]
fn completed_failed_and_pending_turns_keep_their_snapshot_and_context_scope() {
    let database = Database::open_in_memory().unwrap();
    assert_eq!(database.schema_version().unwrap(), 6);
    let system = database
        .create_chat_thread(ChatScope::System, None)
        .unwrap();
    let gpu_one = database
        .create_chat_thread(ChatScope::Device, Some("gpu-one"))
        .unwrap();
    let gpu_two = database
        .create_chat_thread(ChatScope::Device, Some("gpu-two"))
        .unwrap();
    assert_eq!(system.scope, ChatScope::System);
    assert_eq!(gpu_one.target_device_id.as_deref(), Some("gpu-one"));
    assert_eq!(
        database
            .list_chat_threads(ChatScope::Device, Some("gpu-one"))
            .unwrap()
            .len(),
        1
    );
    assert!(
        database
            .list_chat_threads(ChatScope::Device, Some("gpu-two"))
            .unwrap()
            .iter()
            .all(|thread| thread.id == gpu_two.id)
    );

    let evidence = snapshot(AnalysisScope::Device);
    let first = database
        .begin_chat_submission(
            &gpu_one.id,
            &evidence,
            &"a".repeat(64),
            "How is this GPU identified?",
        )
        .unwrap();
    let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    assert_eq!(first.snapshot_id, evidence.snapshot_id);
    let pending = database.load_chat_messages(&gpu_one.id).unwrap();
    assert_eq!(pending[0].status, "PENDING");
    assert_eq!(
        pending[0].evidence_snapshot_ref.as_deref(),
        Some(evidence.snapshot_id.as_str())
    );
    assert_eq!(
        database
            .load_chat_evidence_snapshot(&evidence.snapshot_id)
            .unwrap(),
        Some(evidence.clone())
    );
    database
        .complete_chat_submission(
            &first.user_message_id,
            &answer(&evidence),
            "gpt-fixture",
            &completed_at,
        )
        .unwrap();

    let turns = database.load_recent_chat_turns(&gpu_one.id, 6).unwrap();
    assert_eq!(turns.len(), 2);
    assert!(turns.iter().all(|turn| turn.status == "COMPLETED"));
    let messages = database.load_chat_messages(&gpu_one.id).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].citations, answer(&evidence).citations);
    assert_eq!(messages[1].model_slug.as_deref(), Some("gpt-fixture"));
    assert_eq!(messages[1].source.as_deref(), Some("ChatGPT Responses API"));
    assert_eq!(messages[1].created_at_utc, completed_at);
    assert_eq!(
        messages[1].snapshot_captured_at_utc.as_deref(),
        Some(evidence.captured_at_utc.as_str())
    );
    assert_eq!(
        messages[1].evidence_snapshot_ref.as_deref(),
        Some(evidence.snapshot_id.as_str())
    );
    assert!(database.load_chat_messages(&gpu_two.id).unwrap().is_empty());

    let next_evidence = snapshot(AnalysisScope::Device);
    let failed = database
        .begin_chat_submission(
            &gpu_one.id,
            &next_evidence,
            &"b".repeat(64),
            "What is the temperature?",
        )
        .unwrap();
    database
        .fail_chat_submission(&failed.user_message_id, "FAILED", "AUTH_NETWORK_TIMEOUT")
        .unwrap();
    let failed_messages = database.load_chat_messages(&gpu_one.id).unwrap();
    assert_eq!(failed_messages.len(), 3);
    assert_eq!(failed_messages[2].status, "FAILED");
    assert_eq!(
        failed_messages[2].error_code.as_deref(),
        Some("AUTH_NETWORK_TIMEOUT")
    );
    assert_eq!(
        database
            .load_recent_chat_turns(&gpu_one.id, 6)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn restart_recovers_pending_turn_and_ai_cache_clear_removes_chat_evidence() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope-chat.sqlite3");
    let (thread_id, snapshot_id);
    {
        let database = Database::open_at(&path).unwrap();
        database.set_ai_data_consent(true).unwrap();
        let thread = database
            .create_chat_thread(ChatScope::System, None)
            .unwrap();
        let evidence = snapshot(AnalysisScope::System);
        let pending = database
            .begin_chat_submission(&thread.id, &evidence, &"c".repeat(64), "pending on close")
            .unwrap();
        thread_id = thread.id;
        snapshot_id = pending.snapshot_id;
    }

    let reopened = Database::open_at(&path).unwrap();
    let messages = reopened.load_chat_messages(&thread_id).unwrap();
    assert_eq!(messages[0].status, "INTERRUPTED");
    assert_eq!(
        messages[0].error_code.as_deref(),
        Some("AI_CHAT_APP_RESTARTED")
    );
    assert!(
        reopened
            .load_chat_evidence_snapshot(&snapshot_id)
            .unwrap()
            .is_some()
    );
    assert!(reopened.ai_data_consent_granted().unwrap());

    reopened.clear_ai_cache().unwrap();
    assert!(reopened.load_chat_messages(&thread_id).unwrap().is_empty());
    assert!(
        reopened
            .load_chat_evidence_snapshot(&snapshot_id)
            .unwrap()
            .is_none()
    );
    assert!(reopened.ai_data_consent_granted().unwrap());
}

#[test]
fn v5_database_upgrades_to_v6_without_dropping_existing_chat_rows() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("corescope-v5.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(concat!(
            include_str!("../migrations/001_initial.sql"),
            include_str!("../migrations/002_device_layers.sql"),
            include_str!("../migrations/003_capability_profiles.sql"),
            include_str!("../migrations/004_runtime_history.sql"),
            include_str!("../migrations/005_ai_analysis_lookup.sql")
        ))
        .unwrap();
    connection.pragma_update(None, "user_version", 5).unwrap();
    connection.execute(
        "INSERT INTO chat_threads(id, scope, target_id, created_at, updated_at) VALUES ('thread-v5', 'system', NULL, '2026-10-14T10:00:00Z', '2026-10-14T10:00:00Z')",
        [],
    ).unwrap();
    connection.execute(
        "INSERT INTO chat_messages(id, thread_id, role, body, evidence_snapshot_ref, created_at, status) VALUES ('message-v5', 'thread-v5', 'user', 'preserve me', NULL, '2026-10-14T10:00:00Z', 'COMPLETED')",
        [],
    ).unwrap();
    drop(connection);

    let database = Database::open_at(&path).unwrap();
    assert_eq!(database.schema_version().unwrap(), 6);
    let messages = database.load_chat_messages("thread-v5").unwrap();
    assert_eq!(messages[0].body, "preserve me");
    assert!(messages[0].citations.is_empty());
}

#[test]
fn clearing_one_local_conversation_removes_its_messages_and_snapshots_only() {
    let database = Database::open_in_memory().unwrap();
    let selected = database
        .create_chat_thread(ChatScope::System, None)
        .unwrap();
    let other = database
        .create_chat_thread(ChatScope::Device, Some("gpu-keep"))
        .unwrap();
    let evidence = snapshot(AnalysisScope::System);
    let selected_submission = database
        .begin_chat_submission(
            &selected.id,
            &evidence,
            &"d".repeat(64),
            "remove this local question",
        )
        .unwrap();
    database
        .fail_chat_submission(
            &selected_submission.user_message_id,
            "FAILED",
            "FIXTURE_ERROR",
        )
        .unwrap();
    let other_evidence = snapshot(AnalysisScope::Device);
    database
        .begin_chat_submission(
            &other.id,
            &other_evidence,
            &"e".repeat(64),
            "keep this local question",
        )
        .unwrap();

    assert_eq!(database.clear_chat_thread(&selected.id).unwrap(), 1);
    assert!(
        database
            .load_chat_messages(&selected.id)
            .unwrap()
            .is_empty()
    );
    assert!(database.load_chat_thread(&selected.id).unwrap().is_some());
    assert!(
        database
            .load_chat_evidence_snapshot(&evidence.snapshot_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(database.load_chat_messages(&other.id).unwrap().len(), 1);
    assert!(
        database
            .load_chat_evidence_snapshot(&other_evidence.snapshot_id)
            .unwrap()
            .is_some()
    );
}
