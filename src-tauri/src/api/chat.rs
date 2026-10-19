use crate::{
    analysis::AnalysisCoordinator,
    api::analysis::{load_current_evidence, validate_auth_state},
    auth::{oauth::NetworkRoute, service::AuthService},
    chat::{
        ChatAnswer, ChatCoordinator, ChatPreview, ChatScope, build_chat_prompt, parse_chat_answer,
    },
    persistence::{ChatMessageRecord, ChatThreadRecord, Database},
    telemetry::TelemetryManager,
};
use serde::Serialize;
use tauri::State;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendOutcome {
    pub status: String,
    pub user_message_id: String,
    pub assistant_message: Option<ChatMessageRecord>,
    pub error_code: Option<String>,
}

#[tauri::command(rename_all = "camelCase")]
pub fn list_chat_threads(
    scope: ChatScope,
    target_device_id: Option<String>,
    database: State<'_, Database>,
) -> Result<Vec<ChatThreadRecord>, String> {
    validate_scope_target(scope, target_device_id.as_deref())?;
    database.list_chat_threads(scope, target_device_id.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
pub fn create_chat_thread(
    scope: ChatScope,
    target_device_id: Option<String>,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
) -> Result<ChatThreadRecord, String> {
    validate_scope_target(scope, target_device_id.as_deref())?;
    load_current_evidence(
        &database,
        &telemetry,
        scope.analysis_scope(),
        target_device_id.as_deref(),
    )?;
    database.create_chat_thread(scope, target_device_id.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
pub fn load_chat_messages(
    thread_id: String,
    database: State<'_, Database>,
) -> Result<Vec<ChatMessageRecord>, String> {
    if database.load_chat_thread(&thread_id)?.is_none() {
        return Err("SQLITE_CHAT_THREAD_NOT_FOUND".to_owned());
    }
    database.load_chat_messages(&thread_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn clear_chat_thread(
    thread_id: String,
    database: State<'_, Database>,
) -> Result<usize, String> {
    if database.load_chat_thread(&thread_id)?.is_none() {
        return Err("SQLITE_CHAT_THREAD_NOT_FOUND".to_owned());
    }
    database.clear_chat_thread(&thread_id)
}

#[tauri::command(rename_all = "camelCase")]
pub fn create_chat_preview(
    scope: ChatScope,
    target_device_id: Option<String>,
    thread_id: String,
    question: String,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    coordinator: State<'_, ChatCoordinator>,
) -> Result<ChatPreview, String> {
    validate_scope_target(scope, target_device_id.as_deref())?;
    let thread = database
        .load_chat_thread(&thread_id)?
        .ok_or_else(|| "SQLITE_CHAT_THREAD_NOT_FOUND".to_owned())?;
    validate_thread_context(&thread, scope, target_device_id.as_deref())?;
    let recent_turns = database.load_recent_chat_turns(&thread_id, 6)?;
    let (snapshot, fingerprint) = load_current_evidence(
        &database,
        &telemetry,
        scope.analysis_scope(),
        target_device_id.as_deref(),
    )?;
    coordinator.store_preview(
        scope,
        target_device_id,
        Some(thread_id),
        question,
        recent_turns,
        snapshot,
        fingerprint,
    )
}

#[allow(clippy::too_many_arguments)]
#[tauri::command(rename_all = "camelCase")]
pub async fn send_chat_message(
    preview_token: String,
    scope: ChatScope,
    target_device_id: Option<String>,
    thread_id: String,
    client_id: String,
    model_slug: String,
    route: NetworkRoute,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    auth: State<'_, AuthService>,
    ai_coordinator: State<'_, AnalysisCoordinator>,
    chat_coordinator: State<'_, ChatCoordinator>,
) -> Result<ChatSendOutcome, String> {
    require_chat_consent(database.ai_data_consent_granted()?)?;
    validate_scope_target(scope, target_device_id.as_deref())?;
    validate_auth_state(&auth.public_state()?, &client_id, &model_slug)?;
    let mut job = ai_coordinator.enter_job("ai".to_owned())?;
    let preview = chat_coordinator.take_preview(
        &preview_token,
        scope,
        target_device_id.as_deref(),
        Some(&thread_id),
    )?;

    let thread = database
        .load_chat_thread(&thread_id)?
        .ok_or_else(|| "SQLITE_CHAT_THREAD_NOT_FOUND".to_owned())?;
    validate_thread_context(&thread, preview.scope, target_device_id.as_deref())?;

    let (_, current_fingerprint) = load_current_evidence(
        &database,
        &telemetry,
        scope.analysis_scope(),
        target_device_id.as_deref(),
    )?;
    validate_preview_fingerprint(&preview.input_fingerprint, &current_fingerprint)?;
    if database.load_recent_chat_turns(&thread_id, 6)? != preview.recent_turns {
        return Err("AI_CHAT_CONTEXT_CHANGED".to_owned());
    }

    let prompt = build_chat_prompt(&preview.snapshot, &preview.recent_turns, &preview.question)?;
    let pending = database.begin_chat_submission(
        &thread_id,
        &preview.snapshot,
        &preview.input_fingerprint,
        &preview.question,
    )?;
    let result = async {
        let response = tokio::select! {
            biased;
            _ = job.cancelled() => return Err("AI_ANALYSIS_CANCELLED".to_owned()),
            response = auth.complete_text_response(Some(&client_id), &model_slug, route, &prompt) => response?,
        };
        let answer = parse_chat_answer(&response.response_text, &preview.snapshot)?;
        database.complete_chat_submission(
            &pending.user_message_id,
            &answer,
            &response.model_slug,
            &response.completed_at_utc,
        )?;
        Ok::<ChatAnswer, String>(answer)
    }
    .await;
    drop(job);

    match result {
        Ok(_) => {
            let assistant_message = database
                .load_chat_messages(&thread_id)?
                .into_iter()
                .rev()
                .find(|message| {
                    message.role == "assistant"
                        && message.evidence_snapshot_ref.as_deref()
                            == Some(pending.snapshot_id.as_str())
                });
            Ok(ChatSendOutcome {
                status: "COMPLETED".to_owned(),
                user_message_id: pending.user_message_id,
                assistant_message,
                error_code: None,
            })
        }
        Err(error_code) => {
            let status = if error_code == "AI_ANALYSIS_CANCELLED"
                || error_code.contains("SSE_STREAM_INTERRUPTED")
            {
                "INTERRUPTED"
            } else {
                "FAILED"
            };
            database.fail_chat_submission(&pending.user_message_id, status, &error_code)?;
            tracing::warn!(scope = scope.as_str(), error_code = %error_code, "AI chat request did not complete");
            Ok(ChatSendOutcome {
                status: status.to_owned(),
                user_message_id: pending.user_message_id,
                assistant_message: None,
                error_code: Some(error_code),
            })
        }
    }
}

fn validate_scope_target(scope: ChatScope, target_device_id: Option<&str>) -> Result<(), String> {
    if (scope == ChatScope::Device) != target_device_id.is_some() {
        return Err("AI_CHAT_TARGET_SCOPE_INVALID".to_owned());
    }
    Ok(())
}

fn require_chat_consent(granted: bool) -> Result<(), String> {
    if granted {
        Ok(())
    } else {
        Err("AI_ANALYSIS_CONSENT_REQUIRED".to_owned())
    }
}

fn validate_thread_context(
    thread: &ChatThreadRecord,
    scope: ChatScope,
    target_device_id: Option<&str>,
) -> Result<(), String> {
    if thread.scope != scope || thread.target_device_id.as_deref() != target_device_id {
        return Err("AI_CHAT_THREAD_SCOPE_INVALID".to_owned());
    }
    Ok(())
}

fn validate_preview_fingerprint(preview: &str, current: &str) -> Result<(), String> {
    if preview == current {
        Ok(())
    } else {
        Err("AI_CHAT_PREVIEW_STALE".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_target_pairing_is_enforced_for_local_chat_routes() {
        assert!(validate_scope_target(ChatScope::System, None).is_ok());
        assert!(validate_scope_target(ChatScope::System, Some("device-1")).is_err());
        assert!(validate_scope_target(ChatScope::Device, None).is_err());
        assert!(validate_scope_target(ChatScope::Device, Some("device-1")).is_ok());
    }

    #[test]
    fn chat_submission_rejects_a_thread_from_another_hardware_context() {
        let thread = ChatThreadRecord {
            id: "thread".to_owned(),
            scope: ChatScope::Device,
            target_device_id: Some("gpu-a".to_owned()),
            created_at_utc: String::new(),
            updated_at_utc: String::new(),
        };
        assert!(validate_thread_context(&thread, ChatScope::Device, Some("gpu-a")).is_ok());
        assert_eq!(
            validate_thread_context(&thread, ChatScope::Device, Some("gpu-b")).unwrap_err(),
            "AI_CHAT_THREAD_SCOPE_INVALID"
        );
        assert!(validate_thread_context(&thread, ChatScope::System, None).is_err());
    }

    #[test]
    fn stale_hardware_context_requires_a_new_local_preview() {
        assert!(validate_preview_fingerprint("same", "same").is_ok());
        assert_eq!(
            validate_preview_fingerprint("before", "after").unwrap_err(),
            "AI_CHAT_PREVIEW_STALE"
        );
    }

    #[test]
    fn chat_send_requires_saved_consent_and_authorized_active_account() {
        assert_eq!(
            require_chat_consent(false).unwrap_err(),
            "AI_ANALYSIS_CONSENT_REQUIRED"
        );
        assert!(require_chat_consent(true).is_ok());

        let mut state = crate::auth::service::ChatGptAuthState {
            accounts: Vec::new(),
            active_client_id: Some("oaiapp-one".to_owned()),
            connected: true,
            plan_usage_authorized: true,
        };
        assert!(validate_auth_state(&state, "oaiapp-one", "model-one").is_ok());
        assert_eq!(
            validate_auth_state(&state, "oaiapp-other", "model-one").unwrap_err(),
            "AUTH_ACTIVE_ACCOUNT_CHANGED"
        );
        state.plan_usage_authorized = false;
        assert_eq!(
            validate_auth_state(&state, "oaiapp-one", "model-one").unwrap_err(),
            "AUTH_PLAN_USAGE_SCOPE_REQUIRED"
        );
    }
}
