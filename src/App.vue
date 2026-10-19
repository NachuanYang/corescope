<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import {
  ChevronDown, ChevronRight, CircuitBoard, Database, ExternalLink, KeyRound,
  LogOut, Plus, RefreshCw, Search, Send, ShieldCheck, ScanLine, Trash2, UserRound,
} from "lucide-vue-next";
import { save } from "@tauri-apps/plugin-dialog";
import { useAppStore } from "./app/stores/app";
import { useHardwareStore, type DeviceCategory, type DeviceRecord, type MetricSample } from "./app/stores/hardware";
import { useAuthStore } from "./app/stores/auth";
import { ANALYSIS_SECTION_LABELS, analysisCertaintyLabel, useAnalysisStore, type EvidenceMetric } from "./app/stores/analysis";
import { useChatStore } from "./app/stores/chat";
import { useSettingsStore } from "./app/stores/settings";
import { metricName } from "./app/metrics";
import DeviceDetailDialog from "./components/DeviceDetailDialog.vue";

const app = useAppStore();
const hardware = useHardwareStore();
const auth = useAuthStore();
const analysis = useAnalysisStore();
const chat = useChatStore();
const settings = useSettingsStore();
const activeView = ref<"devices" | "settings">("devices");
const searchQuery = ref("");
const categoryFilter = ref<DeviceCategory | "all">("all");
const selectedDevice = ref<DeviceRecord | null>(null);
const pendingCacheClear = ref(false);
const showAnalysisPreview = ref(false);
const dismissedInitialPreview = ref(false);
const confirmingAnalysisConsent = ref(false);
const analysisWaitingForTelemetry = ref(false);
const queuedAnalysisForce = ref(false);
const chatQuestion = ref("");
const inventoryStarted = ref(false);
const analysisStateLoaded = ref(false);
const initialBatchRunning = computed(() => analysis.initialBatchRunning);
const suppressAutoAnalysisOnScan = ref(false);
const fullAnalysisAfterConsent = ref(false);
const categoryLabels: Record<DeviceCategory, string> = {
  cpu: "处理器", gpu: "图形适配器", memory: "内存", storage: "存储",
  network: "网络", battery: "电池", other: "其他设备",
};
const categoryOrder: DeviceCategory[] = ["cpu", "gpu", "memory", "storage", "network", "battery", "other"];
let telemetryTimer: number | undefined;
let telemetryWaitTimer: number | undefined;

const scannedAt = computed(() => {
  const value = hardware.snapshot?.scannedAtUtc;
  return value ? new Intl.DateTimeFormat("zh-CN", { dateStyle: "medium", timeStyle: "short" }).format(new Date(value)) : "";
});
const allDevices = computed(() => hardware.snapshot?.devices ?? []);
const isVisibleDevice = (device: DeviceRecord) => device.layer === "physical_device"
  && device.kind !== "partition" && device.kind !== "volume" && device.isVirtual !== true;
const mainDevices = computed(() => allDevices.value.filter(isVisibleDevice));
const analysisTargets = computed(() => mainDevices.value.filter((device) => device.isPresent === true
  && ["cpu", "gpu_adapter", "memory_module", "physical_disk", "network_adapter", "battery"].includes(device.kind)));
const hasPendingInitialAnalyses = computed(() => {
  const system = analysis.system;
  if (!system) return false;
  if (!system.initialAttempted) return true;
  return analysisTargets.value.some((device) => {
    const state = analysis.devices[device.id];
    return Boolean(state && !state.initialAttempted);
  });
});
const analysisProgressPercent = computed(() => {
  const { completed, total } = analysis.initialBatchProgress;
  return total > 0 ? Math.round((completed / total) * 100) : 0;
});
const analysisProgressDeviceCounts = computed(() => {
  const { completed, total } = analysis.initialBatchProgress;
  const totalDevices = Math.max(0, total - 1);
  const completedDevices = Math.min(totalDevices, Math.max(0, completed - (completed > 0 ? 1 : 0)));
  return { completed: completedDevices, total: totalDevices };
});
const authReady = computed(() => Boolean(auth.connection?.connected && auth.connection.planUsageAuthorized));
const categoryFilters = computed(() => categoryOrder
  .filter((category) => mainDevices.value.some((device) => device.category === category))
  .map((category) => ({
    id: category,
    label: categoryLabels[category],
    count: mainDevices.value.filter((device) => device.category === category).length,
  })));
const matching = (device: DeviceRecord) => {
  const query = searchQuery.value.trim().toLocaleLowerCase();
  const matchesQuery = !query || [device.displayName, device.manufacturer ?? "", device.kind]
    .some((text) => text.toLocaleLowerCase().includes(query));
  return matchesQuery && (categoryFilter.value === "all" || device.category === categoryFilter.value);
};
const filteredDevices = computed(() => mainDevices.value.filter(matching));
const deviceGroups = computed(() => categoryOrder
  .map((category) => ({
    category,
    label: categoryLabels[category],
    devices: filteredDevices.value.filter((device) => device.category === category),
  }))
  .filter((group) => group.devices.length > 0));
const systemAssessment = computed(() => analysis.system?.latestAnalysis?.assessment ?? null);
const systemPrioritySections = computed(() => systemAssessment.value?.prioritySections?.filter((section) => section.items.length > 0) ?? []);
const analysisReady = computed(() => Boolean(auth.connection?.connected
  && auth.connection.planUsageAuthorized
  && auth.activeAccount?.clientId
  && auth.selectedModelSlug
  && auth.models.some((model) => model.slug === auth.selectedModelSlug)));
const previewDevices = computed(() => analysis.preview?.snapshot.devices ?? []);
const previewMetrics = computed(() => analysis.preview?.snapshot.metrics ?? []);
onMounted(() => {
  void app.loadInfo();
  void auth.initialize();
});
watch(authReady, async (ready) => {
  if (!ready) {
    inventoryStarted.value = false;
    analysisStateLoaded.value = false;
    selectedDevice.value = null;
    activeView.value = "devices";
    if (hardware.monitoring?.running) await hardware.stopMonitoring();
    return;
  }
  if (inventoryStarted.value) return;
  inventoryStarted.value = true;
  activeView.value = "devices";
  analysisStateLoaded.value = false;
  await Promise.all([
    hardware.loadInventory(),
    analysis.system ? Promise.resolve(analysis.system) : analysis.loadTargetState("system"),
  ]);
  if (!hardware.hasScanned && !hardware.isScanning) await hardware.scan();
  await Promise.all(analysisTargets.value.map((device) => analysis.loadTargetState("device", device.id)));
  analysisStateLoaded.value = true;
}, { immediate: true });
watch(() => hardware.snapshot?.scannedAtUtc ?? "", (scanTime) => {
  if (scanTime) void chat.setContext("system");
});
watch(
  () => [
    hardware.snapshot?.scannedAtUtc ?? "",
    hardware.isScanning,
    hardware.telemetryWarmupReady,
    analysisReady.value,
    analysis.system?.consentGranted ?? false,
    analysis.system?.consentDecided ?? false,
    hasPendingInitialAnalyses.value,
    analysisStateLoaded.value,
  ] as const,
  async ([scanTime, scanning, telemetryReady, ready, consentGranted, consentDecided, hasPending, stateLoaded]) => {
    if (!scanTime || scanning || suppressAutoAnalysisOnScan.value || !ready || !stateLoaded || showAnalysisPreview.value || dismissedInitialPreview.value || analysis.creatingPreview) return;
    if (initialBatchRunning.value) return;
    if (consentGranted) {
      if (hasPending && telemetryReady) {
        clearTelemetryWaitTimer();
        analysisWaitingForTelemetry.value = false;
        await runInitialAnalysisBatch(queuedAnalysisForce.value);
      } else if (hasPending) {
        queueAnalysisUntilTelemetryReady(queuedAnalysisForce.value);
      }
      return;
    }
    if (!consentDecided) {
      const preview = await analysis.createPreview("system");
      if (preview) showAnalysisPreview.value = true;
    }
  },
);
watch(() => hardware.monitoring?.running ?? false, (running) => {
  if (running && telemetryTimer === undefined) {
    telemetryTimer = window.setInterval(() => hardware.refreshTelemetry(), 1000);
  } else if (!running && telemetryTimer !== undefined) {
    window.clearInterval(telemetryTimer);
    telemetryTimer = undefined;
  }
});
watch(activeView, (view) => {
  if (view === "settings") {
    void settings.loadCacheSummary();
  }
});
onBeforeUnmount(() => {
  if (telemetryTimer !== undefined) window.clearInterval(telemetryTimer);
  clearTelemetryWaitTimer();
  void hardware.stopMonitoring();
});

function layerLabel(device: DeviceRecord): string {
  if (device.isVirtual === true) return "虚拟设备";
  if (device.layer === "physical_device") return "实体设备";
  if (device.layer === "unknown") return "类型未确认";
  return "逻辑设备";
}

function scanErrorMessage(code: string): string {
  if (code === "SCAN_TIMEOUT") return "扫描耗时过长，请稍后重试。";
  if (code === "SCAN_ALREADY_RUNNING") return "上一轮扫描仍在结束，请稍后重试。";
  return "扫描暂不可用，请查看本地诊断日志。";
}

function sampleLabel(sample: MetricSample): string {
  if (sample.metricKey.includes("memory_controller")) return "显存控制器";
  if (sample.metricKey.includes("utilization")) return "负载";
  if (sample.metricKey.includes("temperature")) return "温度";
  if (sample.metricKey.includes("read")) return "读";
  if (sample.metricKey.includes("write")) return "写";
  if (sample.metricKey.includes("receive")) return "收";
  if (sample.metricKey.includes("send")) return "发";
  return "状态";
}

function latestDeviceSamples(device: DeviceRecord): MetricSample[] {
  const keys = device.kind === "cpu" ? ["cpu.utilization.percent"]
    : device.kind === "gpu_adapter" ? ["gpu.utilization.percent", "gpu.temperature.celsius", "gpu.memory_controller.utilization.percent"]
      : device.kind === "physical_disk" ? ["disk.read.bytes_per_second", "disk.write.bytes_per_second"]
        : device.kind === "network_adapter" ? ["network.receive.bytes_per_second", "network.send.bytes_per_second"]
          : [];
  return keys.map((key) => hardware.telemetry
    .filter((sample) => sample.deviceId === device.id && sample.metricKey === key)
    .slice(-1)[0])
    .filter((sample): sample is MetricSample => sample !== undefined);
}

function stateLabel(state: MetricSample["state"]): string {
  return ({
    PROBING: "读取中", PERMISSION_REQUIRED: "需要权限", NOT_TESTED: "未测试",
    NOT_APPLICABLE: "不适用", UNSUPPORTED: "不支持", TEMP_UNAVAILABLE: "暂不可用",
    UNKNOWN: "状态未知", STALE: "历史读数", AVAILABLE: "等待样本",
  } as const)[state];
}

function sampleReasonLabel(reasonCode: string): string {
  const labels: Record<string, string> = {
    DEVICE_NOT_PRESENT_IN_LATEST_SCAN: "设备未出现在最新扫描中",
    APP_RESTARTED_LIVE_DATA_NOT_CURRENT: "应用重启后需重新采样",
    SCAN_REFRESH_PENDING_NEW_SAMPLE: "扫描已更新，等待新采样",
    MONITORING_PAUSED: "实时监测已暂停",
    SAMPLE_OLDER_THAN_FRESHNESS_WINDOW: "采样超过有效时间窗口",
    MONITOR_START_FAILED: "实时监测未能启动",
    TELEMETRY_REFRESH_FAILED: "实时读数更新失败",
  };
  return labels[reasonCode] ?? "实时读数尚未更新";
}

function sampleValue(sample: MetricSample): string {
  if (sample.state !== "AVAILABLE" || typeof sample.value !== "number") return stateLabel(sample.state);
  if (sample.unit === "percent" || sample.unit === "°C") {
    return `${sample.value.toLocaleString("zh-CN", { maximumFractionDigits: 1 })}${sample.unit === "percent" ? "%" : "°C"}`;
  }
  if (sample.unit === "byte/s") {
    let value = sample.value;
    const units = ["B/s", "KB/s", "MB/s", "GB/s"];
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
    return `${value.toLocaleString("zh-CN", { maximumFractionDigits: 1 })} ${units[unit]}`;
  }
  return `${sample.value.toLocaleString("zh-CN", { maximumFractionDigits: 1 })}${sample.unit ? ` ${sample.unit}` : ""}`;
}

function deviceTelemetry(device: DeviceRecord): string {
  const samples = latestDeviceSamples(device);
  if (!samples.length) return device.kind === "cpu" || device.kind === "gpu_adapter" || device.kind === "physical_disk" || device.kind === "network_adapter" ? "等待采样" : "";
  return samples.map((sample) => `${sampleLabel(sample)} ${sampleValue(sample)}`).join(" · ");
}

function deviceTelemetryTitle(device: DeviceRecord): string {
  return latestDeviceSamples(device).map((sample) =>
    `${sample.metricKey}: ${sample.source} · ${sample.observedAtUtc ?? "无时间戳"} · ${sample.evidenceLevel ?? "证据未分类"}${sample.reasonCode ? ` · 原因 ${sampleReasonLabel(sample.reasonCode)} (${sample.reasonCode})` : ""}`,
  ).join("\n") || "等待实时采样";
}

function selectCategory(category: DeviceCategory | "all") { categoryFilter.value = category; }
function openDevice(device: DeviceRecord) { selectedDevice.value = device; }

function changeAuthModel(event: Event) {
  auth.setModel((event.target as HTMLSelectElement).value);
}

function changeAuthRoute(event: Event) {
  auth.setRoute((event.target as HTMLSelectElement).value === "direct" ? "direct" : "system_proxy");
}

async function exportDiagnostics() {
  settings.error = "";
  settings.operationStatus = "";
  try {
    const path = await save({
      defaultPath: "CoreScope-diagnostics.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (path) await settings.exportDiagnostics(path);
  } catch (error) {
    settings.error = String(error);
  }
}

async function confirmCacheClear() {
  pendingCacheClear.value = false;
  await settings.clearAiCache();
}

async function openAnalysisPreview() {
  if (!hardware.hasScanned) {
    analysis.errorCode = "AI_ANALYSIS_INVENTORY_NOT_AVAILABLE";
    return;
  }
  const preview = await analysis.createPreview("system");
  if (preview) showAnalysisPreview.value = true;
}

function dismissAnalysisPreview() {
  showAnalysisPreview.value = false;
  dismissedInitialPreview.value = true;
  fullAnalysisAfterConsent.value = false;
  analysis.clearPreview();
}

async function declineAnalysisConsent() {
  await analysis.decideConsent(false);
  dismissAnalysisPreview();
}

async function revokeHardwareConsent() {
  await analysis.decideConsent(false);
  dismissAnalysisPreview();
}

function reportTelemetryNotReady() {
  analysis.errorCode = "AI_ANALYSIS_TELEMETRY_NOT_READY";
  analysis.operationStatus = "本轮扫描尚未取得新的遥测样本，已暂停分析；请重新扫描后再试。";
}

function clearTelemetryWaitTimer() {
  if (telemetryWaitTimer !== undefined) {
    window.clearTimeout(telemetryWaitTimer);
    telemetryWaitTimer = undefined;
  }
}

function queueAnalysisUntilTelemetryReady(force: boolean) {
  if (initialBatchRunning.value) return;
  queuedAnalysisForce.value ||= force;
  if (!analysisWaitingForTelemetry.value) {
    analysisWaitingForTelemetry.value = true;
    analysis.updateInitialBatchProgress(0, analysisTargets.value.length + 1, "整机", "等待实时样本");
    analysis.operationStatus = "已保存授权，正在等待本轮实时样本；样本就绪后会自动开始分析。";
    analysis.errorCode = "";
  }
  if (telemetryWaitTimer === undefined) {
    telemetryWaitTimer = window.setTimeout(() => {
      telemetryWaitTimer = undefined;
      if (!analysisWaitingForTelemetry.value || hardware.telemetryWarmupReady) return;
      analysisWaitingForTelemetry.value = false;
      reportTelemetryNotReady();
    }, 20_000);
  }
}

async function confirmAnalysisPreview() {
  if (!analysis.preview || confirmingAnalysisConsent.value || initialBatchRunning.value
    || analysis.isAnyAiOperationRunning || !analysisReady.value) return;
  confirmingAnalysisConsent.value = true;
  let batchStarted = false;
  try {
    if (!analysis.beginInitialBatch(analysisTargets.value.length + 1)) return;
    batchStarted = true;
    const wasConsentGranted = Boolean(analysis.system?.consentGranted);
    const force = fullAnalysisAfterConsent.value || wasConsentGranted;
    const accepted = wasConsentGranted ? true : await analysis.decideConsent(true);
    if (!accepted) return;
    const accountId = auth.activeAccount?.clientId;
    const modelSlug = auth.selectedModelSlug;
    if (!accountId || !modelSlug) return;
    showAnalysisPreview.value = false;
    dismissedInitialPreview.value = false;
    fullAnalysisAfterConsent.value = false;
    analysis.clearPreview();
    if (!hardware.telemetryWarmupReady) {
      analysis.endInitialBatch();
      batchStarted = false;
      queueAnalysisUntilTelemetryReady(force);
      return;
    }
    queuedAnalysisForce.value = false;
    clearTelemetryWaitTimer();
    analysisWaitingForTelemetry.value = false;
    await processAnalysisBatch(accountId, modelSlug, force);
  } finally {
    if (batchStarted) analysis.endInitialBatch();
    confirmingAnalysisConsent.value = false;
  }
}

async function processAnalysisBatch(clientId: string, modelSlug: string, force: boolean) {
  const targets = analysisTargets.value;
  const total = targets.length + 1;
  let completedDevices = 0;
  let incompleteDevices = 0;
  let settled = 0;
  const reportProgress = (current: string, phase: string) => {
    analysis.updateInitialBatchProgress(settled, total, current, phase);
  };

  const initialSystemState = analysis.system ?? await analysis.loadTargetState("system");
  if (force || !initialSystemState?.initialAttempted) {
    analysis.operationStatus = "正在分析整机…";
    reportProgress("整机", "正在分析");
    if (await analysis.createPreview("system")) {
      const currentState = analysis.system ?? initialSystemState;
      if (currentState?.initialAttempted) {
        await analysis.refreshSystem(clientId, modelSlug, auth.route, true);
      } else {
        await analysis.startInitial(clientId, modelSlug, auth.route, true);
      }
    }
  }
  settled += 1;
  reportProgress("整机", "整机分析已完成");

  for (const device of targets) {
    if (!authReady.value || !analysis.system?.consentGranted) {
      const remaining = targets.length - (settled - 1);
      incompleteDevices += remaining;
      settled = total;
      break;
    }
    reportProgress(device.displayName, "正在读取设备状态");
    const state = analysis.devices[device.id] ?? await analysis.loadTargetState("device", device.id);
    const shouldAnalyze = force || Boolean(state && !state.initialAttempted);
    if (!state) {
      incompleteDevices += 1;
      settled += 1;
      reportProgress(device.displayName, "设备状态读取失败");
      continue;
    }
    if (!shouldAnalyze) {
      completedDevices += 1;
      settled += 1;
      reportProgress(device.displayName, "已有分析结果");
      continue;
    }

    analysis.operationStatus = `正在分析：${device.displayName}…`;
    reportProgress(device.displayName, "正在分析");
    let outcome = null;
    if (await analysis.createPreview("device", device.id)) {
      const currentState = analysis.devices[device.id] ?? state;
      outcome = currentState.initialAttempted
        ? await analysis.refreshDevice(device.id, clientId, modelSlug, auth.route, true)
        : await analysis.startInitialDevice(device.id, clientId, modelSlug, auth.route, true);
    }
    if (outcome?.status === "COMPLETED") {
      completedDevices += 1;
    } else {
      incompleteDevices += 1;
    }
    settled += 1;
    reportProgress(device.displayName, outcome?.status === "COMPLETED" ? "设备分析已完成" : "设备分析未完成");
  }

  analysis.operationStatus = targets.length > 0
    ? `整机及设备分析完成 · ${completedDevices} 台完成 · ${incompleteDevices} 台未完成`
    : "整机分析完成";
}

async function runInitialAnalysisBatch(force = false) {
  if (initialBatchRunning.value || analysis.isAnyAiOperationRunning || !analysisReady.value || !analysis.system?.consentGranted || !hardware.hasScanned) return;
  if (!hardware.telemetryWarmupReady) {
    queueAnalysisUntilTelemetryReady(force);
    return;
  }
  const clientId = auth.activeAccount?.clientId;
  const modelSlug = auth.selectedModelSlug;
  if (!clientId || !modelSlug || !analysis.beginInitialBatch(analysisTargets.value.length + 1)) return;
  const shouldForce = force || queuedAnalysisForce.value;
  queuedAnalysisForce.value = false;
  clearTelemetryWaitTimer();
  analysisWaitingForTelemetry.value = false;
  analysis.errorCode = "";
  try {
    await processAnalysisBatch(clientId, modelSlug, shouldForce);
  } finally {
    analysis.endInitialBatch();
  }
}

async function scanAndAnalyzeSystem() {
  if (hardware.isScanning || initialBatchRunning.value || analysis.isAnyAiOperationRunning) return;
  suppressAutoAnalysisOnScan.value = true;
  let scanCompleted = false;
  try {
    scanCompleted = await hardware.scan();
  } finally {
    suppressAutoAnalysisOnScan.value = false;
  }
  if (!scanCompleted) return;

  const systemState = await analysis.loadTargetState("system");
  if (!systemState) return;
  if (!systemState.consentGranted) {
    fullAnalysisAfterConsent.value = true;
    await openAnalysisPreview();
    return;
  }
  if (!analysisReady.value) {
    analysis.operationStatus = "请先在设置中选择可用模型";
    return;
  }
  await runInitialAnalysisBatch(true);
}

async function sendChatQuestion() {
  if (analysis.isAnyAnalysisRunning) {
    chat.operationStatus = "设备分析正在进行，请完成后再发送问题";
    return;
  }
  if (!hardware.hasScanned) {
    chat.errorCode = "AI_CHAT_INVENTORY_NOT_AVAILABLE";
    return;
  }
  if (!analysis.system?.consentGranted) {
    chat.errorCode = "AI_ANALYSIS_CONSENT_REQUIRED";
    chat.operationStatus = "请先在设置中允许发送硬件摘要";
    return;
  }
  if (!analysisReady.value) {
    chat.errorCode = "AI_CHAT_MODEL_NOT_READY";
    chat.operationStatus = "请先在设置中选择可用模型";
    return;
  }
  const question = chatQuestion.value.trim();
  if (!question || chat.isCreatingPreview || chat.isSending) return;
  if (chat.scope !== "system") await chat.setContext("system");
  if (analysis.isAnyAnalysisRunning) {
    chat.operationStatus = "设备分析正在进行，请完成后再发送问题";
    return;
  }
  const accountId = auth.activeAccount?.clientId;
  const modelSlug = auth.selectedModelSlug;
  if (!accountId || !modelSlug || !analysisReady.value) return;

  for (let attempt = 0; attempt < 2; attempt += 1) {
    const preview = await chat.createPreview(question);
    if (!preview) return;
    if (analysis.isAnyAnalysisRunning) {
      chat.clearPreview();
      chat.operationStatus = "设备分析正在进行，请完成后再发送问题";
      return;
    }
    const result = await chat.sendPreview(accountId, modelSlug, auth.route);
    if (result) {
      chatQuestion.value = "";
      return;
    }
    if (!(["AI_CHAT_PREVIEW_STALE", "AI_CHAT_CONTEXT_CHANGED"].includes(chat.errorCode)) || attempt === 1) return;
  }
}

async function clearCurrentChat() {
  if (!chat.selectedThreadId || chat.isSending) return;
  if (!window.confirm("清空当前本地对话及其硬件快照？此操作无法撤销。")) return;
  await chat.clearCurrentThread();
}

function previewMetricValue(metric: EvidenceMetric): string {
  if (metric.state !== "AVAILABLE" || metric.value === null) {
    return ({
      PERMISSION_REQUIRED: "需要权限", NOT_APPLICABLE: "不适用", NOT_TESTED: "未测试",
      UNSUPPORTED: "不支持", TEMP_UNAVAILABLE: "暂不可用", STALE: "历史读数",
      PROBING: "采样中", UNKNOWN: "状态未知",
    } as Record<string, string>)[metric.state] ?? "未提供";
  }
  if (typeof metric.value === "number") {
    return `${metric.value.toLocaleString("zh-CN", { maximumFractionDigits: 1 })}${metric.unit === "percent" ? "%" : metric.unit ? ` ${metric.unit}` : ""}`;
  }
  return String(metric.value);
}

function metricLabel(metric: EvidenceMetric): string {
  return metricName(metric.metricKey);
}
</script>

<template>
  <div class="app-shell">
    <header class="app-header">
      <div class="brand-lockup" aria-label="机枢">
        <svg class="brand-mark" viewBox="0 0 40 40" fill="none" aria-hidden="true">
          <path d="M12 12h8v8h-8zM20 16h8v8h-8zM12 20v8h8" />
          <circle cx="12" cy="12" r="2.5" />
          <circle cx="28" cy="16" r="2.5" />
          <circle cx="28" cy="24" r="2.5" />
          <circle cx="20" cy="28" r="2.5" />
        </svg>
        <span class="brand-title"><strong>机枢</strong></span>
      </div>

      <div v-if="authReady" class="header-controls">
        <nav class="view-switch" role="tablist" aria-label="主视图">
          <button role="tab" class="view-tab" :aria-selected="activeView === 'devices'" @click="activeView = 'devices'">设备</button>
          <button role="tab" class="view-tab" :aria-selected="activeView === 'settings'" @click="activeView = 'settings'">设置</button>
        </nav>
      </div>
    </header>

    <main class="page-content">
      <section v-if="!authReady" class="auth-gate" aria-labelledby="auth-gate-title">
        <div class="auth-gate-copy">
          <h1 id="auth-gate-title">连接 ChatGPT</h1>
        </div>
        <div class="auth-gate-actions">
          <button type="button" class="primary-button" :disabled="auth.isConnecting"
            @click="auth.connect(auth.activeAccount?.clientId ?? null)">
            <KeyRound :size="16" />{{ auth.isConnecting ? "等待浏览器授权…" : auth.activeAccount?.clientId ? "重新授权" : "连接 ChatGPT" }}
          </button>
          <button v-if="auth.isConnecting" type="button" class="quiet-button" @click="auth.cancelConnect">取消</button>
        </div>
        <p v-if="auth.operationStatus" class="settings-status" role="status">{{ auth.operationStatus }}</p>
        <p v-if="auth.errorCode" class="settings-status auth-error" role="status">授权未完成；诊断码 {{ auth.errorCode }}</p>
      </section>

      <section v-else-if="activeView === 'devices'" class="home-view" aria-labelledby="home-title">
        <div class="page-heading">
          <div>
            <h1 id="home-title">硬件清单</h1>
          </div>
          <div class="scan-actions">
            <button class="primary-button" type="button"
              :disabled="hardware.isScanning || initialBatchRunning || analysis.isAnyAiOperationRunning"
              @click="scanAndAnalyzeSystem">
              <ScanLine :size="16" :stroke-width="1.8" />{{ hardware.isScanning ? "正在扫描…" : hardware.hasScanned ? "扫描并分析整机" : "扫描此电脑" }}
            </button>
            <button v-if="hardware.isScanning" class="quiet-button scan-cancel-button" type="button"
              :disabled="hardware.isCancellingScan" @click="hardware.cancelScan()">
              {{ hardware.isCancellingScan ? "正在取消…" : "取消扫描" }}
            </button>
          </div>
        </div>

        <p v-if="app.startupError" class="notice-line" role="status">本地服务暂不可用，请查看诊断日志。</p>
        <p v-if="hardware.scanError" class="notice-line" role="status">
          {{ scanErrorMessage(hardware.scanError) }}
        </p>
        <p v-if="hardware.telemetryError" class="notice-line" role="status">实时采样暂不可用，当前读数不会更新。</p>

        <section v-if="!hardware.hasScanned" class="first-run" aria-label="首次扫描">
          <div class="first-run-mark" aria-hidden="true"><CircuitBoard :size="23" :stroke-width="1.4" /></div>
          <div class="first-run-copy"><h2>扫描本机硬件</h2></div>
        </section>

        <template v-else>
          <div class="inventory-meta">
            <div class="inventory-count"><strong>{{ mainDevices.length }}</strong> 个设备</div>
            <div class="inventory-state">
              <span>{{ scannedAt }}</span>
              <span>{{ hardware.monitoring?.running ? "正在采集" : "监测已暂停" }}</span>
            </div>
          </div>

          <div v-if="(hardware.snapshot?.warnings.length ?? 0) > 0" class="notice-line warning" role="status">
            部分设备来源暂不可用，清单可能不完整；详细原因已记录在本地诊断中。
          </div>

          <div class="inventory-tools">
            <label class="search-field">
              <Search :size="16" :stroke-width="1.8" />
              <input v-model="searchQuery" type="search" placeholder="搜索设备名称或厂商" aria-label="搜索设备" />
              <span v-if="searchQuery" class="search-count">{{ filteredDevices.length }}</span>
            </label>
            <div class="category-filters" role="group" aria-label="设备类别筛选">
              <button type="button" class="filter-button" :aria-pressed="categoryFilter === 'all'" @click="selectCategory('all')">全部 <span>{{ mainDevices.length }}</span></button>
              <button v-for="item in categoryFilters" :key="item.id" type="button" class="filter-button"
                :aria-pressed="categoryFilter === item.id" @click="selectCategory(item.id)">
                {{ item.label }} <span>{{ item.count }}</span>
              </button>
            </div>
          </div>

          <div class="device-groups" role="list" aria-label="本机设备清单">
            <section v-for="group in deviceGroups" :key="group.category" class="category-group" role="listitem">
              <div class="category-heading">
                <h2>{{ group.label }}</h2>
                <span>{{ group.devices.length }} 项</span>
              </div>
              <ul class="device-list">
                <li v-for="(device, deviceIndex) in group.devices" :key="device.id" class="device-entry">
                  <button class="device-row" type="button"
                    :aria-label="`打开设备详情：${device.displayName}`" @click="openDevice(device)">
                    <span class="device-index" aria-hidden="true">{{ String(deviceIndex + 1).padStart(2, "0") }}</span>
                    <span class="device-copy">
                      <strong :title="device.displayName">{{ device.displayName || categoryLabels[device.category] }}</strong>
                      <span>{{ device.manufacturer || categoryLabels[device.category] }} · {{ layerLabel(device) }}</span>
                    </span>
                    <span class="device-readout" :title="deviceTelemetryTitle(device)">{{ deviceTelemetry(device) }}</span>
                    <ChevronRight class="device-chevron" :size="16" :stroke-width="1.7" />
                  </button>
                </li>
                <li v-if="!group.devices.length" class="empty-line">该类别没有匹配设备。</li>
              </ul>
            </section>
            <p v-if="deviceGroups.length === 0" class="empty-line" role="listitem">没有符合搜索条件的设备。</p>
          </div>

        </template>

        <section v-if="hardware.hasScanned" class="analysis-section" aria-labelledby="analysis-title">
          <div class="analysis-heading">
            <div>
              <h2 id="analysis-title">整机分析</h2>
            </div>
            <span class="analysis-state" :class="{ historical: analysis.system?.latestAnalysis?.isHistorical }">
              {{ analysis.system?.latestAnalysis?.isHistorical ? "历史快照" : systemAssessment ? "已保存" : analysis.system?.latestAttemptStatus === "FAILED" ? "待重试" : "尚未分析" }}
            </span>
          </div>
          <div v-if="initialBatchRunning || analysisWaitingForTelemetry" class="analysis-progress" role="status" aria-live="polite">
            <div class="analysis-progress-copy">
              <span>{{ analysis.initialBatchProgress.phase }}<template v-if="analysis.initialBatchProgress.current"> · {{ analysis.initialBatchProgress.current }}</template></span>
              <strong>{{ analysisProgressPercent }}%</strong>
            </div>
            <progress class="analysis-progress-bar" :value="analysisProgressPercent" max="100" aria-label="整机分析完成度">
              {{ analysisProgressPercent }}%
            </progress>
            <small>{{ analysisProgressDeviceCounts.completed }} / {{ analysisProgressDeviceCounts.total }} 台设备已处理 · 整机任务单独计入完成度</small>
          </div>
          <template v-if="systemAssessment">
            <p class="analysis-summary">{{ systemAssessment.summary }}</p>
            <p v-if="analysis.system?.latestAnalysis?.isHistorical" class="analysis-history-note">
              结论基于较早的设备快照；当前状态以本机采集为准。
            </p>
            <details class="analysis-full">
              <summary>完整解读与依据 <ChevronDown :size="14" /></summary>
              <div v-if="systemPrioritySections.length" class="analysis-priority-list">
                <section v-for="section in systemPrioritySections" :key="section.key" class="analysis-priority-section">
                  <h3>{{ ANALYSIS_SECTION_LABELS[section.key] }}</h3>
                  <ul>
                    <li v-for="(item, index) in section.items" :key="`${section.key}-${index}`">
                      <span>{{ item.text }}</span>
                      <small>{{ analysisCertaintyLabel(item.certainty) }} · {{ item.evidenceKeys.join("、") || "未关联可用证据键" }}</small>
                    </li>
                  </ul>
                </section>
              </div>
              <template v-else>
                <ul v-if="systemAssessment.strengths.length" class="analysis-points" aria-label="整机特点">
                  <li v-for="item in systemAssessment.strengths" :key="item"><span>+</span>{{ item }}</li>
                </ul>
                <ul v-if="systemAssessment.concerns.length" class="analysis-points concerns" aria-label="关注事项">
                  <li v-for="item in systemAssessment.concerns" :key="item.claim">
                    <span>·</span>{{ item.claim }}<small>{{ item.certainty === "observed" ? "已观察" : item.certainty === "inferred" ? "推断" : "证据不足" }}</small>
                  </li>
                </ul>
                <ul v-if="systemAssessment.suggestions.length" class="analysis-points" aria-label="建议">
                  <li v-for="(item, index) in systemAssessment.suggestions" :key="`legacy-suggestion-${index}`"><span>·</span>{{ item }}</li>
                </ul>
                <ul v-if="systemAssessment.limitations.length" class="analysis-points" aria-label="分析范围">
                  <li v-for="(item, index) in systemAssessment.limitations" :key="`legacy-limitation-${index}`"><span>·</span>{{ item }}</li>
                </ul>
              </template>
              <div class="analysis-provenance">
                <span>{{ systemAssessment.modelSlug }}</span>
                <span>快照 {{ systemAssessment.evidenceSnapshotId.slice(0, 8) }}</span>
                <time :datetime="systemAssessment.generatedAtUtc">{{ systemAssessment.generatedAtUtc }}</time>
              </div>
            </details>
          </template>
          <p v-else-if="!initialBatchRunning" class="analysis-empty">{{ analysis.system?.consentGranted ? "尚无整机分析。" : "需要先确认硬件摘要发送范围。" }}</p>
          <div v-if="analysis.operationStatus" class="analysis-actions">
            <span class="analysis-operation" role="status">{{ analysis.operationStatus }}</span>
          </div>
          <p v-if="analysis.errorCode" class="analysis-error" role="status">
            分析未完成；诊断码 {{ analysis.errorCode }}
          </p>
        </section>

        <section class="assistant-section" aria-labelledby="assistant-title">
          <div class="assistant-heading">
            <span class="section-index" aria-hidden="true">03</span>
            <div class="assistant-title-copy">
              <h2 id="assistant-title">设备问答</h2>
            </div>
            <button type="button" class="quiet-button chat-new-button" aria-label="新建本地对话"
              :disabled="!hardware.hasScanned || chat.isCreatingThread" @click="chat.createThread()">
              <Plus :size="14" />新对话
            </button>
          </div>
          <details v-if="chat.threads.length > 1" class="chat-history">
            <summary>历史对话 · {{ chat.threads.length }}</summary>
            <ul class="chat-history-list" aria-label="本地历史对话">
              <li v-for="thread in chat.threads" :key="thread.id">
                <button type="button" :aria-current="chat.selectedThreadId === thread.id ? 'true' : undefined" @click="chat.openThread(thread.id)">
                  {{ new Date(thread.updatedAtUtc).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }) }}
                </button>
              </li>
            </ul>
          </details>

          <ol v-if="chat.messages.length" class="chat-message-list" aria-label="本地对话记录" aria-live="polite">
            <li v-for="message in chat.messages" :key="message.id" class="chat-message-row" :class="message.role">
              <div class="chat-message-meta">
                <strong>{{ message.role === "user" ? "你" : "机枢" }}</strong>
                <time :datetime="message.createdAtUtc">{{ new Date(message.createdAtUtc).toLocaleString("zh-CN", { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }) }}</time>
                <span v-if="message.status !== 'COMPLETED'">{{ message.status === "PENDING" ? "等待中" : message.status === "INTERRUPTED" ? "已中断" : "未完成" }}</span>
              </div>
              <p>{{ message.body }}</p>
              <ul v-if="message.citations.length" class="chat-citations" aria-label="回答依据">
                <li v-for="citation in message.citations" :key="citation.claim">
                  {{ citation.claim }} <small>{{ citation.evidenceKeys.join(" · ") }}</small>
                </li>
              </ul>
              <div v-if="message.role === 'assistant'" class="chat-message-source">
                <span>{{ message.modelSlug || "模型未记录" }}</span>
                <span>{{ message.source || "来源未记录" }}</span>
                <span>快照 {{ message.evidenceSnapshotRef?.slice(0, 8) || "—" }}</span>
                <span>采集于 {{ message.snapshotCapturedAtUtc ? new Date(message.snapshotCapturedAtUtc).toLocaleString("zh-CN") : "时间未记录" }}；不代表当前状态</span>
              </div>
              <p v-if="message.errorCode" class="analysis-error">
                诊断码 {{ message.errorCode }}
              </p>
            </li>
          </ol>
          <p v-else class="chat-empty-line">尚无本地对话。</p>
          <p v-if="!analysis.system?.consentGranted" class="chat-setup-note">
            硬件摘要发送已关闭。<button type="button" class="text-action" @click="activeView = 'settings'">前往设置</button>
          </p>
          <p v-else-if="!analysisReady" class="chat-setup-note">
            请在设置中选择可用模型。<button type="button" class="text-action" @click="activeView = 'settings'">前往设置</button>
          </p>
          <form class="question-form" @submit.prevent="sendChatQuestion">
            <textarea v-model="chatQuestion" maxlength="2000" rows="2" :disabled="!hardware.hasScanned || !analysis.system?.consentGranted || !analysisReady || chat.isSending"
              placeholder="输入问题，可直接提及硬件名称…" aria-label="设备问答内容" />
            <button type="submit" :disabled="!hardware.hasScanned || !analysis.system?.consentGranted || !analysisReady || !chatQuestion.trim() || chat.isCreatingPreview || chat.isSending" aria-label="发送问题">
              <Send :size="15" />
            </button>
          </form>
          <div class="chat-actions">
            <span v-if="chat.operationStatus" role="status">{{ chat.operationStatus }}</span>
            <button v-if="chat.isSending" type="button" class="text-action" @click="chat.cancelRequest">取消本次请求</button>
            <button v-if="chat.messages.length" type="button" class="text-action chat-clear-action"
              :disabled="chat.isSending || chat.isLoading" @click="clearCurrentChat"><Trash2 :size="13" />清空此对话</button>
          </div>
          <p v-if="chat.errorCode" class="analysis-error" role="status">
            问答未完成；诊断码 {{ chat.errorCode }}
          </p>
        </section>
      </section>

      <section v-else class="settings-view" aria-labelledby="settings-title">
        <div class="page-heading settings-heading">
          <div>
            <h1 id="settings-title">设置</h1>
          </div>
        </div>

        <section class="settings-group">
          <div class="category-heading"><h2>AI 授权</h2></div>
          <div class="settings-row">
            <span class="settings-icon"><KeyRound :size="16" /></span>
            <div class="settings-copy"><strong>ChatGPT</strong></div>
            <span class="settings-value" :class="{ positive: auth.connection?.connected && auth.connection.planUsageAuthorized }">
              {{ auth.connection?.planUsageAuthorized ? "方案授权已就绪" : auth.connection?.connected ? "未获方案调用权限" : "未连接" }}
            </span>
            <button type="button" class="quiet-button" :disabled="auth.isConnecting" @click="auth.connect(auth.activeAccount?.clientId ?? null)">
              {{ auth.isConnecting ? "等待浏览器授权…" : auth.activeAccount?.clientId ? "重新授权" : "连接账号" }}
            </button>
          </div>
          <div v-if="auth.isConnecting" class="settings-actions">
            <span class="settings-status" role="status">请在系统浏览器完成 ChatGPT 授权。</span>
            <button type="button" class="quiet-button" @click="auth.cancelConnect">取消授权</button>
          </div>
          <div v-if="auth.activeAccount" class="settings-row auth-setting-row">
            <span class="settings-icon"><UserRound :size="16" /></span>
            <div class="settings-copy"><strong>授权账号</strong></div>
            <span class="settings-value">{{ auth.activeAccount.displayName || auth.activeAccount.email || "已连接" }}</span>
          </div>
          <div v-if="auth.connection?.connected && auth.connection.planUsageAuthorized" class="settings-row auth-setting-row">
            <span class="settings-icon"><RefreshCw :size="15" /></span>
            <div class="settings-copy"><strong>模型</strong></div>
            <select class="settings-select" aria-label="ChatGPT 模型" :value="auth.selectedModelSlug" @change="changeAuthModel">
              <option value="" disabled>选择模型</option>
              <option v-for="model in auth.models" :key="model.slug" :value="model.slug">{{ model.displayName }}</option>
            </select>
            <button type="button" class="quiet-button" :disabled="auth.isLoadingModels" @click="auth.loadModels(true)">刷新模型</button>
          </div>
          <div class="settings-row auth-setting-row">
            <span class="settings-icon"><ExternalLink :size="15" /></span>
            <div class="settings-copy"><strong>网络路线</strong></div>
            <select class="settings-select" aria-label="ChatGPT 网络路线" :value="auth.route" @change="changeAuthRoute">
              <option value="system_proxy">系统代理</option>
              <option value="direct">直接连接</option>
            </select>
            <span class="settings-value"></span>
          </div>
          <div class="settings-actions">
            <button v-if="auth.connection?.connected && auth.connection.planUsageAuthorized" type="button" class="quiet-button"
              :disabled="auth.isVerifying || !auth.selectedModelSlug" @click="auth.verifyInference">
              {{ auth.isVerifying ? "等待模型响应…" : "测试模型响应" }}
            </button>
            <button v-if="auth.connection?.connected" type="button" class="quiet-button" :disabled="auth.isDisconnecting" @click="auth.disconnect">
              <LogOut :size="14" />{{ auth.isDisconnecting ? "正在断开…" : "断开授权" }}
            </button>
          </div>
          <p v-if="auth.verification" class="settings-status" role="status">
            上次完整响应 · {{ auth.verification.modelSlug }} · {{ auth.verification.completedAtUtc }}
          </p>
          <p v-if="auth.operationStatus" class="settings-status" role="status">{{ auth.operationStatus }}</p>
          <p v-if="auth.errorCode" class="settings-status auth-error" role="status">操作暂未完成，请检查网络或授权状态；诊断码 {{ auth.errorCode }}</p>
          <div class="settings-row auth-setting-row">
            <span class="settings-icon"><ShieldCheck :size="16" /></span>
            <div class="settings-copy">
              <strong>硬件数据授权</strong>
            </div>
            <span class="settings-value" :class="{ positive: analysis.system?.consentGranted }">{{ analysis.system?.consentGranted ? "已允许" : "未启用" }}</span>
            <button type="button" class="quiet-button" :disabled="!hardware.hasScanned || analysis.creatingPreview" @click="openAnalysisPreview">
              {{ analysis.creatingPreview ? "准备中…" : "查看发送范围" }}
            </button>
          </div>
        </section>

        <section class="settings-group">
          <div class="category-heading"><h2>本机数据</h2></div>
          <div class="settings-row">
            <span class="settings-icon"><Database :size="17" /></span>
            <div class="settings-copy"><strong>本地档案</strong></div>
            <span class="settings-value" :class="{ positive: app.info?.databaseReady }">{{ app.info?.databaseReady ? "已就绪" : "未就绪" }}</span>
          </div>
          <div class="settings-row">
            <span class="settings-icon"><ShieldCheck :size="17" /></span>
            <div class="settings-copy"><strong>采集权限</strong></div>
            <span class="settings-value">标准用户 · 只读</span>
          </div>
        </section>

        <section class="settings-group">
          <div class="category-heading"><h2>缓存与诊断</h2></div>
          <div class="settings-row">
            <span class="settings-icon"><Database :size="17" /></span>
            <div class="settings-copy">
              <strong>本地缓存</strong>
            </div>
            <span class="settings-value">{{ settings.cacheSummary ? `${settings.cacheSummary.clearableAiItems} 条 AI 与问答记录` : "" }}</span>
          </div>
          <div class="settings-actions">
            <button type="button" class="quiet-button export-diagnostics-button" :disabled="settings.isExportingDiagnostics" @click="exportDiagnostics">
              {{ settings.isExportingDiagnostics ? "正在导出…" : "导出脱敏诊断" }}
            </button>
            <button type="button" class="quiet-button clear-cache-button"
              :disabled="settings.isClearingCache || (settings.cacheSummary?.clearableAiItems ?? 0) === 0"
              @click="pendingCacheClear = true">清理 AI 缓存</button>
          </div>
          <div v-if="pendingCacheClear" class="settings-confirm" role="group" aria-label="确认清理缓存">
            <span>将删除 AI 分析与问答记录，设备档案将保留。</span>
            <button type="button" class="confirm-clear-button" :disabled="settings.isClearingCache" @click="confirmCacheClear">{{ settings.isClearingCache ? "正在清理…" : "确认清理" }}</button>
            <button type="button" class="cancel-clear-button" @click="pendingCacheClear = false">取消</button>
          </div>
          <p v-if="settings.operationStatus" class="settings-status" role="status">{{ settings.operationStatus }}</p>
          <p v-if="settings.error" class="settings-note" role="status">操作暂未完成，请查看本地诊断日志。</p>
        </section>
      </section>

    </main>

    <DeviceDetailDialog v-if="authReady && selectedDevice" :device="selectedDevice" :telemetry="hardware.telemetry" @close="selectedDevice = null" />
    <div v-if="showAnalysisPreview && analysis.preview" class="analysis-preview-backdrop" @click.self="dismissAnalysisPreview">
      <section class="analysis-preview-dialog" role="dialog" aria-modal="true" aria-labelledby="analysis-preview-title">
        <header class="analysis-preview-header">
          <div>
            <h2 id="analysis-preview-title">{{ analysis.system?.consentGranted ? "发送数据范围" : "首次分析数据确认" }}</h2>
          </div>
          <button type="button" class="text-action" @click="dismissAnalysisPreview">关闭</button>
        </header>
        <p class="analysis-preview-intro">核对将发送给 ChatGPT 的设备型号、可用规格与采样状态。设备标识、序列号、网络地址、卷标和用户信息不会发送。</p>
        <details class="preview-scope-details">
          <summary>查看设备范围</summary>
          <ul class="analysis-preview-list" aria-label="发送摘要内容">
          <li v-for="device in previewDevices" :key="device.deviceKey">
            <div class="preview-device-heading">
              <strong>{{ device.name || categoryLabels[device.category as DeviceCategory] || device.category }}</strong>
              <span>{{ device.manufacturer || "" }}</span>
            </div>
          </li>
          <li v-for="metric in previewMetrics.filter((item) => item.deviceKey === 'system')" :key="metric.evidenceKey">
            <div class="preview-device-heading"><strong>{{ metricLabel(metric) }}</strong><span>{{ previewMetricValue(metric) }} · {{ metric.source }}</span></div>
          </li>
          </ul>
        </details>
        <footer class="analysis-preview-actions">
          <button v-if="analysis.system?.consentGranted" type="button" class="text-action" @click="revokeHardwareConsent">撤回硬件授权</button>
          <button v-else type="button" class="text-action" @click="declineAnalysisConsent">暂不授权</button>
          <button type="button" class="quiet-button"
            :disabled="!analysisReady || confirmingAnalysisConsent || initialBatchRunning || analysis.isAnyAiOperationRunning"
            @click="confirmAnalysisPreview">
            {{ confirmingAnalysisConsent ? "正在准备分析…" : initialBatchRunning ? "正在分析设备…" : analysis.system?.consentGranted ? analysis.system.initialAttempted ? "重新分析整机" : "开始首次分析" : "授权并开始分析" }}
          </button>
        </footer>
        <p v-if="analysis.errorCode" class="analysis-error" role="status">操作未完成；诊断码 {{ analysis.errorCode }}</p>
      </section>
    </div>
  </div>
</template>
