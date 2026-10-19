use crate::{
    analysis::{
        AiAssessment, AnalysisCoordinator, AnalysisJob, AnalysisPreview, AnalysisRunOutcome,
        AnalysisScope, AnalysisTargetState, EvidenceSnapshot, build_analysis_prompt,
        build_evidence_snapshot_with_capabilities, input_fingerprint, parse_assessment,
    },
    auth::{
        oauth::NetworkRoute,
        service::{AuthService, ChatGptAuthState},
    },
    capabilities::CapabilityRecord,
    inventory::{DeviceKind, DeviceLayer},
    persistence::Database,
    telemetry::TelemetryManager,
};
use chrono::Utc;
use tauri::State;

#[tauri::command(rename_all = "camelCase")]
pub fn get_analysis_target_state(
    scope: AnalysisScope,
    target_device_id: Option<String>,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
) -> Result<AnalysisTargetState, String> {
    validate_scope_target(scope, target_device_id.as_deref())?;
    if let Ok((_, fingerprint)) =
        load_current_evidence(&database, &telemetry, scope, target_device_id.as_deref())
    {
        database.mark_ai_analyses_current(scope, target_device_id.as_deref(), &fingerprint)?;
    }
    database.analysis_target_state(scope, target_device_id.as_deref())
}

#[tauri::command(rename_all = "camelCase")]
pub fn create_analysis_preview(
    scope: AnalysisScope,
    target_device_id: Option<String>,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    coordinator: State<'_, AnalysisCoordinator>,
) -> Result<AnalysisPreview, String> {
    validate_scope_target(scope, target_device_id.as_deref())?;
    let (snapshot, fingerprint) =
        load_current_evidence(&database, &telemetry, scope, target_device_id.as_deref())?;
    coordinator.store_preview(snapshot, target_device_id, fingerprint)
}

#[tauri::command]
pub fn set_ai_data_consent(granted: bool, database: State<'_, Database>) -> Result<bool, String> {
    database.set_ai_data_consent(granted)?;
    database.ai_data_consent_granted()
}

#[tauri::command]
pub fn cancel_ai_request(coordinator: State<'_, AnalysisCoordinator>) -> Result<bool, String> {
    coordinator.cancel_active_job()
}

#[tauri::command(rename_all = "camelCase")]
pub async fn start_initial_system_analysis(
    preview_token: String,
    client_id: String,
    model_slug: String,
    route: NetworkRoute,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    auth: State<'_, AuthService>,
    coordinator: State<'_, AnalysisCoordinator>,
) -> Result<AnalysisRunOutcome, String> {
    if !database.ai_data_consent_granted()? {
        return Err("AI_ANALYSIS_CONSENT_REQUIRED".to_owned());
    }
    ensure_auth_ready(&auth, &client_id, &model_slug)?;
    let mut job = coordinator.enter_job("ai".to_owned())?;
    let (snapshot, preview_fingerprint) = coordinator.take_preview(&preview_token, None)?;
    if snapshot.scope != AnalysisScope::System {
        return Err("AI_ANALYSIS_PREVIEW_SCOPE_INVALID".to_owned());
    }
    let (_, current_fingerprint) =
        load_current_evidence(&database, &telemetry, AnalysisScope::System, None)?;
    if preview_fingerprint != current_fingerprint {
        return Err("AI_ANALYSIS_PREVIEW_STALE".to_owned());
    }
    if !database.claim_initial_system_analysis()? {
        return Ok(run_outcome(
            &database,
            AnalysisScope::System,
            None,
            None,
            None,
        )?);
    }
    database.mark_ai_analyses_current(AnalysisScope::System, None, &current_fingerprint)?;
    let outcome = execute_analysis(
        &database,
        &auth,
        AnalysisScope::System,
        None,
        snapshot,
        preview_fingerprint,
        &client_id,
        &model_slug,
        route,
        &mut job,
    )
    .await;
    drop(job);
    outcome
}

#[tauri::command(rename_all = "camelCase")]
pub async fn start_initial_device_analysis(
    device_id: String,
    preview_token: String,
    client_id: String,
    model_slug: String,
    route: NetworkRoute,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    auth: State<'_, AuthService>,
    coordinator: State<'_, AnalysisCoordinator>,
) -> Result<AnalysisRunOutcome, String> {
    if !database.ai_data_consent_granted()? {
        return Err("AI_ANALYSIS_CONSENT_REQUIRED".to_owned());
    }
    validate_scope_target(AnalysisScope::Device, Some(&device_id))?;
    ensure_auth_ready(&auth, &client_id, &model_slug)?;
    let mut job = coordinator.enter_job("ai".to_owned())?;
    let (snapshot, preview_fingerprint) =
        coordinator.take_preview(&preview_token, Some(&device_id))?;
    if snapshot.scope != AnalysisScope::Device {
        return Err("AI_ANALYSIS_PREVIEW_SCOPE_INVALID".to_owned());
    }
    let (_, current_fingerprint) = load_current_evidence(
        &database,
        &telemetry,
        AnalysisScope::Device,
        Some(&device_id),
    )?;
    if preview_fingerprint != current_fingerprint {
        return Err("AI_ANALYSIS_PREVIEW_STALE".to_owned());
    }
    if !database.claim_initial_device_analysis(&device_id)? {
        return run_outcome(
            &database,
            AnalysisScope::Device,
            Some(&device_id),
            None,
            None,
        );
    }
    database.mark_ai_analyses_current(
        AnalysisScope::Device,
        Some(&device_id),
        &current_fingerprint,
    )?;
    let outcome = execute_analysis(
        &database,
        &auth,
        AnalysisScope::Device,
        Some(&device_id),
        snapshot,
        preview_fingerprint,
        &client_id,
        &model_slug,
        route,
        &mut job,
    )
    .await;
    drop(job);
    outcome
}

#[tauri::command(rename_all = "camelCase")]
pub async fn refresh_system_analysis(
    preview_token: Option<String>,
    client_id: String,
    model_slug: String,
    route: NetworkRoute,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    auth: State<'_, AuthService>,
    coordinator: State<'_, AnalysisCoordinator>,
) -> Result<AnalysisRunOutcome, String> {
    run_manual_analysis(
        AnalysisScope::System,
        None,
        preview_token,
        client_id,
        model_slug,
        route,
        database,
        telemetry,
        auth,
        coordinator,
    )
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn refresh_device_analysis(
    device_id: String,
    preview_token: String,
    client_id: String,
    model_slug: String,
    route: NetworkRoute,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    auth: State<'_, AuthService>,
    coordinator: State<'_, AnalysisCoordinator>,
) -> Result<AnalysisRunOutcome, String> {
    run_manual_analysis(
        AnalysisScope::Device,
        Some(device_id),
        Some(preview_token),
        client_id,
        model_slug,
        route,
        database,
        telemetry,
        auth,
        coordinator,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_manual_analysis(
    scope: AnalysisScope,
    target_device_id: Option<String>,
    preview_token: Option<String>,
    client_id: String,
    model_slug: String,
    route: NetworkRoute,
    database: State<'_, Database>,
    telemetry: State<'_, TelemetryManager>,
    auth: State<'_, AuthService>,
    coordinator: State<'_, AnalysisCoordinator>,
) -> Result<AnalysisRunOutcome, String> {
    if !database.ai_data_consent_granted()? {
        return Err("AI_ANALYSIS_CONSENT_REQUIRED".to_owned());
    }
    validate_scope_target(scope, target_device_id.as_deref())?;
    ensure_auth_ready(&auth, &client_id, &model_slug)?;
    let mut job = coordinator.enter_job("ai".to_owned())?;
    let (snapshot, fingerprint) = if let Some(token) = preview_token {
        let (preview_snapshot, preview_fingerprint) =
            coordinator.take_preview(&token, target_device_id.as_deref())?;
        if preview_snapshot.scope != scope {
            return Err("AI_ANALYSIS_PREVIEW_SCOPE_INVALID".to_owned());
        }
        let (_, current_fingerprint) =
            load_current_evidence(&database, &telemetry, scope, target_device_id.as_deref())?;
        if preview_fingerprint != current_fingerprint {
            return Err("AI_ANALYSIS_PREVIEW_STALE".to_owned());
        }
        (preview_snapshot, preview_fingerprint)
    } else {
        load_current_evidence(&database, &telemetry, scope, target_device_id.as_deref())?
    };
    database.mark_ai_analyses_current(scope, target_device_id.as_deref(), &fingerprint)?;
    execute_analysis(
        &database,
        &auth,
        scope,
        target_device_id.as_deref(),
        snapshot,
        fingerprint,
        &client_id,
        &model_slug,
        route,
        &mut job,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_analysis(
    database: &Database,
    auth: &AuthService,
    scope: AnalysisScope,
    target_device_id: Option<&str>,
    snapshot: EvidenceSnapshot,
    fingerprint: String,
    client_id: &str,
    model_slug: &str,
    route: NetworkRoute,
    job: &mut AnalysisJob,
) -> Result<AnalysisRunOutcome, String> {
    let prompt = build_analysis_prompt(&snapshot)?;
    let id =
        database.begin_ai_analysis(scope, target_device_id, &fingerprint, &snapshot, model_slug)?;
    let result = async {
        let response = tokio::select! {
            biased;
            _ = job.cancelled() => return Err("AI_ANALYSIS_CANCELLED".to_owned()),
            response = auth.complete_text_response(Some(client_id), model_slug, route, &prompt) => response?,
        };
        let assessment = parse_assessment(
            &response.response_text,
            &snapshot,
            target_device_id.map(str::to_owned),
            model_slug,
            response.completed_at_utc,
        )?;
        database.complete_ai_analysis(&id, &assessment)?;
        Ok::<AiAssessment, String>(assessment)
    }
    .await;

    match result {
        Ok(_) => {
            database.mark_ai_analyses_current(scope, target_device_id, &fingerprint)?;
            run_outcome(database, scope, target_device_id, Some(id), None)
        }
        Err(error_code) => {
            let status = if error_code.contains("SSE_STREAM_INTERRUPTED")
                || error_code == "AI_ANALYSIS_CANCELLED"
            {
                "INTERRUPTED"
            } else {
                "FAILED"
            };
            database.fail_ai_analysis(&id, status, &error_code)?;
            tracing::warn!(scope = scope.as_str(), error_code = %error_code, "AI analysis did not complete");
            run_outcome(
                database,
                scope,
                target_device_id,
                Some(id),
                Some(error_code),
            )
        }
    }
}

fn run_outcome(
    database: &Database,
    scope: AnalysisScope,
    target_device_id: Option<&str>,
    analysis_id: Option<String>,
    error_code: Option<String>,
) -> Result<AnalysisRunOutcome, String> {
    let state = database.analysis_target_state(scope, target_device_id)?;
    let status = state
        .latest_attempt_status
        .clone()
        .unwrap_or_else(|| "NOT_STARTED".to_owned());
    Ok(AnalysisRunOutcome {
        status,
        analysis_id,
        error_code,
        device_analysis: (scope == AnalysisScope::Device)
            .then(|| state.latest_analysis.clone())
            .flatten(),
        state,
    })
}

pub(crate) fn ensure_auth_ready(
    auth: &AuthService,
    client_id: &str,
    model_slug: &str,
) -> Result<(), String> {
    validate_auth_state(&auth.public_state()?, client_id, model_slug)
}

pub(crate) fn validate_auth_state(
    state: &ChatGptAuthState,
    client_id: &str,
    model_slug: &str,
) -> Result<(), String> {
    if !state.connected || !state.plan_usage_authorized {
        return Err("AUTH_PLAN_USAGE_SCOPE_REQUIRED".to_owned());
    }
    if state.active_client_id.as_deref() != Some(client_id) {
        return Err("AUTH_ACTIVE_ACCOUNT_CHANGED".to_owned());
    }
    if model_slug.trim().is_empty() || model_slug.len() > 160 {
        return Err("AUTH_MODEL_NOT_SELECTED".to_owned());
    }
    Ok(())
}

pub(crate) fn load_current_evidence(
    database: &Database,
    telemetry: &TelemetryManager,
    scope: AnalysisScope,
    target_device_id: Option<&str>,
) -> Result<(EvidenceSnapshot, String), String> {
    let inventory = database
        .load_inventory()?
        .ok_or_else(|| "AI_ANALYSIS_INVENTORY_NOT_AVAILABLE".to_owned())?;
    let frame = telemetry.frame(None)?;
    let captured_at = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let mut capabilities = Vec::<CapabilityRecord>::new();
    for device in inventory.devices.iter().filter(|device| {
        device.is_present == Some(true)
            && device.layer != DeviceLayer::PnpNode
            && device.is_virtual != Some(true)
            && matches!(
                device.kind,
                DeviceKind::Cpu
                    | DeviceKind::GpuAdapter
                    | DeviceKind::MemoryModule
                    | DeviceKind::PhysicalDisk
                    | DeviceKind::NetworkAdapter
                    | DeviceKind::Battery
            )
            && target_device_id.is_none_or(|target| device.id == target)
    }) {
        if let Some(profile) = database.load_device_profile(&device.id)? {
            capabilities.extend(profile.capabilities);
        }
    }
    let snapshot = build_evidence_snapshot_with_capabilities(
        &inventory,
        &frame,
        &capabilities,
        scope,
        target_device_id,
        captured_at,
    )?;
    let fingerprint = input_fingerprint(&snapshot, target_device_id)?;
    Ok((snapshot, fingerprint))
}

fn validate_scope_target(
    scope: AnalysisScope,
    target_device_id: Option<&str>,
) -> Result<(), String> {
    if (scope == AnalysisScope::Device) != target_device_id.is_some() {
        return Err("AI_ANALYSIS_TARGET_SCOPE_INVALID".to_owned());
    }
    Ok(())
}
