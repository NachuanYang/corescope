import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useSettingsStore } from "./settings";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

describe("settings store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    invokeMock.mockReset();
  });

  it("loads local cache counts and per-device historical evidence", async () => {
    const history = [{
      deviceId: "disk-1", metricKey: "disk.temperature.celsius", value: 35, unit: "°C",
      state: "AVAILABLE", source: "StorageReliability", evidenceLevel: "SOURCE_REPORTED",
      observedAtUtc: "2026-10-14T08:00:00Z", reasonCode: null,
    }];
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_cache_summary") return { databaseBytes: 4096, clearableAiItems: 0 };
      if (command === "load_metric_history") return history;
      return null;
    });
    const settings = useSettingsStore();

    await settings.loadCacheSummary();
    await settings.loadDeviceHistory("disk-1");

    expect(settings.cacheSummary?.databaseBytes).toBe(4096);
    expect(settings.historyByDevice["disk-1"]).toEqual(history);
    expect(invokeMock).toHaveBeenCalledWith("load_metric_history", { deviceId: "disk-1" });
  });

  it("refreshes history when a device dialog is opened again", async () => {
    invokeMock.mockResolvedValue([]);
    const settings = useSettingsStore();

    await settings.loadDeviceHistory("disk-1");
    await settings.loadDeviceHistory("disk-1");

    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it("keeps a separate per-device error when history cannot be loaded", async () => {
    invokeMock.mockRejectedValue(new Error("HISTORY_LOAD_FAILED"));
    const settings = useSettingsStore();

    await settings.loadDeviceHistory("disk-1");

    expect(settings.historyErrors["disk-1"]).toContain("HISTORY_LOAD_FAILED");
  });

  it("refreshes local cache counts after an explicit clear", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "clear_ai_cache") return 4;
      if (command === "get_cache_summary") return { databaseBytes: 4096, clearableAiItems: 0 };
      return null;
    });
    const settings = useSettingsStore();

    await expect(settings.clearAiCache()).resolves.toBe(4);

    expect(invokeMock).toHaveBeenNthCalledWith(1, "clear_ai_cache");
    expect(invokeMock).toHaveBeenNthCalledWith(2, "get_cache_summary");
  });
});
