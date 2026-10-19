import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";
import type { NetworkRoute } from "./auth";
import { useChatStore } from "./chat";

export type AnalysisScope = "system" | "device";
export type AnalysisSectionKey =
  | "performance_evaluation"
  | "health_assessment"
  | "maintenance_plan"
  | "upgrade_recommendation"
  | "other";

export interface AnalysisPriorityItem {
  text: string;
  evidenceKeys: string[];
  certainty: "observed" | "inferred" | "unknown";
}

export interface AnalysisPrioritySection {
  key: AnalysisSectionKey;
  items: AnalysisPriorityItem[];
}

export const ANALYSIS_SECTION_LABELS: Record<AnalysisSectionKey, string> = {
  performance_evaluation: "设备性能客观评价",
  health_assessment: "设备状态正常评估",
  maintenance_plan: "设备维护方案",
  upgrade_recommendation: "设备更新换代建议",
  other: "其他内容",
};

export const analysisCertaintyLabel = (certainty: AnalysisPriorityItem["certainty"]): string => ({
  observed: "观察",
  inferred: "推断",
  unknown: "证据不足",
})[certainty];

export interface EvidenceDevice {
  evidenceKey: string;
  deviceKey: string;
  category: string;
  kind: string;
  name: string | null;
  manufacturer: string | null;
  virtualDevice: boolean | null;
  source: string;
  attributes: Record<string, number | boolean | string>;
}

export interface EvidenceMetric {
  evidenceKey: string;
  deviceKey: string;
  metricKey: string;
  state: string;
  value: number | boolean | string | null;
  unit: string | null;
  source: string;
  evidenceLevel: string | null;
  reasonCode?: string | null;
  observedAtUtc: string | null;
  window: {
    sampleCount: number;
    minimum: number | null;
    mean: number | null;
    maximum: number | null;
    startedAtUtc: string | null;
    endedAtUtc: string | null;
  } | null;
}

export interface EvidenceSnapshot {
  snapshotId: string;
  scope: AnalysisScope;
  capturedAtUtc: string;
  scanAtUtc: string;
  devices: EvidenceDevice[];
  metrics: EvidenceMetric[];
}

export interface AssessmentConcern {
  claim: string;
  evidenceKeys: string[];
  certainty: "observed" | "inferred" | "unknown";
}

export interface AiAssessment {
  scope: AnalysisScope;
  targetDeviceId?: string | null;
  summary: string;
  prioritySections?: AnalysisPrioritySection[];
  strengths: string[];
  concerns: AssessmentConcern[];
  suggestions: string[];
  limitations: string[];
  evidenceSnapshotId: string;
  generatedAtUtc: string;
  modelSlug: string;
  provenance: "AI_INTERPRETATION";
}

export interface StoredAnalysis {
  id: string;
  scope: AnalysisScope;
  targetDeviceId: string | null;
  inputFingerprint: string;
  evidenceSnapshot: EvidenceSnapshot;
  assessment: AiAssessment | null;
  modelSlug: string;
  createdAtUtc: string;
  status: "PENDING" | "COMPLETED" | "FAILED" | "INTERRUPTED";
  isHistorical: boolean;
  errorCode: string | null;
}

export interface AnalysisTargetState {
  consentGranted: boolean;
  consentDecided: boolean;
  initialAttempted: boolean;
  latestAnalysis: StoredAnalysis | null;
  latestAttemptStatus: string | null;
}

export interface AnalysisPreview {
  token: string;
  snapshot: EvidenceSnapshot;
  deviceCount: number;
  metricCount: number;
}

export interface AnalysisRunOutcome {
  status: string;
  analysisId: string | null;
  errorCode: string | null;
  state: AnalysisTargetState;
  deviceAnalysis: StoredAnalysis | null;
}

const SAFE_ERROR_CODE = /^[A-Z0-9_.:-]{1,120}$/;

function errorCode(error: unknown, fallback: string): string {
  return typeof error === "string" && SAFE_ERROR_CODE.test(error) ? error : fallback;
}

async function invokeWithBusyRetry<T>(request: () => Promise<T>): Promise<T> {
  const delays = [750, 1_500];
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await request();
    } catch (error) {
      if (errorCode(error, "") !== "AI_ANALYSIS_ALREADY_RUNNING" || attempt >= delays.length) throw error;
      await new Promise<void>((resolve) => window.setTimeout(resolve, delays[attempt]));
    }
  }
}

export const useAnalysisStore = defineStore("analysis", {
  state: () => ({
    system: null as AnalysisTargetState | null,
    devices: {} as Record<string, AnalysisTargetState>,
    preview: null as AnalysisPreview | null,
    previewTarget: null as { scope: AnalysisScope; targetDeviceId?: string } | null,
    loadingState: false,
    creatingPreview: false,
    runningSystem: false,
    runningDevices: [] as string[],
    initialBatchRunning: false,
    initialBatchProgress: { completed: 0, total: 0, current: "", phase: "" },
    errorCode: "",
    operationStatus: "",
  }),
  getters: {
    isAnyAnalysisRunning: (state) => state.initialBatchRunning || state.runningSystem || state.runningDevices.length > 0,
    isAnyAiOperationRunning(state) {
      const chat = useChatStore();
      return state.initialBatchRunning || state.runningSystem || state.runningDevices.length > 0
        || chat.isCreatingPreview || chat.isSending;
    },
  },
  actions: {
    canStartAiRequest(fromInitialBatch = false) {
      const chat = useChatStore();
      const anotherRequestIsRunning = this.runningSystem || this.runningDevices.length > 0
        || chat.isCreatingPreview || chat.isSending;
      return !anotherRequestIsRunning && (!this.initialBatchRunning || fromInitialBatch);
    },

    beginInitialBatch(total: number) {
      if (!this.canStartAiRequest()) return false;
      this.initialBatchRunning = true;
      this.initialBatchProgress = { completed: 0, total: Math.max(0, total), current: "", phase: "准备分析" };
      return true;
    },

    updateInitialBatchProgress(completed: number, total: number, current: string, phase: string) {
      this.initialBatchProgress = {
        completed: Math.min(Math.max(0, completed), Math.max(0, total)),
        total: Math.max(0, total),
        current,
        phase,
      };
    },

    endInitialBatch() {
      this.initialBatchRunning = false;
      this.initialBatchProgress = { ...this.initialBatchProgress, current: "", phase: "" };
    },

    async loadTargetState(scope: AnalysisScope, targetDeviceId?: string) {
      this.loadingState = true;
      this.errorCode = "";
      try {
        const state = await invoke<AnalysisTargetState>("get_analysis_target_state", {
          scope,
          ...(targetDeviceId ? { targetDeviceId } : {}),
        });
        if (scope === "system") this.system = state;
        else if (targetDeviceId) this.devices[targetDeviceId] = state;
        return state;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_ANALYSIS_STATE_LOAD_FAILED");
        return null;
      } finally {
        this.loadingState = false;
      }
    },

    async createPreview(scope: AnalysisScope = "system", targetDeviceId?: string) {
      if (this.creatingPreview) return null;
      this.creatingPreview = true;
      this.errorCode = "";
      try {
        this.preview = await invoke<AnalysisPreview>("create_analysis_preview", {
          scope,
          ...(targetDeviceId ? { targetDeviceId } : {}),
        });
        this.previewTarget = { scope, ...(targetDeviceId ? { targetDeviceId } : {}) };
        return this.preview;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_ANALYSIS_PREVIEW_FAILED");
        return null;
      } finally {
        this.creatingPreview = false;
      }
    },

    async decideConsent(granted: boolean) {
      this.errorCode = "";
      try {
        await invoke<boolean>("set_ai_data_consent", { granted });
        this.system = {
          ...(this.system ?? {
            initialAttempted: false,
            latestAnalysis: null,
            latestAttemptStatus: null,
          }),
          consentGranted: granted,
          consentDecided: true,
        };
        this.operationStatus = granted ? "已允许发送脱敏硬件摘要" : "已关闭 AI 硬件摘要发送";
        return true;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_ANALYSIS_CONSENT_SAVE_FAILED");
        return false;
      }
    },

    async startInitial(clientId: string, modelSlug: string, route: NetworkRoute, fromInitialBatch = false) {
      if (!this.canStartAiRequest(fromInitialBatch)) {
        this.errorCode = "AI_ANALYSIS_ALREADY_RUNNING";
        this.operationStatus = "已有设备分析正在进行，请稍候";
        return null;
      }
      const token = this.preview?.token;
      if (!token) {
        this.errorCode = "AI_ANALYSIS_PREVIEW_REQUIRED";
        return null;
      }
      this.runningSystem = true;
      this.errorCode = "";
      this.operationStatus = "整机 AI 分析正在运行；本机监测仍会继续";
      try {
        const outcome = await invokeWithBusyRetry(() => invoke<AnalysisRunOutcome>("start_initial_system_analysis", {
          previewToken: token,
          clientId,
          modelSlug,
          route,
        }));
        this.system = outcome.state;
        this.preview = null;
        this.previewTarget = null;
        this.operationStatus = outcome.status === "COMPLETED" ? "整机 AI 分析已完成" : "首次整机分析未完成，可手动重试";
        this.errorCode = outcome.errorCode ?? "";
        return outcome;
      } catch (error) {
        const code = errorCode(error, "AI_ANALYSIS_INITIAL_FAILED");
        this.operationStatus = "首次整机分析未完成";
        await this.loadTargetState("system");
        this.errorCode = code;
        return null;
      } finally {
        this.runningSystem = false;
      }
    },

    async startInitialDevice(deviceId: string, clientId: string, modelSlug: string, route: NetworkRoute, fromInitialBatch = false) {
      if (!this.canStartAiRequest(fromInitialBatch)) {
        this.errorCode = "AI_ANALYSIS_ALREADY_RUNNING";
        this.operationStatus = "已有设备分析正在进行，请稍候";
        return null;
      }
      const token = this.preview?.token;
      if (!token || this.previewTarget?.scope !== "device" || this.previewTarget.targetDeviceId !== deviceId) {
        this.errorCode = "AI_ANALYSIS_PREVIEW_REQUIRED";
        return null;
      }
      this.runningDevices.push(deviceId);
      this.errorCode = "";
      this.operationStatus = "正在完成首次设备分析";
      try {
        const outcome = await invokeWithBusyRetry(() => invoke<AnalysisRunOutcome>("start_initial_device_analysis", {
          deviceId,
          previewToken: token,
          clientId,
          modelSlug,
          route,
        }));
        this.devices[deviceId] = outcome.state;
        this.clearPreview();
        this.errorCode = outcome.errorCode ?? "";
        return outcome;
      } catch (error) {
        const code = errorCode(error, "AI_DEVICE_ANALYSIS_INITIAL_FAILED");
        this.clearPreview();
        await this.loadTargetState("device", deviceId);
        this.errorCode = code;
        this.operationStatus = "首次设备分析未完成";
        return null;
      } finally {
        this.runningDevices = this.runningDevices.filter((id) => id !== deviceId);
      }
    },

    async refreshSystem(clientId: string, modelSlug: string, route: NetworkRoute, fromInitialBatch = false) {
      if (!this.canStartAiRequest(fromInitialBatch)) {
        this.errorCode = "AI_ANALYSIS_ALREADY_RUNNING";
        this.operationStatus = "已有设备分析正在进行，请稍候";
        return null;
      }
      const previewToken = this.previewTarget?.scope === "system" ? this.preview?.token : undefined;
      this.runningSystem = true;
      this.errorCode = "";
      this.operationStatus = "整机 AI 分析正在运行；本机监测仍会继续";
      try {
        const outcome = await invokeWithBusyRetry(() => invoke<AnalysisRunOutcome>("refresh_system_analysis", {
          ...(previewToken ? { previewToken } : {}),
          clientId,
          modelSlug,
          route,
        }));
        this.system = outcome.state;
        if (previewToken) {
          this.preview = null;
          this.previewTarget = null;
        }
        this.operationStatus = outcome.status === "COMPLETED" ? "整机 AI 分析已更新" : "整机 AI 分析未完成，可重试";
        this.errorCode = outcome.errorCode ?? "";
        return outcome;
      } catch (error) {
        const code = errorCode(error, "AI_ANALYSIS_REFRESH_FAILED");
        this.operationStatus = "整机 AI 分析未完成";
        if (previewToken) this.clearPreview();
        await this.loadTargetState("system");
        this.errorCode = code;
        return null;
      } finally {
        this.runningSystem = false;
      }
    },

    async refreshDevice(deviceId: string, clientId: string, modelSlug: string, route: NetworkRoute, fromInitialBatch = false) {
      if (!this.canStartAiRequest(fromInitialBatch)) {
        this.errorCode = "AI_ANALYSIS_ALREADY_RUNNING";
        this.operationStatus = "已有设备分析正在进行，请稍候";
        return null;
      }
      const previewToken = this.preview?.token;
      if (this.previewTarget?.scope !== "device" || this.previewTarget.targetDeviceId !== deviceId || !previewToken) {
        this.errorCode = "AI_ANALYSIS_PREVIEW_REQUIRED";
        return null;
      }
      this.runningDevices.push(deviceId);
      this.errorCode = "";
      this.operationStatus = "设备 AI 分析正在运行";
      try {
        const outcome = await invokeWithBusyRetry(() => invoke<AnalysisRunOutcome>("refresh_device_analysis", {
          deviceId,
          previewToken,
          clientId,
          modelSlug,
          route,
        }));
        this.devices[deviceId] = outcome.state;
        this.preview = null;
        this.previewTarget = null;
        this.operationStatus = outcome.status === "COMPLETED" ? "设备 AI 分析已更新" : "设备 AI 分析未完成，可重试";
        this.errorCode = outcome.errorCode ?? "";
        return outcome;
      } catch (error) {
        const code = errorCode(error, "AI_DEVICE_ANALYSIS_FAILED");
        this.operationStatus = "设备 AI 分析未完成";
        this.clearPreview();
        await this.loadTargetState("device", deviceId);
        this.errorCode = code;
        return null;
      } finally {
        this.runningDevices = this.runningDevices.filter((id) => id !== deviceId);
      }
    },

    clearPreview() {
      this.preview = null;
      this.previewTarget = null;
    },
  },
});
