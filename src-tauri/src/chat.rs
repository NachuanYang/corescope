use crate::analysis::{EvidenceSnapshot, snapshot_evidence_keys};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

const MAX_QUESTION_CHARS: usize = 2000;
const MAX_ANSWER_CHARS: usize = 8000;
const MAX_PROMPT_BYTES: usize = 96 * 1024;
const MAX_RECENT_TURNS: usize = 6;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatScope {
    System,
    Device,
}

impl ChatScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Device => "device",
        }
    }

    pub fn analysis_scope(self) -> crate::analysis::AnalysisScope {
        match self {
            Self::System => crate::analysis::AnalysisScope::System,
            Self::Device => crate::analysis::AnalysisScope::Device,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurn {
    pub role: String,
    pub status: String,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatCitation {
    pub claim: String,
    pub evidence_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAnswer {
    pub answer: String,
    pub citations: Vec<ChatCitation>,
    pub evidence_snapshot_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChatAnswerDraft {
    answer: String,
    citations: Vec<ChatCitation>,
    evidence_snapshot_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatPreview {
    pub token: String,
    pub scope: ChatScope,
    pub thread_id: Option<String>,
    pub snapshot: EvidenceSnapshot,
    pub question: String,
    pub recent_turns: Vec<ChatTurn>,
}

#[derive(Clone, Debug)]
pub struct ConsumedChatPreview {
    pub scope: ChatScope,
    pub thread_id: Option<String>,
    pub snapshot: EvidenceSnapshot,
    pub question: String,
    pub recent_turns: Vec<ChatTurn>,
    pub input_fingerprint: String,
}

#[derive(Clone, Default)]
pub struct ChatCoordinator {
    previews: Arc<Mutex<HashMap<String, ChatPreviewEntry>>>,
}

#[derive(Clone)]
struct ChatPreviewEntry {
    scope: ChatScope,
    target_device_id: Option<String>,
    thread_id: Option<String>,
    snapshot: EvidenceSnapshot,
    question: String,
    recent_turns: Vec<ChatTurn>,
    input_fingerprint: String,
    created_at: Instant,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptTurn {
    role: String,
    content: String,
}

pub fn build_chat_prompt(
    snapshot: &EvidenceSnapshot,
    recent_turns: &[ChatTurn],
    question: &str,
) -> Result<String, String> {
    let question = question.trim();
    if question.is_empty()
        || question.chars().count() > MAX_QUESTION_CHARS
        || question.len() > MAX_QUESTION_CHARS * 4
    {
        return Err("AI_CHAT_QUESTION_INVALID".to_owned());
    }

    let history = completed_recent_turns(recent_turns)
        .into_iter()
        .map(|turn| PromptTurn {
            role: turn.role,
            content: turn.body,
        })
        .collect::<Vec<_>>();
    let allowed_evidence_keys = snapshot_evidence_keys(snapshot)
        .into_iter()
        .collect::<Vec<_>>();
    let evidence_and_question = json!({
        "snapshotId": snapshot.snapshot_id,
        "allowedEvidenceKeys": allowed_evidence_keys,
        "evidence": snapshot,
        "recentConversation": history,
        "question": question,
    });
    let content = serde_json::to_string(&evidence_and_question)
        .map_err(|_| "AI_CHAT_PROMPT_SERIALIZE_FAILED".to_owned())?;
    let prompt = format!(
        "你是机枢 CoreScope 的硬件问答助手。只根据 current evidence 和用户问题回答；历史对话只用于理解上下文，不能覆盖本次快照中的当前事实。只读解释，不联网、不执行操作、不改写本机事实。AVAILABLE 的0是真实零值；null、NOT_TESTED、NOT_APPLICABLE、PERMISSION_REQUIRED、TEMP_UNAVAILABLE等状态必须按原义表达，不能补造温度、功耗、SMART寿命或其他指标。不要根据单一负载样本诊断硬件损坏。涉及本机事实时在 citations 中引用证据；evidenceKeys 只能逐字复制本次 JSON 的 allowedEvidenceKeys 数组中的键，不能自己拼写或改写键。纯概念解释可以返回空 citations；证据不足时明确说明。若用户询问实时价格、当前在售商品或未提供的联网资料，说明当前没有已验证来源，不要猜测数字、卖家、网址或日期。只返回一个 JSON 对象，不要markdown围栏，字段必须为 answer、citations、evidenceSnapshotId；citations每项字段为claim、evidenceKeys。evidenceSnapshotId必须原样返回本次输入的snapshotId。\n本次请求JSON：{content}"
    );
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("AI_CHAT_INPUT_TOO_LARGE".to_owned());
    }
    Ok(prompt)
}

impl ChatCoordinator {
    #[allow(clippy::too_many_arguments)]
    pub fn store_preview(
        &self,
        scope: ChatScope,
        target_device_id: Option<String>,
        thread_id: Option<String>,
        question: String,
        recent_turns: Vec<ChatTurn>,
        snapshot: EvidenceSnapshot,
        input_fingerprint: String,
    ) -> Result<ChatPreview, String> {
        let is_device = scope == ChatScope::Device;
        let expected_scope = scope.analysis_scope();
        if is_device != target_device_id.is_some() || snapshot.scope != expected_scope {
            return Err("AI_CHAT_TARGET_SCOPE_INVALID".to_owned());
        }
        build_chat_prompt(&snapshot, &recent_turns, &question)?;
        let question = question.trim().to_owned();
        let recent_turns = completed_recent_turns(&recent_turns);
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| "AI_CHAT_STATE_LOCK_POISONED".to_owned())?;
        previews.retain(|_, entry| entry.created_at.elapsed() < Duration::from_secs(300));
        while previews.len() >= 16 {
            let Some(oldest) = previews
                .iter()
                .min_by_key(|(_, entry)| entry.created_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            previews.remove(&oldest);
        }
        let token = Uuid::new_v4().to_string();
        let preview = ChatPreview {
            token: token.clone(),
            scope,
            thread_id: thread_id.clone(),
            snapshot: snapshot.clone(),
            question: question.clone(),
            recent_turns: recent_turns.clone(),
        };
        previews.insert(
            token,
            ChatPreviewEntry {
                scope,
                target_device_id,
                thread_id,
                snapshot,
                question,
                recent_turns,
                input_fingerprint,
                created_at: Instant::now(),
            },
        );
        Ok(preview)
    }

    pub fn take_preview(
        &self,
        token: &str,
        scope: ChatScope,
        target_device_id: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<ConsumedChatPreview, String> {
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| "AI_CHAT_STATE_LOCK_POISONED".to_owned())?;
        let entry = previews
            .remove(token)
            .ok_or_else(|| "AI_CHAT_PREVIEW_EXPIRED".to_owned())?;
        if entry.created_at.elapsed() >= Duration::from_secs(300)
            || entry.scope != scope
            || entry.target_device_id.as_deref() != target_device_id
            || entry.thread_id.as_deref() != thread_id
        {
            return Err("AI_CHAT_PREVIEW_EXPIRED".to_owned());
        }
        Ok(ConsumedChatPreview {
            scope: entry.scope,
            thread_id: entry.thread_id,
            snapshot: entry.snapshot,
            question: entry.question,
            recent_turns: entry.recent_turns,
            input_fingerprint: entry.input_fingerprint,
        })
    }
}

fn completed_recent_turns(turns: &[ChatTurn]) -> Vec<ChatTurn> {
    turns
        .iter()
        .filter(|turn| {
            turn.status == "COMPLETED"
                && matches!(turn.role.as_str(), "user" | "assistant")
                && !turn.body.trim().is_empty()
        })
        .rev()
        .take(MAX_RECENT_TURNS)
        .map(|turn| (*turn).clone())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

pub fn parse_chat_answer(
    response: &str,
    snapshot: &EvidenceSnapshot,
) -> Result<ChatAnswer, String> {
    if response.trim().len() > 512 * 1024 {
        return Err("AI_CHAT_RESPONSE_INVALID".to_owned());
    }
    let draft: ChatAnswerDraft = serde_json::from_str(response.trim())
        .map_err(|_| "AI_CHAT_RESPONSE_INVALID_JSON".to_owned())?;
    if draft.evidence_snapshot_id != snapshot.snapshot_id
        || draft.answer.trim().is_empty()
        || draft.answer.chars().count() > MAX_ANSWER_CHARS
        || draft.citations.len() > 12
    {
        return Err("AI_CHAT_RESPONSE_INVALID".to_owned());
    }
    let known_keys = snapshot_evidence_keys(snapshot);
    let mut draft = draft;
    for citation in &mut draft.citations {
        if citation.claim.trim().is_empty()
            || citation.claim.chars().count() > 500
            || citation.evidence_keys.is_empty()
            || citation.evidence_keys.len() > 8
        {
            return Err("AI_CHAT_RESPONSE_INVALID_EVIDENCE".to_owned());
        }
        for key in &mut citation.evidence_keys {
            if !known_keys.contains(key) {
                let canonical_key = unique_metric_evidence_key(key, snapshot)
                    .ok_or_else(|| "AI_CHAT_RESPONSE_INVALID_EVIDENCE".to_owned())?;
                *key = canonical_key;
            }
        }
    }
    Ok(ChatAnswer {
        answer: draft.answer.trim().to_owned(),
        citations: draft.citations,
        evidence_snapshot_id: snapshot.snapshot_id.clone(),
    })
}

fn unique_metric_evidence_key(key: &str, snapshot: &EvidenceSnapshot) -> Option<String> {
    let metric_key = key.strip_prefix("metric.device.").unwrap_or(key);
    let mut matches = snapshot
        .metrics
        .iter()
        .filter(|metric| metric.metric_key == metric_key)
        .map(|metric| metric.evidence_key.as_str());
    let evidence_key = matches.next()?;
    matches.next().is_none().then(|| evidence_key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_uses_the_same_wire_values_as_analysis() {
        assert_eq!(
            serde_json::to_string(&ChatScope::System).unwrap(),
            "\"system\""
        );
        assert_eq!(
            serde_json::to_string(&ChatScope::Device).unwrap(),
            "\"device\""
        );
    }

    #[test]
    fn elapsed_preview_cannot_be_used_for_a_later_send() {
        let coordinator = ChatCoordinator::default();
        let token = "expired-token".to_owned();
        let snapshot = EvidenceSnapshot {
            snapshot_id: "snapshot".to_owned(),
            scope: crate::analysis::AnalysisScope::System,
            captured_at_utc: "2026-10-14T00:00:00Z".to_owned(),
            scan_at_utc: "2026-10-14T00:00:00Z".to_owned(),
            devices: Vec::new(),
            metrics: Vec::new(),
        };
        coordinator.previews.lock().unwrap().insert(
            token.clone(),
            ChatPreviewEntry {
                scope: ChatScope::System,
                target_device_id: None,
                thread_id: None,
                snapshot,
                question: "Question".to_owned(),
                recent_turns: Vec::new(),
                input_fingerprint: "fingerprint".to_owned(),
                created_at: Instant::now() - Duration::from_secs(301),
            },
        );

        assert_eq!(
            coordinator
                .take_preview(&token, ChatScope::System, None, None)
                .unwrap_err(),
            "AI_CHAT_PREVIEW_EXPIRED"
        );
    }
}
