import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAnalysisStore, type AnalysisTargetState } from "./analysis";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const baseState: AnalysisTargetState = {
  consentGranted: false,
  consentDecided: false,
  initialAttempted: false,
  latestAnalysis: null,
  latestAttemptStatus: null,
};

const completed: AnalysisTargetState = {
  consentGranted: true,
  consentDecided: true,
  initialAttempted: true,
  latestAnalysis: {
    id: "analysis-1", scope: "system", targetDeviceId: null, inputFingerprint: "a".repeat(64),
    evidenceSnapshot: {
      snapshotId: "snapshot-1", scope: "system", capturedAtUtc: "2026-10-14T12:00:00Z",
      scanAtUtc: "2026-10-14T11:59:00Z", devices: [], metrics: [],
    },
    assessment: {
      scope: "system", summary: "本机识别到处理器。未提供长期采样。", strengths: [], concerns: [],
      suggestions: [], limitations: ["未提供长期采样"], evidenceSnapshotId: "snapshot-1",
      generatedAtUtc: "2026-10-14T12:00:01Z", modelSlug: "model-a", provenance: "AI_INTERPRETATION",
    },
    modelSlug: "model-a", createdAtUtc: "2026-10-14T12:00:01Z", status: "COMPLETED",
    isHistorical: false, errorCode: null,
  },
  latestAttemptStatus: "COMPLETED",
};

describe("AI analysis store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    invokeMock.mockReset().mockImplementation(async (command: string, args?: { scope?: string }) => {
      if (command === "get_analysis_target_state") return baseState;
      if (command === "create_analysis_preview") return {
        token: "preview-token", snapshot: { ...completed.latestAnalysis!.evidenceSnapshot, scope: args?.scope ?? "system" },
        deviceCount: 3, metricCount: 2,
      };
      if (command === "set_ai_data_consent") return true;
      return null;
    });
  });

  it("loads consent, attempt and cached analysis from the native database", async () => {
    invokeMock.mockResolvedValueOnce(completed);
    const analysis = useAnalysisStore();
    await analysis.loadTargetState("system");

    expect(analysis.system?.latestAnalysis?.assessment?.provenance).toBe("AI_INTERPRETATION");
    expect(analysis.system?.latestAnalysis?.evidenceSnapshot.snapshotId).toBe("snapshot-1");
    expect(invokeMock).toHaveBeenCalledWith("get_analysis_target_state", { scope: "system" });
  });

  it("creates a local preview before consent and persists acceptance or decline explicitly", async () => {
    const analysis = useAnalysisStore();
    await analysis.createPreview();
    expect(analysis.preview?.deviceCount).toBe(3);
    expect(invokeMock).toHaveBeenCalledWith("create_analysis_preview", { scope: "system" });
    expect(invokeMock).not.toHaveBeenCalledWith("start_initial_system_analysis", expect.anything());

    await analysis.decideConsent(false);
    expect(invokeMock).toHaveBeenCalledWith("set_ai_data_consent", { granted: false });
  });

  it("commits first-run consent without reloading state between consent and the reviewed analysis", async () => {
    const analysis = useAnalysisStore();
    analysis.system = { ...baseState };
    await analysis.createPreview();
    invokeMock.mockClear();

    await analysis.decideConsent(true);

    expect(invokeMock).toHaveBeenCalledWith("set_ai_data_consent", { granted: true });
    expect(invokeMock).not.toHaveBeenCalledWith("get_analysis_target_state", expect.anything());
    expect(analysis.system).toMatchObject({ consentGranted: true, consentDecided: true });
  });

  it("uses the reviewed preview token for the one-time initial analysis", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "create_analysis_preview") return {
        token: "preview-1", snapshot: completed.latestAnalysis?.evidenceSnapshot, deviceCount: 3, metricCount: 2,
      };
      if (command === "start_initial_system_analysis") return {
        status: "COMPLETED", analysisId: "analysis-1", errorCode: null, state: completed, deviceAnalysis: null,
      };
      return null;
    });
    const analysis = useAnalysisStore();
    await analysis.createPreview();
    const result = await analysis.startInitial("oaiapp_one", "model-a", "system_proxy");

    expect(result?.status).toBe("COMPLETED");
    expect(invokeMock).toHaveBeenCalledWith("start_initial_system_analysis", {
      previewToken: "preview-1", clientId: "oaiapp_one", modelSlug: "model-a", route: "system_proxy",
    });
    expect(analysis.preview).toBeNull();
  });

  it("keeps the previous completed analysis when a manual retry fails", async () => {
    const analysis = useAnalysisStore();
    analysis.system = completed;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "refresh_system_analysis") throw "AUTH_SSE_STREAM_INTERRUPTED";
      if (command === "get_analysis_target_state") return completed;
      return null;
    });

    await analysis.refreshSystem("oaiapp_one", "model-a", "system_proxy");

    expect(analysis.system?.latestAnalysis?.id).toBe("analysis-1");
    expect(analysis.errorCode).toBe("AUTH_SSE_STREAM_INTERRUPTED");
  });

  it("keeps device refresh scoped to the selected device", async () => {
    invokeMock.mockImplementation(async (command: string, args?: { scope?: string }) => {
      if (command === "create_analysis_preview") return {
        token: "preview-token", snapshot: { ...completed.latestAnalysis!.evidenceSnapshot, scope: args?.scope ?? "system" },
        deviceCount: 1, metricCount: 2,
      };
      if (command === "refresh_device_analysis") return {
        status: "COMPLETED", analysisId: "device-analysis", errorCode: null,
        state: completed, deviceAnalysis: completed.latestAnalysis,
      };
      return null;
    });
    const analysis = useAnalysisStore();
    await analysis.createPreview("device", "gpu-1");
    await analysis.refreshDevice("gpu-1", "oaiapp_one", "model-a", "direct");
    expect(invokeMock).toHaveBeenCalledWith("refresh_device_analysis", {
      deviceId: "gpu-1", previewToken: "preview-token", clientId: "oaiapp_one", modelSlug: "model-a", route: "direct",
    });
    expect(analysis.system).toBeNull();
  });

  it("uses the displayed snapshot token when confirming a system re-analysis", async () => {
    const analysis = useAnalysisStore();
    await analysis.createPreview("system");
    await analysis.refreshSystem("oaiapp_one", "model-a", "system_proxy");

    expect(invokeMock).toHaveBeenCalledWith("refresh_system_analysis", {
      previewToken: "preview-token", clientId: "oaiapp_one", modelSlug: "model-a", route: "system_proxy",
    });
  });
});
