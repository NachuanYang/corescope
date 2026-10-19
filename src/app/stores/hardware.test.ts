import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { flushPromises } from "@vue/test-utils";
import { useHardwareStore, type InventorySnapshot } from "./hardware";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const inventory: InventorySnapshot = {
  scannedAtUtc: "2026-10-14T09:00:00Z",
  devices: [{
    id: "dev_test", identityHash: "identity-hash", identityConfidence: "STRONG",
    category: "cpu", kind: "cpu", layer: "physical_device", displayName: "Test CPU",
    manufacturer: "Vendor", isVirtual: false, isPresent: true, source: "WMI", attributes: {},
  }],
  edges: [],
  warnings: [],
};
const monitoring = {
  running: true, foreground: true, subscriptionCount: 1,
  sampleIntervalMs: 2000, latestObservedAtUtc: null,
};

function freshMemoryFrame(subscriptionCount = 1) {
  const observedAtUtc = new Date().toISOString();
  const frameMonitoring = { ...monitoring, subscriptionCount, latestObservedAtUtc: observedAtUtc };
  return {
    monitoring: frameMonitoring,
    samples: [{
      deviceId: null, metricKey: "memory.used.bytes", state: "AVAILABLE", value: 12,
      unit: "byte", source: "GlobalMemoryStatusEx", evidenceLevel: "OS_REPORTED",
      observedAtUtc, reasonCode: null, nativeErrorCode: null, details: {},
    }],
  };
}

describe("hardware inventory store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    invokeMock.mockReset();
  });

  it("loads the last local inventory without starting a new scan", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "load_inventory") return inventory;
      if (command === "start_monitoring") return monitoring;
      if (command === "load_telemetry") return freshMemoryFrame();
      return null;
    });
    const hardware = useHardwareStore();

    await hardware.loadInventory();

    expect(invokeMock).toHaveBeenCalledWith("load_inventory");
    expect(hardware.snapshot?.devices[0]?.displayName).toBe("Test CPU");
    expect(hardware.hasScanned).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("start_monitoring", { deviceIds: ["dev_test"] });
    expect(hardware.monitoring?.running).toBe(true);
  });

  it("subscribes to the native GPU and memory device kinds", async () => {
    const devices = [
      inventory.devices[0]!,
      { ...inventory.devices[0]!, id: "dev_gpu", category: "gpu" as const, kind: "gpu_adapter", displayName: "Test GPU" },
      { ...inventory.devices[0]!, id: "dev_memory", category: "memory" as const, kind: "memory_module", displayName: "Test memory" },
    ];
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "load_inventory") return { ...inventory, devices };
      if (command === "start_monitoring") return { ...monitoring, subscriptionCount: devices.length };
      if (command === "load_telemetry") return freshMemoryFrame(devices.length);
      return null;
    });
    const hardware = useHardwareStore();

    await hardware.loadInventory();

    expect(invokeMock).toHaveBeenCalledWith("start_monitoring", {
      deviceIds: ["dev_test", "dev_gpu", "dev_memory"],
    });
  });

  it("stores a fresh scan only after the native scan command succeeds", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "scan_devices") return inventory;
      if (command === "start_monitoring") return monitoring;
      if (command === "load_telemetry") return freshMemoryFrame();
      return null;
    });
    const hardware = useHardwareStore();

    await hardware.scan();

    expect(invokeMock).toHaveBeenCalledWith("scan_devices");
    expect(hardware.snapshot?.scannedAtUtc).toBe(inventory.scannedAtUtc);
    expect(hardware.isScanning).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith("start_monitoring", { deviceIds: ["dev_test"] });
  });

  it("waits for fresh memory telemetry and warmed rate counters before completing a scan", async () => {
    vi.useFakeTimers();
    try {
      let telemetryReads = 0;
      invokeMock.mockImplementation(async (command: string) => {
        if (command === "scan_devices") return inventory;
        if (command === "start_monitoring") return monitoring;
        if (command === "load_telemetry") {
          telemetryReads += 1;
          const observedAtUtc = new Date().toISOString();
          const memory = {
            deviceId: null, metricKey: "memory.used.bytes", state: "AVAILABLE", value: 12,
            unit: "byte", source: "GlobalMemoryStatusEx", evidenceLevel: "OS_REPORTED",
            observedAtUtc, reasonCode: null, nativeErrorCode: null, details: {},
          };
          const cpu = {
            deviceId: "dev_test", metricKey: "cpu.utilization.percent",
            state: telemetryReads < 3 ? "PROBING" : "AVAILABLE",
            value: telemetryReads < 3 ? null : 18.5, unit: "percent", source: "GetSystemTimes",
            evidenceLevel: telemetryReads < 3 ? null : "MEASURED", observedAtUtc,
            reasonCode: telemetryReads < 3 ? "WARMUP_REQUIRED" : null, nativeErrorCode: null, details: {},
          };
          return { monitoring, samples: telemetryReads === 1 ? [] : [memory, cpu] };
        }
        return null;
      });
      const hardware = useHardwareStore();
      let scanFinished = false;
      const scanPromise = hardware.scan().then((result) => {
        scanFinished = true;
        return result;
      });
      await flushPromises();

      expect(scanFinished).toBe(false);
      await vi.advanceTimersByTimeAsync(1000);
      await flushPromises();
      expect(scanFinished).toBe(false);
      await vi.advanceTimersByTimeAsync(1000);

      await expect(scanPromise).resolves.toBe(true);
      expect(telemetryReads).toBe(3);
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps the prior inventory when a scan is cancelled", async () => {
    const hardware = useHardwareStore();
    hardware.snapshot = inventory;
    invokeMock.mockRejectedValue(new Error("SCAN_CANCELLED"));

    await hardware.scan();

    expect(hardware.snapshot).toMatchObject(inventory);
    expect(hardware.scanError).toBe("");
    expect(hardware.isScanning).toBe(false);
  });

  it("marks prior live values stale and removes their values when a refresh fails", async () => {
    const hardware = useHardwareStore();
    hardware.telemetry = [{
      deviceId: "dev_test", metricKey: "cpu.utilization.percent", state: "AVAILABLE", value: 24.5,
      unit: "percent", source: "Windows Performance Counters", evidenceLevel: "MEASURED",
      observedAtUtc: "2026-10-14T09:00:00Z", reasonCode: null, nativeErrorCode: null, details: {},
    }];
    invokeMock.mockRejectedValue(new Error("TELEMETRY_PIPE_UNAVAILABLE"));

    await hardware.refreshTelemetry();

    expect(hardware.telemetry[0]).toMatchObject({
      state: "STALE", value: null, reasonCode: "TELEMETRY_REFRESH_FAILED",
      source: "Windows Performance Counters", observedAtUtc: "2026-10-14T09:00:00Z",
    });
    expect(hardware.telemetryError).toContain("TELEMETRY_PIPE_UNAVAILABLE");
  });

  it("marks prior live values stale when monitoring cannot restart after a scan", async () => {
    const hardware = useHardwareStore();
    hardware.snapshot = inventory;
    hardware.telemetry = [{
      deviceId: "dev_test", metricKey: "cpu.utilization.percent", state: "AVAILABLE", value: 24.5,
      unit: "percent", source: "Windows Performance Counters", evidenceLevel: "MEASURED",
      observedAtUtc: "2026-10-14T09:00:00Z", reasonCode: null, nativeErrorCode: null, details: {},
    }];
    invokeMock.mockRejectedValue(new Error("MONITOR_START_FAILED"));

    await hardware.startMonitoring();

    expect(hardware.telemetry[0]).toMatchObject({
      state: "STALE", value: null, reasonCode: "MONITOR_START_FAILED",
      source: "Windows Performance Counters", observedAtUtc: "2026-10-14T09:00:00Z",
    });
    expect(hardware.telemetryError).toContain("MONITOR_START_FAILED");
  });

  it("loads a saved device profile and only retries storage access when explicitly requested", async () => {
    const profile = {
      device: { ...inventory.devices[0], id: "disk_ssd", kind: "physical_disk", category: "storage" },
      capabilities: [{
        deviceId: "disk_ssd", metricKey: "disk.temperature.celsius", kind: "sensor",
        state: "PERMISSION_REQUIRED", value: null, unit: "°C", source: "MSFT_StorageReliabilityCounter",
        evidenceLevel: null, observedAtUtc: "2026-10-14T09:00:00Z", reasonCode: "ACCESS_DENIED",
        nativeErrorCode: "0x80041003", details: {},
      }],
    };
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "load_device_profile") return profile;
      if (command === "probe_storage_reliability") return profile;
      return null;
    });
    const hardware = useHardwareStore();

    await hardware.loadDeviceProfile("disk_ssd");
    await hardware.probeStorageReliability("disk_ssd", true);

    expect(invokeMock).toHaveBeenNthCalledWith(1, "load_device_profile", { deviceId: "disk_ssd" });
    expect(invokeMock).toHaveBeenNthCalledWith(2, "probe_storage_reliability", {
      deviceId: "disk_ssd",
      retryAfterPermissionsChanged: true,
    });
    expect(hardware.profiles.disk_ssd?.capabilities[0]?.state).toBe("PERMISSION_REQUIRED");
  });
});
