use corescope_lib::{
    analysis::{AnalysisScope, EvidenceSnapshot, build_analysis_prompt, parse_assessment},
    auth::{oauth::NetworkRoute, service::AuthService},
    chat::{build_chat_prompt, parse_chat_answer},
};
use serde_json::json;
use std::{env, path::PathBuf};

fn synthetic_snapshot() -> EvidenceSnapshot {
    serde_json::from_value(json!({
        "snapshotId": "fixture-synthetic-snapshot",
        "scope": "system",
        "capturedAtUtc": "2026-10-15T02:55:00Z",
        "scanAtUtc": "2026-10-15T02:54:00Z",
        "devices": [{
            "evidenceKey": "device.synthetic-cpu",
            "deviceKey": "device",
            "category": "cpu",
            "kind": "processor",
            "name": "Fixture CPU",
            "manufacturer": "Example Vendor",
            "virtualDevice": false,
            "source": "OS_REPORTED",
            "attributes": { "physical_cores": 4, "logical_processors": 8 }
        }],
        "metrics": [{
            "evidenceKey": "metric.device.cpu.utilization.percent",
            "deviceKey": "device",
            "metricKey": "cpu.utilization",
            "state": "AVAILABLE",
            "value": 0.0,
            "unit": "percent",
            "source": "OS_REPORTED",
            "evidenceLevel": "OS_REPORTED",
            "observedAtUtc": "2026-10-15T02:54:30Z",
            "window": null
        }]
    }))
    .expect("SYNTHETIC_EVIDENCE_FIXTURE_INVALID")
}

fn synthetic_chat_snapshot() -> EvidenceSnapshot {
    serde_json::from_value(json!({
        "snapshotId": "fixture-synthetic-chat-snapshot",
        "scope": "device",
        "capturedAtUtc": "2026-10-15T03:10:00Z",
        "scanAtUtc": "2026-10-15T03:09:00Z",
        "devices": [{
            "evidenceKey": "device.synthetic-cpu",
            "deviceKey": "device",
            "category": "cpu",
            "kind": "processor",
            "name": "Fixture CPU",
            "manufacturer": "Example Vendor",
            "virtualDevice": false,
            "source": "OS_REPORTED",
            "attributes": { "physical_cores": 4, "logical_processors": 8 }
        }],
        "metrics": [{
            "evidenceKey": "metric.device.cpu.temperature.celsius",
            "deviceKey": "device",
            "metricKey": "cpu.temperature.celsius",
            "state": "NOT_TESTED",
            "value": null,
            "unit": "°C",
            "source": "UNAVAILABLE",
            "evidenceLevel": null,
            "observedAtUtc": null,
            "window": null
        }]
    }))
    .expect("SYNTHETIC_CHAT_EVIDENCE_FIXTURE_INVALID")
}

fn explicit_live_auth_opt_in() {
    assert_eq!(
        env::var("CORESCOPE_LIVE_AUTH_SMOKE").as_deref(),
        Ok("1"),
        "CORESCOPE_LIVE_AUTH_SMOKE must be set to 1 to allow live model requests"
    );
}

fn configured_auth_directory() -> PathBuf {
    let auth_directory = env::var_os("CORESCOPE_AUTH_DIR")
        .map(PathBuf::from)
        .expect("CORESCOPE_AUTH_DIR must point to the existing CoreScope auth directory");
    assert!(
        auth_directory.join("chatgpt-credentials.dpapi").is_file(),
        "the existing protected credential vault is required"
    );
    auth_directory
}

#[tokio::test]
#[ignore = "live ChatGPT Responses smoke test; requires explicit opt-in and sends one fixed non-hardware prompt"]
async fn real_account_completes_the_product_inference_verification_flow() {
    explicit_live_auth_opt_in();
    let auth = AuthService::open(configured_auth_directory()).expect("AUTH_VAULT_OPEN_FAILED");
    let state = auth.public_state().expect("AUTH_STATE_READ_FAILED");
    assert!(state.connected, "AUTH_ACCOUNT_NOT_CONNECTED");
    assert!(
        state.plan_usage_authorized,
        "AUTH_PLAN_USAGE_SCOPE_REQUIRED"
    );
    let client_id = state
        .active_client_id
        .as_deref()
        .expect("AUTH_ACTIVE_ACCOUNT_MISSING");

    let models = auth
        .list_models(Some(client_id), NetworkRoute::SystemProxy, true)
        .await
        .expect("AUTH_MODEL_CATALOG_REQUEST_FAILED");
    let model_slug = models
        .first()
        .map(|model| model.slug.clone())
        .expect("AUTH_MODEL_CATALOG_EMPTY");

    let result = auth
        .verify_inference(Some(client_id), &model_slug, NetworkRoute::SystemProxy)
        .await
        .expect("AUTH_REAL_RESPONSES_VERIFICATION_FAILED");
    assert!(
        result
            .response_text
            .contains("CoreScope ChatGPT connection verified."),
        "AUTH_REAL_RESPONSES_TEXT_MISMATCH"
    );

    println!(
        "LIVE_AUTH_SMOKE status=response.completed models={} model={} completed_at_utc={}",
        models.len(),
        result.model_slug,
        result.completed_at_utc
    );
}

#[tokio::test]
#[ignore = "live ChatGPT analysis-schema smoke test; sends only synthetic hardware evidence"]
async fn real_account_returns_strict_five_section_analysis_for_synthetic_evidence() {
    explicit_live_auth_opt_in();
    let auth = AuthService::open(configured_auth_directory()).expect("AUTH_VAULT_OPEN_FAILED");
    let state = auth.public_state().expect("AUTH_STATE_READ_FAILED");
    assert!(state.connected, "AUTH_ACCOUNT_NOT_CONNECTED");
    assert!(
        state.plan_usage_authorized,
        "AUTH_PLAN_USAGE_SCOPE_REQUIRED"
    );
    let client_id = state
        .active_client_id
        .as_deref()
        .expect("AUTH_ACTIVE_ACCOUNT_MISSING");

    let models = auth
        .list_models(Some(client_id), NetworkRoute::SystemProxy, true)
        .await
        .expect("AUTH_MODEL_CATALOG_REQUEST_FAILED");
    let model_slug = models
        .first()
        .map(|model| model.slug.clone())
        .expect("AUTH_MODEL_CATALOG_EMPTY");
    let snapshot = synthetic_snapshot();
    assert_eq!(snapshot.scope, AnalysisScope::System);
    let prompt = build_analysis_prompt(&snapshot).expect("AI_ANALYSIS_PROMPT_BUILD_FAILED");

    let response = auth
        .complete_text_response(
            Some(client_id),
            &model_slug,
            NetworkRoute::SystemProxy,
            &prompt,
        )
        .await
        .expect("AUTH_REAL_ANALYSIS_RESPONSE_FAILED");
    let assessment = parse_assessment(
        &response.response_text,
        &snapshot,
        None,
        &model_slug,
        response.completed_at_utc.clone(),
    )
    .expect("AI_ANALYSIS_RESPONSE_REJECTED");
    assert_eq!(assessment.priority_sections.len(), 5);

    println!("SYNTHETIC_SUMMARY {}", assessment.summary);
    for section in &assessment.priority_sections {
        let item_text = section
            .items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        println!("SYNTHETIC_SECTION {:?} {}", section.key, item_text);
    }
    println!(
        "LIVE_ANALYSIS_SMOKE status=response.completed sections={} model={} completed_at_utc={}",
        assessment.priority_sections.len(),
        response.model_slug,
        response.completed_at_utc
    );
}

#[tokio::test]
#[ignore = "live ChatGPT hardware-context chat smoke test; sends only synthetic hardware evidence"]
async fn real_account_answers_a_hardware_question_from_synthetic_evidence() {
    explicit_live_auth_opt_in();
    let auth = AuthService::open(configured_auth_directory()).expect("AUTH_VAULT_OPEN_FAILED");
    let state = auth.public_state().expect("AUTH_STATE_READ_FAILED");
    assert!(state.connected, "AUTH_ACCOUNT_NOT_CONNECTED");
    assert!(
        state.plan_usage_authorized,
        "AUTH_PLAN_USAGE_SCOPE_REQUIRED"
    );
    let client_id = state
        .active_client_id
        .as_deref()
        .expect("AUTH_ACTIVE_ACCOUNT_MISSING");

    let models = auth
        .list_models(Some(client_id), NetworkRoute::SystemProxy, true)
        .await
        .expect("AUTH_MODEL_CATALOG_REQUEST_FAILED");
    let model_slug = models
        .first()
        .map(|model| model.slug.clone())
        .expect("AUTH_MODEL_CATALOG_EMPTY");
    let snapshot = synthetic_chat_snapshot();
    assert_eq!(snapshot.scope, AnalysisScope::Device);
    let prompt = build_chat_prompt(
        &snapshot,
        &[],
        "该CPU温度字段当前是什么状态？能否根据此快照判断CPU健康？",
    )
    .expect("AI_CHAT_PROMPT_BUILD_FAILED");

    let response = auth
        .complete_text_response(
            Some(client_id),
            &model_slug,
            NetworkRoute::SystemProxy,
            &prompt,
        )
        .await
        .expect("AUTH_REAL_CHAT_RESPONSE_FAILED");
    let answer =
        parse_chat_answer(&response.response_text, &snapshot).expect("AI_CHAT_RESPONSE_REJECTED");
    assert!(!answer.answer.trim().is_empty(), "AI_CHAT_ANSWER_EMPTY");
    assert!(
        answer.answer.contains("未测试")
            || answer.answer.contains("无法判断")
            || answer.answer.contains("不能判断"),
        "AI_CHAT_DID_NOT_PRESERVE_UNKNOWN_TEMPERATURE_STATE"
    );

    println!("SYNTHETIC_CHAT_ANSWER {}", answer.answer);
    println!(
        "LIVE_CHAT_SMOKE status=response.completed citations={} model={} snapshot={} completed_at_utc={}",
        answer.citations.len(),
        response.model_slug,
        answer.evidence_snapshot_id,
        response.completed_at_utc
    );
}
