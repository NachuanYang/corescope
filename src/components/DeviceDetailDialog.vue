<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { X, RotateCw, Cpu, HardDrive, MemoryStick, Network, BatteryCharging, CircuitBoard, Activity } from "lucide-vue-next";
import { useHardwareStore, type CapabilityRecord, type DeviceRecord, type MetricSample } from "../app/stores/hardware";
import { ANALYSIS_SECTION_LABELS, analysisCertaintyLabel, useAnalysisStore, type AnalysisPreview, type EvidenceMetric } from "../app/stores/analysis";
import { useAuthStore } from "../app/stores/auth";
import { useSettingsStore } from "../app/stores/settings";
import { metricName } from "../app/metrics";

const props = defineProps<{
  device: DeviceRecord;
  telemetry: MetricSample[];
}>();
const emit = defineEmits<{ close: [] }>();
const hardware = useHardwareStore();
const settings = useSettingsStore();
const analysis = useAnalysisStore();
const auth = useAuthStore();
const dialog = ref<HTMLElement | null>(null);
const devicePreview = ref<AnalysisPreview | null>(null);
const deviceActionPending = ref(false);
let returnFocusTo: HTMLElement | null = null;
const profile = computed(() => hardware.profiles[props.device.id] ?? null);
const loading = computed(() => hardware.loadingProfiles.includes(props.device.id));
const error = computed(() => hardware.profileErrors[props.device.id] ?? "");
const history = computed(() => settings.historyByDevice[props.device.id] ?? []);
const historyError = computed(() => settings.historyErrors[props.device.id] ?? "");
const historyLoading = computed(() => settings.loadingHistory.includes(props.device.id));
const deviceAnalysisState = computed(() => analysis.devices[props.device.id] ?? null);
const savedAnalysis = computed(() => deviceAnalysisState.value?.latestAnalysis ?? null);
const assessment = computed(() => savedAnalysis.value?.assessment ?? null);
const nonEmptyPrioritySections = computed(() => assessment.value?.prioritySections?.filter((section) => section.items.length > 0) ?? []);
const retryFailedAttempt = computed(() => !assessment.value
  && ["FAILED", "INTERRUPTED"].includes(deviceAnalysisState.value?.latestAttemptStatus ?? ""));
const isAiSupportedDevice = computed(() => props.device.layer !== "pnp_node"
  && props.device.isPresent === true
  && ["cpu", "gpu_adapter", "memory_module", "physical_disk", "network_adapter", "battery"].includes(props.device.kind));
const inferenceReady = computed(() => Boolean(auth.connection?.connected
  && auth.connection.planUsageAuthorized
  && auth.activeAccount?.clientId
  && auth.selectedModelSlug
  && auth.models.some((model) => model.slug === auth.selectedModelSlug)));
const deviceAnalysisRunning = computed(() => deviceActionPending.value
  || analysis.runningDevices.includes(props.device.id) || analysis.runningSystem);
const aiActionBlocked = computed(() => analysis.isAnyAiOperationRunning);
const activeSection = ref<"current" | "history">("current");
const currentTab = ref<HTMLButtonElement | null>(null);
const historyTab = ref<HTMLButtonElement | null>(null);
const mediaType = computed(() => props.device.attributes.mediaType);
const isSsd = computed(() => mediaType.value === "SSD");
const hasPermissionDenial = computed(() => profile.value?.capabilities.some((capability) =>
  capability.source === "MSFT_StorageReliabilityCounter" && capability.state === "PERMISSION_REQUIRED") ?? false);
const reliabilityMetrics = computed(() => profile.value?.capabilities.filter((capability) =>
  capability.source === "MSFT_StorageReliabilityCounter") ?? []);
const capabilityGroups = computed(() => {
  const capabilities = profile.value?.capabilities ?? [];
  const staticValues = capabilities.filter((capability) => capability.kind === "STATIC");
  const liveValues = capabilities.filter((capability) => capability.kind !== "STATIC"
    && capability.source !== "MSFT_StorageReliabilityCounter");
  return [
    { title: "设备参数", items: staticValues },
    { title: "运行状态与能力", items: liveValues },
    { title: "存储可靠性", items: reliabilityMetrics.value },
  ].filter((group) => group.items.length > 0);
});
const categoryName: Record<string, string> = {
  cpu: "处理器", gpu: "图形适配器", memory: "内存", storage: "存储设备",
  network: "网络", battery: "电池", other: "其他设备",
};
const stateNames: Record<string, string> = {
  AVAILABLE: "可读取", PERMISSION_REQUIRED: "需要额外权限", NOT_TESTED: "未测试",
  NOT_APPLICABLE: "不适用", UNSUPPORTED: "不支持", TEMP_UNAVAILABLE: "暂不可用",
  UNKNOWN: "状态未知", PROBING: "等待采样", STALE: "历史读数",
};
const trendKeys = computed(() => {
  const cutoff = Date.now() - 60_000;
  const counts = new Map<string, number>();
  for (const sample of props.telemetry) {
    if (sample.deviceId !== props.device.id || sample.state !== "AVAILABLE"
      || typeof sample.value !== "number" || !Number.isFinite(sample.value) || !sample.observedAtUtc
      || Date.parse(sample.observedAtUtc) < cutoff) continue;
    counts.set(sample.metricKey, (counts.get(sample.metricKey) ?? 0) + 1);
  }
  return [...counts.entries()].filter(([, count]) => count >= 2).map(([key]) => key);
});

function iconForDevice(device: DeviceRecord) {
  if (device.category === "cpu" || device.category === "gpu") return Cpu;
  if (device.category === "memory") return MemoryStick;
  if (device.category === "storage") return HardDrive;
  if (device.category === "network") return Network;
  if (device.category === "battery") return BatteryCharging;
  if (device.category === "other") return CircuitBoard;
  return Activity;
}

const reasonNames: Record<string, string> = {
  DEVICE_NOT_PRESENT_IN_LATEST_SCAN: "设备未出现在最新扫描中",
  APP_RESTARTED_LIVE_DATA_NOT_CURRENT: "应用重启后需重新采样",
  SCAN_REFRESH_PENDING_NEW_SAMPLE: "扫描已更新，等待新样本",
  MONITOR_START_FAILED: "实时监测未能启动",
  TELEMETRY_REFRESH_FAILED: "实时读数更新失败",
  SOURCE_NOT_AVAILABLE: "数据来源暂不可用",
  PROVIDER_NOT_SUPPORTED: "当前设备或驱动不支持此参数",
  LIVE_PROBE_NOT_RUN: "动态采样尚未运行",
  TELEMETRY_NOT_STARTED: "实时采样尚未启动",
  SOURCE_FIELD_NOT_COLLECTED: "系统清单未提供此参数",
  NO_UNIFORM_MEMORY_HEALTH_INTERFACE: "暂无统一的内存健康读取接口",
  DISK_MEDIA_TYPE_UNCONFIRMED: "尚未确认存储介质类型",
  WARMUP_REQUIRED: "正在等待下一采样周期",
  NVML_RUNTIME_UNAVAILABLE: "显卡管理接口不可用",
  NVML_PERMISSION_REQUIRED: "显卡管理接口拒绝读取",
  NVML_INITIALIZATION_FAILED: "显卡管理接口初始化失败",
  NVML_API_INCOMPLETE: "显卡驱动未提供完整读取接口",
  SOURCE_REPORTED_NOT_SUPPORTED: "显卡或驱动不支持此参数",
  NVML_QUERY_FAILED: "显卡管理接口读取失败",
  GPU_PCI_MATCH_AMBIGUOUS: "无法唯一匹配显卡",
  GPU_PCI_ID_NOT_RECORDED: "系统未提供显卡识别信息",
  GPU_NOT_FOUND_IN_NVML: "显卡未出现在驱动管理接口中",
  ACCESS_DENIED: "系统拒绝访问",
  COUNTER_NOT_FOUND: "系统未提供此计数器",
  NO_SAMPLE_YET: "尚未收到采样值",
};

function currentSample(capability: CapabilityRecord): MetricSample | undefined {
  if (capability.kind === "STATIC") return undefined;
  const sample = latestSample(capability.metricKey);
  if (!sample) return undefined;
  const savedTime = capability.observedAtUtc ? Date.parse(capability.observedAtUtc) : Number.NaN;
  const sampleTime = sample.observedAtUtc ? Date.parse(sample.observedAtUtc) : Number.NaN;
  if (Number.isFinite(savedTime) && Number.isFinite(sampleTime) && sampleTime < savedTime) return undefined;
  return sample;
}

function capabilityState(capability: CapabilityRecord): string {
  return currentSample(capability)?.state ?? capability.state;
}

function capabilityValue(capability: CapabilityRecord): string {
  const sample = currentSample(capability);
  const state = sample?.state ?? capability.state;
  if (state !== "AVAILABLE") return stateNames[state] ?? "状态未知";
  return localizedValue(capability.metricKey, sample ? sample.value : capability.value, sample ? sample.unit : capability.unit);
}

function capabilitySource(capability: CapabilityRecord): string {
  return currentSample(capability)?.source || capability.source || "未记录";
}

function capabilityObservedAt(capability: CapabilityRecord): string {
  const observed = currentSample(capability)?.observedAtUtc ?? capability.observedAtUtc;
  return observed ? historyTime(observed) : "无时间戳";
}

function capabilityReason(capability: CapabilityRecord): string | null {
  const sample = currentSample(capability);
  const reason = sample ? sample.reasonCode : capability.reasonCode;
  return reason ? `${reasonNames[reason] ?? "读取原因未分类"} · ${reason}` : null;
}

function capabilityReasonLabel(capability: CapabilityRecord): string | null {
  const reason = currentSample(capability)?.reasonCode ?? capability.reasonCode;
  return reason ? reasonNames[reason] ?? "读取原因未分类" : null;
}

function formatValue(value: unknown, unit: string | null): string {
  if (value === null || value === undefined) return "未提供数值";
  if (typeof value === "number") {
    const formatted = value.toLocaleString("zh-CN", { maximumFractionDigits: 1 });
    if (unit === "percent") return `${formatted}%`;
    if (unit === "°C") return `${formatted} °C`;
    return unit === "byte" || unit === "bytes" ? formatBytes(value) : `${formatted}${unit ? ` ${unit}` : ""}`;
  }
  if (typeof value === "boolean") return value ? "是" : "否";
  if (typeof value === "string") return value;
  return JSON.stringify(value);
}

function localizedValue(metricKey: string, value: unknown, unit: string | null): string {
  if (typeof value !== "string") return formatValue(value, unit);
  const normalized = value.trim().toLocaleLowerCase();
  const values: Record<string, Record<string, string>> = {
    "disk.health_status": { healthy: "健康", warning: "警告", unhealthy: "异常", unknown: "未知" },
    "disk.operational_status": { online: "在线", offline: "离线", degraded: "降级", unknown: "未知" },
    "disk.media_type": { hdd: "机械硬盘", ssd: "固态硬盘", unspecified: "未指定" },
    "disk.partition_style": { gpt: "GPT", mbr: "MBR", raw: "未初始化" },
    "network.status": { up: "已连接", down: "已断开", disconnected: "已断开", dormant: "待机" },
    "network.virtual_classification_status": { physical: "已确认实体接口", virtual: "已确认虚拟接口", unknown: "尚未确认" },
    "memory.module.form_factor": { dimm: "DIMM", sodimm: "SO-DIMM", rdimm: "RDIMM", udimm: "UDIMM" },
  };
  return values[metricKey]?.[normalized] ?? value;
}

function historyTime(value: string | null): string {
  if (!value) return "时间未记录";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? "时间未记录"
    : new Intl.DateTimeFormat("zh-CN", { dateStyle: "medium", timeStyle: "short" }).format(parsed);
}

function historyValue(item: (typeof history.value)[number]): string {
  if (item.state !== "AVAILABLE") return stateNames[item.state] ?? "状态未知";
  return item.value === null ? "无数值" : formatValue(item.value, item.unit);
}

function historyEvidence(item: (typeof history.value)[number]): string {
  const evidence = evidenceLevelLabel(item.evidenceLevel);
  const reason = item.reasonCode
    ? `原因 ${reasonNames[item.reasonCode] ?? "读取原因未分类"} · ${item.reasonCode}`
    : "";
  return [stateNames[item.state] ?? "状态未知", evidence, reason].filter(Boolean).join(" · ");
}

function evidenceLevelLabel(value: string | null): string {
  const labels: Record<string, string> = {
    MEASURED: "实时测量", SOURCE_REPORTED: "设备来源报告", OS_REPORTED: "操作系统报告",
    DERIVED: "根据原始读数计算", INFERRED: "推断", UNVERIFIED: "未验证",
  };
  return value ? labels[value] ?? "来源状态" : "证据未记录";
}

function formatBytes(value: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let scaled = value;
  let unit = 0;
  while (scaled >= 1024 && unit < units.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  return `${scaled.toLocaleString("zh-CN", { maximumFractionDigits: 1 })} ${units[unit]}`;
}

function evidenceLabel(capability: CapabilityRecord): string {
  return evidenceLevelLabel(capability.evidenceLevel);
}

function sampleLabel(key: string): string {
  if (key.includes("utilization")) return "负载";
  if (key.includes("temperature")) return "温度";
  if (key.includes("memory_controller")) return "显存控制器";
  if (key.includes("read")) return "读取速率";
  if (key.includes("write")) return "写入速率";
  if (key.includes("receive")) return "接收速率";
  if (key.includes("send")) return "发送速率";
  return metricName(key);
}

function trendPoints(metricKey: string): string {
  const cutoff = Date.now() - 60_000;
  const values = props.telemetry
    .filter((sample) => sample.deviceId === props.device.id && sample.metricKey === metricKey
      && sample.state === "AVAILABLE" && typeof sample.value === "number" && sample.observedAtUtc
      && Date.parse(sample.observedAtUtc) >= cutoff)
    .slice(-60);
  if (values.length < 2) return "";
  const numeric = values.map((sample) => sample.value as number);
  const low = Math.min(...numeric);
  const high = Math.max(...numeric);
  const spread = high - low || 1;
  return numeric.map((value, index) => {
    const x = (index / (numeric.length - 1)) * 240;
    const y = 42 - ((value - low) / spread) * 34;
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  }).join(" ");
}

function formatSample(sample: MetricSample): string {
  if (sample.state !== "AVAILABLE" || typeof sample.value !== "number") {
    return stateNames[sample.state] ?? "状态未知";
  }
  if (sample.unit === "byte/s") {
    let value = sample.value;
    const units = ["B/s", "KB/s", "MB/s", "GB/s"];
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return `${value.toLocaleString("zh-CN", { maximumFractionDigits: 1 })} ${units[unit]}`;
  }
  return `${sample.value.toLocaleString("zh-CN", { maximumFractionDigits: 1 })}${sample.unit ? ` ${sample.unit}` : ""}`;
}

function latestSample(metricKey: string): MetricSample | undefined {
  return props.telemetry.filter((sample) => sample.deviceId === props.device.id && sample.metricKey === metricKey).slice(-1)[0];
}

async function loadReliability() {
  await hardware.probeStorageReliability(props.device.id, hasPermissionDenial.value);
}

function aiMetricValue(metric: EvidenceMetric): string {
  if (metric.state !== "AVAILABLE") return stateNames[metric.state] ?? "状态未知";
  return metric.value === null ? "无数值" : formatValue(metric.value, metric.unit);
}

function reportTelemetryNotReady() {
  analysis.errorCode = "AI_ANALYSIS_TELEMETRY_NOT_READY";
  analysis.operationStatus = "本轮扫描尚未取得新的遥测样本，已暂停分析；请重新扫描后再试。";
}

async function previewDeviceAnalysis() {
  if (!isAiSupportedDevice.value) return;
  analysis.clearPreview();
  const preview = await analysis.createPreview("device", props.device.id);
  if (preview) devicePreview.value = preview;
}

async function startDeviceAnalysis() {
  if (!isAiSupportedDevice.value || !inferenceReady.value || aiActionBlocked.value) return;
  deviceActionPending.value = true;
  try {
    if (!analysis.system?.consentGranted) {
      await previewDeviceAnalysis();
      return;
    }
    const clientId = auth.connection?.activeClientId;
    const modelSlug = auth.selectedModelSlug;
    if (!clientId || !modelSlug) return;
    if (!hardware.telemetryWarmupReady) {
      reportTelemetryNotReady();
      return;
    }

    for (let attempt = 0; attempt < 2; attempt += 1) {
      const preview = await analysis.createPreview("device", props.device.id);
      if (!preview) return;
      const outcome = await analysis.refreshDevice(props.device.id, clientId, modelSlug, auth.route);
      if (analysis.errorCode !== "AI_ANALYSIS_PREVIEW_STALE" || attempt === 1 || outcome) return;
    }
  } finally {
    deviceActionPending.value = false;
  }
}

function cancelDevicePreview() {
  devicePreview.value = null;
  analysis.clearPreview();
}

async function confirmDeviceAnalysis() {
  if (deviceActionPending.value || aiActionBlocked.value) return;
  const token = devicePreview.value?.token;
  if (!token || !inferenceReady.value) return;
  if (!hardware.telemetryWarmupReady) {
    reportTelemetryNotReady();
    return;
  }
  deviceActionPending.value = true;
  try {
    if (!analysis.system?.consentGranted && !await analysis.decideConsent(true)) return;
    const clientId = auth.connection?.activeClientId;
    const modelSlug = auth.selectedModelSlug;
    if (!clientId || !modelSlug) return;
    const outcome = await analysis.refreshDevice(props.device.id, clientId, modelSlug, auth.route);
    if (analysis.errorCode === "AI_ANALYSIS_PREVIEW_STALE") {
      devicePreview.value = null;
      const freshPreview = await analysis.createPreview("device", props.device.id);
      if (freshPreview) devicePreview.value = freshPreview;
      return;
    }
    if (outcome) devicePreview.value = null;
  } finally {
    deviceActionPending.value = false;
  }
}

async function onKeyDown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    emit("close");
    return;
  }
  if ((event.key === "ArrowLeft" || event.key === "ArrowRight")
    && (event.target as HTMLElement | null)?.getAttribute("role") === "tab") {
    event.preventDefault();
    activeSection.value = activeSection.value === "current" ? "history" : "current";
    await nextTick();
    (activeSection.value === "current" ? currentTab : historyTab).value?.focus();
    return;
  }
  if (event.key !== "Tab" || !dialog.value) return;
  const items = [...dialog.value.querySelectorAll<HTMLElement>(
    'button:not(:disabled), a[href], input:not(:disabled), summary, [tabindex]:not([tabindex="-1"])',
  )];
  const first = items[0];
  const last = items.slice(-1)[0];
  const activeIndex = items.indexOf(document.activeElement as HTMLElement);
  if (!first || !last) {
    event.preventDefault();
    dialog.value.focus();
  } else if (activeIndex === -1) {
    event.preventDefault();
    (event.shiftKey ? last : first).focus();
  } else if (event.shiftKey && activeIndex === 0) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && activeIndex === items.length - 1) {
    event.preventDefault();
    first.focus();
  }
}

onMounted(async () => {
  returnFocusTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  window.addEventListener("keydown", onKeyDown);
  await Promise.all([
    hardware.loadDeviceProfile(props.device.id),
    settings.loadDeviceHistory(props.device.id),
    analysis.loadTargetState("system"),
    ...(isAiSupportedDevice.value ? [analysis.loadTargetState("device", props.device.id)] : []),
  ]);
  await nextTick();
  dialog.value?.focus();
});
onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKeyDown);
  returnFocusTo?.focus();
});
</script>

<template>
  <div class="dialog-scrim" @click.self="emit('close')">
    <section ref="dialog" class="device-dialog" role="dialog" aria-modal="true"
      :aria-labelledby="`device-title-${device.id}`" tabindex="-1">
      <header class="dialog-header">
        <div class="dialog-heading">
          <div class="dialog-icon"><component :is="iconForDevice(device)" :size="18" :stroke-width="1.7" /></div>
          <div class="dialog-title-copy">
            <span class="dialog-kicker">{{ categoryName[device.category] ?? "设备" }} · 只读档案</span>
            <h2 :id="`device-title-${device.id}`" :title="device.displayName">{{ device.displayName || categoryName[device.category] }}</h2>
          </div>
        </div>
        <button class="icon-button" type="button" aria-label="关闭详情" @click="emit('close')"><X :size="19" /></button>
      </header>

      <div class="dialog-content">
        <p class="device-origin">{{ device.manufacturer || "制造商未报告" }} · {{ device.layer === "pnp_node" ? "Windows 设备节点" : device.isVirtual ? "虚拟设备" : device.layer === "physical_device" ? "实体设备" : "逻辑设备" }}</p>

        <div class="detail-tabs" role="tablist" aria-label="设备详情内容">
          <button id="device-current-tab" ref="currentTab" type="button" role="tab"
            aria-controls="device-current-panel" :aria-selected="activeSection === 'current'"
            :tabindex="activeSection === 'current' ? 0 : -1" @click="activeSection = 'current'">当前与参数</button>
          <button id="device-history-tab" ref="historyTab" type="button" role="tab"
            aria-controls="device-history-panel" :aria-selected="activeSection === 'history'"
            :tabindex="activeSection === 'history' ? 0 : -1" @click="activeSection = 'history'">
            历史读数 <span>{{ history.length }}</span>
          </button>
        </div>

        <section v-if="activeSection === 'current'" id="device-current-panel" class="detail-panel" role="tabpanel" aria-labelledby="device-current-tab" tabindex="0">
          <div v-if="trendKeys.length" class="trend-section">
            <h3><Activity :size="15" /> 最近一分钟</h3>
            <div v-for="metricKey in trendKeys" :key="metricKey" class="trend-row">
              <div class="trend-name">{{ sampleLabel(metricKey) }}<strong>{{ formatSample(latestSample(metricKey)!) }}</strong></div>
              <svg class="trend-graph" viewBox="0 0 240 48" role="img" :aria-label="`${sampleLabel(metricKey)}最近一分钟采样曲线`">
                <line x1="0" y1="44" x2="240" y2="44" class="trend-axis" />
                <polyline :points="trendPoints(metricKey)" class="trend-line" />
              </svg>
              <span class="trend-time">{{ latestSample(metricKey)?.observedAtUtc ?? "" }}</span>
            </div>
          </div>

          <div v-if="loading && !profile" class="dialog-loading" role="status">正在读取设备档案…</div>
          <p v-else-if="error && !profile" class="dialog-message" role="status">无法读取设备档案，请稍后重试。</p>
          <template v-else-if="profile">
            <section v-for="group in capabilityGroups" :key="group.title" class="capability-group">
              <div class="dialog-section-title"><h3>{{ group.title }}</h3><span>{{ group.items.length }}</span></div>
              <div class="capability-list" role="list">
                <article v-for="capability in group.items" :key="capability.metricKey" class="capability-row" role="listitem">
                  <div class="capability-main">
                    <div class="capability-label"><strong>{{ metricName(capability.metricKey) }}</strong></div>
                    <div class="capability-value">
                      <span v-if="capabilityState(capability) === 'AVAILABLE'">{{ capabilityValue(capability) }}</span>
                      <span v-else class="state-copy" :class="`state-${capabilityState(capability).toLowerCase()}`">{{ capabilityValue(capability) }}</span>
                    </div>
                  </div>
                  <p v-if="capabilityReasonLabel(capability)" class="capability-reason">{{ capabilityReasonLabel(capability) }}</p>
                  <details class="capability-provenance">
                    <summary>来源与状态</summary>
                    <dl>
                      <div><dt>指标键</dt><dd>{{ capability.metricKey }}</dd></div>
                      <div><dt>来源</dt><dd>{{ capabilitySource(capability) }}</dd></div>
                      <div><dt>证据</dt><dd>{{ evidenceLabel(capability) }}</dd></div>
                      <div><dt>观察时间</dt><dd>{{ capabilityObservedAt(capability) }}</dd></div>
                      <div v-if="capabilityReason(capability)"><dt>原因</dt><dd>{{ capabilityReason(capability) }}</dd></div>
                      <div v-if="capability.nativeErrorCode"><dt>原生代码</dt><dd>{{ capability.nativeErrorCode }}</dd></div>
                    </dl>
                  </details>
                </article>
              </div>
            </section>
            <p v-if="!profile.capabilities.length" class="dialog-message">暂无可报告的设备参数。</p>
          </template>

          <section v-if="isSsd" class="reliability-action">
            <div>
              <strong>SSD 可靠性参数</strong>
              <span v-if="hasPermissionDenial">读取受限；仅在权限变化后手动重试。</span>
              <span v-else>按需读取，不会自动轮询。</span>
            </div>
            <button type="button" class="quiet-button" :disabled="loading" @click="loadReliability">
              <RotateCw :size="14" />{{ loading ? "读取中…" : hasPermissionDenial ? "权限变化后重试" : "读取参数" }}
            </button>
          </section>
          <p v-if="error && profile" class="inline-error" role="status">读取未完成，当前仍显示上次保存的结果。</p>

          <section v-if="isAiSupportedDevice" class="device-ai-section" aria-labelledby="device-ai-title">
            <div class="dialog-section-title device-ai-title-row">
              <h3 id="device-ai-title">AI 设备解读</h3>
            </div>
            <p class="device-ai-note">AI 结论按独立快照保存，不代表当前实时读数。</p>
            <div v-if="savedAnalysis" class="device-ai-result">
              <p v-if="savedAnalysis.isHistorical" class="device-ai-historical">这份结果对应较早的硬件快照。</p>
              <p v-if="assessment" class="device-ai-summary">{{ assessment.summary }}</p>
              <p v-else class="device-ai-note">上次分析未完成；可保留查看此前结果或重新分析。</p>
              <div v-if="nonEmptyPrioritySections.length" class="device-ai-priority-list">
                <section v-for="section in nonEmptyPrioritySections" :key="section.key" class="device-ai-priority-section">
                  <strong>{{ ANALYSIS_SECTION_LABELS[section.key] }}</strong>
                  <ul><li v-for="(item, index) in section.items" :key="`${section.key}-${index}`">
                    {{ item.text }}<small>{{ analysisCertaintyLabel(item.certainty) }} · {{ item.evidenceKeys.join("、") || "未关联可用证据键" }}</small>
                  </li></ul>
                </section>
              </div>
              <template v-else>
                <div v-if="assessment?.strengths.length" class="device-ai-list-group">
                  <strong>观察到的特点</strong>
                  <ul><li v-for="(item, index) in assessment.strengths" :key="`strength-${index}`">{{ item }}</li></ul>
                </div>
                <div v-if="assessment?.concerns.length" class="device-ai-list-group">
                  <strong>需要留意</strong>
                  <ul><li v-for="(item, index) in assessment.concerns" :key="`concern-${index}`">
                    {{ item.claim }}<small>{{ item.certainty === "observed" ? "观察" : item.certainty === "inferred" ? "推断" : "未知" }} · {{ item.evidenceKeys.join("、") || "未引用证据" }}</small>
                  </li></ul>
                </div>
                <div v-if="assessment?.suggestions.length" class="device-ai-list-group">
                  <strong>建议</strong>
                  <ul><li v-for="(item, index) in assessment.suggestions" :key="`suggestion-${index}`">{{ item }}</li></ul>
                </div>
                <div v-if="assessment?.limitations.length" class="device-ai-list-group">
                  <strong>分析范围</strong>
                  <ul><li v-for="(item, index) in assessment.limitations" :key="`limitation-${index}`">{{ item }}</li></ul>
                </div>
              </template>
              <p class="device-ai-provenance">AI 解读 · {{ assessment?.modelSlug || savedAnalysis.modelSlug }} · {{ historyTime(assessment?.generatedAtUtc ?? savedAnalysis.createdAtUtc) }} · 快照 {{ savedAnalysis.evidenceSnapshot.snapshotId.slice(0, 8) }}</p>
            </div>
            <p v-else-if="deviceAnalysisState?.latestAttemptStatus" class="device-ai-note">
              {{ retryFailedAttempt ? "首次分析未完成，可手动重试。" : "最近一次分析状态：" + deviceAnalysisState.latestAttemptStatus + "。" }}
            </p>
            <p v-else class="device-ai-note">尚无此设备的 AI 分析记录。</p>

            <div v-if="devicePreview" class="device-ai-preview" aria-label="设备分析发送预览">
              <p class="device-ai-preview-intro">发送前预览 · {{ devicePreview.metricCount }} 项指标状态 · 快照 {{ devicePreview.snapshot.snapshotId.slice(0, 8) }}</p>
              <ul class="device-ai-evidence-list">
                <li v-for="item in devicePreview.snapshot.devices" :key="item.deviceKey">
                  <strong>{{ item.name || categoryName[item.category] || item.category }}</strong>
                  <span>{{ item.manufacturer || item.source }} · {{ item.kind }}</span>
                  <small v-for="(value, key) in item.attributes" :key="key">{{ key }}：{{ typeof value === "number" && key.includes("bytes") ? formatBytes(value) : value }}</small>
                </li>
                <li v-for="metric in devicePreview.snapshot.metrics" :key="metric.evidenceKey">
                  <strong>{{ metricName(metric.metricKey) }}：{{ aiMetricValue(metric) }}</strong>
                  <span>{{ metric.source }} · {{ metric.evidenceLevel || "来源状态" }}</span>
                  <small v-if="metric.window">最近 {{ metric.window.sampleCount }} 个样本：最小 {{ metric.window.minimum ?? "—" }} · 平均 {{ metric.window.mean ?? "—" }} · 最大 {{ metric.window.maximum ?? "—" }}</small>
                </li>
              </ul>
              <div class="device-ai-preview-actions">
                <button type="button" class="text-action" @click="cancelDevicePreview">取消</button>
                <button type="button" class="quiet-button" :disabled="!inferenceReady || aiActionBlocked" @click="confirmDeviceAnalysis">
                  {{ deviceAnalysisRunning ? "正在分析…" : aiActionBlocked ? "AI 请求处理中…" : analysis.system?.consentGranted ? "确认并开始分析" : "同意发送并开始" }}
                </button>
              </div>
              <p v-if="!inferenceReady" class="device-ai-note">请先在设置页完成 ChatGPT 授权并选择可用模型。</p>
            </div>
            <div v-else class="device-ai-actions">
              <button type="button" class="quiet-button" :disabled="analysis.creatingPreview || aiActionBlocked || !inferenceReady" @click="startDeviceAnalysis">
                {{ analysis.creatingPreview ? "正在准备…" : deviceAnalysisRunning ? "正在分析…" : aiActionBlocked ? "AI 请求处理中…" : retryFailedAttempt ? "重试分析" : savedAnalysis ? "重新分析" : "开始分析" }}
              </button>
              <span v-if="analysis.errorCode" class="device-ai-error" role="status">未完成 · {{ analysis.errorCode }}</span>
            </div>
          </section>
        </section>

        <section v-else id="device-history-panel" class="detail-panel history-section" role="tabpanel" aria-labelledby="device-history-tab" tabindex="0">
          <div class="dialog-section-title"><h3>历史读数 · 非实时</h3><span>{{ history.length }}</span></div>
          <p v-if="historyLoading && !history.length" class="history-loading" role="status">正在读取历史记录…</p>
          <div v-if="historyError" class="history-error" role="status">
            <span>{{ history.length ? "更新失败，以下是此前读取的归档记录。" : "历史记录暂不可用，请重试。" }}</span>
            <button type="button" class="history-retry-button" :disabled="historyLoading" @click="settings.loadDeviceHistory(device.id)">重新加载</button>
          </div>
          <p v-else-if="!historyLoading && !history.length" class="history-empty">尚无已归档读数。</p>
          <p v-if="historyLoading && history.length" class="history-loading" role="status">正在更新历史记录…</p>
          <div v-if="history.length" class="history-list" role="list">
            <div v-for="item in history" :key="`${item.metricKey}-${item.observedAtUtc}`" class="history-row" role="listitem">
              <div class="history-copy">
                <strong>{{ metricName(item.metricKey) }}</strong>
                <span>{{ item.source }} · {{ historyTime(item.observedAtUtc) }}</span>
                <small>{{ historyEvidence(item) }}</small>
              </div>
              <span class="history-value">{{ historyValue(item) }}</span>
            </div>
          </div>
        </section>
      </div>
    </section>
  </div>
</template>
