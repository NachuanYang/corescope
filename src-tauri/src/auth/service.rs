use super::{
    credential_vault::{CredentialProfile, CredentialVault, TokenSet},
    http::{
        AuthHttpClients, get_response, max_json_response_bytes, post_form_response,
        post_json_response, post_streaming_json_response,
    },
    models::{AvailableModel, filter_listed_models},
    oauth::{
        AuthorizationResult, NetworkRoute, VerifiedIdentity, parse_callback_target, pkce_challenge,
        validate_id_token,
    },
    responses::{build_responses_request, consume_response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use jsonwebtoken::jwk::JwkSet;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use tauri::AppHandle;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
    time::timeout,
};
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

const AUTH_CONFIG_ENDPOINT: &str = "https://auth.openai.com/.well-known/openid-configuration";
const OPENAI_API_BASE: &str = "https://api.openai.com/v1";
const RESOURCE: &str = "https://api.openai.com/v1";
const AUTHORIZATION_SCOPE: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DYNAMIC_CLIENT_ID: &str = "dynamic_agent_client";
const AGENT_NAME: &str = "机枢";
const AUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(12);
const CALLBACK_MAX_REQUEST_BYTES: usize = 16 * 1024;
const TOKEN_RESPONSE_MAX_BYTES: usize = 128 * 1024;
const DISCOVERY_RESPONSE_MAX_BYTES: usize = 64 * 1024;
const JWKS_RESPONSE_MAX_BYTES: usize = 256 * 1024;
const MODELS_CACHE_LIFETIME: Duration = Duration::from_secs(10 * 60);
const AI_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAccount {
    pub client_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub connected: bool,
    pub plan_usage_authorized: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptAuthState {
    pub accounts: Vec<PublicAccount>,
    pub active_client_id: Option<String>,
    pub connected: bool,
    pub plan_usage_authorized: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InferenceVerification {
    pub model_slug: String,
    pub response_text: String,
    pub completed_at_utc: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisconnectResult {
    pub state: ChatGptAuthState,
    pub remote_revocation_confirmed: bool,
}

#[derive(Clone)]
struct CachedModels {
    loaded_at: std::time::Instant,
    models: Vec<AvailableModel>,
}

pub struct AuthService {
    vault: CredentialVault,
    clients: AuthHttpClients,
    pending_cancel: Mutex<Option<watch::Sender<bool>>>,
    model_cache: Mutex<HashMap<String, CachedModels>>,
    process_refresh_locks: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
}

impl AuthService {
    pub fn open(auth_directory: PathBuf) -> Result<Self, String> {
        Ok(Self {
            vault: CredentialVault::open(auth_directory)?,
            clients: AuthHttpClients::new()?,
            pending_cancel: Mutex::new(None),
            model_cache: Mutex::new(HashMap::new()),
            process_refresh_locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn public_state(&self) -> Result<ChatGptAuthState, String> {
        let profiles = self.vault.account_profiles()?;
        let stored_active_client_id = self.vault.active_account()?;
        let active = select_single_profile(&profiles, stored_active_client_id.as_deref());
        let active_client_id = active.map(|profile| profile.client_id.clone());
        if active_client_id.as_deref() != stored_active_client_id.as_deref() {
            self.vault.set_active_account(active_client_id.as_deref())?;
        }
        let accounts = active.into_iter().map(public_account).collect();
        let connected = active.and_then(|profile| profile.tokens.as_ref()).is_some();
        let plan_usage_authorized = active
            .and_then(|profile| profile.tokens.as_ref())
            .is_some_and(TokenSet::permits_direct_plan_usage);
        Ok(ChatGptAuthState {
            accounts,
            active_client_id,
            connected,
            plan_usage_authorized,
        })
    }

    pub fn cancel_sign_in(&self) -> bool {
        self.pending_cancel
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|sender| sender.send(true).is_ok()))
            .unwrap_or(false)
    }

    fn pending_lock(&self) -> Result<MutexGuard<'_, Option<watch::Sender<bool>>>, String> {
        self.pending_cancel
            .lock()
            .map_err(|_| "AUTH_STATE_LOCK_POISONED".to_owned())
    }

    pub async fn connect(
        &self,
        app: &AppHandle,
        registered_client_id: Option<String>,
        route: NetworkRoute,
    ) -> Result<ChatGptAuthState, String> {
        let profiles = self.vault.account_profiles()?;
        let active_client_id = self.vault.active_account()?;
        let registered_client_id = select_single_profile(&profiles, active_client_id.as_deref())
            .map(|profile| profile.client_id.clone())
            .or(registered_client_id);
        let (cancel_sender, mut cancel_receiver) = watch::channel(false);
        {
            let mut pending = self.pending_lock()?;
            if pending.is_some() {
                return Err("AUTH_SIGN_IN_ALREADY_RUNNING".to_owned());
            }
            *pending = Some(cancel_sender);
        }
        let result = self
            .connect_attempt(app, registered_client_id, route, &mut cancel_receiver)
            .await;
        if let Ok(mut pending) = self.pending_lock() {
            *pending = None;
        }
        result?;
        self.public_state()
    }

    async fn connect_attempt(
        &self,
        app: &AppHandle,
        registered_client_id: Option<String>,
        route: NetworkRoute,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), String> {
        let client = self.clients.client(route);
        let configuration = tokio::select! {
            _ = cancel.changed() => return Err("AUTH_SIGN_IN_CANCELLED".to_owned()),
            configuration = self.discover(client) => configuration?,
        };
        let host_id = self.vault.load_or_create_host_id()?;
        let previous_profile = match registered_client_id.as_deref() {
            Some(client_id) => Some(
                self.vault
                    .account_profile(client_id)?
                    .filter(|profile| profile.client_id == client_id)
                    .ok_or_else(|| "AUTH_ACCOUNT_NOT_FOUND".to_owned())?,
            ),
            None => None,
        };
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|_| "AUTH_CALLBACK_LISTENER_UNAVAILABLE".to_owned())?;
        let address = listener
            .local_addr()
            .map_err(|_| "AUTH_CALLBACK_LISTENER_UNAVAILABLE".to_owned())?;
        let redirect_uri = format!("http://127.0.0.1:{}/auth/callback", address.port());
        let state = random_url_value();
        let nonce = random_url_value();
        let verifier = random_url_value();
        let challenge = pkce_challenge(&verifier)?;
        let authorize_url = build_authorization_url(
            &configuration.authorization_endpoint,
            registered_client_id.as_deref(),
            previous_profile.as_ref(),
            &host_id,
            &redirect_uri,
            &state,
            &nonce,
            &challenge,
        )?;
        self.finish_connect(
            app,
            listener,
            configuration,
            route,
            previous_profile,
            host_id,
            redirect_uri,
            state,
            nonce,
            verifier,
            authorize_url,
            cancel,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_connect(
        &self,
        app: &AppHandle,
        listener: TcpListener,
        configuration: OpenIdConfiguration,
        route: NetworkRoute,
        previous_profile: Option<CredentialProfile>,
        host_id: String,
        redirect_uri: String,
        expected_state: Zeroizing<String>,
        expected_nonce: Zeroizing<String>,
        verifier: Zeroizing<String>,
        authorize_url: Zeroizing<String>,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), String> {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(authorize_url.as_str().to_owned(), None::<&str>)
            .map_err(|_| "AUTH_BROWSER_OPEN_FAILED".to_owned())?;

        let (mut stream, peer) = tokio::select! {
            _ = cancel.changed() => return Err("AUTH_SIGN_IN_CANCELLED".to_owned()),
            accepted = timeout(AUTH_CALLBACK_TIMEOUT, listener.accept()) => {
                accepted.map_err(|_| "AUTH_CALLBACK_TIMEOUT".to_owned())?
                    .map_err(|_| "AUTH_CALLBACK_LISTENER_FAILED".to_owned())?
            }
        };
        if !peer.ip().is_loopback() {
            let _ = write_callback_response(&mut stream, false).await;
            return Err("AUTH_CALLBACK_PEER_INVALID".to_owned());
        }
        let request_target = tokio::select! {
            _ = cancel.changed() => return Err("AUTH_SIGN_IN_CANCELLED".to_owned()),
            request = timeout(CALLBACK_READ_TIMEOUT, read_callback_target(&mut stream)) => {
                request.map_err(|_| "AUTH_CALLBACK_READ_TIMEOUT".to_owned())??
            }
        };
        let authorization = parse_callback_target(
            &request_target,
            &expected_state,
            previous_profile
                .as_ref()
                .map(|profile| profile.client_id.as_str()),
        );
        write_callback_response(
            &mut stream,
            authorization
                .as_ref()
                .is_ok_and(|result| matches!(result, AuthorizationResult::Code { .. })),
        )
        .await?;
        let (code, client_id) = match authorization? {
            AuthorizationResult::Denied => return Err("AUTH_USER_DENIED".to_owned()),
            AuthorizationResult::Code { code, client_id } => (code, client_id),
        };
        let identity = self
            .exchange_code(
                self.clients.client(route),
                &configuration,
                &client_id,
                &code,
                &verifier,
                &redirect_uri,
                &expected_nonce,
            )
            .await?;
        if previous_profile
            .as_ref()
            .is_some_and(|profile| profile.subject != identity.subject)
        {
            return Err("AUTH_ACCOUNT_IDENTITY_MISMATCH".to_owned());
        }

        // exchange_code stores only after validating the ID token; account activation follows here.
        let profile = self
            .vault
            .account_profile(&client_id)?
            .ok_or_else(|| "AUTH_ACCOUNT_SAVE_FAILED".to_owned())?;
        if profile.host_id != host_id || profile.subject != identity.subject {
            return Err("AUTH_ACCOUNT_IDENTITY_MISMATCH".to_owned());
        }
        self.vault.set_active_account(Some(&client_id))?;
        Ok(())
    }

    async fn discover(&self, client: &reqwest::Client) -> Result<OpenIdConfiguration, String> {
        let response = get_response(
            client,
            AUTH_CONFIG_ENDPOINT,
            None,
            DISCOVERY_RESPONSE_MAX_BYTES,
        )
        .await?;
        if !response.status.is_success() {
            return Err(http_status_error("AUTH_DISCOVERY", response.status));
        }
        let configuration: OpenIdConfiguration = serde_json::from_slice(&response.body)
            .map_err(|_| "AUTH_DISCOVERY_RESPONSE_INVALID".to_owned())?;
        if configuration.issuer != "https://auth.openai.com"
            || !is_openai_auth_endpoint(&configuration.authorization_endpoint)
            || !is_openai_auth_endpoint(&configuration.token_endpoint)
            || !is_openai_auth_endpoint(&configuration.jwks_uri)
            || configuration
                .revocation_endpoint
                .as_deref()
                .is_some_and(|endpoint| !is_openai_auth_endpoint(endpoint))
        {
            return Err("AUTH_DISCOVERY_ENDPOINT_INVALID".to_owned());
        }
        Ok(configuration)
    }

    async fn fetch_jwks(
        &self,
        client: &reqwest::Client,
        configuration: &OpenIdConfiguration,
    ) -> Result<JwkSet, String> {
        let response = get_response(
            client,
            &configuration.jwks_uri,
            None,
            JWKS_RESPONSE_MAX_BYTES,
        )
        .await?;
        if !response.status.is_success() {
            return Err(http_status_error("AUTH_JWKS", response.status));
        }
        let keys: JwkSet = serde_json::from_slice(&response.body)
            .map_err(|_| "AUTH_JWKS_RESPONSE_INVALID".to_owned())?;
        if keys.keys.is_empty() || keys.keys.len() > 64 {
            return Err("AUTH_JWKS_RESPONSE_INVALID".to_owned());
        }
        Ok(keys)
    }

    async fn exchange_code(
        &self,
        client: &reqwest::Client,
        configuration: &OpenIdConfiguration,
        client_id: &str,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
        nonce: &str,
    ) -> Result<VerifiedIdentity, String> {
        let response = post_form_response(
            client,
            &configuration.token_endpoint,
            &authorization_code_grant_fields(client_id, code, verifier, redirect_uri),
            TOKEN_RESPONSE_MAX_BYTES,
        )
        .await?;
        if !response.status.is_success() {
            return Err(http_status_error("AUTH_TOKEN_EXCHANGE", response.status));
        }
        let token_response: TokenEndpointResponse = serde_json::from_slice(&response.body)
            .map_err(|_| "AUTH_TOKEN_RESPONSE_INVALID".to_owned())?;
        validate_token_response(&token_response)?;
        let id_token = token_response
            .id_token
            .as_deref()
            .ok_or_else(|| "AUTH_ID_TOKEN_MISSING".to_owned())?;
        let jwks = self.fetch_jwks(client, configuration).await?;
        let identity =
            validate_id_token(id_token, client_id, nonce, &jwks, Utc::now().timestamp())?;
        let scopes = parse_scope_set(token_response.scope.as_deref())?;
        let expires_at = Utc::now()
            .timestamp()
            .checked_add(
                token_response
                    .expires_in
                    .ok_or_else(|| "AUTH_TOKEN_EXPIRY_INVALID".to_owned())?,
            )
            .ok_or_else(|| "AUTH_TOKEN_EXPIRY_INVALID".to_owned())?;
        let host_id = self.vault.load_or_create_host_id()?;
        let profile = CredentialProfile {
            client_id: client_id.to_owned(),
            subject: identity.subject.clone(),
            email: identity.email.clone(),
            display_name: identity.display_name.clone(),
            host_id,
            tokens: Some(TokenSet {
                id_token: id_token.to_owned(),
                access_token: token_response.access_token.to_string(),
                refresh_token: token_response
                    .refresh_token
                    .as_deref()
                    .ok_or_else(|| "AUTH_REFRESH_TOKEN_MISSING".to_owned())?
                    .to_owned(),
                token_type: token_response
                    .token_type
                    .as_deref()
                    .ok_or_else(|| "AUTH_TOKEN_TYPE_INVALID".to_owned())?
                    .to_string(),
                scopes,
                expires_at_unix: expires_at,
            }),
        };
        self.vault.save_account(profile)?;
        Ok(identity)
    }

    pub async fn list_models(
        &self,
        client_id: Option<&str>,
        route: NetworkRoute,
        force_refresh: bool,
    ) -> Result<Vec<AvailableModel>, String> {
        let profile = self.resolve_profile(client_id)?;
        let tokens = self.ensure_fresh_tokens(&profile.client_id, route).await?;
        if !tokens.permits_direct_plan_usage() {
            return Err("AUTH_PLAN_USAGE_SCOPE_REQUIRED".to_owned());
        }
        if !force_refresh && let Ok(cache) = self.model_cache.lock() {
            if let Some(cached) = cache.get(&profile.client_id) {
                if cached.loaded_at.elapsed() < MODELS_CACHE_LIFETIME {
                    return Ok(cached.models.clone());
                }
            }
        }
        let response = get_response(
            self.clients.client(route),
            &format!("{OPENAI_API_BASE}/models"),
            Some(&tokens.access_token),
            max_json_response_bytes(),
        )
        .await?;
        if !response.status.is_success() {
            return Err(http_status_error("AUTH_MODELS", response.status));
        }
        let catalog: Value = serde_json::from_slice(&response.body)
            .map_err(|_| "AUTH_MODEL_CATALOG_INVALID".to_owned())?;
        let models = filter_listed_models(&catalog)?;
        self.model_cache
            .lock()
            .map_err(|_| "AUTH_STATE_LOCK_POISONED".to_owned())?
            .insert(
                profile.client_id,
                CachedModels {
                    loaded_at: std::time::Instant::now(),
                    models: models.clone(),
                },
            );
        Ok(models)
    }

    pub async fn verify_inference(
        &self,
        client_id: Option<&str>,
        model_slug: &str,
        route: NetworkRoute,
    ) -> Result<InferenceVerification, String> {
        let profile = self.resolve_profile(client_id)?;
        let allowed_models = self
            .list_models(Some(&profile.client_id), route, false)
            .await?;
        if !allowed_models.iter().any(|model| model.slug == model_slug) {
            return Err("AUTH_MODEL_NOT_AVAILABLE".to_owned());
        }
        let tokens = self.ensure_fresh_tokens(&profile.client_id, route).await?;
        let body = build_responses_request(
            model_slug,
            "Say exactly: CoreScope ChatGPT connection verified.",
        )?;
        let response = post_json_response(
            self.clients.client(route),
            &format!("{OPENAI_API_BASE}/responses"),
            &tokens.access_token,
            &body,
        )
        .await?;
        let response_text = consume_response(response).await?;
        Ok(InferenceVerification {
            model_slug: model_slug.to_owned(),
            response_text,
            completed_at_utc: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        })
    }

    pub async fn complete_text_response(
        &self,
        client_id: Option<&str>,
        model_slug: &str,
        route: NetworkRoute,
        input: &str,
    ) -> Result<InferenceVerification, String> {
        if input.trim().is_empty() || input.len() > 128 * 1024 {
            return Err("AUTH_RESPONSES_REQUEST_INVALID".to_owned());
        }
        let profile = self.resolve_profile(client_id)?;
        let allowed_models = self
            .list_models(Some(&profile.client_id), route, false)
            .await?;
        if !allowed_models.iter().any(|model| model.slug == model_slug) {
            return Err("AUTH_MODEL_NOT_AVAILABLE".to_owned());
        }
        let tokens = self.ensure_fresh_tokens(&profile.client_id, route).await?;
        if !tokens.permits_direct_plan_usage() {
            return Err("AUTH_PLAN_USAGE_SCOPE_REQUIRED".to_owned());
        }
        let body = build_responses_request(model_slug, input)?;
        let response = post_streaming_json_response(
            self.clients.client(route),
            &format!("{OPENAI_API_BASE}/responses"),
            &tokens.access_token,
            &body,
            AI_RESPONSE_TIMEOUT,
        )
        .await?;
        let response_text = consume_response(response).await?;
        Ok(InferenceVerification {
            model_slug: model_slug.to_owned(),
            response_text,
            completed_at_utc: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        })
    }

    pub async fn disconnect(
        &self,
        client_id: Option<&str>,
        route: NetworkRoute,
    ) -> Result<DisconnectResult, String> {
        let profile = self.resolve_profile(client_id)?;
        let remote_revocation_confirmed = match profile.tokens.as_ref() {
            Some(tokens) => self.revoke_session(&profile.client_id, tokens, route).await,
            None => true,
        };
        self.vault.clear_account_tokens(&profile.client_id)?;
        self.model_cache
            .lock()
            .map_err(|_| "AUTH_STATE_LOCK_POISONED".to_owned())?
            .remove(&profile.client_id);
        Ok(DisconnectResult {
            state: self.public_state()?,
            remote_revocation_confirmed,
        })
    }

    fn resolve_profile(&self, client_id: Option<&str>) -> Result<CredentialProfile, String> {
        let selected_client_id = match client_id {
            Some(client_id) => client_id.to_owned(),
            None => self
                .vault
                .active_account()?
                .ok_or_else(|| "AUTH_ACCOUNT_NOT_CONNECTED".to_owned())?,
        };
        let profile = self
            .vault
            .account_profile(&selected_client_id)?
            .ok_or_else(|| "AUTH_ACCOUNT_NOT_FOUND".to_owned())?;
        if profile.tokens.is_none() {
            return Err("AUTH_ACCOUNT_NOT_CONNECTED".to_owned());
        }
        Ok(profile)
    }

    async fn ensure_fresh_tokens(
        &self,
        client_id: &str,
        route: NetworkRoute,
    ) -> Result<TokenSet, String> {
        let process_lock = self.process_refresh_lock(client_id)?;
        let _process_guard = process_lock.lock().await;
        let lock_vault = self.vault.clone();
        let lock_client_id = client_id.to_owned();
        let refresh_lock =
            tokio::task::spawn_blocking(move || lock_vault.acquire_refresh_lock(&lock_client_id))
                .await
                .map_err(|_| "AUTH_REFRESH_LOCK_FAILED".to_owned())??;
        let mut profile = self
            .vault
            .account_profile(client_id)?
            .ok_or_else(|| "AUTH_ACCOUNT_NOT_FOUND".to_owned())?;
        let current = profile
            .tokens
            .take()
            .ok_or_else(|| "AUTH_ACCOUNT_NOT_CONNECTED".to_owned())?;
        if current.expires_at_unix > Utc::now().timestamp().saturating_add(90) {
            drop(refresh_lock);
            return Ok(current);
        }
        if !current.permits_direct_plan_usage() {
            return Err("AUTH_PLAN_USAGE_SCOPE_REQUIRED".to_owned());
        }
        let configuration = self.discover(self.clients.client(route)).await?;
        let response = post_form_response(
            self.clients.client(route),
            &configuration.token_endpoint,
            &refresh_token_grant_fields(client_id, &current.refresh_token),
            TOKEN_RESPONSE_MAX_BYTES,
        )
        .await?;
        if !response.status.is_success() {
            return Err(http_status_error("AUTH_TOKEN_REFRESH", response.status));
        }
        let token_response: TokenEndpointResponse = serde_json::from_slice(&response.body)
            .map_err(|_| "AUTH_TOKEN_RESPONSE_INVALID".to_owned())?;
        validate_token_response(&token_response)?;
        let scopes = match token_response.scope.as_deref() {
            Some(scope) => parse_scope_set(Some(scope))?,
            None => current.scopes.clone(),
        };
        let expires_at = Utc::now()
            .timestamp()
            .checked_add(
                token_response
                    .expires_in
                    .ok_or_else(|| "AUTH_TOKEN_EXPIRY_INVALID".to_owned())?,
            )
            .ok_or_else(|| "AUTH_TOKEN_EXPIRY_INVALID".to_owned())?;
        let refreshed = TokenSet {
            id_token: current.id_token.clone(),
            access_token: token_response.access_token.to_string(),
            refresh_token: token_response
                .refresh_token
                .as_deref()
                .unwrap_or(&current.refresh_token)
                .to_owned(),
            token_type: token_response
                .token_type
                .as_deref()
                .unwrap_or(&current.token_type)
                .to_owned(),
            scopes,
            expires_at_unix: expires_at,
        };
        profile.tokens = Some(refreshed.clone());
        self.vault.save_account(profile)?;
        drop(refresh_lock);
        if !refreshed.permits_direct_plan_usage() {
            return Err("AUTH_PLAN_USAGE_SCOPE_REQUIRED".to_owned());
        }
        Ok(refreshed)
    }

    fn process_refresh_lock(
        &self,
        client_id: &str,
    ) -> Result<std::sync::Arc<tokio::sync::Mutex<()>>, String> {
        if !client_id.starts_with("oaiapp_") || client_id.len() > 512 {
            return Err("AUTH_CLIENT_ID_INVALID".to_owned());
        }
        let mut locks = self
            .process_refresh_locks
            .lock()
            .map_err(|_| "AUTH_STATE_LOCK_POISONED".to_owned())?;
        Ok(locks
            .entry(client_id.to_owned())
            .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    async fn revoke_session(
        &self,
        client_id: &str,
        tokens: &TokenSet,
        route: NetworkRoute,
    ) -> bool {
        let client = self.clients.client(route);
        let Ok(configuration) = self.discover(client).await else {
            tracing::warn!(
                error_code = "AUTH_REVOCATION_DISCOVERY_FAILED",
                "Could not load ChatGPT revocation metadata"
            );
            return false;
        };
        let Some(endpoint) = configuration.revocation_endpoint.as_deref() else {
            return false;
        };
        for attempt in 0..3 {
            match post_form_response(
                client,
                endpoint,
                &[
                    ("token", &tokens.refresh_token),
                    ("token_type_hint", "refresh_token"),
                    ("client_id", client_id),
                ],
                16 * 1024,
            )
            .await
            {
                Ok(response) if response.status == StatusCode::OK => return true,
                Ok(response) if !response.status.is_server_error() => return false,
                _ if attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(250 * (attempt + 1))).await
                }
                _ => return false,
            }
        }
        false
    }
}

#[derive(Deserialize)]
struct OpenIdConfiguration {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    revocation_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct TokenEndpointResponse {
    access_token: Zeroizing<String>,
    refresh_token: Option<Zeroizing<String>>,
    id_token: Option<Zeroizing<String>>,
    token_type: Option<Zeroizing<String>>,
    scope: Option<String>,
    expires_in: Option<i64>,
}

fn public_account(profile: &CredentialProfile) -> PublicAccount {
    let tokens = profile.tokens.as_ref();
    PublicAccount {
        client_id: profile.client_id.clone(),
        email: profile.email.clone(),
        display_name: profile.display_name.clone(),
        connected: tokens.is_some(),
        plan_usage_authorized: tokens.is_some_and(TokenSet::permits_direct_plan_usage),
    }
}

fn select_single_profile<'a>(
    profiles: &'a [CredentialProfile],
    active_client_id: Option<&str>,
) -> Option<&'a CredentialProfile> {
    let active = active_client_id.and_then(|client_id| {
        profiles
            .iter()
            .find(|profile| profile.client_id == client_id)
    });
    if let Some(active) = active {
        if active
            .tokens
            .as_ref()
            .is_some_and(TokenSet::permits_direct_plan_usage)
        {
            return Some(active);
        }
        return profiles
            .iter()
            .find(|profile| {
                profile.subject == active.subject
                    && profile
                        .tokens
                        .as_ref()
                        .is_some_and(TokenSet::permits_direct_plan_usage)
            })
            .or(Some(active));
    }
    profiles
        .iter()
        .find(|profile| {
            profile
                .tokens
                .as_ref()
                .is_some_and(TokenSet::permits_direct_plan_usage)
        })
        .or_else(|| profiles.first())
}

fn random_url_value() -> Zeroizing<String> {
    let mut random = [0u8; 32];
    random[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    random[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    Zeroizing::new(URL_SAFE_NO_PAD.encode(random))
}

#[allow(clippy::too_many_arguments)]
fn build_authorization_url(
    authorization_endpoint: &str,
    registered_client_id: Option<&str>,
    previous_profile: Option<&CredentialProfile>,
    host_id: &str,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
    challenge: &str,
) -> Result<Zeroizing<String>, String> {
    if !is_openai_auth_endpoint(authorization_endpoint)
        || !redirect_uri.starts_with("http://127.0.0.1:")
        || !redirect_uri.ends_with("/auth/callback")
        || !host_id.starts_with("urn:uuid:")
    {
        return Err("AUTH_AUTHORIZATION_REQUEST_INVALID".to_owned());
    }
    let mut url = Url::parse(authorization_endpoint)
        .map_err(|_| "AUTH_AUTHORIZATION_ENDPOINT_INVALID".to_owned())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair(
                "client_id",
                registered_client_id.unwrap_or(DYNAMIC_CLIENT_ID),
            )
            .append_pair("ext_agent_host_id", host_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", AUTHORIZATION_SCOPE)
            .append_pair("resource", RESOURCE)
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", challenge);
        if registered_client_id.is_none() {
            query.append_pair("agent_name_hint", AGENT_NAME);
        }
        if let Some(profile) = previous_profile {
            if let Some(tokens) = &profile.tokens {
                query.append_pair("id_token_hint", &tokens.id_token);
            }
            if let Some(email) = &profile.email {
                query.append_pair("login_hint", email);
            }
        }
    }
    Ok(Zeroizing::new(url.into()))
}

fn is_openai_auth_endpoint(endpoint: &str) -> bool {
    Url::parse(endpoint).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("auth.openai.com")
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none()
    })
}

fn parse_scope_set(scope: Option<&str>) -> Result<Vec<String>, String> {
    let mut scopes = Vec::new();
    for value in scope.unwrap_or_default().split_ascii_whitespace() {
        if value.len() > 256 || !value.is_ascii() {
            return Err("AUTH_TOKEN_SCOPE_INVALID".to_owned());
        }
        if !scopes.iter().any(|existing| existing == value) {
            scopes.push(value.to_owned());
        }
        if scopes.len() > 32 {
            return Err("AUTH_TOKEN_SCOPE_INVALID".to_owned());
        }
    }
    Ok(scopes)
}

fn authorization_code_grant_fields<'a>(
    client_id: &'a str,
    code: &'a str,
    verifier: &'a str,
    redirect_uri: &'a str,
) -> [(&'static str, &'a str); 6] {
    [
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
        ("resource", RESOURCE),
    ]
}

fn refresh_token_grant_fields<'a>(
    client_id: &'a str,
    refresh_token: &'a str,
) -> [(&'static str, &'a str); 4] {
    [
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
        ("resource", RESOURCE),
    ]
}

fn validate_token_response(response: &TokenEndpointResponse) -> Result<(), String> {
    if response.access_token.is_empty()
        || response.access_token.len() > 64 * 1024
        || response
            .refresh_token
            .as_deref()
            .is_some_and(|token| token.is_empty() || token.len() > 64 * 1024)
        || response
            .id_token
            .as_deref()
            .is_some_and(|token| token.is_empty() || token.len() > 64 * 1024)
        || response
            .token_type
            .as_deref()
            .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("bearer"))
        || response
            .expires_in
            .is_some_and(|expires| !(1..=31_536_000).contains(&expires))
        || response
            .scope
            .as_ref()
            .is_some_and(|scope| scope.len() > 8192)
    {
        return Err("AUTH_TOKEN_RESPONSE_INVALID".to_owned());
    }
    Ok(())
}

fn http_status_error(prefix: &str, status: StatusCode) -> String {
    format!("{prefix}_HTTP_{}", status.as_u16())
}

async fn read_callback_target(stream: &mut TcpStream) -> Result<Zeroizing<String>, String> {
    let mut request = Zeroizing::new(Vec::<u8>::with_capacity(2048));
    let mut buffer = [0u8; 2048];
    loop {
        let count = stream
            .read(&mut buffer)
            .await
            .map_err(|_| "AUTH_CALLBACK_READ_FAILED".to_owned())?;
        if count == 0 {
            return Err("AUTH_CALLBACK_REQUEST_INCOMPLETE".to_owned());
        }
        if request.len().saturating_add(count) > CALLBACK_MAX_REQUEST_BYTES {
            return Err("AUTH_CALLBACK_REQUEST_TOO_LARGE".to_owned());
        }
        request.extend_from_slice(&buffer[..count]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = std::str::from_utf8(&request[..header_end])
            .map_err(|_| "AUTH_CALLBACK_REQUEST_INVALID".to_owned())?;
        let mut lines = headers.split("\r\n");
        let request_line = lines
            .next()
            .ok_or_else(|| "AUTH_CALLBACK_REQUEST_INVALID".to_owned())?;
        let mut parts = request_line.split_ascii_whitespace();
        if parts.next() != Some("GET") {
            return Err("AUTH_CALLBACK_METHOD_INVALID".to_owned());
        }
        let target = parts
            .next()
            .ok_or_else(|| "AUTH_CALLBACK_REQUEST_INVALID".to_owned())?;
        let version = parts.next();
        if !matches!(version, Some("HTTP/1.1" | "HTTP/1.0")) || parts.next().is_some() {
            return Err("AUTH_CALLBACK_REQUEST_INVALID".to_owned());
        }
        for header in lines {
            if let Some((name, value)) = header.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") && value.trim() != "0" {
                    return Err("AUTH_CALLBACK_BODY_UNEXPECTED".to_owned());
                }
                if name.eq_ignore_ascii_case("transfer-encoding") {
                    return Err("AUTH_CALLBACK_BODY_UNEXPECTED".to_owned());
                }
            }
        }
        return Ok(Zeroizing::new(target.to_owned()));
    }
}

async fn write_callback_response(stream: &mut TcpStream, success: bool) -> Result<(), String> {
    let (status, message) = if success {
        ("200 OK", "授权已返回机枢，你可以关闭此页面并回到应用。")
    } else {
        (
            "400 Bad Request",
            "授权回调未通过校验，请回到机枢重新发起连接。",
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{message}",
        message.len(),
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|_| "AUTH_CALLBACK_RESPONSE_FAILED".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn ai_analysis_stream_has_a_five_minute_response_timeout() {
        assert_eq!(AI_RESPONSE_TIMEOUT, Duration::from_secs(5 * 60));
    }

    fn fixture_profile(client_id: &str, authorized: bool) -> CredentialProfile {
        CredentialProfile {
            client_id: client_id.to_owned(),
            subject: format!("subject-{client_id}"),
            email: None,
            display_name: Some("Fixture account".to_owned()),
            host_id: "urn:uuid:00000000-0000-4000-8000-000000000001".to_owned(),
            tokens: authorized.then(|| TokenSet {
                id_token: "id".to_owned(),
                access_token: "access".to_owned(),
                refresh_token: "refresh".to_owned(),
                token_type: "Bearer".to_owned(),
                scopes: vec!["chatgpt.tokens.use.direct".to_owned()],
                expires_at_unix: 2_000_000_000,
            }),
        }
    }

    #[test]
    fn legacy_account_vault_is_presented_as_one_active_account() {
        let profiles = [fixture_profile("oaiapp_legacy", true), {
            let mut active = fixture_profile("oaiapp_active", false);
            active.subject = "subject-oaiapp_legacy".to_owned();
            active
        }];

        let active = select_single_profile(&profiles, Some("oaiapp_active")).unwrap();
        assert_eq!(active.client_id, "oaiapp_legacy");
        assert_eq!(
            select_single_profile(&profiles, None).unwrap().client_id,
            "oaiapp_legacy"
        );
    }

    async fn callback_response(success: bool) -> String {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            write_callback_response(&mut stream, success).await.unwrap();
        });
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        server.await.unwrap();
        String::from_utf8(response).unwrap()
    }

    #[tokio::test]
    async fn callback_result_page_uses_localized_brand_and_plain_text() {
        let success = callback_response(true).await;
        assert!(success.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(success.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(success.contains("授权已返回机枢，你可以关闭此页面并回到应用。"));
        assert!(!success.contains("<!doctype html>"));

        let failure = callback_response(false).await;
        assert!(failure.starts_with("HTTP/1.1 400 Bad Request\r\n"));
        assert!(failure.contains("授权回调未通过校验，请回到机枢重新发起连接。"));
    }

    fn query_pairs(url: &str) -> HashMap<String, String> {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    }

    #[test]
    fn dynamic_registration_and_saved_account_reauthorization_use_official_distinct_parameters() {
        let host_id = "urn:uuid:00000000-0000-4000-8000-000000000001";
        let redirect = "http://127.0.0.1:1455/auth/callback";
        let first = build_authorization_url(
            "https://auth.openai.com/api/accounts/authorize",
            None,
            None,
            host_id,
            redirect,
            "state-fixture",
            "nonce-fixture",
            "challenge-fixture",
        )
        .unwrap();
        let first_query = query_pairs(&first);
        assert_eq!(first_query["client_id"], DYNAMIC_CLIENT_ID);
        assert_eq!(first_query["agent_name_hint"], "机枢");
        assert_eq!(first_query["ext_agent_host_id"], host_id);
        assert_eq!(first_query["redirect_uri"], redirect);
        assert_eq!(first_query["resource"], RESOURCE);
        assert_eq!(first_query["code_challenge_method"], "S256");
        assert!(
            first_query["scope"]
                .split_ascii_whitespace()
                .any(|scope| scope == "chatgpt.tokens.use.direct")
        );
        assert!(!first_query.contains_key("id_token_hint"));
        assert!(!first_query.contains_key("login_hint"));

        let saved = CredentialProfile {
            client_id: "oaiapp_saved".to_owned(),
            subject: "fixture-subject".to_owned(),
            email: Some("sample@example.invalid".to_owned()),
            display_name: Some("Sample".to_owned()),
            host_id: host_id.to_owned(),
            tokens: Some(TokenSet {
                id_token: "fixture-expired-id-token".to_owned(),
                access_token: "fixture-access-token".to_owned(),
                refresh_token: "fixture-refresh-token".to_owned(),
                token_type: "Bearer".to_owned(),
                scopes: vec!["openid".to_owned()],
                expires_at_unix: 1,
            }),
        };
        let returning = build_authorization_url(
            "https://auth.openai.com/api/accounts/authorize",
            Some(&saved.client_id),
            Some(&saved),
            host_id,
            redirect,
            "state-fixture-2",
            "nonce-fixture-2",
            "challenge-fixture-2",
        )
        .unwrap();
        let returning_query = query_pairs(&returning);
        assert_eq!(returning_query["client_id"], "oaiapp_saved");
        assert_eq!(returning_query["id_token_hint"], "fixture-expired-id-token");
        assert_eq!(returning_query["login_hint"], "sample@example.invalid");
        assert!(!returning_query.contains_key("agent_name_hint"));
    }

    #[test]
    fn one_registration_shares_an_in_process_refresh_lock_but_accounts_do_not() {
        let directory = tempfile::tempdir().unwrap();
        let service = AuthService::open(directory.path().to_path_buf()).unwrap();
        let first = service.process_refresh_lock("oaiapp_one").unwrap();
        let same = service.process_refresh_lock("oaiapp_one").unwrap();
        let other = service.process_refresh_lock("oaiapp_two").unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &same));
        assert!(!std::sync::Arc::ptr_eq(&first, &other));
    }

    async fn capture_form_request(fields: &[(&str, &str)]) -> (String, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let expected_end = loop {
                let count = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..count]);
                if let Some(header_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let body_len = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")?
                                .parse::<usize>()
                                .ok()
                        })
                        .unwrap_or(0);
                    break header_end + 4 + body_len;
                }
            };
            while request.len() < expected_end {
                let count = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..count]);
            }
            let request = String::from_utf8(request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .await
                .unwrap();
            request
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let endpoint = format!("http://{address}/token");
        let response = post_form_response(&client, &endpoint, fields, 1024)
            .await
            .unwrap();
        assert_eq!(response.status, StatusCode::OK);
        let request = server.await.unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        (headers.to_owned(), body.to_owned())
    }

    #[tokio::test]
    async fn authorization_code_exchange_is_form_encoded_with_the_exact_loopback_redirect_and_resource()
     {
        let fields = authorization_code_grant_fields(
            "oaiapp_fixture",
            "synthetic-code",
            "synthetic-pkce-verifier",
            "http://127.0.0.1:41733/auth/callback",
        );
        let (headers, body) = capture_form_request(&fields).await;
        assert!(headers.starts_with("POST /token HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        let values = url::form_urlencoded::parse(body.as_bytes()).collect::<HashMap<_, _>>();
        assert_eq!(values["grant_type"], "authorization_code");
        assert_eq!(values["client_id"], "oaiapp_fixture");
        assert_eq!(values["code"], "synthetic-code");
        assert_eq!(values["code_verifier"], "synthetic-pkce-verifier");
        assert_eq!(
            values["redirect_uri"],
            "http://127.0.0.1:41733/auth/callback"
        );
        assert_eq!(values["resource"], RESOURCE);
        assert!(!values.contains_key("client_secret"));
        assert!(!values.contains_key("scope"));
    }

    #[tokio::test]
    async fn refresh_request_uses_issued_client_id_rotating_token_and_omits_scope() {
        let fields = refresh_token_grant_fields("oaiapp_fixture", "synthetic-refresh-token");
        let (_, body) = capture_form_request(&fields).await;
        let values = url::form_urlencoded::parse(body.as_bytes()).collect::<HashMap<_, _>>();
        assert_eq!(values["grant_type"], "refresh_token");
        assert_eq!(values["client_id"], "oaiapp_fixture");
        assert_eq!(values["refresh_token"], "synthetic-refresh-token");
        assert_eq!(values["resource"], RESOURCE);
        assert!(!values.contains_key("scope"));
    }
}
