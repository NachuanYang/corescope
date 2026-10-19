// @ts-expect-error Node built-ins are available to Vitest, but @types/node is not a frontend dependency.
import { readFileSync } from "node:fs";
// @ts-expect-error Node built-ins are available to Vitest, but @types/node is not a frontend dependency.
import { resolve } from "node:path";
// @ts-expect-error Node built-ins are available to Vitest, but @types/node is not a frontend dependency.
import process from "node:process";
import { mount } from "@vue/test-utils";
import { flushPromises } from "@vue/test-utils";
import { createPinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App.vue";
import { useAnalysisStore } from "./app/stores/analysis";
import { useAuthStore } from "./app/stores/auth";
import { useHardwareStore } from "./app/stores/hardware";
import { useSettingsStore } from "./app/stores/settings";
import DeviceDetailDialog from "./components/DeviceDetailDialog.vue";

const { invokeMock, saveMock } = vi.hoisted(() => ({ invokeMock: vi.fn(), saveMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: saveMock }));

function freshMemoryFrame(subscriptionCount = 1) {
  const observedAtUtc = new Date().toISOString();
  const monitoring = { running: true, foreground: true, subscriptionCount, sampleIntervalMs: 2000, latestObservedAtUtc: observedAtUtc };
  return {
    monitoring,
    samples: [{
      deviceId: null, metricKey: "memory.used.bytes", state: "AVAILABLE", value: 12,
      unit: "byte", source: "GlobalMemoryStatusEx", evidenceLevel: "OS_REPORTED",
      observedAtUtc, reasonCode: null, nativeErrorCode: null, details: {},
    }],
  };
}

describe("CoreScope application shell", () => {
  beforeEach(() => {
    invokeMock.mockReset().mockImplementation(async (command: string) => {
      if (command === "app_info") {
        return { productName: "机枢", version: "0.1.1", databaseReady: true };
      }
      if (command === "get_cache_summary") return {
        databaseBytes: 4096, scanCount: 2, deviceCount: 5, metricHistoryCount: 1,
        aiAnalysisCount: 1, chatMessageCount: 2, chatThreadCount: 1,
        modelKnowledgeCount: 0, priceReferenceCount: 0, clearableAiItems: 4,
      };
      if (command === "clear_ai_cache") return 3;
      return null;
    });
    saveMock.mockReset().mockResolvedValue(null);
  });

  function mountAuthorizedApp(pinia = createPinia(), options: { attachTo?: Element } = {}) {
    const auth = useAuthStore(pinia);
    auth.isInitializing = true;
    auth.connection = {
      accounts: [{ clientId: "oaiapp_one", email: null, displayName: "One", connected: true, planUsageAuthorized: true }],
      activeClientId: "oaiapp_one", connected: true, planUsageAuthorized: true,
    };
    auth.models = [{ slug: "model-one", displayName: "Model One" }];
    auth.selectedModelByClient = { oaiapp_one: "model-one" };
    const analysis = useAnalysisStore(pinia);
    analysis.system = {
      consentGranted: true, consentDecided: true, initialAttempted: true,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    const wrapper = mount(App, { ...options, global: { plugins: [pinia] } });
    auth.isInitializing = false;
    return wrapper;
  }

  it("gives native dropdown options explicit readable colors in the dark WebView theme", () => {
    const stylesheet = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
    const declarations = stylesheet.match(/\.settings-select\s+option\s*\{([^}]*)\}/)?.[1] ?? "";

    expect(declarations).toMatch(/(?:^|;)\s*color:\s*#d0cec8(?:;|$)/i);
    expect(declarations).toMatch(/(?:^|;)\s*background-color:\s*#1a1b1c(?:;|$)/i);
  });

  it("keeps the one-time hardware-summary consent dialog responsive and scrollable", () => {
    const stylesheet = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
    const backdrop = stylesheet.match(/\.analysis-preview-backdrop\s*\{([^}]*)\}/)?.[1] ?? "";
    const dialog = stylesheet.match(/\.analysis-preview-dialog\s*\{([^}]*)\}/)?.[1] ?? "";
    const mobile = stylesheet.match(/@media\s*\(max-width:\s*700px\)\s*\{([\s\S]*?)\n\}/)?.[1] ?? "";

    expect(backdrop).toMatch(/position:\s*fixed/);
    expect(backdrop).toMatch(/inset:\s*0/);
    expect(dialog).toMatch(/max-height:\s*min\(/);
    expect(dialog).toMatch(/overflow-y:\s*auto/);
    expect(mobile).toContain(".analysis-preview-dialog");
    expect(mobile).not.toContain(".chat-preview-dialog");
  });

  it("identifies the product and gates the home behind ChatGPT authorization", () => {
    const wrapper = mount(App, { global: { plugins: [createPinia()] } });

    expect(wrapper.get(".brand-title").text()).toContain("机枢");
    expect(wrapper.get("#auth-gate-title").text()).toBe("连接 ChatGPT");
    expect(wrapper.find(".auth-gate .eyebrow").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("连接 ChatGPT 后，机枢会扫描本机硬件并开始首次分析。");
    expect(wrapper.find(".home-view").exists()).toBe(false);
    expect(wrapper.find(".settings-view").exists()).toBe(false);
    expect(wrapper.text()).toContain("连接 ChatGPT");
    expect(wrapper.text()).not.toContain("Welcome to Tauri + Vue");
  });

  it("does not expose account switching or account creation on the authorization gate", async () => {
    const duplicateState = {
      accounts: [
        { clientId: "oaiapp_old", email: null, displayName: "同一账号", connected: false, planUsageAuthorized: false },
        { clientId: "oaiapp_current", email: null, displayName: "同一账号", connected: true, planUsageAuthorized: false },
      ],
      activeClientId: "oaiapp_current", connected: true, planUsageAuthorized: false,
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_chatgpt_auth_state") return duplicateState;
      return null;
    });
    const wrapper = mount(App, { global: { plugins: [createPinia()] } });
    await flushPromises();

    expect(wrapper.find('select[aria-label="ChatGPT 授权账号"]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain("添加账号");
  });

  it("shows local persistence status from the native application", async () => {
    const wrapper = mountAuthorizedApp();
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("app_info");
    expect(wrapper.text()).not.toContain("本地档案已就绪");
  });

  it("removes usage management UI and does not show a modal after authorization", async () => {
    const wrapper = mountAuthorizedApp();
    await flushPromises();

    expect(wrapper.find(".usage-welcome-dialog").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("管理用量");
    expect(wrapper.text()).not.toContain("方案用量");
    expect(wrapper.text()).not.toContain("用量与授权管理");
    expect(wrapper.text()).not.toContain("使用 ChatGPT 方案");
    expect(invokeMock).not.toHaveBeenCalledWith("open_chatgpt_usage");
  });

  it("keeps the hardware-summary consent dialog free of labels and duplicate snapshot counters", async () => {
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [], edges: [], warnings: [] };
    const preview = {
      token: "preview-system", deviceCount: 11, metricCount: 11,
      snapshot: {
        snapshotId: "snapshot-system-preview", scope: "system",
        capturedAtUtc: "2026-10-15T09:01:00Z", scanAtUtc: inventory.scannedAtUtc,
        devices: [], metrics: [],
      },
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "scan_devices") return inventory;
      if (command === "create_analysis_preview") return preview;
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return { monitoring: { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null }, samples: [] };
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    useAnalysisStore(pinia).system = {
      consentGranted: false, consentDecided: false, initialAttempted: false,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    useHardwareStore(pinia).snapshot = inventory;
    await flushPromises();

    const dialog = wrapper.get('.analysis-preview-dialog[role="dialog"]');
    expect(invokeMock).toHaveBeenCalledWith("create_analysis_preview", { scope: "system" });
    expect(dialog.find(".eyebrow").exists()).toBe(false);
    expect(dialog.find(".analysis-preview-count").exists()).toBe(false);
    expect(dialog.text()).not.toContain("11 个设备");
    expect(dialog.text()).not.toContain("2026/10/15");
  });

  it("starts the first analysis on the consent click using a fresh snapshot", async () => {
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [], edges: [], warnings: [] };
    const targetState = {
      consentGranted: false, consentDecided: false, initialAttempted: false,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    let previewCount = 0;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "scan_devices") return inventory;
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return { monitoring: { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null }, samples: [] };
      if (command === "create_analysis_preview") {
        previewCount += 1;
        return {
          token: `first-run-preview-${previewCount}`, deviceCount: 0, metricCount: 0,
          snapshot: { snapshotId: `first-run-snapshot-${previewCount}`, scope: "system", capturedAtUtc: inventory.scannedAtUtc, scanAtUtc: inventory.scannedAtUtc, devices: [], metrics: [] },
        };
      }
      if (command === "set_ai_data_consent") return true;
      if (command === "start_initial_system_analysis") return {
        status: "COMPLETED", analysisId: "first-run-analysis", errorCode: null,
        state: { ...targetState, consentGranted: true, consentDecided: true, initialAttempted: true, latestAttemptStatus: "COMPLETED" }, deviceAnalysis: null,
      };
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    analysis.system = { ...targetState };
    await flushPromises();
    expect(wrapper.find(".analysis-preview-dialog").exists()).toBe(true);
    expect(wrapper.get(".analysis-preview-actions .quiet-button").attributes("disabled")).toBeUndefined();

    await wrapper.get(".analysis-preview-actions .quiet-button").trigger("click");
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("set_ai_data_consent", { granted: true });
    expect(invokeMock).toHaveBeenCalledWith("start_initial_system_analysis", {
      previewToken: "first-run-preview-2", clientId: "oaiapp_one", modelSlug: "model-one", route: "system_proxy",
    });
    expect(previewCount).toBe(2);
    expect(wrapper.find(".analysis-preview-dialog").exists()).toBe(false);
    expect(wrapper.find(".analysis-progress").exists()).toBe(false);
  });

  it("shows task-based percentage progress during a long analysis batch", async () => {
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    const hardware = useHardwareStore(pinia);
    await flushPromises();
    analysis.system!.latestAttemptStatus = "COMPLETED";
    hardware.snapshot = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [], edges: [], warnings: [] };
    await flushPromises();
    analysis.beginInitialBatch(4);
    analysis.updateInitialBatchProgress(2, 4, "处理器", "正在分析");
    await flushPromises();

    expect(wrapper.get(".analysis-progress-copy").text()).toContain("处理器");
    expect(wrapper.get(".analysis-progress-copy strong").text()).toBe("50%");
    expect(wrapper.get(".analysis-progress-bar").attributes("value")).toBe("50");
  });

  it("shows the processed device count separately from the whole-machine task", async () => {
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    const hardware = useHardwareStore(pinia);
    await flushPromises();
    hardware.snapshot = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [], edges: [], warnings: [] };
    analysis.beginInitialBatch(4);
    analysis.updateInitialBatchProgress(2, 4, "处理器", "正在分析");
    await flushPromises();

    expect(wrapper.get(".analysis-progress small").text()).toBe("1 / 3 台设备已处理 · 整机任务单独计入完成度");
  });

  it("does not duplicate the running whole-machine status", async () => {
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    const message = "整机 AI 分析正在运行；本机监测仍会继续";
    await flushPromises();
    analysis.operationStatus = message;
    analysis.beginInitialBatch(1);
    useHardwareStore(pinia).snapshot = {
      scannedAtUtc: "2026-10-15T09:00:00Z", devices: [], edges: [], warnings: [],
    };
    await flushPromises();

    const statusCopies = wrapper.findAll(".analysis-section .analysis-empty, .analysis-section .analysis-operation")
      .filter((node) => node.text() === message);
    expect(statusCopies).toHaveLength(1);
  });

  it("keeps batch progress monotonic and avoids telemetry timeout while analysis is running", async () => {
    const cpu = {
      id: "cpu_progress_stability", identityHash: "cpu-progress-stability", identityConfidence: "STRONG" as const,
      category: "cpu" as const, kind: "cpu", layer: "physical_device" as const,
      displayName: "Progress Stability CPU", manufacturer: "Example", isVirtual: false, isPresent: true,
      source: "Windows", attributes: {},
    };
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
    const pendingState = {
      consentGranted: true, consentDecided: true, initialAttempted: false,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    const completedSystemState = { ...pendingState, initialAttempted: true, latestAttemptStatus: "COMPLETED" };
    const completedDeviceState = { ...pendingState, initialAttempted: true, latestAttemptStatus: "COMPLETED" };
    let finishDeviceAnalysis: ((outcome: unknown) => void) | undefined;
    invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "load_inventory") return inventory;
      if (command === "start_monitoring") return {
        running: true, foreground: true, subscriptionCount: 1, sampleIntervalMs: 2000,
        latestObservedAtUtc: new Date().toISOString(),
      };
      if (command === "load_telemetry") return freshMemoryFrame(1);
      if (command === "get_analysis_target_state") return pendingState;
      if (command === "create_analysis_preview") return {
        token: `preview-${String(args?.scope)}-${String(args?.targetDeviceId ?? "system")}`,
        deviceCount: 1, metricCount: 1,
        snapshot: {
          snapshotId: `snapshot-${String(args?.scope)}-${String(args?.targetDeviceId ?? "system")}`,
          scope: args?.scope ?? "system", capturedAtUtc: inventory.scannedAtUtc,
          scanAtUtc: inventory.scannedAtUtc, devices: [], metrics: [],
        },
      };
      if (command === "start_initial_system_analysis") return {
        status: "COMPLETED", analysisId: "system-progress-stability", errorCode: null,
        state: completedSystemState, deviceAnalysis: null,
      };
      if (command === "start_initial_device_analysis") return new Promise((resolve) => {
        finishDeviceAnalysis = resolve;
      });
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    const hardware = useHardwareStore(pinia);
    analysis.system = { ...pendingState };
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("start_initial_device_analysis", expect.objectContaining({ deviceId: cpu.id }));
    expect(analysis.initialBatchProgress.completed).toBe(1);
    expect(wrapper.get(".analysis-progress small").text()).toBe("0 / 1 台设备已处理 · 整机任务单独计入完成度");

    const timeoutSpy = vi.spyOn(window, "setTimeout");
    hardware.telemetryWarmupReady = false;
    await flushPromises();

    expect(analysis.initialBatchProgress.completed).toBe(1);
    expect(timeoutSpy).not.toHaveBeenCalled();
    expect(analysis.errorCode).toBe("");
    expect(wrapper.get(".analysis-progress small").text()).toBe("0 / 1 台设备已处理 · 整机任务单独计入完成度");

    finishDeviceAnalysis?.({
      status: "COMPLETED", analysisId: "device-progress-stability", errorCode: null,
      state: completedDeviceState, deviceAnalysis: null,
    });
    await flushPromises();

    expect(analysis.initialBatchProgress.completed).toBe(2);
    expect(analysis.errorCode).toBe("");
    expect(analysis.operationStatus).toContain("1 台完成 · 0 台未完成");
    expect(wrapper.find(".analysis-progress").exists()).toBe(false);
    timeoutSpy.mockRestore();
    wrapper.unmount();
  });

  it("keeps the home focused on the device list and folds long system analysis details", async () => {
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    await flushPromises();

    useHardwareStore(pinia).snapshot = {
      scannedAtUtc: "2026-10-14T09:00:00Z", devices: [], edges: [], warnings: [],
    };
    useAnalysisStore(pinia).system = {
      consentGranted: true,
      consentDecided: true,
      initialAttempted: true,
      latestAttemptStatus: "COMPLETED",
      latestAnalysis: {
        id: "analysis-system", scope: "system", targetDeviceId: null,
        inputFingerprint: "fingerprint", modelSlug: "model-one",
        evidenceSnapshot: {
          snapshotId: "snapshot-system", scope: "system",
          capturedAtUtc: "2026-10-14T09:00:00Z", scanAtUtc: "2026-10-14T08:59:00Z",
          devices: [], metrics: [],
        },
        assessment: {
          scope: "system", summary: "整机摘要保留在首页首屏。", strengths: ["设备清单完整"],
          concerns: [{ claim: "部分指标无法读取", evidenceKeys: [], certainty: "observed" }],
          suggestions: [], limitations: [], evidenceSnapshotId: "snapshot-system",
          generatedAtUtc: "2026-10-14T09:00:00Z", modelSlug: "model-one", provenance: "AI_INTERPRETATION",
        },
        createdAtUtc: "2026-10-14T09:00:00Z", status: "COMPLETED", isHistorical: false, errorCode: null,
      },
    };
    await wrapper.vm.$nextTick();

    const analysis = wrapper.get(".analysis-section");
    expect(analysis.get(".analysis-summary").text()).toBe("整机摘要保留在首页首屏。");
    expect(analysis.get(".analysis-full").element.tagName).toBe("DETAILS");
    expect((analysis.get(".analysis-full").element as HTMLDetailsElement).open).toBe(false);
    expect(analysis.get(".analysis-full").text()).toContain("完整解读与依据");
    expect(analysis.get(".analysis-full").text()).toContain("设备清单完整");

    useAnalysisStore(pinia).system!.latestAnalysis!.assessment!.prioritySections = [
      { key: "performance_evaluation", items: [] },
      { key: "health_assessment", items: [{ text: "当前状态不能完整判断。", evidenceKeys: [], certainty: "unknown" }] },
      { key: "maintenance_plan", items: [] },
      { key: "upgrade_recommendation", items: [] },
      { key: "other", items: [] },
    ];
    await wrapper.vm.$nextTick();
    expect(analysis.findAll(".analysis-priority-section h3").map((heading) => heading.text())).toEqual([
      "设备状态正常评估",
    ]);
    expect(wrapper.get(".assistant-section h2").text()).toBe("设备问答");
  });

  it("starts a local scan and renders discovered devices as a flat list", async () => {
    const snapshot = {
      scannedAtUtc: "2026-10-14T09:00:00Z",
      devices: [{
        id: "dev_cpu", identityHash: "hash", identityConfidence: "STRONG", category: "cpu",
        kind: "cpu", layer: "physical_device", displayName: "AMD Ryzen 7 7800X3D",
        manufacturer: "AMD", isVirtual: false, isPresent: true, source: "WMI", attributes: {},
      }],
      edges: [], warnings: [],
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return snapshot;
      return null;
    });
    const wrapper = mountAuthorizedApp();
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("scan_devices");
    expect(wrapper.text()).toContain("AMD Ryzen 7 7800X3D");
    expect(wrapper.findAll(".device-row")).toHaveLength(1);
    expect(wrapper.get(".device-index").text()).toBe("01");
    expect(wrapper.find(".device-icon").exists()).toBe(false);
    expect(wrapper.findAll(".device-card")).toHaveLength(0);
    expect(wrapper.get(".device-groups").attributes("role")).toBe("list");
    expect(wrapper.get(".device-row").attributes("role")).toBeUndefined();
    expect(wrapper.get(".device-row").element.tagName).toBe("BUTTON");
    expect(wrapper.get(".device-row").element.parentElement?.tagName).toBe("LI");
    expect(wrapper.get(".assistant-section").text()).toContain("设备问答");
  });

  it("shows only physical hardware and omits volumes and virtual devices from the main inventory", async () => {
    const physicalCpu = {
      id: "physical_cpu", identityHash: "cpu-hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Fixture CPU",
      manufacturer: "Fixture", isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const physicalDisk = {
      id: "physical_disk", identityHash: "disk-hash", identityConfidence: "STRONG", category: "storage",
      kind: "physical_disk", layer: "physical_device", displayName: "Fixture SSD",
      manufacturer: "Fixture", isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const volume = {
      ...physicalDisk, id: "logical_volume", identityHash: "volume-hash", kind: "volume",
      layer: "logical_device", displayName: "C: (系统)",
    };
    const virtualAdapter = {
      ...physicalCpu, id: "virtual_adapter", identityHash: "virtual-hash", category: "network",
      kind: "network_adapter", layer: "logical_device", displayName: "Virtual Adapter", isVirtual: true,
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return {
        scannedAtUtc: "2026-10-14T09:00:00Z",
        devices: [physicalCpu, physicalDisk, volume, virtualAdapter], edges: [], warnings: [],
      };
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 2, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(2);
      return null;
    });
    const wrapper = mountAuthorizedApp();
    await flushPromises();

    expect(wrapper.text()).toContain("Fixture CPU");
    expect(wrapper.text()).toContain("Fixture SSD");
    expect(wrapper.text()).not.toContain("C: (系统)");
    expect(wrapper.text()).not.toContain("Virtual Adapter");
    expect(wrapper.findAll(".device-row")).toHaveLength(2);
  });

  it("keeps hardware questions on the whole machine and accepts a hardware name in the prompt", async () => {
    const wrapper = mountAuthorizedApp();
    await flushPromises();

    expect(wrapper.find(".chat-context-row").exists()).toBe(false);
    expect(wrapper.find('select[aria-label="问答范围"]').exists()).toBe(false);
    expect(wrapper.find('select[aria-label="问答硬件"]').exists()).toBe(false);
    expect(wrapper.find(".assistant-status").exists()).toBe(false);
    expect(wrapper.get('[aria-label="设备问答内容"]').attributes("placeholder")).toContain("硬件名称");
    expect(wrapper.find('button[aria-label="新建本地对话"]').exists()).toBe(true);
  });

  it("labels old live values as historical and explains why they are not current", async () => {
    const cpu = {
      id: "cpu_stale", identityHash: "hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Fixture CPU",
      manufacturer: "Example", isVirtual: false, isPresent: true, source: "Windows Processor Provider", attributes: {},
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 1, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return {
        monitoring: { running: true, foreground: true, subscriptionCount: 1, sampleIntervalMs: 2000, latestObservedAtUtc: "2026-10-15T08:59:00Z" },
        samples: [...freshMemoryFrame(1).samples, { deviceId: cpu.id, metricKey: "cpu.utilization.percent", state: "STALE", value: null,
          unit: "percent", source: "PDH", evidenceLevel: "MEASURED", observedAtUtc: "2026-10-15T08:59:00Z",
          reasonCode: "APP_RESTARTED_LIVE_DATA_NOT_CURRENT", nativeErrorCode: null, details: {} }],
      };
      return null;
    });
    const wrapper = mountAuthorizedApp();
    await flushPromises();

    expect(wrapper.get(".device-readout").text()).toContain("历史读数");
    expect(wrapper.get(".device-readout").text()).not.toContain("已过期");
    expect(wrapper.get(".device-readout").attributes("title")).toContain("应用重启后需重新采样");
  });

  it("automatically analyzes eligible hardware once after consent and omits virtual, partition and PnP entries", async () => {
    const cpu = {
      id: "cpu_auto", identityHash: "cpu-hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Auto CPU",
      manufacturer: "Example", isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const disk = {
      id: "disk_auto", identityHash: "disk-hash", identityConfidence: "STRONG", category: "storage",
      kind: "physical_disk", layer: "physical_device", displayName: "Auto SSD",
      manufacturer: "Example", isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const excluded = [
      { ...cpu, id: "gpu_virtual", category: "gpu", kind: "gpu_adapter", layer: "logical_device", isVirtual: true, displayName: "Virtual GPU" },
      { ...disk, id: "partition_auto", kind: "partition", layer: "logical_device", displayName: "Hidden partition" },
      { ...cpu, id: "pnp_auto", category: "other", kind: "pnp_node", layer: "pnp_node", displayName: "Hidden PnP node" },
    ];
    const devices = [cpu, disk, ...excluded];
    const snapshot = { scannedAtUtc: "2026-10-15T09:00:00Z", devices, edges: [], warnings: [] };
    const targetState = {
      consentGranted: true, consentDecided: true, initialAttempted: false,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "load_inventory") return null;
      if (command === "scan_devices") return snapshot;
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 2, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(2);
      if (command === "get_analysis_target_state") return targetState;
      if (command === "create_analysis_preview") {
        const scope = args?.scope;
        const targetDeviceId = args?.targetDeviceId;
        return {
          token: `preview-${scope}-${targetDeviceId ?? "system"}`,
          snapshot: { snapshotId: `snapshot-${scope}-${targetDeviceId ?? "system"}`, scope, capturedAtUtc: "2026-10-15T09:00:00Z", scanAtUtc: snapshot.scannedAtUtc, devices: [], metrics: [] },
          deviceCount: scope === "device" ? 1 : 2, metricCount: 0,
        };
      }
      if (command === "start_initial_system_analysis") return {
        status: "COMPLETED", analysisId: "initial-system", errorCode: null,
        state: { ...targetState, initialAttempted: true }, deviceAnalysis: null,
      };
      if (command === "start_initial_device_analysis") return {
        status: args?.deviceId === disk.id ? "FAILED" : "COMPLETED",
        analysisId: `initial-${args?.deviceId}`, errorCode: args?.deviceId === disk.id ? "AI_REQUEST_FAILED" : null,
        state: { ...targetState, initialAttempted: true }, deviceAnalysis: null,
      };
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    useAnalysisStore(pinia).system!.initialAttempted = false;
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("start_initial_system_analysis", expect.anything());
    const analyzedDeviceIds = invokeMock.mock.calls
      .filter(([command]) => command === "start_initial_device_analysis")
      .map(([, args]) => args.deviceId);
    expect(analyzedDeviceIds).toEqual([cpu.id, disk.id]);
    expect(wrapper.text()).toContain("Auto CPU");
    expect(wrapper.text()).toContain("Auto SSD");
    expect(wrapper.text()).not.toContain("Virtual GPU");
    expect(wrapper.text()).not.toContain("Hidden partition");
    expect(wrapper.text()).not.toContain("Hidden PnP node");
    expect(wrapper.get(".analysis-operation").text()).toContain("1 台完成 · 1 台未完成");
  });

  it("does not retry an interrupted device analysis until the user explicitly requests it", async () => {
    const cpu = {
      id: "cpu_retry", identityHash: "cpu-retry-hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Retry CPU", manufacturer: "Example",
      isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
    const systemState = { consentGranted: true, consentDecided: true, initialAttempted: false, latestAnalysis: null, latestAttemptStatus: null };
    const deviceState = { ...systemState };
    let previewNumber = 0;
    let deviceStartNumber = 0;
    invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "load_inventory") return inventory;
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 1, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(1);
      if (command === "get_analysis_target_state") return args?.scope === "device" ? deviceState : systemState;
      if (command === "create_analysis_preview") return {
        token: `retry-preview-${++previewNumber}`,
        snapshot: { snapshotId: `retry-snapshot-${previewNumber}`, scope: args?.scope, capturedAtUtc: inventory.scannedAtUtc, scanAtUtc: inventory.scannedAtUtc, devices: [], metrics: [] },
        deviceCount: 1, metricCount: 0,
      };
      if (command === "start_initial_system_analysis") return {
        status: "COMPLETED", analysisId: "retry-system", errorCode: null,
        state: { ...systemState, initialAttempted: true, latestAttemptStatus: "COMPLETED" }, deviceAnalysis: null,
      };
      if (command === "start_initial_device_analysis") {
        deviceStartNumber += 1;
        return {
          status: "INTERRUPTED", analysisId: "retry-device-first", errorCode: "AUTH_SSE_STREAM_INTERRUPTED",
          state: { ...deviceState, initialAttempted: true, latestAttemptStatus: "INTERRUPTED" }, deviceAnalysis: null,
        };
      }
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    useAnalysisStore(pinia).system = { ...systemState };
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("start_initial_device_analysis", expect.objectContaining({ deviceId: cpu.id }));
    expect(deviceStartNumber).toBe(1);
    expect(invokeMock).not.toHaveBeenCalledWith("refresh_device_analysis", expect.anything());
    expect(previewNumber).toBe(2);
    expect(wrapper.get(".analysis-operation").text()).toContain("0 台完成 · 1 台未完成");
  });

  it("does not reanalyze failed attempts automatically after startup", async () => {
    const cpu = {
      id: "cpu_failed_startup", identityHash: "cpu-failed-hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Previously Failed CPU", manufacturer: "Example",
      isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
    const failedState = {
      consentGranted: true, consentDecided: true, initialAttempted: true,
      latestAnalysis: null, latestAttemptStatus: "FAILED",
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "load_inventory") return inventory;
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 1, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(1);
      if (command === "get_analysis_target_state") return failedState;
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    useAnalysisStore(pinia).system = { ...failedState };
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("get_analysis_target_state", { scope: "device", targetDeviceId: cpu.id });
    expect(invokeMock).not.toHaveBeenCalledWith("create_analysis_preview", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("start_initial_system_analysis", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("refresh_system_analysis", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("start_initial_device_analysis", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("refresh_device_analysis", expect.anything());
    expect(wrapper.find(".analysis-progress").exists()).toBe(false);
    expect(wrapper.get(".analysis-state").text()).toBe("待重试");
  });

  it("does not repeat a completed whole-machine analysis on startup", async () => {
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [], edges: [], warnings: [] };
    const completedAnalysis = {
      id: "completed-system-analysis", scope: "system" as const, targetDeviceId: null,
      inputFingerprint: "a".repeat(64),
      evidenceSnapshot: {
        snapshotId: "completed-system-snapshot", scope: "system" as const,
        capturedAtUtc: inventory.scannedAtUtc, scanAtUtc: inventory.scannedAtUtc, devices: [], metrics: [],
      },
      assessment: {
        scope: "system" as const, summary: "这是已经保存的整机分析。", prioritySections: [],
        strengths: [], concerns: [], suggestions: [], limitations: [],
        evidenceSnapshotId: "completed-system-snapshot", generatedAtUtc: inventory.scannedAtUtc,
        modelSlug: "model-one", provenance: "AI_INTERPRETATION" as const,
      },
      modelSlug: "model-one", createdAtUtc: inventory.scannedAtUtc,
      status: "COMPLETED" as const, isHistorical: false, errorCode: null,
    };
    const completedState = {
      consentGranted: true, consentDecided: true, initialAttempted: true,
      latestAnalysis: completedAnalysis, latestAttemptStatus: "COMPLETED",
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "load_inventory") return inventory;
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    useAnalysisStore(pinia).system = completedState;
    await flushPromises();

    expect(wrapper.get(".analysis-state").text()).toBe("已保存");
    expect(wrapper.text()).toContain("这是已经保存的整机分析。");
    expect(invokeMock).not.toHaveBeenCalledWith("create_analysis_preview", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("start_initial_system_analysis", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("refresh_system_analysis", expect.anything());
    expect(wrapper.find(".analysis-progress").exists()).toBe(false);
  });

  it("does not submit an initial batch when the fresh telemetry warmup fails", async () => {
    const cpu = {
      id: "cpu_no_warmup", identityHash: "cpu-no-warmup", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Warmup CPU", manufacturer: "Example",
      isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
    const pendingState = {
      consentGranted: true, consentDecided: true, initialAttempted: false,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    const unavailableMonitoring = {
      running: false, foreground: false, subscriptionCount: 1,
      sampleIntervalMs: 2000, latestObservedAtUtc: null,
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "load_inventory") return inventory;
      if (command === "start_monitoring") return unavailableMonitoring;
      if (command === "load_telemetry") return { monitoring: unavailableMonitoring, samples: [] };
      if (command === "get_analysis_target_state") return pendingState;
      if (command === "create_analysis_preview") return {
        token: "should-not-be-created", deviceCount: 1, metricCount: 0,
        snapshot: { snapshotId: "no-warmup", scope: "system", capturedAtUtc: inventory.scannedAtUtc, scanAtUtc: inventory.scannedAtUtc, devices: [], metrics: [] },
      };
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    analysis.system = { ...pendingState };
    await flushPromises();

    expect(analysis.errorCode).toBe("");
    expect(analysis.operationStatus).toContain("等待本轮实时样本");
    expect(wrapper.get(".analysis-progress").text()).toContain("等待实时样本");
    expect(invokeMock).not.toHaveBeenCalledWith("create_analysis_preview", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("start_initial_system_analysis", expect.anything());
    expect(wrapper.find(".analysis-progress").exists()).toBe(true);
  });

  it("saves first consent once and automatically starts when telemetry becomes ready", async () => {
    const cpu = {
      id: "cpu_unavailable_consent", identityHash: "cpu-unavailable", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Unavailable CPU", manufacturer: "Example",
      isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const inventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
    const pendingState = {
      consentGranted: false, consentDecided: false, initialAttempted: false,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    const unavailableMonitoring = {
      running: false, foreground: false, subscriptionCount: 1,
      sampleIntervalMs: 2000, latestObservedAtUtc: null,
    };
    let freshTelemetry = false;
    let previewCount = 0;
    let finishConsent: ((granted: boolean) => void) | undefined;
    invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "load_inventory") return inventory;
      if (command === "start_monitoring") return unavailableMonitoring;
      if (command === "load_telemetry") return freshTelemetry
        ? freshMemoryFrame(1)
        : { monitoring: unavailableMonitoring, samples: [] };
      if (command === "get_analysis_target_state") return pendingState;
      if (command === "create_analysis_preview") {
        previewCount += 1;
        return {
          token: `consent-preview-${args?.scope}-${previewCount}`, deviceCount: 1, metricCount: 0,
          snapshot: { snapshotId: `consent-snapshot-${previewCount}`, scope: args?.scope ?? "system", capturedAtUtc: inventory.scannedAtUtc, scanAtUtc: inventory.scannedAtUtc, devices: [], metrics: [] },
        };
      }
      if (command === "set_ai_data_consent") {
        return new Promise<boolean>((resolve) => { finishConsent = resolve; });
      }
      if (command === "start_initial_system_analysis") return {
        status: "COMPLETED", analysisId: "first-system-analysis", errorCode: null,
        state: { ...pendingState, consentGranted: true, consentDecided: true, initialAttempted: true, latestAttemptStatus: "COMPLETED" }, deviceAnalysis: null,
      };
      if (command === "start_initial_device_analysis") return {
        status: "COMPLETED", analysisId: "first-device-analysis", errorCode: null,
        state: { ...pendingState, consentGranted: true, consentDecided: true, initialAttempted: true, latestAttemptStatus: "COMPLETED" }, deviceAnalysis: null,
      };
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    analysis.system = { ...pendingState };
    await flushPromises();
    expect(wrapper.find(".analysis-preview-dialog").exists()).toBe(true);
    expect(wrapper.get(".analysis-preview-actions .quiet-button").attributes("disabled")).toBeUndefined();

    const consentButton = wrapper.get(".analysis-preview-actions .quiet-button");
    const firstConsentClick = consentButton.trigger("click");
    await flushPromises();

    expect(consentButton.attributes("disabled")).toBeDefined();
    await consentButton.trigger("click");
    expect(invokeMock.mock.calls.filter(([command]) => command === "set_ai_data_consent")).toHaveLength(1);
    finishConsent?.(true);
    await firstConsentClick;
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("set_ai_data_consent", { granted: true });
    expect(analysis.system?.consentGranted).toBe(true);
    expect(wrapper.find(".analysis-preview-dialog").exists()).toBe(false);
    expect(wrapper.get(".analysis-progress").text()).toContain("等待实时样本");
    expect(invokeMock).not.toHaveBeenCalledWith("start_initial_system_analysis", expect.anything());

    freshTelemetry = true;
    await useHardwareStore(pinia).refreshTelemetry();
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("start_initial_system_analysis", {
      previewToken: "consent-preview-system-2", clientId: "oaiapp_one", modelSlug: "model-one", route: "system_proxy",
    });
    expect(analysis.errorCode).toBe("");
  });

  it("scans first and refreshes the whole-machine analysis from the new snapshot with one action", async () => {
    const cpu = {
      id: "cpu_full_scan", identityHash: "cpu-full-scan-hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Fresh CPU", manufacturer: "Example",
      isVirtual: false, isPresent: true, source: "Windows", attributes: {},
    };
    const firstInventory = { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
    const newInventory = { ...firstInventory, scannedAtUtc: "2026-10-16T09:00:00Z" };
    const targetState = {
      consentGranted: true, consentDecided: true, initialAttempted: true,
      latestAnalysis: null, latestAttemptStatus: "COMPLETED",
    };
    let deviceStateCalls = 0;
    invokeMock.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "load_inventory") return firstInventory;
      if (command === "scan_devices") return newInventory;
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(1);
      if (command === "get_analysis_target_state") {
        if (args?.scope === "device") {
          deviceStateCalls += 1;
          return targetState;
        }
        return targetState;
      }
      if (command === "create_analysis_preview") return {
        token: `fresh-${args?.scope}-preview`, snapshot: { snapshotId: `fresh-${args?.scope}-snapshot`, scope: args?.scope, capturedAtUtc: newInventory.scannedAtUtc, scanAtUtc: newInventory.scannedAtUtc, devices: [], metrics: [] }, deviceCount: 1, metricCount: 0,
      };
      if (command === "refresh_system_analysis") return {
        status: "COMPLETED", analysisId: "fresh-system-analysis", errorCode: null, state: targetState, deviceAnalysis: null,
      };
      if (command === "refresh_device_analysis") return {
        status: "COMPLETED", analysisId: "fresh-device-analysis", errorCode: null, state: targetState, deviceAnalysis: null,
      };
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia);
    const analysis = useAnalysisStore(pinia);
    analysis.system = { ...targetState };
    analysis.devices[cpu.id] = { ...targetState };
    await flushPromises();

    const scanButton = wrapper.get(".scan-actions .primary-button");
    expect(scanButton.text()).toContain("扫描并分析整机");
    await scanButton.trigger("click");
    await flushPromises();

    const commands = invokeMock.mock.calls.map(([command]) => command);
    const scanIndex = commands.lastIndexOf("scan_devices");
    const systemPreviewIndex = invokeMock.mock.calls.findIndex(([command, args]) => command === "create_analysis_preview" && args?.scope === "system");
    const systemAnalysisIndex = commands.lastIndexOf("refresh_system_analysis");
    const devicePreviewIndex = invokeMock.mock.calls.findIndex(([command, args]) => command === "create_analysis_preview" && args?.scope === "device");
    const deviceAnalysisIndex = commands.lastIndexOf("refresh_device_analysis");
    expect(scanIndex).toBeGreaterThanOrEqual(0);
    expect(systemPreviewIndex).toBeGreaterThan(scanIndex);
    expect(systemAnalysisIndex).toBeGreaterThan(systemPreviewIndex);
    expect(devicePreviewIndex).toBeGreaterThan(systemAnalysisIndex);
    expect(deviceAnalysisIndex).toBeGreaterThan(devicePreviewIndex);
    expect(invokeMock).toHaveBeenCalledWith("refresh_system_analysis", expect.objectContaining({ previewToken: "fresh-system-preview" }));
    expect(invokeMock).toHaveBeenCalledWith("refresh_device_analysis", expect.objectContaining({ deviceId: cpu.id, previewToken: "fresh-device-preview" }));
    expect(deviceStateCalls).toBe(1);
  });

  it("lets the user cancel an in-progress hardware scan", async () => {
    let rejectScan: ((reason?: unknown) => void) | undefined;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") {
        return await new Promise((_, reject) => { rejectScan = reject; });
      }
      if (command === "cancel_scan") {
        rejectScan?.(new Error("SCAN_CANCELLED"));
        return null;
      }
      return null;
    });
    const wrapper = mountAuthorizedApp();
    await flushPromises();
    await wrapper.vm.$nextTick();
    expect(wrapper.get(".primary-button").text()).toContain("正在扫描");
    await wrapper.get(".scan-cancel-button").trigger("click");
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("cancel_scan");
    expect(wrapper.get(".primary-button").text()).toContain("扫描此电脑");
    expect(wrapper.find(".scan-cancel-button").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("扫描暂不可用");
  });

  it("treats a late scan cancellation as a completed scan", async () => {
    const snapshot = { scannedAtUtc: "2026-10-14T09:00:00Z", devices: [], edges: [], warnings: [] };
    let resolveScan: ((value: typeof snapshot) => void) | undefined;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return await new Promise<typeof snapshot>((resolve) => { resolveScan = resolve; });
      if (command === "cancel_scan") {
        resolveScan?.(snapshot);
        throw new Error("SCAN_NOT_RUNNING");
      }
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(1);
      return null;
    });
    const wrapper = mountAuthorizedApp();
    await flushPromises();
    await wrapper.vm.$nextTick();
    await wrapper.get(".scan-cancel-button").trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("0 个设备");
    expect(wrapper.text()).not.toContain("扫描暂不可用");
  });

  it("shows a retry message after a native scan timeout", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") throw new Error("SCAN_TIMEOUT");
      return null;
    });
    const wrapper = mountAuthorizedApp();
    await flushPromises();

    expect(wrapper.text()).toContain("扫描耗时过长，请稍后重试。");
    expect(wrapper.get(".primary-button").attributes("disabled")).toBeUndefined();
  });

  it("explains when the timed-out native worker is still finishing before a retry", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") throw new Error("SCAN_ALREADY_RUNNING");
      return null;
    });
    const wrapper = mountAuthorizedApp();

    await flushPromises();

    expect(wrapper.text()).toContain("上一轮扫描仍在结束，请稍后重试。");
  });

  it("sends a home-page question from the inline composer without a confirmation modal", async () => {
    const cpu = {
      id: "dev_cpu", identityHash: "hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Test CPU", manufacturer: "Example",
      isVirtual: false, isPresent: true, source: "Windows inventory", attributes: {},
    };
    const thread = {
      id: "thread-system", scope: "system", targetDeviceId: null,
      createdAtUtc: "2026-10-14T10:00:00Z", updatedAtUtc: "2026-10-14T10:00:00Z",
    };
    const question = "这台电脑的温度数据能读取到吗？";
    const preview = {
      token: "chat-preview-token", scope: "system", threadId: thread.id, question,
      snapshot: {
        snapshotId: "chat-snapshot", scope: "system", capturedAtUtc: "2026-10-14T10:00:00Z",
        scanAtUtc: "2026-10-14T09:59:00Z",
        devices: [{ deviceKey: "hardware-1", evidenceKey: "hardware-1.kind", category: "cpu", kind: "cpu",
          name: "Test CPU", manufacturer: "Example", virtualDevice: false, source: "Windows inventory", attributes: {} }],
        metrics: [{ evidenceKey: "metric.hardware-1.cpu.temperature.celsius", deviceKey: "hardware-1",
          metricKey: "cpu.temperature.celsius", state: "NOT_TESTED", value: null, unit: "°C",
          source: "capability registry", evidenceLevel: null, observedAtUtc: null, window: null }],
      },
      recentTurns: [],
    };
    const authState = {
      accounts: [{ clientId: "oaiapp_one", email: null, displayName: "One", connected: true, planUsageAuthorized: true }],
      activeClientId: "oaiapp_one", connected: true, planUsageAuthorized: true,
    };
    window.localStorage.setItem("corescope.auth.models", JSON.stringify({ oaiapp_one: "model-one" }));
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "get_chatgpt_auth_state") return authState;
      if (command === "list_chatgpt_models") return [{ slug: "model-one", displayName: "Model One" }];
      if (command === "load_inventory") return null;
      if (command === "scan_devices") return { scannedAtUtc: "2026-10-14T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(1);
      if (command === "get_analysis_target_state") return { consentGranted: true, consentDecided: true, initialAttempted: true, latestAnalysis: null, latestAttemptStatus: null };
      if (command === "set_ai_data_consent") return true;
      if (command === "list_chat_threads") return [];
      if (command === "create_chat_thread") return thread;
      if (command === "create_chat_preview") return preview;
      if (command === "send_chat_message") return { status: "COMPLETED", userMessageId: "message-1", assistantMessage: null, errorCode: null };
      if (command === "load_chat_messages") return [];
      return null;
    });
    const wrapper = mount(App, { global: { plugins: [createPinia()] } });
    await flushPromises();

    await wrapper.get(".primary-button").trigger("click");
    await flushPromises();
    await wrapper.get('[aria-label="设备问答内容"]').setValue(question);
    await wrapper.get(".question-form").trigger("submit");
    await flushPromises();

    expect(wrapper.find('[role="dialog"]').exists()).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith("create_chat_preview", {
      scope: "system", threadId: thread.id, question,
    });
    expect(invokeMock).toHaveBeenCalledWith("send_chat_message", {
      previewToken: "chat-preview-token", scope: "system", threadId: thread.id,
      clientId: "oaiapp_one", modelSlug: "model-one", route: "system_proxy",
    });
    const previewIndex = invokeMock.mock.calls.findIndex(([command]) => command === "create_chat_preview");
    const sendIndex = invokeMock.mock.calls.findIndex(([command]) => command === "send_chat_message");
    expect(previewIndex).toBeGreaterThanOrEqual(0);
    expect(previewIndex).toBeLessThan(sendIndex);
  });

  it("uses a compact top switch for devices and settings instead of a sidebar", async () => {
    const wrapper = mountAuthorizedApp();

    expect(wrapper.find(".sidebar").exists()).toBe(false);
    expect(wrapper.findAll("[role=tab]").map((tab) => tab.text())).toEqual(["设备", "设置"]);

    await wrapper.get("[role=tab]:nth-child(2)").trigger("click");
    expect(wrapper.get("h1").text()).toBe("设置");
    expect(wrapper.text()).toContain("ChatGPT");
    expect(wrapper.text()).toContain("One");
  });

  it("keeps settings concise and removes redundant product, quota and about copy", async () => {
    const wrapper = mountAuthorizedApp();

    await wrapper.get('[role="tab"]:nth-child(2)').trigger("click");
    expect(wrapper.get(".settings-view").text()).toContain("硬件数据授权");
    expect(wrapper.get(".settings-view").text()).toContain("标准用户 · 只读");
    expect(wrapper.get(".settings-view").text()).not.toContain("用量与授权管理");
    expect(wrapper.get(".settings-view").text()).not.toContain("发布前需由权利人确定授权条款");
    expect(wrapper.get(".settings-view").text()).not.toContain("扫描结果保存在本机 SQLite 数据库");
  });

  it("keeps ChatGPT model verification user initiated and reports the connected account in settings", async () => {
    const availableModels = Array.from({ length: 7 }, (_, index) => ({
      slug: `model-${index + 1}`,
      displayName: `Model ${index + 1}`,
    }));
    const authState = {
      accounts: [
        { clientId: "oaiapp_one", email: "one@example.invalid", displayName: "One", connected: true, planUsageAuthorized: true },
        { clientId: "oaiapp_legacy", email: "one@example.invalid", displayName: "One", connected: false, planUsageAuthorized: false },
      ],
      activeClientId: "oaiapp_one", connected: true, planUsageAuthorized: true,
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "get_chatgpt_auth_state") return authState;
      if (command === "list_chatgpt_models") return availableModels;
      if (command === "verify_chatgpt_inference") return {
        modelSlug: "model-1", responseText: "verified", completedAtUtc: "2026-10-14T10:00:00Z",
      };
      return null;
    });
    const wrapper = mount(App, { global: { plugins: [createPinia()] } });
    await flushPromises();
    expect(invokeMock).toHaveBeenCalledWith("list_chatgpt_models", { clientId: "oaiapp_one", route: "system_proxy" });
    expect(invokeMock).not.toHaveBeenCalledWith("verify_chatgpt_inference", expect.anything());

    await wrapper.get('[role="tab"]:nth-child(2)').trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("方案授权已就绪");
    expect(wrapper.text()).toContain("One");
    expect(wrapper.find('select[aria-label="ChatGPT 授权账号"]').exists()).toBe(false);
    expect(wrapper.text()).not.toContain("添加账号");
    const modelSelect = wrapper.get('select[aria-label="ChatGPT 模型"]');
    expect(modelSelect.findAll("option").map((option) => option.text())).toEqual([
      "选择模型", "Model 1", "Model 2", "Model 3", "Model 4", "Model 5", "Model 6", "Model 7",
    ]);
    expect((modelSelect.element as HTMLSelectElement).value).toBe("model-1");
    await modelSelect.setValue("model-1");
    await wrapper.get(".settings-actions .quiet-button").trigger("click");
    await flushPromises();
    expect(invokeMock).toHaveBeenCalledWith("verify_chatgpt_inference", {
      clientId: "oaiapp_one", modelSlug: "model-1", route: "system_proxy",
    });
  });

  it("opens device details from the inventory and presents the saved capability state", async () => {
    const snapshot = {
      scannedAtUtc: "2026-10-14T09:00:00Z",
      devices: [{
        id: "disk_ssd", identityHash: "hash", identityConfidence: "STRONG", category: "storage",
        kind: "physical_disk", layer: "physical_device", displayName: "Example NVMe SSD",
        manufacturer: "Example", isVirtual: false, isPresent: true, source: "Windows Storage", attributes: { mediaType: "SSD", diskNumber: 2 },
      }],
      edges: [], warnings: [],
    };
    const profile = {
      device: snapshot.devices[0],
      capabilities: [{
        deviceId: "disk_ssd", metricKey: "disk.temperature.celsius", kind: "sensor",
        state: "PERMISSION_REQUIRED", value: null, unit: "°C", source: "MSFT_StorageReliabilityCounter",
        evidenceLevel: null, observedAtUtc: "2026-10-14T09:00:00Z", reasonCode: "ACCESS_DENIED",
        nativeErrorCode: "0x80041003", details: {},
      }, {
        deviceId: "disk_ssd", metricKey: "disk.rpm", kind: "DYNAMIC",
        state: "NOT_APPLICABLE", value: null, unit: "rpm", source: "capability_registry",
        evidenceLevel: null, observedAtUtc: "2026-10-14T09:00:00Z", reasonCode: "SSD_HAS_NO_SPINDLE",
        nativeErrorCode: null, details: {},
      }, {
        deviceId: "disk_ssd", metricKey: "disk.health", kind: "DYNAMIC",
        state: "NOT_TESTED", value: null, unit: null, source: "capability_registry",
        evidenceLevel: null, observedAtUtc: "2026-10-14T09:00:00Z", reasonCode: "HEALTH_QUERY_NOT_RUN",
        nativeErrorCode: null, details: {},
      }],
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return snapshot;
      if (command === "load_device_profile") return profile;
      if (command === "probe_storage_reliability") return profile;
      if (command === "load_metric_history") return [{
        deviceId: "disk_ssd", metricKey: "disk.temperature.celsius", value: 36, unit: "°C",
        state: "AVAILABLE", source: "StorageReliability", evidenceLevel: "SOURCE_REPORTED",
        observedAtUtc: "2026-10-14T08:00:00Z", reasonCode: null,
      }];
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return {
        ...freshMemoryFrame(1),
        samples: [...freshMemoryFrame(1).samples, { deviceId: "disk_ssd", metricKey: "disk.read.bytes_per_second", state: "AVAILABLE", value: 120,
          unit: "byte/s", source: "Windows Performance Counters", evidenceLevel: "MEASURED",
          observedAtUtc: new Date().toISOString(), reasonCode: null, nativeErrorCode: null, details: {} }],
      };
      return null;
    });
    const wrapper = mountAuthorizedApp(createPinia(), { attachTo: document.body });

    await wrapper.get(".primary-button").trigger("click");
    await flushPromises();
    const deviceRow = wrapper.get(".device-row").element as HTMLButtonElement;
    deviceRow.focus();
    await wrapper.get(".device-row").trigger("click");
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("load_device_profile", { deviceId: "disk_ssd" });
    expect(invokeMock).toHaveBeenCalledWith("load_metric_history", { deviceId: "disk_ssd" });
    const dialog = wrapper.get('[role="dialog"]');
    const tabs = dialog.findAll('[role="tab"]');
    expect(tabs.map((tab) => tab.text().trim().replace(/\s+1$/, ""))).toEqual(["当前与参数", "历史读数"]);
    expect(tabs[0].attributes("aria-selected")).toBe("true");
    expect(dialog.text()).toContain("需要额外权限");
    expect(dialog.text()).toContain("不适用");
    expect(dialog.text()).toContain("未测试");
    expect(dialog.text()).toContain("MSFT_StorageReliabilityCounter");
    expect(dialog.text()).not.toContain("36 °C");
    expect(wrapper.findAll('[role="dialog"] .reliability-action')).toHaveLength(1);
    expect(wrapper.findAll('[role="dialog"] .trend-section')).toHaveLength(0);
    expect(invokeMock).not.toHaveBeenCalledWith("probe_storage_reliability", expect.anything());

    (wrapper.get('[role="dialog"]').element as HTMLElement).focus();
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, bubbles: true }));
    expect(wrapper.get('[role="dialog"] .device-ai-actions .quiet-button').element).toBe(document.activeElement);

    await wrapper.get('[role="dialog"] .reliability-action .quiet-button').trigger("click");
    await flushPromises();
    expect(invokeMock).toHaveBeenCalledWith("probe_storage_reliability", {
      deviceId: "disk_ssd", retryAfterPermissionsChanged: true,
    });

    await tabs[0].trigger("keydown", { key: "ArrowRight" });
    expect(tabs[1].attributes("aria-selected")).toBe("true");
    expect(tabs[1].element).toBe(document.activeElement);
    expect(dialog.text()).toContain("历史读数");
    expect(dialog.text()).toContain("36 °C");
    expect(dialog.text()).toContain("可读取");
    expect(dialog.text()).toContain("设备来源报告");
    expect(dialog.text()).not.toContain("需要额外权限");
    await wrapper.get('[role="dialog"] [aria-label="关闭详情"]').trigger("click");
    expect(document.activeElement).toBe(deviceRow);
    wrapper.unmount();
  });

  it("shows the newest live telemetry instead of a stale saved capability", async () => {
    const cpu = {
      id: "cpu_live", identityHash: "hash", identityConfidence: "STRONG", category: "cpu",
      kind: "cpu", layer: "physical_device", displayName: "Fixture CPU",
      manufacturer: "Example", isVirtual: false, isPresent: true, source: "Windows Processor Provider", attributes: {},
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return { scannedAtUtc: "2026-10-15T09:00:00Z", devices: [cpu], edges: [], warnings: [] };
      if (command === "load_device_profile") return { device: cpu, capabilities: [{
        deviceId: cpu.id, metricKey: "cpu.utilization.percent", kind: "DYNAMIC", state: "STALE",
        value: null, unit: "percent", source: "PDH", evidenceLevel: "MEASURED",
        observedAtUtc: "2026-10-15T08:00:00Z", reasonCode: "DEVICE_NOT_PRESENT_IN_LATEST_SCAN",
        nativeErrorCode: null, details: {},
      }] };
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 1, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return {
        ...freshMemoryFrame(1),
        samples: [...freshMemoryFrame(1).samples, { deviceId: cpu.id, metricKey: "cpu.utilization.percent", state: "AVAILABLE", value: 8.7,
          unit: "percent", source: "PDH", evidenceLevel: "MEASURED", observedAtUtc: new Date().toISOString(),
          reasonCode: null, nativeErrorCode: null, details: {} }],
      };
      if (command === "load_metric_history") return [];
      return null;
    });
    const wrapper = mountAuthorizedApp();

    await wrapper.get(".primary-button").trigger("click");
    await flushPromises();
    await wrapper.get(".device-row").trigger("click");
    await flushPromises();

    const metric = wrapper.get(".capability-row");
    expect(metric.text()).toContain("处理器利用率");
    expect(metric.text()).toContain("8.7%");
    expect(metric.text()).not.toContain("数据已过期");
    wrapper.unmount();
  });

  it("does not offer an SSD-only probe for an unnormalized numeric media type", async () => {
    const disk = {
      id: "disk_unknown", identityHash: "hash", identityConfidence: "STRONG", category: "storage",
      kind: "physical_disk", layer: "physical_device", displayName: "Unconfirmed disk",
      manufacturer: "Example", isVirtual: false, isPresent: true, source: "Windows Storage", attributes: { mediaType: 4, diskNumber: 2 },
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return { scannedAtUtc: "2026-10-14T09:00:00Z", devices: [disk], edges: [], warnings: [] };
      if (command === "load_device_profile") return { device: disk, capabilities: [] };
      return null;
    });
    const wrapper = mountAuthorizedApp();

    await wrapper.get(".primary-button").trigger("click");
    await flushPromises();
    await wrapper.get(".device-row").trigger("click");
    await flushPromises();

    expect(wrapper.findAll('[role="dialog"] .reliability-action')).toHaveLength(0);
    expect(invokeMock).not.toHaveBeenCalledWith("probe_storage_reliability", expect.anything());
  });

  it("starts device analysis directly from the detail dialog and changes the action after a result exists", async () => {
    const gpu = {
      id: "gpu-local-only", identityHash: "local-hash", identityConfidence: "STRONG", category: "gpu",
      kind: "gpu_adapter", layer: "physical_device", displayName: "Fixture GPU",
      manufacturer: "Fixture Vendor", isVirtual: false, isPresent: true, source: "Windows inventory", attributes: {},
    };
    const snapshot = {
      snapshotId: "fixture-device-snapshot", scope: "device", capturedAtUtc: "2026-10-14T12:00:00Z",
      scanAtUtc: "2026-10-14T11:59:00Z",
      devices: [{
        evidenceKey: "device.kind", deviceKey: "device", category: "gpu", kind: "gpu_adapter",
        name: "Fixture GPU", manufacturer: "Fixture Vendor", virtualDevice: false,
        source: "Windows inventory", attributes: {},
      }],
      metrics: [{
        evidenceKey: "metric.device.gpu.temperature.celsius", deviceKey: "device",
        metricKey: "gpu.temperature.celsius", state: "NOT_TESTED", value: null, unit: "°C",
        source: "NVML", evidenceLevel: null, observedAtUtc: null, window: null,
      }],
    };
    const targetState = {
      consentGranted: true, consentDecided: true, initialAttempted: true,
      latestAnalysis: null, latestAttemptStatus: null,
    };
    const storedAnalysis = {
      id: "fixture-analysis", scope: "device", targetDeviceId: "gpu-local-only",
      inputFingerprint: "a".repeat(64), evidenceSnapshot: snapshot,
      assessment: {
        scope: "device", targetDeviceId: "gpu-local-only", summary: "设备资料已记录；当前温度未测试。",
        prioritySections: [
          { key: "performance_evaluation", items: [] },
          { key: "health_assessment", items: [{ text: "温度状态未测试，不能判断设备是否正常。", evidenceKeys: ["metric.device.gpu.temperature.celsius"], certainty: "observed" }] },
          { key: "maintenance_plan", items: [] },
          { key: "upgrade_recommendation", items: [] },
          { key: "other", items: [] },
        ],
        strengths: [], concerns: [], suggestions: [], limitations: ["当前温度未测试"],
        evidenceSnapshotId: snapshot.snapshotId, generatedAtUtc: "2026-10-14T12:01:00Z",
        modelSlug: "model-one", provenance: "AI_INTERPRETATION",
      },
      modelSlug: "model-one", createdAtUtc: "2026-10-14T12:01:00Z", status: "COMPLETED",
      isHistorical: false, errorCode: null,
    };
    const authState = {
      accounts: [{ clientId: "oaiapp_one", email: null, displayName: "Fixture account", connected: true, planUsageAuthorized: true }],
      activeClientId: "oaiapp_one", connected: true, planUsageAuthorized: true,
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "get_chatgpt_auth_state") return authState;
      if (command === "list_chatgpt_models") return [{ slug: "model-one", displayName: "Model One" }];
      if (command === "get_analysis_target_state") return targetState;
      if (command === "scan_devices") return { scannedAtUtc: "2026-10-14T12:00:00Z", devices: [gpu], edges: [], warnings: [] };
      if (command === "load_device_profile") return { device: gpu, capabilities: [{
        deviceId: gpu.id, metricKey: "gpu.temperature.celsius", kind: "DYNAMIC", state: "NOT_TESTED",
        value: null, unit: "°C", source: "NVML", evidenceLevel: null,
        observedAtUtc: "2026-10-14T12:00:00Z", reasonCode: "LIVE_PROBE_NOT_RUN", nativeErrorCode: null, details: {},
      }] };
      if (command === "load_metric_history") return [];
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 0, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") return freshMemoryFrame(1);
      if (command === "create_analysis_preview") return { token: "preview-device-1", snapshot, deviceCount: 1, metricCount: 1 };
      if (command === "refresh_device_analysis") return {
        status: "COMPLETED", analysisId: "fixture-analysis", errorCode: null,
        state: { ...targetState, latestAnalysis: storedAnalysis, latestAttemptStatus: "COMPLETED" },
        deviceAnalysis: storedAnalysis,
      };
      return null;
    });
    const pinia = createPinia();
    const wrapper = mountAuthorizedApp(pinia, { attachTo: document.body });
    await flushPromises();
    useAuthStore(pinia).setModel("model-one");
    await wrapper.get(".primary-button").trigger("click");
    await flushPromises();
    await wrapper.get(".device-row").trigger("click");
    await flushPromises();

    const analysisButton = wrapper.get(".device-ai-actions .quiet-button");
    expect(wrapper.get('[role="dialog"]').text()).toContain("动态采样尚未运行");
    expect(analysisButton.text()).toBe("开始分析");
    await analysisButton.trigger("click");
    await flushPromises();
    expect(invokeMock).toHaveBeenCalledWith("create_analysis_preview", { scope: "device", targetDeviceId: "gpu-local-only" });
    expect(invokeMock).toHaveBeenCalledWith("refresh_device_analysis", {
      deviceId: "gpu-local-only", previewToken: "preview-device-1", clientId: "oaiapp_one",
      modelSlug: "model-one", route: "system_proxy",
    });
    expect(wrapper.find('[role="dialog"] .device-ai-preview').exists()).toBe(false);
    expect(wrapper.get('[role="dialog"]').text()).toContain("当前温度未测试");
    expect(wrapper.get(".device-ai-actions .quiet-button").text()).toBe("重新分析");
    expect(wrapper.findAll(".device-ai-priority-section > strong").map((item) => item.text())).toEqual([
      "设备状态正常评估",
    ]);
    wrapper.unmount();
  });

  it("does not analyze a device from the detail dialog before telemetry warmup is ready", async () => {
    const gpu = {
      id: "gpu_warmup_guard", identityHash: "warmup-guard-hash", identityConfidence: "STRONG" as const,
      category: "gpu" as const, kind: "gpu_adapter", layer: "physical_device" as const,
      displayName: "Warmup Guard GPU", manufacturer: "Example", isVirtual: false, isPresent: true,
      source: "Windows inventory", attributes: {},
    };
    const targetState = {
      consentGranted: true, consentDecided: true, initialAttempted: true,
      latestAnalysis: null, latestAttemptStatus: "COMPLETED",
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_analysis_target_state") return targetState;
      if (command === "load_device_profile") return { device: gpu, capabilities: [] };
      if (command === "load_metric_history") return [];
      return null;
    });
    const pinia = createPinia();
    const hardware = useHardwareStore(pinia);
    hardware.telemetryWarmupReady = false;
    const auth = useAuthStore(pinia);
    auth.connection = {
      accounts: [{ clientId: "oaiapp_one", email: null, displayName: "One", connected: true, planUsageAuthorized: true }],
      activeClientId: "oaiapp_one", connected: true, planUsageAuthorized: true,
    };
    auth.models = [{ slug: "model-one", displayName: "Model One" }];
    auth.setModel("model-one");
    const analysis = useAnalysisStore(pinia);
    analysis.system = targetState;
    useSettingsStore(pinia);

    const wrapper = mount(DeviceDetailDialog, {
      props: { device: gpu, telemetry: [] },
      attachTo: document.body,
      global: { plugins: [pinia] },
    });
    await flushPromises();

    await wrapper.get(".device-ai-actions .quiet-button").trigger("click");
    await flushPromises();

    expect(analysis.errorCode).toBe("AI_ANALYSIS_TELEMETRY_NOT_READY");
    expect(invokeMock).not.toHaveBeenCalledWith("create_analysis_preview", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("refresh_device_analysis", expect.anything());
    wrapper.unmount();
  });

  it("shows a concise notice when realtime telemetry is temporarily unavailable", async () => {
    const cpu = {
      id: "cpu_telemetry_unavailable", identityHash: "telemetry-unavailable-hash", identityConfidence: "STRONG" as const,
      category: "cpu" as const, kind: "cpu", layer: "physical_device" as const,
      displayName: "Telemetry CPU", manufacturer: "Example", isVirtual: false, isPresent: true,
      source: "Windows inventory", attributes: {},
    };
    const snapshot = {
      scannedAtUtc: "2026-10-14T09:00:00Z", devices: [cpu], edges: [], warnings: [],
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return snapshot;
      if (command === "start_monitoring") return { running: true, foreground: true, subscriptionCount: 1, sampleIntervalMs: 2000, latestObservedAtUtc: null };
      if (command === "load_telemetry") throw new Error("TELEMETRY_PIPE_UNAVAILABLE");
      return null;
    });
    const wrapper = mountAuthorizedApp();

    await wrapper.get(".primary-button").trigger("click");
    await flushPromises();

    expect(wrapper.text()).toContain("实时采样暂不可用，当前读数不会更新。");
  });

  it("distinguishes history loading failure from an empty archive and lets the user retry", async () => {
    let historyCalls = 0;
    const disk = {
      id: "disk_history", identityHash: "hash", identityConfidence: "STRONG", category: "storage",
      kind: "physical_disk", layer: "physical_device", displayName: "History disk",
      manufacturer: "Example", isVirtual: false, isPresent: true, source: "Windows Storage", attributes: { mediaType: "SSD" },
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "app_info") return { productName: "机枢", version: "0.1.1", databaseReady: true };
      if (command === "scan_devices") return { scannedAtUtc: "2026-10-14T09:00:00Z", devices: [disk], edges: [], warnings: [] };
      if (command === "load_device_profile") return { device: disk, capabilities: [] };
      if (command === "load_metric_history") {
        historyCalls += 1;
        throw new Error("HISTORY_LOAD_FAILED");
      }
      return null;
    });
    const wrapper = mountAuthorizedApp();

    await wrapper.get(".primary-button").trigger("click");
    await flushPromises();
    await wrapper.get(".device-row").trigger("click");
    await flushPromises();
    await wrapper.get("#device-history-tab").trigger("click");

    expect(wrapper.text()).toContain("历史记录暂不可用");
    expect(wrapper.text()).not.toContain("尚无已归档读数");
    await wrapper.get(".history-retry-button").trigger("click");
    await flushPromises();
    expect(historyCalls).toBe(2);
    expect(wrapper.text()).toContain("历史记录暂不可用");
  });

  it("requires explicit confirmation before clearing AI and chat cache", async () => {
    const wrapper = mountAuthorizedApp();
    await wrapper.get('[role="tab"]:nth-child(2)').trigger("click");
    await flushPromises();

    expect(invokeMock).toHaveBeenCalledWith("get_cache_summary");
    expect(wrapper.text()).toContain("4 条 AI 与问答记录");
    await wrapper.get(".clear-cache-button").trigger("click");
    expect(invokeMock).not.toHaveBeenCalledWith("clear_ai_cache");
    expect(wrapper.text()).toContain("设备档案将保留");

    await wrapper.get(".confirm-clear-button").trigger("click");
    await flushPromises();
    expect(invokeMock).toHaveBeenCalledWith("clear_ai_cache");
    expect(wrapper.text()).toContain("AI 与问答缓存已清理");
  });

  it("does not export when the save dialog is cancelled and exports after selection", async () => {
    const wrapper = mountAuthorizedApp();
    await wrapper.get('[role="tab"]:nth-child(2)').trigger("click");
    await flushPromises();

    await wrapper.get(".export-diagnostics-button").trigger("click");
    await flushPromises();
    expect(invokeMock).not.toHaveBeenCalledWith("export_sanitized_diagnostics", expect.anything());

    saveMock.mockResolvedValueOnce("C:\\Users\\Public\\CoreScope-diagnostics.json");
    await wrapper.get(".export-diagnostics-button").trigger("click");
    await flushPromises();
    expect(invokeMock).toHaveBeenCalledWith("export_sanitized_diagnostics", {
      path: "C:\\Users\\Public\\CoreScope-diagnostics.json",
    });
    expect(wrapper.text()).toContain("脱敏诊断已导出");
  });
});
