use crate::analysis::{
    AiAssessment, AnalysisScope, AnalysisTargetState, EvidenceSnapshot, StoredAnalysis,
    error_payload,
};
use crate::capabilities::{
    CapabilityKind, CapabilityRecord, CapabilityState, DeviceProfile, EvidenceLevel,
    build_device_profile,
};
use crate::chat::{ChatAnswer, ChatCitation, ChatScope, ChatTurn, parse_chat_answer};
use crate::inventory::{
    DeviceEdge, DeviceRecord, IdentityConfidence, InventorySnapshot, InventorySourceWarning,
};
use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

const INITIAL_MIGRATION: &str = include_str!("../../migrations/001_initial.sql");
const DEVICE_LAYERS_MIGRATION: &str = include_str!("../../migrations/002_device_layers.sql");
const CAPABILITY_PROFILES_MIGRATION: &str =
    include_str!("../../migrations/003_capability_profiles.sql");
const RUNTIME_HISTORY_MIGRATION: &str = include_str!("../../migrations/004_runtime_history.sql");
const AI_ANALYSIS_MIGRATION: &str = include_str!("../../migrations/005_ai_analysis_lookup.sql");
const CHAT_EVIDENCE_MIGRATION: &str = include_str!("../../migrations/006_chat_evidence.sql");
pub const LATEST_SCHEMA_VERSION: i64 = 6;

pub struct Database {
    connection: Mutex<Connection>,
}

impl Database {
    pub fn open_at(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| "SQLITE_DIRECTORY_CREATE_FAILED".to_owned())?;
        }

        let connection =
            Connection::open(path).map_err(|error| format!("SQLITE_OPEN_FAILED: {error}"))?;
        Self::from_connection(connection, true)
    }

    pub fn open_in_memory() -> Result<Self, String> {
        let connection =
            Connection::open_in_memory().map_err(|error| format!("SQLITE_OPEN_FAILED: {error}"))?;
        Self::from_connection(connection, false)
    }

    fn from_connection(mut connection: Connection, persistent: bool) -> Result<Self, String> {
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| format!("SQLITE_BUSY_TIMEOUT_FAILED: {error}"))?;
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .map_err(|error| format!("SQLITE_FOREIGN_KEYS_FAILED: {error}"))?;

        if persistent {
            connection
                .pragma_update(None, "journal_mode", "WAL")
                .map_err(|error| format!("SQLITE_JOURNAL_MODE_FAILED: {error}"))?;
            connection
                .pragma_update(None, "synchronous", "NORMAL")
                .map_err(|error| format!("SQLITE_SYNCHRONOUS_MODE_FAILED: {error}"))?;
        }

        Self::apply_migrations(&mut connection)?;
        Self::ensure_install_record(&connection)?;

        let database = Self {
            connection: Mutex::new(connection),
        };
        database.backfill_capability_profiles()?;
        database.archive_live_capabilities("APP_RESTARTED_LIVE_DATA_NOT_CURRENT")?;
        database.recover_interrupted_ai_analyses()?;
        database.recover_interrupted_chat_messages()?;
        Ok(database)
    }

    fn apply_migrations(connection: &mut Connection) -> Result<(), String> {
        let mut current_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(|error| format!("SQLITE_VERSION_READ_FAILED: {error}"))?;

        if current_version > LATEST_SCHEMA_VERSION {
            return Err("SQLITE_SCHEMA_NEWER_THAN_APPLICATION".to_owned());
        }
        if current_version == LATEST_SCHEMA_VERSION {
            return Ok(());
        }

        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_MIGRATION_BEGIN_FAILED: {error}"))?;
        if current_version < 1 {
            transaction
                .execute_batch(INITIAL_MIGRATION)
                .map_err(|error| format!("SQLITE_MIGRATION_001_FAILED: {error}"))?;
            current_version = 1;
        }
        if current_version < 2 {
            transaction
                .execute_batch(DEVICE_LAYERS_MIGRATION)
                .map_err(|error| format!("SQLITE_MIGRATION_002_FAILED: {error}"))?;
            current_version = 2;
        }
        if current_version < 3 {
            transaction
                .execute_batch(CAPABILITY_PROFILES_MIGRATION)
                .map_err(|error| format!("SQLITE_MIGRATION_003_FAILED: {error}"))?;
            current_version = 3;
        }
        if current_version < 4 {
            transaction
                .execute_batch(RUNTIME_HISTORY_MIGRATION)
                .map_err(|error| format!("SQLITE_MIGRATION_004_FAILED: {error}"))?;
            current_version = 4;
        }
        if current_version < 5 {
            transaction
                .execute_batch(AI_ANALYSIS_MIGRATION)
                .map_err(|error| format!("SQLITE_MIGRATION_005_FAILED: {error}"))?;
            current_version = 5;
        }
        if current_version < 6 {
            transaction
                .execute_batch(CHAT_EVIDENCE_MIGRATION)
                .map_err(|error| format!("SQLITE_MIGRATION_006_FAILED: {error}"))?;
        }
        transaction
            .pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)
            .map_err(|error| format!("SQLITE_MIGRATION_VERSION_FAILED: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_MIGRATION_COMMIT_FAILED: {error}"))?;
        Ok(())
    }

    fn ensure_install_record(connection: &Connection) -> Result<(), String> {
        let row_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM install", [], |row| row.get(0))
            .map_err(|error| format!("SQLITE_INSTALL_READ_FAILED: {error}"))?;
        if row_count == 0 {
            let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            connection
                .execute(
                    "INSERT INTO install(id, install_id, created_at, local_salt_ref) \
                     VALUES (1, ?1, ?2, ?3)",
                    params![
                        Uuid::new_v4().to_string(),
                        created_at,
                        Uuid::new_v4().to_string()
                    ],
                )
                .map_err(|error| format!("SQLITE_INSTALL_CREATE_FAILED: {error}"))?;
        }
        Ok(())
    }

    fn backfill_capability_profiles(&self) -> Result<(), String> {
        let Some(snapshot) = self.load_inventory()? else {
            return Ok(());
        };
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_CAPABILITY_BACKFILL_BEGIN_FAILED: {error}"))?;
        for device in snapshot.devices {
            let profile = build_device_profile(device, &snapshot.scanned_at_utc);
            for capability in profile.capabilities {
                capability.validate()?;
                persist_capability(&transaction, &capability, CapabilityWriteMode::Backfill)?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_CAPABILITY_BACKFILL_COMMIT_FAILED: {error}"))
    }

    fn archive_live_capabilities(&self, stale_reason: &str) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_BEGIN_FAILED: {error}"))?;
        archive_live_capabilities_in(&transaction, stale_reason)?;
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_COMMIT_FAILED: {error}"))
    }

    pub fn schema_version(&self) -> Result<i64, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| format!("SQLITE_VERSION_READ_FAILED: {error}"))
    }

    pub fn has_table(&self, name: &str) -> Result<bool, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [name],
                |row| row.get(0),
            )
            .map_err(|error| format!("SQLITE_SCHEMA_CHECK_FAILED: {error}"))
    }

    pub fn ai_data_consent_granted(&self) -> Result<bool, String> {
        self.read_boolean_setting("ai_data_consent_v1")
    }

    pub fn ai_data_consent_decided(&self) -> Result<bool, String> {
        self.read_boolean_setting("ai_data_consent_decided_v1")
    }

    pub fn set_ai_data_consent(&self, granted: bool) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_SETTING_WRITE_BEGIN_FAILED: {error}"))?;
        transaction
            .execute(
                "INSERT INTO app_settings(key, value_json) VALUES ('ai_data_consent_v1', ?1) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
                [if granted { "true" } else { "false" }],
            )
            .map_err(|error| format!("SQLITE_SETTING_WRITE_FAILED: {error}"))?;
        transaction
            .execute(
                "INSERT INTO app_settings(key, value_json) VALUES ('ai_data_consent_decided_v1', 'true') ON CONFLICT(key) DO UPDATE SET value_json = 'true'",
                [],
            )
            .map_err(|error| format!("SQLITE_SETTING_WRITE_FAILED: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_SETTING_WRITE_COMMIT_FAILED: {error}"))
    }

    pub fn initial_system_analysis_attempted(&self) -> Result<bool, String> {
        self.read_boolean_setting("system_analysis_initial_attempted_v1")
    }

    pub fn claim_initial_system_analysis(&self) -> Result<bool, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let changed = connection
            .execute(
                "INSERT OR IGNORE INTO app_settings(key, value_json) VALUES ('system_analysis_initial_attempted_v1', 'true')",
                [],
            )
            .map_err(|error| format!("SQLITE_INITIAL_ANALYSIS_CLAIM_FAILED: {error}"))?;
        Ok(changed == 1)
    }

    pub fn initial_device_analysis_attempted(&self, device_id: &str) -> Result<bool, String> {
        if device_id.is_empty() || device_id.len() > 512 {
            return Err("SQLITE_DEVICE_ANALYSIS_ID_INVALID".to_owned());
        }
        if self
            .read_boolean_setting(&format!("device_analysis_initial_attempted_v1:{device_id}"))?
        {
            return Ok(true);
        }
        Ok(self
            .latest_ai_analysis_attempt_status(AnalysisScope::Device, Some(device_id))?
            .is_some())
    }

    pub fn claim_initial_device_analysis(&self, device_id: &str) -> Result<bool, String> {
        if device_id.is_empty() || device_id.len() > 512 {
            return Err("SQLITE_DEVICE_ANALYSIS_ID_INVALID".to_owned());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let changed = connection
            .execute(
                "INSERT OR IGNORE INTO app_settings(key, value_json) VALUES (?1, 'true')",
                [format!("device_analysis_initial_attempted_v1:{device_id}")],
            )
            .map_err(|error| format!("SQLITE_INITIAL_DEVICE_ANALYSIS_CLAIM_FAILED: {error}"))?;
        Ok(changed == 1)
    }

    pub fn begin_ai_analysis(
        &self,
        scope: AnalysisScope,
        target_device_id: Option<&str>,
        input_fingerprint: &str,
        evidence_snapshot: &EvidenceSnapshot,
        model_slug: &str,
    ) -> Result<String, String> {
        if (scope == AnalysisScope::Device) != target_device_id.is_some()
            || input_fingerprint.len() != 64
            || model_slug.trim().is_empty()
            || model_slug.len() > 160
        {
            return Err("SQLITE_AI_ANALYSIS_RECORD_INVALID".to_owned());
        }
        let evidence_json = serde_json::to_string(evidence_snapshot)
            .map_err(|_| "SQLITE_AI_EVIDENCE_SERIALIZE_FAILED".to_owned())?;
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .execute(
                "INSERT INTO ai_analyses(id, scope, target_id, input_fingerprint, evidence_snapshot_json, response_json, model_slug, created_at, status, is_historical) \
                 VALUES (?1, ?2, ?3, ?4, ?5, '{}', ?6, ?7, 'PENDING', 0)",
                params![
                    id,
                    scope.as_str(),
                    target_device_id,
                    input_fingerprint,
                    evidence_json,
                    model_slug,
                    created_at,
                ],
            )
            .map_err(|error| format!("SQLITE_AI_ANALYSIS_BEGIN_FAILED: {error}"))?;
        Ok(id)
    }

    pub fn complete_ai_analysis(&self, id: &str, assessment: &AiAssessment) -> Result<(), String> {
        let response_json = serde_json::to_string(assessment)
            .map_err(|_| "SQLITE_AI_RESULT_SERIALIZE_FAILED".to_owned())?;
        self.finish_ai_analysis(id, "COMPLETED", &response_json)
    }

    pub fn fail_ai_analysis(&self, id: &str, status: &str, error_code: &str) -> Result<(), String> {
        if !matches!(status, "FAILED" | "INTERRUPTED") {
            return Err("SQLITE_AI_ANALYSIS_STATUS_INVALID".to_owned());
        }
        self.finish_ai_analysis(id, status, &error_payload(error_code))
    }

    fn finish_ai_analysis(
        &self,
        id: &str,
        status: &str,
        response_json: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let changed = connection
            .execute(
                "UPDATE ai_analyses SET response_json = ?2, status = ?3 WHERE id = ?1 AND status = 'PENDING'",
                params![id, response_json, status],
            )
            .map_err(|error| format!("SQLITE_AI_ANALYSIS_FINISH_FAILED: {error}"))?;
        if changed != 1 {
            return Err("SQLITE_AI_ANALYSIS_PENDING_NOT_FOUND".to_owned());
        }
        Ok(())
    }

    pub fn mark_ai_analyses_current(
        &self,
        scope: AnalysisScope,
        target_device_id: Option<&str>,
        current_fingerprint: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .execute(
                "UPDATE ai_analyses SET is_historical = CASE WHEN input_fingerprint = ?3 THEN 0 ELSE 1 END \
                 WHERE scope = ?1 AND target_id IS ?2 AND status = 'COMPLETED'",
                params![scope.as_str(), target_device_id, current_fingerprint],
            )
            .map_err(|error| format!("SQLITE_AI_ANALYSIS_INVALIDATE_FAILED: {error}"))?;
        Ok(())
    }

    pub fn latest_successful_ai_analysis(
        &self,
        scope: AnalysisScope,
        target_device_id: Option<&str>,
    ) -> Result<Option<StoredAnalysis>, String> {
        let raw = {
            let connection = self
                .connection
                .lock()
                .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
            connection
                .query_row(
                    "SELECT id, scope, target_id, input_fingerprint, evidence_snapshot_json, response_json, model_slug, created_at, status, is_historical \
                     FROM ai_analyses WHERE scope = ?1 AND target_id IS ?2 AND status = 'COMPLETED' \
                     ORDER BY created_at DESC, rowid DESC LIMIT 1",
                    params![scope.as_str(), target_device_id],
                    raw_analysis_from_row,
                )
                .optional()
                .map_err(|error| format!("SQLITE_AI_ANALYSIS_READ_FAILED: {error}"))?
        };
        raw.map(decode_stored_analysis).transpose()
    }

    pub fn latest_ai_analysis_attempt_status(
        &self,
        scope: AnalysisScope,
        target_device_id: Option<&str>,
    ) -> Result<Option<String>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .query_row(
                "SELECT status FROM ai_analyses WHERE scope = ?1 AND target_id IS ?2 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![scope.as_str(), target_device_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("SQLITE_AI_ANALYSIS_STATUS_READ_FAILED: {error}"))
    }

    pub fn analysis_target_state(
        &self,
        scope: AnalysisScope,
        target_device_id: Option<&str>,
    ) -> Result<AnalysisTargetState, String> {
        Ok(AnalysisTargetState {
            consent_granted: self.ai_data_consent_granted()?,
            consent_decided: self.ai_data_consent_decided()?,
            initial_attempted: match scope {
                AnalysisScope::System => self.initial_system_analysis_attempted()?,
                AnalysisScope::Device => self.initial_device_analysis_attempted(
                    target_device_id.ok_or_else(|| "AI_ANALYSIS_DEVICE_REQUIRED".to_owned())?,
                )?,
            },
            latest_analysis: self.latest_successful_ai_analysis(scope, target_device_id)?,
            latest_attempt_status: self
                .latest_ai_analysis_attempt_status(scope, target_device_id)?,
        })
    }

    fn recover_interrupted_ai_analyses(&self) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .execute(
                "UPDATE ai_analyses SET status = 'INTERRUPTED', response_json = '{\"errorCode\":\"AI_ANALYSIS_INTERRUPTED_BY_EXIT\"}' WHERE status = 'PENDING'",
                [],
            )
            .map_err(|error| format!("SQLITE_AI_ANALYSIS_RECOVERY_FAILED: {error}"))?;
        Ok(())
    }

    pub fn create_chat_thread(
        &self,
        scope: ChatScope,
        target_device_id: Option<&str>,
    ) -> Result<ChatThreadRecord, String> {
        validate_chat_target(scope, target_device_id)?;
        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .execute(
                "INSERT INTO chat_threads(id, scope, target_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                params![id, scope.as_str(), target_device_id, created_at],
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_CREATE_FAILED: {error}"))?;
        Ok(ChatThreadRecord {
            id,
            scope,
            target_device_id: target_device_id.map(str::to_owned),
            created_at_utc: created_at.clone(),
            updated_at_utc: created_at,
        })
    }

    pub fn list_chat_threads(
        &self,
        scope: ChatScope,
        target_device_id: Option<&str>,
    ) -> Result<Vec<ChatThreadRecord>, String> {
        validate_chat_target(scope, target_device_id)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let mut statement = connection
            .prepare(
                "SELECT id, scope, target_id, created_at, updated_at FROM chat_threads \
                 WHERE scope = ?1 AND target_id IS ?2 ORDER BY updated_at DESC, id DESC LIMIT 50",
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_LIST_PREPARE_FAILED: {error}"))?;
        let raw_threads = statement
            .query_map(
                params![scope.as_str(), target_device_id],
                raw_chat_thread_from_row,
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_LIST_FAILED: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("SQLITE_CHAT_THREAD_LIST_FAILED: {error}"))?;
        raw_threads.into_iter().map(decode_chat_thread).collect()
    }

    pub fn clear_chat_thread(&self, thread_id: &str) -> Result<usize, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_CHAT_CLEAR_BEGIN_FAILED: {error}"))?;
        let exists = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_threads WHERE id = ?1)",
                [thread_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_READ_FAILED: {error}"))?;
        if !exists {
            return Err("SQLITE_CHAT_THREAD_NOT_FOUND".to_owned());
        }
        let has_pending = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM chat_messages WHERE thread_id = ?1 AND status = 'PENDING')",
                [thread_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| format!("SQLITE_CHAT_PENDING_READ_FAILED: {error}"))?;
        if has_pending {
            return Err("AI_CHAT_THREAD_BUSY".to_owned());
        }
        transaction
            .execute(
                "DELETE FROM chat_evidence_snapshots WHERE thread_id = ?1",
                [thread_id],
            )
            .map_err(|error| format!("SQLITE_CHAT_CLEAR_EVIDENCE_FAILED: {error}"))?;
        let removed_messages = transaction
            .execute(
                "DELETE FROM chat_messages WHERE thread_id = ?1",
                [thread_id],
            )
            .map_err(|error| format!("SQLITE_CHAT_CLEAR_MESSAGES_FAILED: {error}"))?;
        let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        transaction
            .execute(
                "UPDATE chat_threads SET updated_at = ?2 WHERE id = ?1",
                params![thread_id, updated_at],
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_UPDATE_FAILED: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_CHAT_CLEAR_COMMIT_FAILED: {error}"))?;
        Ok(removed_messages)
    }

    pub fn load_chat_thread(&self, thread_id: &str) -> Result<Option<ChatThreadRecord>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let raw = connection
            .query_row(
                "SELECT id, scope, target_id, created_at, updated_at FROM chat_threads WHERE id = ?1",
                [thread_id],
                raw_chat_thread_from_row,
            )
            .optional()
            .map_err(|error| format!("SQLITE_CHAT_THREAD_READ_FAILED: {error}"))?;
        raw.map(decode_chat_thread).transpose()
    }

    pub fn load_chat_messages(&self, thread_id: &str) -> Result<Vec<ChatMessageRecord>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let mut statement = connection
            .prepare(
                "SELECT id, thread_id, role, body, evidence_snapshot_ref, citations_json, created_at, status, error_code, model_slug, source, snapshot_captured_at_utc \
                 FROM (SELECT id, thread_id, role, body, evidence_snapshot_ref, citations_json, created_at, status, error_code, model_slug, source, \
                              (SELECT captured_at_utc FROM chat_evidence_snapshots WHERE snapshot_id = chat_messages.evidence_snapshot_ref) AS snapshot_captured_at_utc, \
                              rowid AS sequence \
                       FROM chat_messages WHERE thread_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 200) \
                 ORDER BY created_at ASC, sequence ASC",
            )
            .map_err(|error| format!("SQLITE_CHAT_MESSAGES_PREPARE_FAILED: {error}"))?;
        let raw_messages = statement
            .query_map([thread_id], raw_chat_message_from_row)
            .map_err(|error| format!("SQLITE_CHAT_MESSAGES_READ_FAILED: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("SQLITE_CHAT_MESSAGES_READ_FAILED: {error}"))?;
        raw_messages.into_iter().map(decode_chat_message).collect()
    }

    pub fn load_recent_chat_turns(
        &self,
        thread_id: &str,
        limit: usize,
    ) -> Result<Vec<ChatTurn>, String> {
        let limit = limit.clamp(1, 12);
        let turns = self
            .load_chat_messages(thread_id)?
            .into_iter()
            .filter(|message| {
                message.status == "COMPLETED"
                    && matches!(message.role.as_str(), "user" | "assistant")
            })
            .map(|message| ChatTurn {
                role: message.role,
                status: message.status,
                body: message.body,
            })
            .collect::<Vec<_>>();
        Ok(turns
            .into_iter()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect())
    }

    pub fn load_chat_evidence_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<EvidenceSnapshot>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let raw = connection
            .query_row(
                "SELECT snapshot_json FROM chat_evidence_snapshots WHERE snapshot_id = ?1",
                [snapshot_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("SQLITE_CHAT_EVIDENCE_READ_FAILED: {error}"))?;
        raw.map(|json| {
            serde_json::from_str(&json).map_err(|_| "SQLITE_CHAT_EVIDENCE_INVALID".to_owned())
        })
        .transpose()
    }

    pub fn begin_chat_submission(
        &self,
        thread_id: &str,
        snapshot: &EvidenceSnapshot,
        input_fingerprint: &str,
        question: &str,
    ) -> Result<ChatPendingSubmission, String> {
        if input_fingerprint.len() != 64
            || !input_fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || question.trim().is_empty()
            || question.trim().chars().count() > 2000
            || question.trim().len() > 8000
        {
            return Err("SQLITE_CHAT_SUBMISSION_INVALID".to_owned());
        }
        let evidence_json = serde_json::to_string(snapshot)
            .map_err(|_| "SQLITE_CHAT_EVIDENCE_SERIALIZE_FAILED".to_owned())?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_CHAT_SUBMISSION_BEGIN_FAILED: {error}"))?;
        let raw_thread = transaction
            .query_row(
                "SELECT id, scope, target_id, created_at, updated_at FROM chat_threads WHERE id = ?1",
                [thread_id],
                raw_chat_thread_from_row,
            )
            .optional()
            .map_err(|error| format!("SQLITE_CHAT_THREAD_READ_FAILED: {error}"))?
            .ok_or_else(|| "SQLITE_CHAT_THREAD_NOT_FOUND".to_owned())?;
        let thread = decode_chat_thread(raw_thread)?;
        if thread.scope.analysis_scope() != snapshot.scope {
            return Err("SQLITE_CHAT_THREAD_SCOPE_INVALID".to_owned());
        }
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let user_message_id = Uuid::new_v4().to_string();
        transaction
            .execute(
                "INSERT INTO chat_evidence_snapshots(snapshot_id, thread_id, target_id, input_fingerprint, snapshot_json, captured_at_utc, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    snapshot.snapshot_id,
                    thread_id,
                    thread.target_device_id,
                    input_fingerprint,
                    evidence_json,
                    snapshot.captured_at_utc,
                    created_at,
                ],
            )
            .map_err(|error| format!("SQLITE_CHAT_EVIDENCE_INSERT_FAILED: {error}"))?;
        transaction
            .execute(
                "INSERT INTO chat_messages(id, thread_id, role, body, evidence_snapshot_ref, citations_json, created_at, status, error_code) \
                 VALUES (?1, ?2, 'user', ?3, ?4, '[]', ?5, 'PENDING', NULL)",
                params![
                    user_message_id,
                    thread_id,
                    question.trim(),
                    snapshot.snapshot_id,
                    created_at,
                ],
            )
            .map_err(|error| format!("SQLITE_CHAT_MESSAGE_INSERT_FAILED: {error}"))?;
        transaction
            .execute(
                "UPDATE chat_threads SET updated_at = ?2 WHERE id = ?1",
                params![thread_id, created_at],
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_UPDATE_FAILED: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_CHAT_SUBMISSION_COMMIT_FAILED: {error}"))?;
        Ok(ChatPendingSubmission {
            user_message_id,
            snapshot_id: snapshot.snapshot_id.clone(),
        })
    }

    pub fn complete_chat_submission(
        &self,
        user_message_id: &str,
        answer: &ChatAnswer,
        model_slug: &str,
        completed_at_utc: &str,
    ) -> Result<(), String> {
        if model_slug.trim().is_empty()
            || model_slug.len() > 160
            || chrono::DateTime::parse_from_rfc3339(completed_at_utc).is_err()
        {
            return Err("SQLITE_CHAT_PROVENANCE_INVALID".to_owned());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_CHAT_COMPLETE_BEGIN_FAILED: {error}"))?;
        let pending = transaction
            .query_row(
                "SELECT thread_id, evidence_snapshot_ref FROM chat_messages WHERE id = ?1 AND role = 'user' AND status = 'PENDING'",
                [user_message_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|error| format!("SQLITE_CHAT_PENDING_READ_FAILED: {error}"))?
            .ok_or_else(|| "SQLITE_CHAT_PENDING_NOT_FOUND".to_owned())?;
        let snapshot_ref = pending
            .1
            .ok_or_else(|| "SQLITE_CHAT_EVIDENCE_REFERENCE_MISSING".to_owned())?;
        let snapshot_json = transaction
            .query_row(
                "SELECT snapshot_json FROM chat_evidence_snapshots WHERE snapshot_id = ?1 AND thread_id = ?2",
                params![snapshot_ref, pending.0],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("SQLITE_CHAT_EVIDENCE_READ_FAILED: {error}"))?
            .ok_or_else(|| "SQLITE_CHAT_EVIDENCE_NOT_FOUND".to_owned())?;
        let snapshot: EvidenceSnapshot = serde_json::from_str(&snapshot_json)
            .map_err(|_| "SQLITE_CHAT_EVIDENCE_INVALID".to_owned())?;
        let answer_json = serde_json::to_string(answer)
            .map_err(|_| "SQLITE_CHAT_ANSWER_SERIALIZE_FAILED".to_owned())?;
        let answer = parse_chat_answer(&answer_json, &snapshot)?;
        let citations_json = serde_json::to_string(&answer.citations)
            .map_err(|_| "SQLITE_CHAT_CITATIONS_SERIALIZE_FAILED".to_owned())?;
        let created_at = completed_at_utc.to_owned();
        let changed = transaction
            .execute(
                "UPDATE chat_messages SET status = 'COMPLETED', error_code = NULL WHERE id = ?1 AND status = 'PENDING'",
                [user_message_id],
            )
            .map_err(|error| format!("SQLITE_CHAT_MESSAGE_COMPLETE_FAILED: {error}"))?;
        if changed != 1 {
            return Err("SQLITE_CHAT_PENDING_NOT_FOUND".to_owned());
        }
        transaction
            .execute(
                "INSERT INTO chat_messages(id, thread_id, role, body, evidence_snapshot_ref, citations_json, created_at, status, error_code, model_slug, source) \
                 VALUES (?1, ?2, 'assistant', ?3, ?4, ?5, ?6, 'COMPLETED', NULL, ?7, 'ChatGPT Responses API')",
                params![
                    Uuid::new_v4().to_string(),
                    pending.0,
                    answer.answer,
                    snapshot_ref,
                    citations_json,
                    created_at,
                    model_slug,
                ],
            )
            .map_err(|error| format!("SQLITE_CHAT_ASSISTANT_INSERT_FAILED: {error}"))?;
        transaction
            .execute(
                "UPDATE chat_threads SET updated_at = ?2 WHERE id = ?1",
                params![pending.0, created_at],
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_UPDATE_FAILED: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_CHAT_COMPLETE_COMMIT_FAILED: {error}"))
    }

    pub fn fail_chat_submission(
        &self,
        user_message_id: &str,
        status: &str,
        error_code: &str,
    ) -> Result<(), String> {
        if !matches!(status, "FAILED" | "INTERRUPTED") {
            return Err("SQLITE_CHAT_STATUS_INVALID".to_owned());
        }
        let error_code = safe_chat_error_code(error_code);
        let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_CHAT_FAILURE_BEGIN_FAILED: {error}"))?;
        let thread_id = transaction
            .query_row(
                "UPDATE chat_messages SET status = ?2, error_code = ?3 WHERE id = ?1 AND role = 'user' AND status = 'PENDING' RETURNING thread_id",
                params![user_message_id, status, error_code],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("SQLITE_CHAT_FAILURE_UPDATE_FAILED: {error}"))?
            .ok_or_else(|| "SQLITE_CHAT_PENDING_NOT_FOUND".to_owned())?;
        transaction
            .execute(
                "UPDATE chat_threads SET updated_at = ?2 WHERE id = ?1",
                params![thread_id, created_at],
            )
            .map_err(|error| format!("SQLITE_CHAT_THREAD_UPDATE_FAILED: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_CHAT_FAILURE_COMMIT_FAILED: {error}"))
    }

    fn recover_interrupted_chat_messages(&self) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection
            .execute(
                "UPDATE chat_messages SET status = 'INTERRUPTED', error_code = 'AI_CHAT_APP_RESTARTED' \
                 WHERE status = 'PENDING' AND role = 'user'",
                [],
            )
            .map_err(|error| format!("SQLITE_CHAT_RECOVERY_FAILED: {error}"))?;
        Ok(())
    }

    fn read_boolean_setting(&self, key: &str) -> Result<bool, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let value = connection
            .query_row(
                "SELECT value_json FROM app_settings WHERE key = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("SQLITE_SETTING_READ_FAILED: {error}"))?;
        Ok(value
            .as_deref()
            .and_then(|raw| serde_json::from_str::<bool>(raw).ok())
            .unwrap_or(false))
    }

    pub fn identity_salt(&self) -> Result<Vec<u8>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let value: String = connection
            .query_row(
                "SELECT local_salt_ref FROM install WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| format!("SQLITE_INSTALL_SALT_READ_FAILED: {error}"))?;
        Ok(value.into_bytes())
    }

    pub fn persist_inventory(
        &self,
        snapshot: &InventorySnapshot,
        app_version: &str,
    ) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_INVENTORY_BEGIN_FAILED: {error}"))?;
        archive_live_capabilities_in(&transaction, "SCAN_REFRESH_PENDING_NEW_SAMPLE")?;
        let observed_at = &snapshot.scanned_at_utc;
        let scan_id = Uuid::new_v4().to_string();
        let status = if snapshot.warnings.is_empty() {
            "COMPLETED"
        } else {
            "COMPLETED_WITH_WARNINGS"
        };
        transaction.execute(
            "INSERT INTO scan_sessions(id, started_at, completed_at, status, schema_version, app_version) VALUES (?1, ?2, ?2, ?3, ?4, ?5)",
            params![scan_id, observed_at, status, LATEST_SCHEMA_VERSION, app_version],
        ).map_err(|error| format!("SQLITE_SCAN_SESSION_INSERT_FAILED: {error}"))?;
        transaction
            .execute("UPDATE devices SET is_present = 0", [])
            .map_err(|error| format!("SQLITE_MARK_REMOVED_DEVICES_FAILED: {error}"))?;
        transaction
            .execute("DELETE FROM device_edges", [])
            .map_err(|error| format!("SQLITE_REPLACE_DEVICE_EDGES_FAILED: {error}"))?;
        for device in &snapshot.devices {
            let attributes =
                attributes_with_manufacturer(&device.attributes, device.manufacturer.as_deref())?;
            let attributes_json = serde_json::to_string(&attributes)
                .map_err(|error| format!("SQLITE_DEVICE_ATTRIBUTES_SERIALIZE_FAILED: {error}"))?;
            transaction.execute(
                "INSERT INTO devices(id, kind, class, display_name, is_virtual, is_present, identity_hash, first_seen, last_seen, version, attributes_json, layer, identity_confidence, source) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, NULL, ?9, ?10, ?11, ?12) \
                 ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, class=excluded.class, display_name=excluded.display_name, \
                 is_virtual=excluded.is_virtual, is_present=excluded.is_present, identity_hash=excluded.identity_hash, last_seen=excluded.last_seen, \
                 attributes_json=excluded.attributes_json, layer=excluded.layer, identity_confidence=excluded.identity_confidence, source=excluded.source",
                params![
                    device.id,
                    device.kind.as_str(),
                    device.category.as_str(),
                    device.display_name,
                    device.is_virtual.map(i64::from),
                    device.is_present.map(i64::from),
                    device.identity_hash,
                    observed_at,
                    attributes_json,
                    device.layer.as_str(),
                    confidence_as_str(device.identity_confidence),
                    device.source,
                ],
            ).map_err(|error| format!("SQLITE_DEVICE_UPSERT_FAILED: {error}"))?;

            let profile = build_device_profile(device.clone(), observed_at);
            for capability in profile.capabilities {
                capability.validate()?;
                persist_capability(
                    &transaction,
                    &capability,
                    CapabilityWriteMode::InventoryRefresh,
                )?;
            }
        }

        transaction
            .execute(
                "UPDATE capabilities SET state = 'STALE', value_json = NULL, reason_code = 'DEVICE_NOT_PRESENT_IN_LATEST_SCAN' \
                 WHERE device_id IN (SELECT id FROM devices WHERE is_present = 0)",
                [],
            )
            .map_err(|error| format!("SQLITE_MARK_REMOVED_CAPABILITIES_STALE_FAILED: {error}"))?;
        transaction
            .execute(
                "UPDATE capabilities SET state = 'PROBING', value_json = NULL, reason_code = 'SCAN_REFRESH_PENDING_NEW_SAMPLE' \
                 WHERE capability_kind = 'DYNAMIC' AND state = 'STALE' \
                   AND reason_code = 'DEVICE_NOT_PRESENT_IN_LATEST_SCAN' \
                   AND device_id IN (SELECT id FROM devices WHERE is_present = 1)",
                [],
            )
            .map_err(|error| format!("SQLITE_RESTORE_PRESENT_CAPABILITIES_FAILED: {error}"))?;

        for edge in &snapshot.edges {
            transaction.execute(
                "INSERT INTO device_edges(parent_id, child_id, relationship, evidence_source, evidence_status, observed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![edge.parent_id, edge.child_id, edge.relationship, edge.evidence_source, edge.evidence_status, observed_at],
            ).map_err(|error| format!("SQLITE_DEVICE_EDGE_INSERT_FAILED: {error}"))?;
        }
        for warning in &snapshot.warnings {
            transaction.execute(
                "INSERT INTO diagnostic_events(id, error_code, adapter, method, occurred_at, native_code, request_id, sanitized_detail) \
                 VALUES (?1, ?2, ?3, 'scan_devices', ?4, ?5, ?6, ?7)",
                params![
                    Uuid::new_v4().to_string(), warning.reason_code, warning.source,
                    observed_at, warning.native_code, scan_id, format!("source={}", warning.source),
                ],
            ).map_err(|error| format!("SQLITE_SCAN_WARNING_INSERT_FAILED: {error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_INVENTORY_COMMIT_FAILED: {error}"))
    }

    pub fn inventory_summary(&self) -> Result<InventorySummary, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        connection.query_row(
            "SELECT COALESCE(SUM(CASE WHEN is_present = 1 THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN is_present = 0 THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN is_present IS NULL THEN 1 ELSE 0 END), 0), \
                    (SELECT COUNT(*) FROM device_edges), \
                    (SELECT COUNT(*) FROM scan_sessions WHERE status IN ('COMPLETED', 'COMPLETED_WITH_WARNINGS')), \
                    (SELECT MAX(completed_at) FROM scan_sessions WHERE status IN ('COMPLETED', 'COMPLETED_WITH_WARNINGS')) \
             FROM devices",
            [],
            |row| Ok(InventorySummary {
                current_devices: row.get(0)?, historical_devices: row.get(1)?, unknown_presence_devices: row.get(2)?,
                current_edges: row.get(3)?, completed_scans: row.get(4)?, latest_scan_at_utc: row.get(5)?,
            }),
        ).map_err(|error| format!("SQLITE_INVENTORY_SUMMARY_FAILED: {error}"))
    }

    pub fn cache_summary(&self) -> Result<CacheSummary, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let (
            page_count,
            page_size,
            scan_count,
            device_count,
            history_count,
            analyses,
            messages,
            threads,
            evidence_snapshots,
            knowledge,
            prices,
        ): (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) = connection
            .query_row(
                "SELECT \
                    (SELECT page_count FROM pragma_page_count), \
                    (SELECT page_size FROM pragma_page_size), \
                    (SELECT COUNT(*) FROM scan_sessions), \
                    (SELECT COUNT(*) FROM devices), \
                    (SELECT COUNT(*) FROM metric_snapshots), \
                    (SELECT COUNT(*) FROM ai_analyses), \
                    (SELECT COUNT(*) FROM chat_messages), \
                    (SELECT COUNT(*) FROM chat_threads), \
                    (SELECT COUNT(*) FROM chat_evidence_snapshots), \
                    (SELECT COUNT(*) FROM model_knowledge), \
                    (SELECT COUNT(*) FROM price_references)",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                        row.get(9)?,
                        row.get(10)?,
                    ))
                },
            )
            .map_err(|error| format!("SQLITE_CACHE_SUMMARY_FAILED: {error}"))?;
        Ok(CacheSummary {
            database_bytes: page_count.saturating_mul(page_size).max(0) as u64,
            scan_count,
            device_count,
            metric_history_count: history_count,
            ai_analysis_count: analyses,
            chat_message_count: messages,
            chat_thread_count: threads,
            model_knowledge_count: knowledge,
            price_reference_count: prices,
            clearable_ai_items: analyses
                + messages
                + threads
                + evidence_snapshots
                + knowledge
                + prices,
        })
    }

    pub fn clear_ai_cache(&self) -> Result<usize, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_CACHE_CLEAR_BEGIN_FAILED: {error}"))?;
        let mut removed = 0usize;
        for statement in [
            "DELETE FROM chat_messages",
            "DELETE FROM chat_threads",
            "DELETE FROM ai_analyses",
            "DELETE FROM model_knowledge",
            "DELETE FROM price_references",
        ] {
            removed += transaction
                .execute(statement, [])
                .map_err(|error| format!("SQLITE_CACHE_CLEAR_FAILED: {error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_CACHE_CLEAR_COMMIT_FAILED: {error}"))?;
        Ok(removed)
    }

    pub fn sanitized_diagnostic_report(
        &self,
        app_version: &str,
    ) -> Result<SanitizedDiagnosticReport, String> {
        let inventory = self.inventory_summary()?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let schema_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(|error| format!("SQLITE_VERSION_READ_FAILED: {error}"))?;
        let mut statement = connection
            .prepare(
                "SELECT error_code, adapter, method, occurred_at, duration_ms, permission_state, native_code, retry_count, request_id \
                 FROM diagnostic_events ORDER BY occurred_at DESC, id DESC LIMIT 100",
            )
            .map_err(|error| format!("SQLITE_DIAGNOSTIC_EXPORT_QUERY_PREPARE_FAILED: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(|error| format!("SQLITE_DIAGNOSTIC_EXPORT_QUERY_FAILED: {error}"))?;
        let diagnostics = rows
            .map(|row| {
                let (
                    error_code,
                    adapter,
                    method,
                    occurred_at_utc,
                    duration_ms,
                    permission_state,
                    native_code,
                    retry_count,
                    request_id,
                ) = row.map_err(|error| format!("SQLITE_DIAGNOSTIC_EXPORT_ROW_FAILED: {error}"))?;
                Ok(SanitizedDiagnosticEvent {
                    error_code: safe_export_error_code(&error_code),
                    adapter: safe_export_adapter(adapter.as_deref()),
                    method: safe_export_method(method.as_deref()),
                    occurred_at_utc,
                    duration_ms: duration_ms.filter(|value| *value >= 0 && *value <= 86_400_000),
                    permission_state: permission_state.as_deref().and_then(safe_export_code),
                    native_code: safe_native_code(native_code.as_deref()),
                    retry_count: retry_count.clamp(0, 10),
                    request_id: request_id
                        .as_deref()
                        .and_then(|value| Uuid::parse_str(value).ok())
                        .map(|value| value.to_string()),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(SanitizedDiagnosticReport {
            generated_at_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            app_version: app_version.to_owned(),
            schema_version,
            inventory,
            diagnostics,
        })
    }

    pub fn save_capabilities(&self, capabilities: &[CapabilityRecord]) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("SQLITE_CAPABILITY_BEGIN_FAILED: {error}"))?;
        for capability in capabilities {
            capability.validate()?;
            persist_capability(&transaction, capability, CapabilityWriteMode::Replace)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("SQLITE_CAPABILITY_COMMIT_FAILED: {error}"))
    }

    pub fn load_inventory(&self) -> Result<Option<InventorySnapshot>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let latest_scan: Option<(String, String)> = connection.query_row(
            "SELECT id, completed_at FROM scan_sessions WHERE status IN ('COMPLETED', 'COMPLETED_WITH_WARNINGS') ORDER BY completed_at DESC LIMIT 1", [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional().map_err(|error| format!("SQLITE_LATEST_SCAN_READ_FAILED: {error}"))?;
        let Some((latest_scan_id, scanned_at_utc)) = latest_scan else {
            return Ok(None);
        };

        let mut statement = connection.prepare(
            "SELECT id, identity_hash, identity_confidence, class, kind, layer, display_name, is_virtual, is_present, source, attributes_json \
             FROM devices WHERE last_seen = ?1 ORDER BY class, display_name",
        ).map_err(|error| format!("SQLITE_CURRENT_DEVICES_QUERY_PREPARE_FAILED: {error}"))?;
        let rows = statement
            .query_map([&scanned_at_utc], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<bool>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })
            .map_err(|error| format!("SQLITE_CURRENT_DEVICES_QUERY_FAILED: {error}"))?;
        let mut devices = Vec::new();
        for row in rows {
            let (
                id,
                identity_hash,
                confidence,
                category,
                kind,
                layer,
                display_name,
                is_virtual,
                is_present,
                source,
                attributes_json,
            ) = row.map_err(|error| format!("SQLITE_CURRENT_DEVICE_ROW_FAILED: {error}"))?;
            let mut attributes: Value = serde_json::from_str(&attributes_json)
                .map_err(|error| format!("SQLITE_DEVICE_ATTRIBUTES_PARSE_FAILED: {error}"))?;
            let manufacturer = attributes
                .as_object_mut()
                .and_then(|object| object.remove("manufacturer"))
                .and_then(|value| value.as_str().map(ToOwned::to_owned));
            devices.push(DeviceRecord {
                id,
                identity_hash,
                identity_confidence: parse_enum(&confidence)?,
                category: parse_enum(&category)?,
                kind: parse_enum(&kind)?,
                layer: parse_enum(&layer)?,
                display_name,
                manufacturer,
                is_virtual: is_virtual.map(|value| value != 0),
                is_present,
                source,
                attributes,
            });
        }

        let mut statement = connection.prepare(
            "SELECT parent_id, child_id, relationship, evidence_source, evidence_status FROM device_edges ORDER BY parent_id, child_id",
        ).map_err(|error| format!("SQLITE_DEVICE_EDGES_QUERY_PREPARE_FAILED: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(DeviceEdge {
                    parent_id: row.get(0)?,
                    child_id: row.get(1)?,
                    relationship: row.get(2)?,
                    evidence_source: row.get(3)?,
                    evidence_status: row.get(4)?,
                })
            })
            .map_err(|error| format!("SQLITE_DEVICE_EDGES_QUERY_FAILED: {error}"))?;
        let mut edges = Vec::new();
        for row in rows {
            edges.push(row.map_err(|error| format!("SQLITE_DEVICE_EDGE_ROW_FAILED: {error}"))?);
        }

        let mut statement = connection.prepare(
            "SELECT adapter, error_code, native_code FROM diagnostic_events WHERE request_id = ?1 AND method = 'scan_devices' ORDER BY occurred_at, id",
        ).map_err(|error| format!("SQLITE_SCAN_WARNINGS_QUERY_PREPARE_FAILED: {error}"))?;
        let rows = statement
            .query_map([latest_scan_id], |row| {
                Ok(InventorySourceWarning {
                    source: row
                        .get::<_, Option<String>>(0)?
                        .unwrap_or_else(|| "unknown".to_owned()),
                    reason_code: row.get(1)?,
                    native_code: row.get(2)?,
                })
            })
            .map_err(|error| format!("SQLITE_SCAN_WARNINGS_QUERY_FAILED: {error}"))?;
        let mut warnings = Vec::new();
        for row in rows {
            warnings.push(row.map_err(|error| format!("SQLITE_SCAN_WARNING_ROW_FAILED: {error}"))?);
        }
        Ok(Some(InventorySnapshot {
            scanned_at_utc,
            devices,
            edges,
            warnings,
        }))
    }

    pub fn load_device_profile(&self, device_id: &str) -> Result<Option<DeviceProfile>, String> {
        let Some(snapshot) = self.load_inventory()? else {
            return Ok(None);
        };
        let Some(device) = snapshot
            .devices
            .into_iter()
            .find(|device| device.id == device_id)
        else {
            return Ok(None);
        };
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let mut statement = connection
            .prepare(
                "SELECT metric_key, capability_kind, state, value_json, unit, source, evidence_level, last_checked_at, reason_code, native_error_code, details_json \
                 FROM capabilities WHERE device_id = ?1 ORDER BY metric_key",
            )
            .map_err(|error| format!("SQLITE_DEVICE_PROFILE_QUERY_PREPARE_FAILED: {error}"))?;
        let rows = statement
            .query_map([device_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })
            .map_err(|error| format!("SQLITE_DEVICE_PROFILE_QUERY_FAILED: {error}"))?;
        let mut capabilities = Vec::new();
        for row in rows {
            let (
                metric_key,
                kind,
                state,
                value_json,
                unit,
                source,
                evidence_level,
                observed_at_utc,
                reason_code,
                native_error_code,
                details_json,
            ) = row.map_err(|error| format!("SQLITE_DEVICE_PROFILE_ROW_FAILED: {error}"))?;
            let value = value_json
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(|error| format!("SQLITE_CAPABILITY_VALUE_PARSE_FAILED: {error}"))?;
            let details = serde_json::from_str(&details_json)
                .map_err(|error| format!("SQLITE_CAPABILITY_DETAILS_PARSE_FAILED: {error}"))?;
            let capability = CapabilityRecord {
                device_id: device.id.clone(),
                metric_key,
                kind: parse_enum::<CapabilityKind>(&kind)?,
                state: parse_enum::<CapabilityState>(&state)?,
                value,
                unit,
                source,
                evidence_level: evidence_level
                    .as_deref()
                    .map(parse_enum::<EvidenceLevel>)
                    .transpose()?,
                observed_at_utc,
                reason_code,
                native_error_code,
                details,
            };
            capability.validate()?;
            capabilities.push(capability);
        }
        Ok(Some(DeviceProfile {
            device,
            capabilities,
        }))
    }

    pub fn load_metric_history(&self, device_id: &str) -> Result<Vec<MetricHistoryRecord>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLITE_CONNECTION_LOCK_POISONED".to_owned())?;
        let mut statement = connection
            .prepare(
                "SELECT device_id, metric_key, value_json, unit, state, observed_at, source, evidence_level, reason_code \
                 FROM metric_snapshots WHERE device_id = ?1 ORDER BY observed_at DESC, id DESC",
            )
            .map_err(|error| format!("SQLITE_METRIC_HISTORY_QUERY_PREPARE_FAILED: {error}"))?;
        let rows = statement
            .query_map([device_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(|error| format!("SQLITE_METRIC_HISTORY_QUERY_FAILED: {error}"))?;
        rows.map(|row| {
            let (
                device_id,
                metric_key,
                value_json,
                unit,
                state,
                observed_at_utc,
                source,
                evidence_level,
                reason_code,
            ) = row.map_err(|error| format!("SQLITE_METRIC_HISTORY_ROW_FAILED: {error}"))?;
            let value = value_json
                .map(|encoded| serde_json::from_str(&encoded))
                .transpose()
                .map_err(|error| format!("SQLITE_METRIC_HISTORY_VALUE_PARSE_FAILED: {error}"))?;
            Ok(MetricHistoryRecord {
                device_id,
                metric_key,
                value,
                unit,
                state,
                observed_at_utc,
                source,
                evidence_level,
                reason_code,
            })
        })
        .collect()
    }
}

type RawAnalysis = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
);

type RawChatThread = (String, String, Option<String>, String, String);
type RawChatMessage = (
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Debug)]
pub struct ChatPendingSubmission {
    pub user_message_id: String,
    pub snapshot_id: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatThreadRecord {
    pub id: String,
    pub scope: ChatScope,
    pub target_device_id: Option<String>,
    pub created_at_utc: String,
    pub updated_at_utc: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageRecord {
    pub id: String,
    pub thread_id: String,
    pub role: String,
    pub body: String,
    pub evidence_snapshot_ref: Option<String>,
    pub citations: Vec<ChatCitation>,
    pub created_at_utc: String,
    pub status: String,
    pub error_code: Option<String>,
    pub model_slug: Option<String>,
    pub source: Option<String>,
    pub snapshot_captured_at_utc: Option<String>,
}

fn raw_chat_thread_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawChatThread> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn decode_chat_thread(raw: RawChatThread) -> Result<ChatThreadRecord, String> {
    let (id, raw_scope, target_device_id, created_at_utc, updated_at_utc) = raw;
    let scope = match raw_scope.as_str() {
        "system" => ChatScope::System,
        "device" => ChatScope::Device,
        _ => return Err("SQLITE_CHAT_THREAD_SCOPE_INVALID".to_owned()),
    };
    validate_chat_target(scope, target_device_id.as_deref())?;
    Ok(ChatThreadRecord {
        id,
        scope,
        target_device_id,
        created_at_utc,
        updated_at_utc,
    })
}

fn raw_chat_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawChatMessage> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}

fn decode_chat_message(raw: RawChatMessage) -> Result<ChatMessageRecord, String> {
    let (
        id,
        thread_id,
        role,
        body,
        evidence_snapshot_ref,
        citations_json,
        created_at_utc,
        status,
        error_code,
        model_slug,
        source,
        snapshot_captured_at_utc,
    ) = raw;
    if !matches!(role.as_str(), "user" | "assistant")
        || !matches!(
            status.as_str(),
            "PENDING" | "COMPLETED" | "FAILED" | "INTERRUPTED"
        )
    {
        return Err("SQLITE_CHAT_MESSAGE_INVALID".to_owned());
    }
    let citations = serde_json::from_str(&citations_json)
        .map_err(|_| "SQLITE_CHAT_CITATIONS_INVALID".to_owned())?;
    let error_code = error_code.map(|code| safe_chat_error_code(&code));
    Ok(ChatMessageRecord {
        id,
        thread_id,
        role,
        body,
        evidence_snapshot_ref,
        citations,
        created_at_utc,
        status,
        error_code,
        model_slug,
        source,
        snapshot_captured_at_utc,
    })
}

fn validate_chat_target(scope: ChatScope, target_device_id: Option<&str>) -> Result<(), String> {
    if (scope == ChatScope::Device) != target_device_id.is_some() {
        return Err("SQLITE_CHAT_TARGET_SCOPE_INVALID".to_owned());
    }
    if target_device_id.is_some_and(|target| target.is_empty() || target.len() > 256) {
        return Err("SQLITE_CHAT_TARGET_INVALID".to_owned());
    }
    Ok(())
}

fn safe_chat_error_code(value: &str) -> String {
    (!value.is_empty()
        && value.len() <= 120
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b'-' | b':')
        }))
    .then(|| value.to_owned())
    .unwrap_or_else(|| "AI_CHAT_FAILED".to_owned())
}

fn raw_analysis_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawAnalysis> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn decode_stored_analysis(raw: RawAnalysis) -> Result<StoredAnalysis, String> {
    let (
        id,
        scope,
        target_device_id,
        fingerprint,
        evidence,
        response,
        model,
        created,
        status,
        historical,
    ) = raw;
    let scope = serde_json::from_str::<AnalysisScope>(&format!("\"{scope}\""))
        .map_err(|_| "SQLITE_AI_ANALYSIS_SCOPE_INVALID".to_owned())?;
    let evidence_snapshot = serde_json::from_str::<EvidenceSnapshot>(&evidence)
        .map_err(|_| "SQLITE_AI_EVIDENCE_CACHE_INVALID".to_owned())?;
    let (assessment, error_code) = if status == "COMPLETED" {
        (
            Some(
                serde_json::from_str::<AiAssessment>(&response)
                    .map_err(|_| "SQLITE_AI_RESULT_CACHE_INVALID".to_owned())?,
            ),
            None,
        )
    } else {
        let value: Value = serde_json::from_str(&response)
            .map_err(|_| "SQLITE_AI_ERROR_CACHE_INVALID".to_owned())?;
        (
            None,
            value
                .get("errorCode")
                .and_then(Value::as_str)
                .filter(|code| {
                    !code.is_empty()
                        && code.len() <= 120
                        && code.bytes().all(|byte| {
                            byte.is_ascii_uppercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'_' | b'.' | b'-' | b':')
                        })
                })
                .map(str::to_owned),
        )
    };
    Ok(StoredAnalysis {
        id,
        scope,
        target_device_id,
        input_fingerprint: fingerprint,
        evidence_snapshot,
        assessment,
        model_slug: model,
        created_at_utc: created,
        status,
        is_historical: historical != 0,
        error_code,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricHistoryRecord {
    pub device_id: String,
    pub metric_key: String,
    pub value: Option<Value>,
    pub unit: Option<String>,
    pub state: String,
    pub observed_at_utc: Option<String>,
    pub source: String,
    pub evidence_level: Option<String>,
    pub reason_code: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventorySummary {
    pub current_devices: i64,
    pub historical_devices: i64,
    pub unknown_presence_devices: i64,
    pub current_edges: i64,
    pub completed_scans: i64,
    pub latest_scan_at_utc: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSummary {
    pub database_bytes: u64,
    pub scan_count: i64,
    pub device_count: i64,
    pub metric_history_count: i64,
    pub ai_analysis_count: i64,
    pub chat_message_count: i64,
    pub chat_thread_count: i64,
    pub model_knowledge_count: i64,
    pub price_reference_count: i64,
    pub clearable_ai_items: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedDiagnosticReport {
    pub generated_at_utc: String,
    pub app_version: String,
    pub schema_version: i64,
    pub inventory: InventorySummary,
    pub diagnostics: Vec<SanitizedDiagnosticEvent>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedDiagnosticEvent {
    pub error_code: Option<String>,
    pub adapter: Option<String>,
    pub method: Option<String>,
    pub occurred_at_utc: String,
    pub duration_ms: Option<i64>,
    pub permission_state: Option<String>,
    pub native_code: Option<String>,
    pub retry_count: i64,
    pub request_id: Option<String>,
}

fn safe_export_code(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_'))
    .then(|| value.to_owned())
}

fn safe_export_error_code(value: &str) -> Option<String> {
    matches!(
        value,
        "STORAGE_MEDIA_TYPE_QUERY_FAILED"
            | "SETUPAPI_DEVICE_SET_FAILED"
            | "SETUPAPI_PRESENT_SET_FAILED"
            | "SETUPAPI_ENUMERATION_FAILED"
            | "SETUPAPI_INSTANCE_ID_UNAVAILABLE"
            | "SETUPAPI_PRESENT_ENUMERATION_FAILED"
            | "SETUPAPI_PRESENT_ID_UNAVAILABLE"
            | "WMI_QUERY_FAILED"
    )
    .then(|| value.to_owned())
}

fn safe_export_adapter(value: Option<&str>) -> Option<String> {
    let value = value?;
    matches!(
        value,
        "WMI"
            | "SetupAPI"
            | "MSFT_PhysicalDisk"
            | "WMI:Win32_PnPEntity"
            | "WMI:Win32_DiskDriveToDiskPartition"
            | "WMI:Win32_LogicalDiskToPartition"
    )
    .then(|| value.to_owned())
}

fn safe_export_method(value: Option<&str>) -> Option<String> {
    let value = value?;
    matches!(
        value,
        "scan_devices"
            | "load_inventory"
            | "load_device_profile"
            | "probe_storage_reliability"
            | "start_monitoring"
            | "load_telemetry"
            | "stop_monitoring"
    )
    .then(|| value.to_owned())
}

fn safe_native_code(value: Option<&str>) -> Option<String> {
    let value = value?;
    let bytes = value.as_bytes();
    let prefix = bytes
        .windows(2)
        .position(|window| window.eq_ignore_ascii_case(b"0x"))?;
    let digits = bytes.get(prefix + 2..prefix + 10)?;
    if !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    Some(format!(
        "0x{}",
        String::from_utf8(digits.to_ascii_lowercase()).ok()?
    ))
}

fn attributes_with_manufacturer(
    attributes: &Value,
    manufacturer: Option<&str>,
) -> Result<Value, String> {
    let mut result = attributes.clone();
    if let (Some(object), Some(manufacturer)) = (result.as_object_mut(), manufacturer) {
        object.insert(
            "manufacturer".to_owned(),
            Value::String(manufacturer.to_owned()),
        );
    }
    Ok(result)
}

fn archive_live_capabilities_in(connection: &Connection, stale_reason: &str) -> Result<(), String> {
    let scan_session_id: Option<String> = connection
        .query_row(
            "SELECT id FROM scan_sessions WHERE status IN ('COMPLETED', 'COMPLETED_WITH_WARNINGS') ORDER BY completed_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_SCAN_READ_FAILED: {error}"))?;
    let pending = {
        let mut statement = connection
            .prepare(
                "SELECT device_id, metric_key, value_json, unit, source, evidence_level, last_checked_at, reason_code, details_json \
                 FROM capabilities WHERE capability_kind = 'DYNAMIC' AND state = 'AVAILABLE' \
                 AND value_json IS NOT NULL AND value_json <> 'null' AND last_checked_at IS NOT NULL",
            )
            .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_QUERY_PREPARE_FAILED: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
            .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_QUERY_FAILED: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_ROW_FAILED: {error}"))?
    };

    for (
        device_id,
        metric_key,
        value_json,
        unit,
        source,
        evidence_level,
        observed_at,
        reason_code,
        details_json,
    ) in pending
    {
        connection
            .execute(
                "INSERT INTO metric_snapshots(id, device_id, metric_key, value_json, unit, state, observed_at, source, evidence_level, reason_code, details_json, scan_session_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'AVAILABLE', ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    Uuid::new_v4().to_string(),
                    device_id,
                    metric_key,
                    value_json,
                    unit,
                    observed_at,
                    source,
                    evidence_level,
                    reason_code,
                    details_json,
                    scan_session_id,
                ],
            )
            .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_INSERT_FAILED: {error}"))?;
    }
    connection
        .execute(
            "UPDATE capabilities SET state = 'STALE', value_json = NULL, reason_code = ?1 \
             WHERE capability_kind = 'DYNAMIC' AND state = 'AVAILABLE' AND value_json IS NOT NULL",
            [stale_reason],
        )
        .map_err(|error| format!("SQLITE_RUNTIME_CAPABILITIES_STALE_FAILED: {error}"))?;
    connection
        .execute(
            "DELETE FROM metric_snapshots WHERE id IN ( \
                SELECT id FROM ( \
                    SELECT id, ROW_NUMBER() OVER (PARTITION BY device_id, metric_key ORDER BY observed_at DESC, id DESC) AS ordinal \
                    FROM metric_snapshots \
                ) WHERE ordinal > 20 \
             )",
            [],
        )
        .map_err(|error| format!("SQLITE_RUNTIME_HISTORY_RETENTION_FAILED: {error}"))?;
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CapabilityWriteMode {
    Backfill,
    InventoryRefresh,
    Replace,
}

fn persist_capability(
    connection: &Connection,
    capability: &CapabilityRecord,
    mode: CapabilityWriteMode,
) -> Result<(), String> {
    let value_json = capability
        .value
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| format!("SQLITE_CAPABILITY_VALUE_SERIALIZE_FAILED: {error}"))?;
    let details_json = serde_json::to_string(&capability.details)
        .map_err(|error| format!("SQLITE_CAPABILITY_DETAILS_SERIALIZE_FAILED: {error}"))?;
    let statement = if mode == CapabilityWriteMode::Backfill
        || (mode == CapabilityWriteMode::InventoryRefresh
            && capability.kind == CapabilityKind::Dynamic)
    {
        "INSERT OR IGNORE INTO capabilities(device_id, metric_key, state, source, reason_code, last_checked_at, details_json, capability_kind, value_json, unit, evidence_level, native_error_code) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
    } else {
        "INSERT INTO capabilities(device_id, metric_key, state, source, reason_code, last_checked_at, details_json, capability_kind, value_json, unit, evidence_level, native_error_code) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
         ON CONFLICT(device_id, metric_key) DO UPDATE SET state=excluded.state, source=excluded.source, \
         reason_code=excluded.reason_code, last_checked_at=excluded.last_checked_at, details_json=excluded.details_json, \
         capability_kind=excluded.capability_kind, value_json=excluded.value_json, unit=excluded.unit, \
         evidence_level=excluded.evidence_level, native_error_code=excluded.native_error_code"
    };
    connection
        .execute(
            statement,
            params![
                capability.device_id,
                capability.metric_key,
                capability.state.as_str(),
                capability.source,
                capability.reason_code,
                capability.observed_at_utc,
                details_json,
                capability.kind.as_str(),
                value_json,
                capability.unit,
                capability.evidence_level.map(EvidenceLevel::as_str),
                capability.native_error_code,
            ],
        )
        .map_err(|error| format!("SQLITE_CAPABILITY_UPSERT_FAILED: {error}"))?;
    Ok(())
}

fn confidence_as_str(confidence: IdentityConfidence) -> &'static str {
    match confidence {
        IdentityConfidence::Strong => "STRONG",
        IdentityConfidence::Moderate => "MODERATE",
        IdentityConfidence::Weak => "WEAK",
    }
}

fn parse_enum<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, String> {
    serde_json::from_value(Value::String(value.to_owned()))
        .map_err(|error| format!("SQLITE_ENUM_PARSE_FAILED: {error}"))
}

#[cfg(test)]
mod tests {
    use super::Database;

    #[test]
    fn in_memory_database_applies_initial_migration() {
        let database = Database::open_in_memory().unwrap();
        assert_eq!(database.schema_version().unwrap(), 6);
        assert!(database.has_table("devices").unwrap());
        assert!(database.has_table("ai_analyses").unwrap());
        assert!(!database.has_table("oauth_tokens").unwrap());
    }
}
