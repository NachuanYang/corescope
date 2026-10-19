use corescope_lib::{
    analysis::AnalysisCoordinator,
    analysis::{AnalysisScope, EvidenceDevice, EvidenceMetric, EvidenceSnapshot, MetricWindow},
    chat::{ChatCoordinator, ChatScope, ChatTurn, build_chat_prompt, parse_chat_answer},
};
use serde_json::json;
use std::collections::BTreeMap;

fn snapshot() -> EvidenceSnapshot {
    EvidenceSnapshot {
        snapshot_id: "chat-snapshot-001".to_owned(),
        scope: AnalysisScope::Device,
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
        metrics: vec![
            EvidenceMetric {
                evidence_key: "metric.device.gpu.utilization.percent".to_owned(),
                device_key: "device".to_owned(),
                metric_key: "gpu.utilization.percent".to_owned(),
                state: "AVAILABLE".to_owned(),
                value: Some(json!(0.0)),
                unit: Some("percent".to_owned()),
                source: "NVML".to_owned(),
                evidence_level: Some("MEASURED".to_owned()),
                reason_code: None,
                observed_at_utc: Some("2026-10-14T12:00:00Z".to_owned()),
                window: Some(MetricWindow {
                    sample_count: 2,
                    minimum: Some(0.0),
                    mean: Some(0.0),
                    maximum: Some(0.0),
                    started_at_utc: Some("2026-10-14T11:59:58Z".to_owned()),
                    ended_at_utc: Some("2026-10-14T12:00:00Z".to_owned()),
                }),
            },
            EvidenceMetric {
                evidence_key: "metric.device.gpu.temperature.celsius".to_owned(),
                device_key: "device".to_owned(),
                metric_key: "gpu.temperature.celsius".to_owned(),
                state: "NOT_TESTED".to_owned(),
                value: None,
                unit: Some("°C".to_owned()),
                source: "NVML".to_owned(),
                evidence_level: None,
                reason_code: Some("LIVE_PROBE_NOT_RUN".to_owned()),
                observed_at_utc: None,
                window: None,
            },
        ],
    }
}

fn turn(role: &str, status: &str, body: &str) -> ChatTurn {
    ChatTurn {
        role: role.to_owned(),
        status: status.to_owned(),
        body: body.to_owned(),
    }
}

#[test]
fn prompt_includes_only_recent_completed_turns_and_preserves_zero_and_unavailable_states() {
    let snapshot = snapshot();
    let prompt = build_chat_prompt(
        &snapshot,
        &[
            turn("user", "COMPLETED", "请解释显卡负载。"),
            turn("assistant", "COMPLETED", "负载证据是0%。"),
            turn("user", "FAILED", "PRIVATE_FAILED_TURN"),
            turn("system", "COMPLETED", "PRIVATE_SYSTEM_TURN"),
        ],
        "温度现在是多少？",
    )
    .unwrap();

    assert!(prompt.contains("chat-snapshot-001"));
    assert!(prompt.contains("温度现在是多少？"));
    assert!(prompt.contains("请解释显卡负载。"));
    assert!(prompt.contains("负载证据是0%。"));
    assert!(prompt.contains("NOT_TESTED"));
    assert!(prompt.contains("\"value\":0.0"));
    assert!(prompt.contains("allowedEvidenceKeys"));
    assert!(prompt.contains("\"metric.device.gpu.utilization.percent\""));
    assert!(!prompt.contains("PRIVATE_FAILED_TURN"));
    assert!(!prompt.contains("PRIVATE_SYSTEM_TURN"));
}

#[test]
fn answer_canonicalizes_a_unique_device_metric_alias_to_the_snapshot_key() {
    let mut snapshot = snapshot();
    snapshot.scope = AnalysisScope::System;
    snapshot.devices[0].device_key = "hardware-1".to_owned();
    snapshot.devices[0].evidence_key = "hardware-1.kind".to_owned();
    snapshot.metrics[0].device_key = "hardware-1".to_owned();
    snapshot.metrics[0].evidence_key = "metric.hardware-1.gpu.utilization.percent".to_owned();
    snapshot.metrics[1].device_key = "hardware-1".to_owned();
    snapshot.metrics[1].evidence_key = "metric.hardware-1.gpu.temperature.celsius".to_owned();
    let answer_json = json!({
        "answer": "显卡利用率为0%。",
        "citations": [{
            "claim": "显卡利用率为0%。",
            "evidenceKeys": ["metric.device.gpu.utilization.percent"]
        }],
        "evidenceSnapshotId": snapshot.snapshot_id
    })
    .to_string();

    let answer = parse_chat_answer(&answer_json, &snapshot).unwrap();

    assert_eq!(
        answer.citations[0].evidence_keys,
        ["metric.hardware-1.gpu.utilization.percent"]
    );
}

#[test]
fn answer_rejects_an_ambiguous_metric_alias_across_devices() {
    let mut snapshot = snapshot();
    snapshot.scope = AnalysisScope::System;
    snapshot.devices[0].device_key = "hardware-1".to_owned();
    snapshot.devices[0].evidence_key = "hardware-1.kind".to_owned();
    for metric in &mut snapshot.metrics {
        metric.device_key = "hardware-1".to_owned();
        metric.evidence_key = format!("metric.hardware-1.{}", metric.metric_key);
    }
    let second_gpu = EvidenceDevice {
        evidence_key: "hardware-2.kind".to_owned(),
        device_key: "hardware-2".to_owned(),
        category: "gpu".to_owned(),
        kind: "gpu_adapter".to_owned(),
        name: Some("Second GPU".to_owned()),
        manufacturer: Some("Fixture Vendor".to_owned()),
        virtual_device: Some(false),
        source: "Windows inventory".to_owned(),
        attributes: BTreeMap::new(),
    };
    let mut second_gpu_metric = snapshot.metrics[0].clone();
    second_gpu_metric.device_key = "hardware-2".to_owned();
    second_gpu_metric.evidence_key = "metric.hardware-2.gpu.utilization.percent".to_owned();
    snapshot.devices.push(second_gpu);
    snapshot.metrics.push(second_gpu_metric);
    let answer_json = json!({
        "answer": "显卡利用率为0%。",
        "citations": [{
            "claim": "显卡利用率为0%。",
            "evidenceKeys": ["metric.device.gpu.utilization.percent"]
        }],
        "evidenceSnapshotId": snapshot.snapshot_id
    })
    .to_string();

    assert_eq!(
        parse_chat_answer(&answer_json, &snapshot).unwrap_err(),
        "AI_CHAT_RESPONSE_INVALID_EVIDENCE"
    );
}

#[test]
fn prompt_rejects_empty_or_oversized_user_questions() {
    assert_eq!(
        build_chat_prompt(&snapshot(), &[], "  ").unwrap_err(),
        "AI_CHAT_QUESTION_INVALID"
    );
    assert_eq!(
        build_chat_prompt(&snapshot(), &[], &"Q".repeat(2001)).unwrap_err(),
        "AI_CHAT_QUESTION_INVALID"
    );
}

#[test]
fn answer_requires_matching_snapshot_and_known_evidence_keys() {
    let snapshot = snapshot();
    let valid = json!({
        "answer": "当前报告的显卡负载为0%；温度未测试。",
        "citations": [{
            "claim": "当前报告的显卡负载为0%。",
            "evidenceKeys": ["metric.device.gpu.utilization.percent"]
        }],
        "evidenceSnapshotId": snapshot.snapshot_id
    })
    .to_string();
    let answer = parse_chat_answer(&valid, &snapshot).unwrap();
    assert_eq!(answer.citations.len(), 1);
    assert_eq!(answer.evidence_snapshot_id, "chat-snapshot-001");

    let unknown = valid.replace(
        "metric.device.gpu.utilization.percent",
        "local.gpu.instance.id",
    );
    assert_eq!(
        parse_chat_answer(&unknown, &snapshot).unwrap_err(),
        "AI_CHAT_RESPONSE_INVALID_EVIDENCE"
    );
    let mismatched = valid.replace("chat-snapshot-001", "different-snapshot");
    assert_eq!(
        parse_chat_answer(&mismatched, &snapshot).unwrap_err(),
        "AI_CHAT_RESPONSE_INVALID"
    );
}

#[test]
fn answer_rejects_malformed_and_unbounded_output() {
    let snapshot = snapshot();
    assert_eq!(
        parse_chat_answer("not json", &snapshot).unwrap_err(),
        "AI_CHAT_RESPONSE_INVALID_JSON"
    );
    let oversized = json!({
        "answer": "A".repeat(8001),
        "citations": [],
        "evidenceSnapshotId": snapshot.snapshot_id
    })
    .to_string();
    assert_eq!(
        parse_chat_answer(&oversized, &snapshot).unwrap_err(),
        "AI_CHAT_RESPONSE_INVALID"
    );
}

#[test]
fn preview_token_is_one_use_and_bound_to_scope_device_and_thread() {
    let coordinator = ChatCoordinator::default();
    let preview = coordinator
        .store_preview(
            ChatScope::Device,
            Some("gpu-local-only".to_owned()),
            Some("thread-1".to_owned()),
            "Is this GPU adequate?".to_owned(),
            vec![turn("user", "COMPLETED", "previous turn")],
            snapshot(),
            "fingerprint-1".to_owned(),
        )
        .unwrap();

    assert_eq!(preview.snapshot.snapshot_id, "chat-snapshot-001");
    let consumed = coordinator
        .take_preview(
            &preview.token,
            ChatScope::Device,
            Some("gpu-local-only"),
            Some("thread-1"),
        )
        .unwrap();
    assert_eq!(consumed.question, "Is this GPU adequate?");
    assert_eq!(consumed.input_fingerprint, "fingerprint-1");
    assert_eq!(
        coordinator
            .take_preview(
                &preview.token,
                ChatScope::Device,
                Some("gpu-local-only"),
                Some("thread-1"),
            )
            .unwrap_err(),
        "AI_CHAT_PREVIEW_EXPIRED"
    );
}

#[tokio::test]
async fn chat_preview_cannot_be_reused_for_another_target_and_shared_ai_job_cancels() {
    let coordinator = ChatCoordinator::default();
    let preview = coordinator
        .store_preview(
            ChatScope::Device,
            Some("gpu-local-only".to_owned()),
            None,
            "Question".to_owned(),
            Vec::new(),
            snapshot(),
            "fingerprint-1".to_owned(),
        )
        .unwrap();
    assert_eq!(
        coordinator
            .take_preview(
                &preview.token,
                ChatScope::Device,
                Some("other-device"),
                None
            )
            .unwrap_err(),
        "AI_CHAT_PREVIEW_EXPIRED"
    );

    let jobs = AnalysisCoordinator::default();
    let mut job = jobs.enter_job("ai".to_owned()).unwrap();
    assert!(jobs.cancel_active_job().unwrap());
    job.cancelled().await;
}
