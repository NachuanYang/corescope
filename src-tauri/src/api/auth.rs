use crate::auth::{
    models::AvailableModel,
    oauth::NetworkRoute,
    service::{AuthService, ChatGptAuthState, DisconnectResult, InferenceVerification},
};
use tauri::{AppHandle, State};

fn log_auth_error(operation: &'static str, error: &str) {
    if matches!(error, "AUTH_SIGN_IN_CANCELLED" | "AUTH_USER_DENIED") {
        tracing::info!(operation, error_code = %error, "ChatGPT operation ended without connecting");
    } else {
        tracing::warn!(operation, error_code = %error, "ChatGPT operation did not complete");
    }
}

#[tauri::command(rename_all = "camelCase")]
pub fn get_chatgpt_auth_state(auth: State<'_, AuthService>) -> Result<ChatGptAuthState, String> {
    auth.public_state()
        .inspect_err(|error| log_auth_error("state", error))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn connect_chatgpt(
    app: AppHandle,
    auth: State<'_, AuthService>,
    client_id: Option<String>,
    route: NetworkRoute,
) -> Result<ChatGptAuthState, String> {
    auth.connect(&app, client_id, route)
        .await
        .inspect_err(|error| log_auth_error("connect", error))
}

#[tauri::command(rename_all = "camelCase")]
pub fn cancel_chatgpt_sign_in(auth: State<'_, AuthService>) -> bool {
    auth.cancel_sign_in()
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_chatgpt_models(
    auth: State<'_, AuthService>,
    client_id: Option<String>,
    route: NetworkRoute,
    force_refresh: Option<bool>,
) -> Result<Vec<AvailableModel>, String> {
    auth.list_models(client_id.as_deref(), route, force_refresh.unwrap_or(false))
        .await
        .inspect_err(|error| log_auth_error("models", error))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn verify_chatgpt_inference(
    auth: State<'_, AuthService>,
    client_id: Option<String>,
    model_slug: String,
    route: NetworkRoute,
) -> Result<InferenceVerification, String> {
    auth.verify_inference(client_id.as_deref(), &model_slug, route)
        .await
        .inspect_err(|error| log_auth_error("responses", error))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn disconnect_chatgpt(
    auth: State<'_, AuthService>,
    client_id: Option<String>,
    route: NetworkRoute,
) -> Result<DisconnectResult, String> {
    auth.disconnect(client_id.as_deref(), route)
        .await
        .inspect_err(|error| log_auth_error("disconnect", error))
}
