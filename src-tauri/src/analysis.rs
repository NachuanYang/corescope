use crate::{
    capabilities::{CapabilityKind, CapabilityRecord, CapabilityState, EvidenceLevel},
    inventory::{DeviceCategory, DeviceKind, DeviceLayer, DeviceRecord, InventorySnapshot},
    telemetry::{MetricSample, TelemetryFrame},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use uuid::Uuid;

const MAX_LABEL_CHARS: usize = 96;
const MAX_TEXT_CHARS: usize = 1200;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AnalysisScope {
    System,
    Device,
}

impl AnalysisScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Device => "device",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceSnapshot {
    pub snapshot_id: String,
    pub scope: AnalysisScope,
    pub captured_at_utc: String,
    pub scan_at_utc: String,
    pub devices: Vec<EvidenceDevice>,
    pub metrics: Vec<EvidenceMetric>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceDevice {
    pub evidence_key: String,
    pub device_key: String,
    pub category: String,
    pub kind: String,
    pub name: Option<String>,
    pub manufacturer: Option<String>,
    pub virtual_device: Option<bool>,
    pub source: String,
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceMetric {
    pub evidence_key: String,
    pub device_key: String,
    pub metric_key: String,
    pub state: String,
    pub value: Option<Value>,
    pub unit: Option<String>,
    pub source: String,
    pub evidence_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    pub observed_at_utc: Option<String>,
    pub window: Option<MetricWindow>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricWindow {
    pub sample_count: usize,
    pub minimum: Option<f64>,
    pub mean: Option<f64>,
    pub maximum: Option<f64>,
    pub started_at_utc: Option<String>,
    pub ended_at_utc: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAssessment {
    pub scope: AnalysisScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<String>,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub priority_sections: Vec<AnalysisPrioritySection>,
    pub strengths: Vec<String>,
    pub concerns: Vec<AssessmentConcern>,
    pub suggestions: Vec<String>,
    pub limitations: Vec<String>,
    pub evidence_snapshot_id: String,
    pub generated_at_utc: String,
    pub model_slug: String,
    pub provenance: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisSectionKey {
    PerformanceEvaluation,
    HealthAssessment,
    MaintenancePlan,
    UpgradeRecommendation,
    Other,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisPrioritySection {
    pub key: AnalysisSectionKey,
    pub items: Vec<AnalysisPriorityItem>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalysisPriorityItem {
    pub text: String,
    pub evidence_keys: Vec<String>,
    pub certainty: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssessmentConcern {
    pub claim: String,
    pub evidence_keys: Vec<String>,
    pub certainty: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssessmentDraft {
    summary: String,
    priority_sections: Vec<AnalysisPrioritySection>,
    evidence_snapshot_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAnalysis {
    pub id: String,
    pub scope: AnalysisScope,
    pub target_device_id: Option<String>,
    pub input_fingerprint: String,
    pub evidence_snapshot: EvidenceSnapshot,
    pub assessment: Option<AiAssessment>,
    pub model_slug: String,
    pub created_at_utc: String,
    pub status: String,
    pub is_historical: bool,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisTargetState {
    pub consent_granted: bool,
    pub consent_decided: bool,
    pub initial_attempted: bool,
    pub latest_analysis: Option<StoredAnalysis>,
    pub latest_attempt_status: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisPreview {
    pub token: String,
    pub snapshot: EvidenceSnapshot,
    pub device_count: usize,
    pub metric_count: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisRunOutcome {
    pub status: String,
    pub analysis_id: Option<String>,
    pub error_code: Option<String>,
    pub state: AnalysisTargetState,
    pub device_analysis: Option<StoredAnalysis>,
}

#[derive(Clone, Default)]
pub struct AnalysisCoordinator {
    active_jobs: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    previews: Arc<Mutex<HashMap<String, PreviewEntry>>>,
}

#[derive(Clone)]
struct PreviewEntry {
    snapshot: EvidenceSnapshot,
    target_device_id: Option<String>,
    fingerprint: String,
    created_at: Instant,
}

#[derive(Debug)]
pub struct AnalysisJob {
    active_jobs: Arc<Mutex<HashMap<String, watch::Sender<bool>>>>,
    key: String,
    cancel_receiver: watch::Receiver<bool>,
}

impl AnalysisCoordinator {
    pub fn enter_job(&self, key: String) -> Result<AnalysisJob, String> {
        let mut active = self
            .active_jobs
            .lock()
            .map_err(|_| "AI_ANALYSIS_STATE_LOCK_POISONED".to_owned())?;
        if active.contains_key(&key) {
            return Err("AI_ANALYSIS_ALREADY_RUNNING".to_owned());
        }
        let (cancel_sender, cancel_receiver) = watch::channel(false);
        active.insert(key.clone(), cancel_sender);
        Ok(AnalysisJob {
            active_jobs: Arc::clone(&self.active_jobs),
            key,
            cancel_receiver,
        })
    }

    pub fn cancel_active_job(&self) -> Result<bool, String> {
        let active = self
            .active_jobs
            .lock()
            .map_err(|_| "AI_ANALYSIS_STATE_LOCK_POISONED".to_owned())?;
        Ok(active.values().any(|sender| sender.send(true).is_ok()))
    }

    pub fn store_preview(
        &self,
        snapshot: EvidenceSnapshot,
        target_device_id: Option<String>,
        fingerprint: String,
    ) -> Result<AnalysisPreview, String> {
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| "AI_ANALYSIS_STATE_LOCK_POISONED".to_owned())?;
        previews.retain(|_, entry| entry.created_at.elapsed() < Duration::from_secs(300));
        while previews.len() >= 8 {
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
        let device_count = snapshot.devices.len();
        let metric_count = snapshot.metrics.len();
        previews.insert(
            token.clone(),
            PreviewEntry {
                snapshot: snapshot.clone(),
                target_device_id,
                fingerprint,
                created_at: Instant::now(),
            },
        );
        Ok(AnalysisPreview {
            token,
            snapshot,
            device_count,
            metric_count,
        })
    }

    pub fn take_preview(
        &self,
        token: &str,
        target_device_id: Option<&str>,
    ) -> Result<(EvidenceSnapshot, String), String> {
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| "AI_ANALYSIS_STATE_LOCK_POISONED".to_owned())?;
        let entry = previews
            .remove(token)
            .ok_or_else(|| "AI_ANALYSIS_PREVIEW_EXPIRED".to_owned())?;
        if entry.created_at.elapsed() >= Duration::from_secs(300)
            || entry.target_device_id.as_deref() != target_device_id
        {
            return Err("AI_ANALYSIS_PREVIEW_EXPIRED".to_owned());
        }
        Ok((entry.snapshot, entry.fingerprint))
    }
}

impl AnalysisJob {
    pub async fn cancelled(&mut self) {
        if *self.cancel_receiver.borrow() {
            return;
        }
        let _ = self.cancel_receiver.changed().await;
    }
}

impl Drop for AnalysisJob {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active_jobs.lock() {
            active.remove(&self.key);
        }
    }
}

pub fn build_evidence_snapshot(
    inventory: &InventorySnapshot,
    telemetry: &TelemetryFrame,
    scope: AnalysisScope,
    target_device_id: Option<&str>,
    captured_at_utc: String,
) -> Result<EvidenceSnapshot, String> {
    build_evidence_snapshot_with_capabilities(
        inventory,
        telemetry,
        &[],
        scope,
        target_device_id,
        captured_at_utc,
    )
}

pub fn build_evidence_snapshot_with_capabilities(
    inventory: &InventorySnapshot,
    telemetry: &TelemetryFrame,
    capabilities: &[CapabilityRecord],
    scope: AnalysisScope,
    target_device_id: Option<&str>,
    captured_at_utc: String,
) -> Result<EvidenceSnapshot, String> {
    if scope == AnalysisScope::Device && target_device_id.is_none() {
        return Err("AI_ANALYSIS_DEVICE_REQUIRED".to_owned());
    }
    if scope == AnalysisScope::System && target_device_id.is_some() {
        return Err("AI_ANALYSIS_TARGET_SCOPE_INVALID".to_owned());
    }

    let mut eligible = inventory
        .devices
        .iter()
        .filter(|device| {
            device.is_present == Some(true)
                && device.layer != DeviceLayer::PnpNode
                && device.is_virtual != Some(true)
        })
        .filter(|device| is_supported_ai_device(device.kind));
    let selected = if let Some(target) = target_device_id {
        let device = eligible
            .find(|device| device.id == target)
            .ok_or_else(|| "AI_ANALYSIS_DEVICE_NOT_FOUND_OR_UNSUPPORTED".to_owned())?;
        vec![device]
    } else {
        eligible.collect::<Vec<_>>()
    };

    if selected.is_empty() {
        return Err("AI_ANALYSIS_NO_SUPPORTED_DEVICES".to_owned());
    }

    let mut selected_sorted = selected;
    selected_sorted.sort_by(|left, right| {
        left.kind
            .as_str()
            .cmp(right.kind.as_str())
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut device_key_by_local_id = HashMap::<&str, String>::new();
    let mut device_kind_by_local_id = HashMap::<&str, DeviceKind>::new();
    let mut devices = Vec::with_capacity(selected_sorted.len());
    for (index, device) in selected_sorted.iter().enumerate() {
        let device_key = if scope == AnalysisScope::Device {
            "device".to_owned()
        } else {
            format!("hardware-{}", index + 1)
        };
        device_key_by_local_id.insert(device.id.as_str(), device_key.clone());
        device_kind_by_local_id.insert(device.id.as_str(), device.kind);
        let evidence_key = format!("{}.kind", device_key);
        let attributes = safe_attributes(device);
        let name = sanitize_label(&device.display_name);
        let manufacturer = device.manufacturer.as_deref().and_then(sanitize_label);
        devices.push(EvidenceDevice {
            evidence_key,
            device_key,
            category: category_name(device.category),
            kind: device.kind.as_str().to_owned(),
            name,
            manufacturer,
            virtual_device: device.is_virtual,
            source: "Windows inventory".to_owned(),
            attributes,
        });
    }

    let captured_time = DateTime::parse_from_rfc3339(&captured_at_utc)
        .ok()
        .map(|time| time.with_timezone(&Utc));
    let window_start = captured_time
        .as_ref()
        .map(|time| time.to_owned() - ChronoDuration::seconds(30));
    let mut grouped_samples = HashMap::<(String, String), Vec<&MetricSample>>::new();
    for sample in &telemetry.samples {
        let device_key = match sample.device_id.as_deref() {
            Some(id) => {
                let (Some(device_key), Some(kind)) = (
                    device_key_by_local_id.get(id),
                    device_kind_by_local_id.get(id),
                ) else {
                    continue;
                };
                if !metric_applies_to_device(&sample.metric_key, *kind) {
                    continue;
                }
                device_key.clone()
            }
            None if is_system_metric(&sample.metric_key) => "system".to_owned(),
            None => continue,
        };
        if !is_allowed_metric(&sample.metric_key) {
            continue;
        }
        if metric_source(&sample.metric_key).is_none() {
            continue;
        }
        grouped_samples
            .entry((device_key, sample.metric_key.clone()))
            .or_default()
            .push(sample);
    }
    let mut metrics_by_key = grouped_samples
        .into_iter()
        .filter_map(|((device_key, metric_key), samples)| {
            let latest = samples.last().copied()?;
            let source = metric_source(&metric_key)?;
            let evidence_source = safe_evidence_source(&latest.source, &metric_key)
                .unwrap_or_else(|| source.to_owned());
            let value = if latest.state == CapabilityState::Available {
                latest
                    .value
                    .as_ref()
                    .filter(|value| is_finite_number(value))
                    .cloned()
            } else {
                None
            };
            Some(EvidenceMetric {
                evidence_key: format!("metric.{}.{}", device_key, metric_key),
                device_key,
                metric_key,
                state: capability_state_name(latest.state).to_owned(),
                value,
                unit: latest.unit.as_deref().and_then(safe_unit),
                source: evidence_source,
                evidence_level: latest.evidence_level.map(evidence_level_name),
                reason_code: latest.reason_code.as_deref().and_then(safe_reason_code),
                observed_at_utc: latest.observed_at_utc.clone(),
                window: metric_window(&samples, window_start),
            })
        })
        .map(|metric| {
            (
                (metric.device_key.clone(), metric.metric_key.clone()),
                metric,
            )
        })
        .collect::<HashMap<_, _>>();

    for capability in capabilities {
        if capability.kind != CapabilityKind::Dynamic || !is_allowed_metric(&capability.metric_key)
        {
            continue;
        }
        let Some(device_key) = device_key_by_local_id.get(capability.device_id.as_str()) else {
            continue;
        };
        let Some(kind) = device_kind_by_local_id.get(capability.device_id.as_str()) else {
            continue;
        };
        if !metric_applies_to_device(&capability.metric_key, *kind) {
            continue;
        }
        let key = (device_key.clone(), capability.metric_key.clone());
        if metrics_by_key.contains_key(&key) {
            continue;
        }

        let (state, value, reason_code) =
            capability_evidence(capability, &telemetry.monitoring, captured_time.as_ref());
        metrics_by_key.insert(
            key,
            EvidenceMetric {
                evidence_key: format!("metric.{}.{}", device_key, capability.metric_key),
                device_key: device_key.clone(),
                metric_key: capability.metric_key.clone(),
                state: capability_state_name(state).to_owned(),
                value,
                unit: capability.unit.as_deref().and_then(safe_unit),
                source: safe_evidence_source(&capability.source, &capability.metric_key)
                    .unwrap_or_else(|| {
                        metric_source(&capability.metric_key)
                            .unwrap_or("Hardware capability source")
                            .to_owned()
                    }),
                evidence_level: capability.evidence_level.map(evidence_level_name),
                reason_code,
                observed_at_utc: capability.observed_at_utc.clone(),
                window: None,
            },
        );
    }

    let mut metrics = metrics_by_key.into_values().collect::<Vec<_>>();
    metrics.sort_by(|left, right| left.evidence_key.cmp(&right.evidence_key));

    Ok(EvidenceSnapshot {
        snapshot_id: Uuid::new_v4().to_string(),
        scope,
        captured_at_utc,
        scan_at_utc: inventory.scanned_at_utc.clone(),
        devices,
        metrics,
    })
}

pub fn build_analysis_prompt(snapshot: &EvidenceSnapshot) -> Result<String, String> {
    let evidence = serde_json::to_string(snapshot)
        .map_err(|_| "AI_ANALYSIS_SNAPSHOT_SERIALIZE_FAILED".to_owned())?;
    let evidence_guidance = "对未采集指标，应结合 state 与 reasonCode 说明具体限制（如未启动采样、权限受限、驱动不支持、接口暂不可用）；未测试与不适用含义不同。优先引用本次快照里实际存在的设备型号、规格、驱动信息、读数、来源与时间，不要用泛化套话替代这些事实。一个瞬时负载只能说明采样时状态，不能代表长期性能；没有基准与用户工作负载时，明确说明无法做横向性能评级。健康判断仅依据已读取的健康指标和有根据的阈值；未读取温度、SMART 或功耗时不得称设备正常/故障，也不要反复重复同一限制。维护建议应与已观察到的问题相关；没有具体问题时给出低风险的观察建议。";
    let instructions = concat!(
        "你是机枢 CoreScope 的硬件解释助手。只根据本机证据审慎解读，不联网、不执行操作、不改写事实。缺失值和非 AVAILABLE 状态绝不推断为 0；区分 OS_REPORTED 与实测来源；没有长期窗口、基准测试或用户工作负载时，不把单次读数说成长期性能或设备健康结论。所有判断必须绑定本次快照中存在的 evidenceKey；证据不足时不要包装成结论。\n",
        "按固定优先级输出五个部分，顺序和 key 不得改变：1. 设备性能客观评价（performance_evaluation）：仅描述可证实的规格、样本和窗口，不编造基准排名；2. 设备状态正常评估（health_assessment）：只评价实际检查到的指标，不把未测试、权限不足、不适用或暂不可用当作正常或故障；3. 设备维护方案（maintenance_plan）：只给低风险、非破坏性的观察或常规维护建议，不要求管理员权限、不提供修改设备状态的命令或压力测试；4. 设备更新换代建议（upgrade_recommendation）：仅在现有证据与明确瓶颈支持时给出条件式建议；5. 其他内容（other）：仅列出前四部分未覆盖且对用户有用的重要事实。\n",
        "每部分 items 可以为空数组，最多三条；没有可支持的结论时留空，不用“证据不足”、通用观察建议或“无额外结论”填充，也不要在多个部分重复同一项采集限制。若有关键数据不可用，在 summary 中用一句话集中说明最重要的限制及原因，其余限制留在证据状态中。每项包含 text、evidenceKeys、certainty；evidenceKeys 只能引用输入中存在的 evidenceKey，没有直接证据的判断用空数组且 certainty 为 unknown。summary 用简洁中文写 1 至 3 句，优先给出本次证据支持的实质结论。\n",
        "只返回一个 JSON 对象，不要 markdown 或额外说明，字段为 summary、prioritySections、evidenceSnapshotId。prioritySections 每项字段为 key、items；items 每项字段为 text、evidenceKeys、certainty；key 必须依次为 performance_evaluation、health_assessment、maintenance_plan、upgrade_recommendation、other；certainty 只能是 observed、inferred、unknown。evidenceSnapshotId 必须原样返回输入 snapshotId。"
    );
    let prompt = format!("{evidence_guidance}\n{instructions}\n本次证据：{evidence}");
    if prompt.len() > 128 * 1024 {
        return Err("AI_ANALYSIS_INPUT_TOO_LARGE".to_owned());
    }
    Ok(prompt)
}

pub fn parse_assessment(
    response: &str,
    snapshot: &EvidenceSnapshot,
    target_device_id: Option<String>,
    model_slug: &str,
    generated_at_utc: String,
) -> Result<AiAssessment, String> {
    if response.trim().len() > 512 * 1024 || model_slug.is_empty() || model_slug.len() > 160 {
        return Err("AI_ANALYSIS_RESPONSE_INVALID".to_owned());
    }
    let draft: AssessmentDraft = serde_json::from_str(response.trim())
        .map_err(|_| "AI_ANALYSIS_RESPONSE_INVALID_JSON".to_owned())?;
    let expected_sections = [
        AnalysisSectionKey::PerformanceEvaluation,
        AnalysisSectionKey::HealthAssessment,
        AnalysisSectionKey::MaintenancePlan,
        AnalysisSectionKey::UpgradeRecommendation,
        AnalysisSectionKey::Other,
    ];
    if draft.evidence_snapshot_id != snapshot.snapshot_id
        || draft.summary.trim().is_empty()
        || draft.summary.chars().count() > MAX_TEXT_CHARS
        || draft.priority_sections.len() != expected_sections.len()
        || draft
            .priority_sections
            .iter()
            .enumerate()
            .any(|(index, section)| section.key != expected_sections[index])
        || draft.priority_sections.iter().any(|section| {
            section.items.len() > 3
                || section.items.iter().any(|item| {
                    item.text.trim().is_empty()
                        || item.text.chars().count() > 500
                        || item.evidence_keys.len() > 8
                        || !matches!(item.certainty.as_str(), "observed" | "inferred" | "unknown")
                        || (item.evidence_keys.is_empty() && item.certainty != "unknown")
                })
        })
    {
        return Err("AI_ANALYSIS_RESPONSE_INVALID".to_owned());
    }
    let known_evidence = snapshot_evidence_keys(snapshot);
    if draft
        .priority_sections
        .iter()
        .flat_map(|section| &section.items)
        .any(|item| {
            item.evidence_keys
                .iter()
                .any(|key| !known_evidence.contains(key))
        })
    {
        return Err("AI_ANALYSIS_RESPONSE_INVALID_EVIDENCE".to_owned());
    }
    Ok(AiAssessment {
        scope: snapshot.scope,
        target_device_id,
        summary: draft.summary.trim().to_owned(),
        priority_sections: draft
            .priority_sections
            .into_iter()
            .map(|mut section| {
                section.items = section
                    .items
                    .into_iter()
                    .map(|mut item| {
                        item.text = item.text.trim().to_owned();
                        item
                    })
                    .collect();
                section
            })
            .collect(),
        strengths: Vec::new(),
        concerns: Vec::new(),
        suggestions: Vec::new(),
        limitations: Vec::new(),
        evidence_snapshot_id: snapshot.snapshot_id.clone(),
        generated_at_utc,
        model_slug: model_slug.to_owned(),
        provenance: "AI_INTERPRETATION".to_owned(),
    })
}

pub fn input_fingerprint(
    snapshot: &EvidenceSnapshot,
    target_device_id: Option<&str>,
) -> Result<String, String> {
    let devices = snapshot
        .devices
        .iter()
        .map(|device| {
            json!({
                "deviceKey": device.device_key,
                "category": device.category,
                "kind": device.kind,
                "name": device.name,
                "manufacturer": device.manufacturer,
                "virtualDevice": device.virtual_device,
                "source": device.source,
                "attributes": device.attributes,
            })
        })
        .collect::<Vec<_>>();
    // Historical status tracks inventory scans. Live measurements change every
    // second, but remain bound to the immutable evidence snapshot stored below.
    let stable = json!({
        "scope": snapshot.scope,
        "targetDeviceId": target_device_id,
        "scanAtUtc": snapshot.scan_at_utc,
        "devices": devices,
    });
    let bytes = serde_json::to_vec(&stable)
        .map_err(|_| "AI_ANALYSIS_FINGERPRINT_SERIALIZE_FAILED".to_owned())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub fn snapshot_is_historical(
    saved: &EvidenceSnapshot,
    current: &EvidenceSnapshot,
    target_device_id: Option<&str>,
) -> bool {
    input_fingerprint(saved, target_device_id).ok()
        != input_fingerprint(current, target_device_id).ok()
        || saved.scope != current.scope
}

pub fn assessment_json(assessment: &AiAssessment) -> Result<String, String> {
    serde_json::to_string(assessment).map_err(|_| "AI_ANALYSIS_RESULT_SERIALIZE_FAILED".to_owned())
}

pub fn error_payload(error_code: &str) -> String {
    let safe_code = if !error_code.is_empty()
        && error_code.len() <= 120
        && error_code.bytes().all(|byte| {
            byte.is_ascii_uppercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b'-' | b':')
        }) {
        error_code
    } else {
        "AI_ANALYSIS_FAILED"
    };
    json!({ "errorCode": safe_code }).to_string()
}

fn is_supported_ai_device(kind: DeviceKind) -> bool {
    matches!(
        kind,
        DeviceKind::Cpu
            | DeviceKind::GpuAdapter
            | DeviceKind::MemoryModule
            | DeviceKind::PhysicalDisk
            | DeviceKind::NetworkAdapter
            | DeviceKind::Battery
    )
}

fn is_allowed_metric(key: &str) -> bool {
    matches!(
        key,
        "cpu.utilization.percent"
            | "system.cpu.utilization.percent"
            | "memory.used.bytes"
            | "memory.total.bytes"
            | "gpu.utilization.percent"
            | "gpu.memory_controller.utilization.percent"
            | "gpu.memory.total.bytes"
            | "gpu.memory.used.bytes"
            | "gpu.temperature.celsius"
            | "gpu.fan.percent"
            | "gpu.power.watts"
            | "memory.module.health.percent"
            | "disk.read.bytes_per_second"
            | "disk.write.bytes_per_second"
            | "disk.temperature.celsius"
            | "disk.temperature.max.celsius"
            | "disk.wear.provider_value"
            | "disk.power_on_hours"
            | "disk.start_stop_cycle_count"
            | "disk.start_stop_count"
            | "disk.load_unload_cycle_count"
            | "disk.read_errors.total"
            | "disk.read_errors.uncorrected"
            | "disk.write_errors.total"
            | "disk.write_errors.uncorrected"
            | "network.receive.bytes_per_second"
            | "network.send.bytes_per_second"
            | "network.bytes_received.total"
            | "network.bytes_sent.total"
            | "disk.spindle_speed.rpm"
    )
}

fn is_system_metric(key: &str) -> bool {
    matches!(
        key,
        "memory.used.bytes" | "memory.total.bytes" | "system.cpu.utilization.percent"
    )
}

fn metric_applies_to_device(key: &str, kind: DeviceKind) -> bool {
    if key.starts_with("cpu.") {
        kind == DeviceKind::Cpu
    } else if key.starts_with("gpu.") {
        kind == DeviceKind::GpuAdapter
    } else if key.starts_with("memory.") {
        kind == DeviceKind::MemoryModule
    } else if key.starts_with("disk.") {
        kind == DeviceKind::PhysicalDisk
    } else if key.starts_with("network.") {
        kind == DeviceKind::NetworkAdapter
    } else {
        false
    }
}

fn metric_source(key: &str) -> Option<&'static str> {
    if key.starts_with("gpu.") {
        Some("NVML")
    } else if key.starts_with("cpu.") || key.starts_with("system.cpu.") {
        Some("Windows system time counters")
    } else if key.starts_with("memory.") {
        Some("Windows memory status")
    } else if key.starts_with("disk.") {
        Some("Windows disk performance counters")
    } else if key.starts_with("network.") {
        Some("Windows IP Helper")
    } else {
        None
    }
}

fn safe_evidence_source(source: &str, metric_key: &str) -> Option<String> {
    let label = match source {
        "NVML" => "NVML",
        "PDH" => "Windows performance counters",
        "Windows memory status" | "Windows Memory Provider" => "Windows memory status",
        "Windows system time counters" | "Windows Processor Provider" => {
            "Windows system time counters"
        }
        "Windows IP Helper" => "Windows IP Helper",
        "StorageReliability" | "MSFT_StorageReliabilityCounter" => {
            "Windows Storage reliability counters"
        }
        "CoreScope.CapabilityRegistry" => "CoreScope capability registry",
        "Windows Storage" => "Windows storage inventory",
        _ => return metric_source(metric_key).map(str::to_owned),
    };
    Some(label.to_owned())
}

fn safe_reason_code(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 120
        && value.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"_.:-".contains(&byte)
        }))
    .then(|| value.to_owned())
}

fn capability_evidence(
    capability: &CapabilityRecord,
    monitoring: &crate::telemetry::MonitoringStatus,
    captured_at: Option<&DateTime<Utc>>,
) -> (CapabilityState, Option<Value>, Option<String>) {
    if capability.state != CapabilityState::Available {
        return (
            capability.state,
            None,
            capability.reason_code.as_deref().and_then(safe_reason_code),
        );
    }

    let observed_at = capability
        .observed_at_utc
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    let too_old = match (captured_at, observed_at.as_ref()) {
        (Some(captured), Some(observed)) => {
            captured
                .to_owned()
                .signed_duration_since(observed.to_owned())
                > ChronoDuration::seconds(30)
                || observed.to_owned() > captured.to_owned() + ChronoDuration::seconds(5)
        }
        _ => true,
    };
    let is_streaming_metric = matches!(
        capability.metric_key.as_str(),
        "cpu.utilization.percent"
            | "gpu.utilization.percent"
            | "gpu.memory_controller.utilization.percent"
            | "gpu.memory.total.bytes"
            | "gpu.memory.used.bytes"
            | "gpu.temperature.celsius"
            | "gpu.fan.percent"
            | "gpu.power.watts"
            | "disk.read.bytes_per_second"
            | "disk.write.bytes_per_second"
            | "network.receive.bytes_per_second"
            | "network.send.bytes_per_second"
    );
    let monitoring_paused = is_streaming_metric && (!monitoring.running || !monitoring.foreground);
    if monitoring_paused || too_old {
        return (
            CapabilityState::Stale,
            None,
            Some(if monitoring_paused {
                "MONITORING_PAUSED".to_owned()
            } else {
                "SAMPLE_OLDER_THAN_FRESHNESS_WINDOW".to_owned()
            }),
        );
    }

    let value = capability
        .value
        .as_ref()
        .filter(|value| is_finite_number(value))
        .cloned();
    if value.is_none() {
        return (
            CapabilityState::Unknown,
            None,
            Some("AVAILABLE_VALUE_NOT_NUMERIC".to_owned()),
        );
    }
    (
        CapabilityState::Available,
        value,
        capability.reason_code.as_deref().and_then(safe_reason_code),
    )
}

fn metric_window(
    samples: &[&MetricSample],
    window_start: Option<DateTime<Utc>>,
) -> Option<MetricWindow> {
    let usable = samples
        .iter()
        .filter(|sample| sample.state == CapabilityState::Available)
        .filter_map(|sample| {
            let observed = sample
                .observed_at_utc
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc));
            if window_start.is_some_and(|start| observed.is_none_or(|time| time < start)) {
                return None;
            }
            let value = sample
                .value
                .as_ref()
                .and_then(Value::as_f64)
                .filter(|value| value.is_finite())?;
            Some((observed, value))
        })
        .collect::<Vec<_>>();
    if usable.is_empty() {
        return None;
    }
    let minimum = usable
        .iter()
        .map(|(_, value)| *value)
        .fold(f64::INFINITY, f64::min);
    let maximum = usable
        .iter()
        .map(|(_, value)| *value)
        .fold(f64::NEG_INFINITY, f64::max);
    let mean = usable.iter().map(|(_, value)| *value).sum::<f64>() / usable.len() as f64;
    let started_at_utc = usable
        .iter()
        .filter_map(|(time, _)| *time)
        .min()
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let ended_at_utc = usable
        .iter()
        .filter_map(|(time, _)| *time)
        .max()
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    Some(MetricWindow {
        sample_count: usable.len(),
        minimum: Some(minimum),
        mean: Some(mean),
        maximum: Some(maximum),
        started_at_utc,
        ended_at_utc,
    })
}

fn safe_attributes(device: &DeviceRecord) -> BTreeMap<String, Value> {
    let allowed: &[(&str, &str)] = match device.kind {
        DeviceKind::Cpu => &[
            ("physicalCores", "physical_cores"),
            ("logicalProcessors", "logical_processors"),
            ("maxClockMhz", "max_clock_mhz"),
        ],
        DeviceKind::GpuAdapter => &[
            ("driverVersion", "driver_version"),
            ("driverDate", "driver_date"),
            ("videoProcessor", "video_processor"),
        ],
        DeviceKind::MemoryModule => &[
            ("capacityBytes", "capacity_bytes"),
            ("speedMhz", "speed_mhz"),
            ("configuredClockMhz", "configured_clock_mhz"),
        ],
        DeviceKind::PhysicalDisk => &[
            ("sizeBytes", "size_bytes"),
            ("interfaceType", "interface_type"),
            ("mediaType", "media_type"),
            ("reportedMediaType", "reported_media_type"),
        ],
        DeviceKind::NetworkAdapter => &[
            ("speedBps", "speed_bps"),
            ("physicalAdapter", "physical_adapter"),
        ],
        DeviceKind::Battery => &[("designCapacityMwh", "design_capacity_mwh")],
        _ => &[],
    };
    allowed
        .iter()
        .filter_map(|(source_key, public_key)| {
            let value = device.attributes.get(*source_key)?;
            let safe_value = if value.is_number() && is_finite_number(value) {
                Some(value.clone())
            } else if value.is_boolean() {
                Some(value.clone())
            } else {
                value
                    .as_str()
                    .and_then(sanitize_attribute_text)
                    .map(Value::String)
            }?;
            Some(((*public_key).to_owned(), safe_value))
        })
        .collect()
}

fn sanitize_attribute_text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_LABEL_CHARS
        || trimmed.chars().any(char::is_control)
        || trimmed.contains(['@', '\\'])
        || looks_like_ipv4(trimmed)
        || looks_like_mac(trimmed)
        || trimmed
            .split_whitespace()
            .any(|part| part.chars().count() > 32)
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn sanitize_label(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_LABEL_CHARS
        || trimmed.chars().any(char::is_control)
        || trimmed.contains(['@', '/', '\\', ':'])
        || looks_like_ipv4(trimmed)
        || looks_like_mac(trimmed)
        || trimmed
            .split_whitespace()
            .any(|part| part.chars().count() > 32)
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn looks_like_ipv4(value: &str) -> bool {
    let pieces = value.split('.').collect::<Vec<_>>();
    pieces.len() == 4
        && pieces
            .iter()
            .all(|piece| !piece.is_empty() && piece.parse::<u8>().is_ok())
}

fn looks_like_mac(value: &str) -> bool {
    let pieces = value.split([':', '-']).collect::<Vec<_>>();
    pieces.len() == 6
        && pieces
            .iter()
            .all(|piece| piece.len() == 2 && piece.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn category_name(category: DeviceCategory) -> String {
    category.as_str().to_owned()
}

fn capability_state_name(state: CapabilityState) -> &'static str {
    state.as_str()
}

fn evidence_level_name(level: EvidenceLevel) -> String {
    level.as_str().to_owned()
}

fn safe_unit(unit: &str) -> Option<String> {
    matches!(
        unit,
        "percent" | "byte" | "byte/s" | "°C" | "W" | "rpm" | "hour" | "count"
    )
    .then(|| unit.to_owned())
}

fn is_finite_number(value: &Value) -> bool {
    value.as_i64().is_some()
        || value.as_u64().is_some()
        || value.as_f64().is_some_and(f64::is_finite)
}

pub(crate) fn snapshot_evidence_keys(snapshot: &EvidenceSnapshot) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for device in &snapshot.devices {
        keys.insert(device.evidence_key.clone());
        if device.name.is_some() {
            keys.insert(format!("{}.name", device.device_key));
        }
        if device.manufacturer.is_some() {
            keys.insert(format!("{}.manufacturer", device.device_key));
        }
        for key in device.attributes.keys() {
            keys.insert(format!("{}.attributes.{}", device.device_key, key));
        }
    }
    keys.extend(
        snapshot
            .metrics
            .iter()
            .map(|metric| metric.evidence_key.clone()),
    );
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        inventory::{
            IdentityConfidence, InventorySourceWarning, RawCpu, RawInventory, RawMemoryModule,
            normalize_inventory,
        },
        telemetry::MonitoringStatus,
    };
    use serde_json::json;

    fn device(
        id: &str,
        kind: DeviceKind,
        layer: DeviceLayer,
        name: &str,
        attributes: Value,
    ) -> DeviceRecord {
        DeviceRecord {
            id: id.to_owned(),
            identity_hash: format!("private-{id}"),
            identity_confidence: IdentityConfidence::Strong,
            category: match kind {
                DeviceKind::Cpu => DeviceCategory::Cpu,
                DeviceKind::GpuAdapter => DeviceCategory::Gpu,
                DeviceKind::MemoryModule => DeviceCategory::Memory,
                DeviceKind::PhysicalDisk => DeviceCategory::Storage,
                DeviceKind::NetworkAdapter => DeviceCategory::Network,
                DeviceKind::Battery => DeviceCategory::Battery,
                _ => DeviceCategory::Other,
            },
            kind,
            layer,
            display_name: name.to_owned(),
            manufacturer: Some("Example Manufacturer".to_owned()),
            is_virtual: Some(false),
            is_present: Some(true),
            source: "WMI".to_owned(),
            attributes,
        }
    }

    fn fixture() -> (InventorySnapshot, TelemetryFrame) {
        let inventory = InventorySnapshot {
            scanned_at_utc: "2026-10-14T12:00:00Z".to_owned(),
            devices: vec![
                device(
                    "local-cpu-id",
                    DeviceKind::Cpu,
                    DeviceLayer::PhysicalDevice,
                    "Intel Core i5",
                    json!({
                        "physicalCores": 10,
                        "logicalProcessors": 16,
                        "serial_number": "DO-NOT-SEND-123456",
                        "socket_designation": "U3E1"
                    }),
                ),
                device(
                    "local-gpu-id",
                    DeviceKind::GpuAdapter,
                    DeviceLayer::PhysicalDevice,
                    "NVIDIA RTX 4060",
                    json!({}),
                ),
                device(
                    "local-pnp-id",
                    DeviceKind::PnpNode,
                    DeviceLayer::PnpNode,
                    "Ethernet 192.168.1.9",
                    json!({}),
                ),
                device(
                    "local-volume-id",
                    DeviceKind::Volume,
                    DeviceLayer::LogicalDevice,
                    "Personal Files",
                    json!({}),
                ),
            ],
            edges: Vec::new(),
            warnings: Vec::<InventorySourceWarning>::new(),
        };
        let telemetry = TelemetryFrame {
            monitoring: MonitoringStatus::default(),
            samples: vec![
                MetricSample {
                    device_id: Some("local-cpu-id".to_owned()),
                    metric_key: "cpu.utilization.percent".to_owned(),
                    state: CapabilityState::Available,
                    value: Some(json!(0.0)),
                    unit: Some("percent".to_owned()),
                    source: "Windows".to_owned(),
                    evidence_level: Some(EvidenceLevel::Measured),
                    observed_at_utc: Some("2026-10-14T12:00:59Z".to_owned()),
                    reason_code: None,
                    native_error_code: None,
                    details: json!({ "native": "not forwarded" }),
                },
                MetricSample {
                    device_id: Some("local-gpu-id".to_owned()),
                    metric_key: "gpu.power.watts".to_owned(),
                    state: CapabilityState::NotApplicable,
                    value: None,
                    unit: Some("W".to_owned()),
                    source: "Windows".to_owned(),
                    evidence_level: Some(EvidenceLevel::SourceReported),
                    observed_at_utc: Some("2026-10-14T12:00:59Z".to_owned()),
                    reason_code: Some("N_A".to_owned()),
                    native_error_code: Some("PRIVATE_NATIVE_CODE".to_owned()),
                    details: json!({ "private": true }),
                },
            ],
        };
        (inventory, telemetry)
    }

    #[test]
    fn evidence_is_allowlisted_and_excludes_private_identity_and_pnp_data() {
        let (inventory, telemetry) = fixture();
        let snapshot = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::System,
            None,
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        let payload = serde_json::to_string(&snapshot).unwrap();
        for forbidden in [
            "local-cpu-id",
            "private-local",
            "DO-NOT-SEND",
            "serial_number",
            "U3E1",
            "Personal Files",
            "192.168.1.9",
            "native",
            "PRIVATE_NATIVE_CODE",
        ] {
            assert!(!payload.contains(forbidden), "payload leaked {forbidden}");
        }
        assert_eq!(snapshot.devices.len(), 2);
        assert_eq!(
            snapshot.devices[0].attributes.get("physical_cores"),
            Some(&json!(10))
        );
        assert_eq!(snapshot.metrics[0].value, Some(json!(0.0)));
        assert_eq!(snapshot.metrics[1].state, "NOT_APPLICABLE");
        assert_eq!(snapshot.metrics[1].value, None);
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap()["metrics"][1]["reasonCode"],
            json!("N_A")
        );
        assert!(
            build_analysis_prompt(&snapshot)
                .unwrap()
                .contains("\"reasonCode\":\"N_A\"")
        );
        let mut reason_changed = snapshot.clone();
        reason_changed.metrics[1].reason_code = Some("PROBE_PENDING".to_owned());
        assert_eq!(
            input_fingerprint(&snapshot, None).unwrap(),
            input_fingerprint(&reason_changed, None).unwrap()
        );
    }

    #[test]
    fn evidence_preserves_safe_specs_from_normalized_inventory_attributes() {
        let inventory = normalize_inventory(
            &RawInventory {
                cpus: vec![RawCpu {
                    name: "Example CPU".to_owned(),
                    physical_cores: Some(8),
                    logical_processors: Some(16),
                    max_clock_mhz: Some(5200),
                    ..RawCpu::default()
                }],
                memory_modules: vec![RawMemoryModule {
                    name: "Example DIMM".to_owned(),
                    capacity_bytes: Some(16 * 1024 * 1024 * 1024),
                    speed_mhz: Some(3200),
                    configured_clock_mhz: Some(3000),
                    part_number: Some("EXAMPLE-PART-123".to_owned()),
                    slot_locator: Some("DIMM 0".to_owned()),
                    identity_material: Some("private-memory-id".to_owned()),
                    ..RawMemoryModule::default()
                }],
                ..RawInventory::default()
            },
            b"test-install-salt",
        );
        let telemetry = TelemetryFrame {
            monitoring: MonitoringStatus::default(),
            samples: Vec::new(),
        };

        let snapshot = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::System,
            None,
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        let cpu = snapshot
            .devices
            .iter()
            .find(|device| device.kind == "cpu")
            .unwrap();
        let memory = snapshot
            .devices
            .iter()
            .find(|device| device.kind == "memory_module")
            .unwrap();

        assert_eq!(cpu.attributes.get("physical_cores"), Some(&json!(8)));
        assert_eq!(cpu.attributes.get("logical_processors"), Some(&json!(16)));
        assert_eq!(cpu.attributes.get("max_clock_mhz"), Some(&json!(5200)));
        assert_eq!(
            memory.attributes.get("capacity_bytes"),
            Some(&json!(16_u64 * 1024 * 1024 * 1024))
        );
        assert_eq!(memory.attributes.get("speed_mhz"), Some(&json!(3200)));
        assert_eq!(
            memory.attributes.get("configured_clock_mhz"),
            Some(&json!(3000))
        );

        let payload = serde_json::to_string(&snapshot).unwrap();
        assert!(!payload.contains("EXAMPLE-PART-123"));
        assert!(!payload.contains("DIMM 0"));
        assert!(!payload.contains("private-memory-id"));
    }

    #[test]
    fn assessment_requires_matching_snapshot_and_known_evidence_keys() {
        let (inventory, telemetry) = fixture();
        let snapshot = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::System,
            None,
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        let good = json!({
            "summary": "处理器信息来自本机系统报告。当前负载采样为 0%。",
            "prioritySections": [
                { "key": "performance_evaluation", "items": [{ "text": "当前处理器负载采样为0%。", "evidenceKeys": ["metric.hardware-1.cpu.utilization.percent"], "certainty": "observed" }] },
                { "key": "health_assessment", "items": [{ "text": "仅凭单个时点不能判定设备长期状态正常。", "evidenceKeys": ["metric.hardware-1.cpu.utilization.percent"], "certainty": "inferred" }] },
                { "key": "maintenance_plan", "items": [{ "text": "可在日常使用中继续观察负载变化。", "evidenceKeys": ["metric.hardware-1.cpu.utilization.percent"], "certainty": "inferred" }] },
                { "key": "upgrade_recommendation", "items": [{ "text": "缺少常用任务与长期负载，暂不能判断是否需要更换。", "evidenceKeys": [], "certainty": "unknown" }] },
                { "key": "other", "items": [{ "text": "没有其他可由本次证据支持的结论。", "evidenceKeys": [], "certainty": "unknown" }] }
            ],
            "evidenceSnapshotId": snapshot.snapshot_id
        }).to_string();
        let assessment = parse_assessment(
            &good,
            &snapshot,
            None,
            "model-a",
            "2026-10-14T12:02:00Z".to_owned(),
        )
        .unwrap();
        assert_eq!(assessment.provenance, "AI_INTERPRETATION");
        assert_eq!(assessment.evidence_snapshot_id, snapshot.snapshot_id);
        assert_eq!(assessment.priority_sections.len(), 5);
        assert_eq!(
            assessment
                .priority_sections
                .iter()
                .map(|section| section.key)
                .collect::<Vec<_>>(),
            vec![
                AnalysisSectionKey::PerformanceEvaluation,
                AnalysisSectionKey::HealthAssessment,
                AnalysisSectionKey::MaintenancePlan,
                AnalysisSectionKey::UpgradeRecommendation,
                AnalysisSectionKey::Other,
            ]
        );

        let mut wrong_order: Value = serde_json::from_str(&good).unwrap();
        wrong_order["prioritySections"]
            .as_array_mut()
            .unwrap()
            .swap(0, 1);
        assert_eq!(
            parse_assessment(
                &wrong_order.to_string(),
                &snapshot,
                None,
                "model-a",
                "now".to_owned()
            )
            .unwrap_err(),
            "AI_ANALYSIS_RESPONSE_INVALID"
        );

        let mut bad_key: Value = serde_json::from_str(&good).unwrap();
        bad_key["prioritySections"][0]["items"][0]["evidenceKeys"] = json!(["private.local.id"]);
        assert_eq!(
            parse_assessment(
                &bad_key.to_string(),
                &snapshot,
                None,
                "model-a",
                "now".to_owned()
            )
            .unwrap_err(),
            "AI_ANALYSIS_RESPONSE_INVALID_EVIDENCE"
        );

        let mut unexpected_field: Value = serde_json::from_str(&good).unwrap();
        unexpected_field["prioritySections"][0]["items"][0]["unsupported"] = json!("ignored");
        assert_eq!(
            parse_assessment(
                &unexpected_field.to_string(),
                &snapshot,
                None,
                "model-a",
                "now".to_owned()
            )
            .unwrap_err(),
            "AI_ANALYSIS_RESPONSE_INVALID_JSON"
        );

        let wrong_snapshot = good.replace(&snapshot.snapshot_id, "different");
        assert_eq!(
            parse_assessment(
                &wrong_snapshot,
                &snapshot,
                None,
                "model-a",
                "now".to_owned()
            )
            .unwrap_err(),
            "AI_ANALYSIS_RESPONSE_INVALID"
        );
    }

    #[test]
    fn assessment_allows_empty_sections_when_the_snapshot_has_no_supporting_evidence() {
        let (inventory, telemetry) = fixture();
        let snapshot = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::System,
            None,
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        let response = json!({
            "summary": "本次快照只确认了设备型号，没有可用于健康或升级判断的读数。",
            "prioritySections": [
                { "key": "performance_evaluation", "items": [{
                    "text": "清单识别到 Intel Core i5 处理器。",
                    "evidenceKeys": ["hardware-1.kind"], "certainty": "observed"
                }] },
                { "key": "health_assessment", "items": [] },
                { "key": "maintenance_plan", "items": [] },
                { "key": "upgrade_recommendation", "items": [] },
                { "key": "other", "items": [] }
            ],
            "evidenceSnapshotId": snapshot.snapshot_id
        })
        .to_string();

        let assessment = parse_assessment(
            &response,
            &snapshot,
            None,
            "model-a",
            "2026-10-14T12:02:00Z".to_owned(),
        )
        .unwrap();

        assert_eq!(assessment.priority_sections[0].items.len(), 1);
        assert!(
            assessment.priority_sections[1..]
                .iter()
                .all(|section| section.items.is_empty())
        );
    }

    #[test]
    fn prompt_requires_the_user_priorities_in_order_and_blocks_unsupported_actions() {
        let (inventory, telemetry) = fixture();
        let snapshot = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::System,
            None,
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        let prompt = build_analysis_prompt(&snapshot).unwrap();
        let priority_labels = [
            "设备性能客观评价",
            "设备状态正常评估",
            "设备维护方案",
            "设备更新换代建议",
            "其他内容",
        ];
        let positions =
            priority_labels.map(|label| prompt.find(label).expect("priority label in prompt"));
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(prompt.contains("压力测试"));
        assert!(prompt.contains("证据不足时不要包装成结论"));
        assert!(prompt.contains("每部分 items 可以为空数组"));
        assert!(prompt.contains("不要在多个部分重复同一项采集限制"));
    }

    #[test]
    fn old_cached_assessments_deserialize_without_priority_sections() {
        let cached = json!({
            "scope": "system",
            "targetDeviceId": null,
            "summary": "旧版缓存摘要。",
            "strengths": ["设备清单完整"],
            "concerns": [{
                "claim": "部分指标未测试",
                "evidenceKeys": [],
                "certainty": "unknown"
            }],
            "suggestions": ["继续观察设备状态"],
            "limitations": ["缺少长期负载数据"],
            "evidenceSnapshotId": "snapshot-legacy",
            "generatedAtUtc": "2026-10-14T12:00:00Z",
            "modelSlug": "model-legacy",
            "provenance": "AI_INTERPRETATION"
        });

        let assessment: AiAssessment = serde_json::from_value(cached).unwrap();

        assert!(assessment.priority_sections.is_empty());
        assert_eq!(assessment.strengths, vec!["设备清单完整"]);
        assert_eq!(assessment.evidence_snapshot_id, "snapshot-legacy");
    }

    #[test]
    fn fingerprint_tracks_inventory_scan_but_ignores_live_measurements() {
        let (inventory, telemetry) = fixture();
        let first = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::System,
            None,
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        let mut second = first.clone();
        second.snapshot_id = Uuid::new_v4().to_string();
        second.captured_at_utc = "2026-10-14T12:10:00Z".to_owned();
        second.metrics[0].observed_at_utc = Some("2026-10-14T12:09:59Z".to_owned());
        second.metrics[0].window = Some(MetricWindow {
            sample_count: 1,
            minimum: Some(0.0),
            mean: Some(0.0),
            maximum: Some(0.0),
            started_at_utc: Some("2026-10-14T12:09:59Z".to_owned()),
            ended_at_utc: Some("2026-10-14T12:09:59Z".to_owned()),
        });
        assert_eq!(
            input_fingerprint(&first, None).unwrap(),
            input_fingerprint(&second, None).unwrap()
        );
        second.metrics[0].value = Some(json!(50.0));
        second.metrics[0].state = "STALE".to_owned();
        second.metrics[0].reason_code = Some("MONITORING_PAUSED".to_owned());
        assert_eq!(
            input_fingerprint(&first, None).unwrap(),
            input_fingerprint(&second, None).unwrap()
        );
        second.scan_at_utc = "2026-10-14T12:11:00Z".to_owned();
        assert_ne!(
            input_fingerprint(&first, None).unwrap(),
            input_fingerprint(&second, None).unwrap()
        );
        second.scan_at_utc = first.scan_at_utc.clone();
        second.devices[0].name = Some("Changed hardware inventory".to_owned());
        assert_ne!(
            input_fingerprint(&first, None).unwrap(),
            input_fingerprint(&second, None).unwrap()
        );
    }

    #[test]
    fn device_scope_only_accepts_a_present_supported_non_pnp_hardware_target() {
        let (inventory, telemetry) = fixture();
        let selected = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::Device,
            Some("local-gpu-id"),
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        assert_eq!(selected.devices.len(), 1);
        assert_eq!(selected.devices[0].kind, "gpu_adapter");
        assert_eq!(selected.devices[0].device_key, "device");
        assert!(
            build_evidence_snapshot(
                &inventory,
                &telemetry,
                AnalysisScope::Device,
                Some("local-pnp-id"),
                "2026-10-14T12:01:00Z".to_owned(),
            )
            .is_err()
        );
    }

    #[test]
    fn initial_preview_token_is_one_use_and_analysis_jobs_are_serialized() {
        let (inventory, telemetry) = fixture();
        let snapshot = build_evidence_snapshot(
            &inventory,
            &telemetry,
            AnalysisScope::System,
            None,
            "2026-10-14T12:01:00Z".to_owned(),
        )
        .unwrap();
        let fingerprint = input_fingerprint(&snapshot, None).unwrap();
        let coordinator = AnalysisCoordinator::default();
        let preview = coordinator
            .store_preview(snapshot.clone(), None, fingerprint)
            .unwrap();
        let (restored, _) = coordinator.take_preview(&preview.token, None).unwrap();
        assert_eq!(restored, snapshot);
        assert_eq!(
            coordinator.take_preview(&preview.token, None).unwrap_err(),
            "AI_ANALYSIS_PREVIEW_EXPIRED"
        );

        let first = coordinator.enter_job("ai".to_owned()).unwrap();
        assert_eq!(
            coordinator.enter_job("ai".to_owned()).unwrap_err(),
            "AI_ANALYSIS_ALREADY_RUNNING"
        );
        drop(first);
        assert!(coordinator.enter_job("ai".to_owned()).is_ok());
    }
}
