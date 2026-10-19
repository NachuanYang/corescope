import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";

export type DeviceCategory = "cpu" | "gpu" | "memory" | "storage" | "network" | "battery" | "other";
export type DeviceLayer = "physical_device" | "logical_device" | "unknown" | "pnp_node";

export interface DeviceRecord {
  id: string;
  identityHash: string;
  identityConfidence: "STRONG" | "MODERATE" | "WEAK";
  category: DeviceCategory;
  kind: string;
  layer: DeviceLayer;
  displayName: string;
  manufacturer: string | null;
  isVirtual: boolean | null;
  isPresent: boolean | null;
  source: string;
  attributes: Record<string, unknown>;
}

export interface DeviceEdge {
  parentId: string;
  childId: string;
  relationship: string;
  evidenceSource: string;
  evidenceStatus: string;
}

export interface InventoryWarning {
  source: string;
  reasonCode: string;
  nativeCode: string | null;
}

export interface InventorySnapshot {
  scannedAtUtc: string;
  devices: DeviceRecord[];
  edges: DeviceEdge[];
  warnings: InventoryWarning[];
}

export interface CapabilityRecord {
  deviceId: string;
  metricKey: string;
  kind: string;
  state: MetricSample["state"];
  value: unknown | null;
  unit: string | null;
  source: string;
  evidenceLevel: MetricSample["evidenceLevel"];
  observedAtUtc: string | null;
  reasonCode: string | null;
  nativeErrorCode: string | null;
  details: Record<string, unknown>;
}

export interface DeviceProfile {
  device: DeviceRecord;
  capabilities: CapabilityRecord[];
}

export interface MetricSample {
  deviceId: string | null;
  metricKey: string;
  state: "AVAILABLE" | "NOT_APPLICABLE" | "NOT_TESTED" | "UNSUPPORTED" | "PERMISSION_REQUIRED" | "TEMP_UNAVAILABLE" | "UNKNOWN" | "PROBING" | "STALE";
  value: unknown | null;
  unit: string | null;
  source: string;
  evidenceLevel: "MEASURED" | "OS_REPORTED" | "SOURCE_REPORTED" | null;
  observedAtUtc: string | null;
  reasonCode: string | null;
  nativeErrorCode: string | null;
  details: Record<string, unknown>;
}

export interface MonitoringStatus {
  running: boolean;
  foreground: boolean;
  subscriptionCount: number;
  sampleIntervalMs: number;
  latestObservedAtUtc: string | null;
}

function hasFreshTelemetryFrame(
  monitoring: MonitoringStatus | null,
  samples: MetricSample[],
  startedAtUtc: string | null,
): boolean {
  if (!monitoring?.running || !monitoring.foreground || !startedAtUtc) return false;
  const startedAt = Date.parse(startedAtUtc);
  if (!Number.isFinite(startedAt)) return false;

  const now = Date.now();
  const freshnessMs = Math.max(1, monitoring.sampleIntervalMs) * 3;
  const latestByKey = new Map<string, MetricSample>();
  for (const sample of samples) {
    if (!sample.observedAtUtc) continue;
    const observedAt = Date.parse(sample.observedAtUtc);
    if (!Number.isFinite(observedAt) || observedAt < startedAt || observedAt > now + 1000
      || now - observedAt > freshnessMs) continue;
    const key = `${sample.deviceId ?? "system"}:${sample.metricKey}`;
    const current = latestByKey.get(key);
    if (!current || Date.parse(current.observedAtUtc ?? "") <= observedAt) latestByKey.set(key, sample);
  }

  const memory = latestByKey.get("system:memory.used.bytes");
  if (memory?.state !== "AVAILABLE" || typeof memory.value !== "number") return false;
  return ![...latestByKey.values()].some((sample) => sample.state === "PROBING");
}

export const useHardwareStore = defineStore("hardware", {
  state: () => ({
    snapshot: null as InventorySnapshot | null,
    isScanning: false,
    isCancellingScan: false,
    scanError: "",
    telemetry: [] as MetricSample[],
    monitoring: null as MonitoringStatus | null,
    telemetryWarmupStartedAtUtc: null as string | null,
    telemetryWarmupReady: false,
    telemetryError: "",
    profiles: {} as Record<string, DeviceProfile>,
    loadingProfiles: [] as string[],
    profileErrors: {} as Record<string, string>,
  }),
  getters: {
    hasScanned: (state) => state.snapshot !== null,
  },
  actions: {
    async loadInventory() {
      this.scanError = "";
      try {
        this.snapshot = await invoke<InventorySnapshot | null>("load_inventory");
        if (this.snapshot) await this.startMonitoring();
      } catch (error) {
        this.scanError = String(error);
      }
    },
    async scan(): Promise<boolean> {
      if (this.isScanning) return false;
      this.isScanning = true;
      this.isCancellingScan = false;
      this.scanError = "";
      try {
        this.snapshot = await invoke<InventorySnapshot>("scan_devices");
        await this.startMonitoring();
        return true;
      } catch (error) {
        const code = String(error);
        this.scanError = code.includes("SCAN_CANCELLED") ? ""
          : code.includes("SCAN_TIMEOUT") ? "SCAN_TIMEOUT"
            : code.includes("SCAN_ALREADY_RUNNING") ? "SCAN_ALREADY_RUNNING"
              : code;
        return false;
      } finally {
        this.isScanning = false;
        this.isCancellingScan = false;
      }
    },
    async cancelScan() {
      if (!this.isScanning || this.isCancellingScan) return;
      this.isCancellingScan = true;
      try {
        await invoke("cancel_scan");
      } catch (error) {
        const code = String(error);
        if (!code.includes("SCAN_NOT_RUNNING")) this.scanError = code;
        this.isCancellingScan = false;
      }
    },
    async startMonitoring() {
      if (!this.snapshot) return;
      this.telemetryError = "";
      const deviceIds = [...new Set(this.snapshot.devices
        .filter((device) => device.layer !== "pnp_node" && device.isPresent === true
          && device.isVirtual !== true && device.kind !== "partition"
          && ["cpu", "gpu_adapter", "memory_module", "physical_disk", "network_adapter"].includes(device.kind))
        .map((device) => device.id))];
      if (deviceIds.length === 0) {
        this.monitoring = null;
        this.telemetry = [];
        this.telemetryWarmupStartedAtUtc = null;
        this.telemetryWarmupReady = true;
        return;
      }
      const warmupStartedAt = Date.now();
      this.telemetryWarmupStartedAtUtc = new Date(warmupStartedAt).toISOString();
      this.telemetryWarmupReady = false;
      try {
        this.monitoring = await invoke<MonitoringStatus>("start_monitoring", { deviceIds });
        await this.refreshTelemetry();
        const sampleIntervalMs = Math.max(250, this.monitoring.sampleIntervalMs || 2000);
        const pollIntervalMs = Math.min(sampleIntervalMs, 1000);
        const warmupTimeoutMs = Math.max(3000, sampleIntervalMs * 2);
        const deadline = warmupStartedAt + warmupTimeoutMs;
        while (!this.telemetryWarmupReady && Date.now() < deadline
          && this.monitoring?.running && this.monitoring.foreground) {
          await new Promise<void>((resolve) => window.setTimeout(resolve, pollIntervalMs));
          await this.refreshTelemetry();
        }
      } catch (error) {
        this.telemetryWarmupReady = false;
        this.telemetryError = String(error);
        this.telemetry = this.telemetry.map((sample) => sample.state === "AVAILABLE"
          ? { ...sample, state: "STALE", value: null, reasonCode: "MONITOR_START_FAILED" }
          : sample);
      }
    },
    async refreshTelemetry() {
      try {
        const frame = await invoke<{ monitoring: MonitoringStatus; samples: MetricSample[] }>("load_telemetry");
        this.monitoring = frame.monitoring;
        this.telemetry = frame.samples;
        this.telemetryWarmupReady = hasFreshTelemetryFrame(
          this.monitoring,
          this.telemetry,
          this.telemetryWarmupStartedAtUtc,
        );
        this.telemetryError = "";
      } catch (error) {
        this.telemetryWarmupReady = false;
        this.telemetryError = String(error);
        this.telemetry = this.telemetry.map((sample) => sample.state === "AVAILABLE"
          ? { ...sample, state: "STALE", value: null, reasonCode: "TELEMETRY_REFRESH_FAILED" }
          : sample);
      }
    },
    async loadDeviceProfile(deviceId: string) {
      if (!this.loadingProfiles.includes(deviceId)) this.loadingProfiles.push(deviceId);
      delete this.profileErrors[deviceId];
      try {
        const profile = await invoke<DeviceProfile | null>("load_device_profile", { deviceId });
        if (!profile) throw new Error("DEVICE_PROFILE_NOT_FOUND");
        this.profiles[deviceId] = profile;
        return profile;
      } catch (error) {
        this.profileErrors[deviceId] = String(error);
        return null;
      } finally {
        this.loadingProfiles = this.loadingProfiles.filter((id) => id !== deviceId);
      }
    },
    async probeStorageReliability(deviceId: string, retryAfterPermissionsChanged = false) {
      if (!this.loadingProfiles.includes(deviceId)) this.loadingProfiles.push(deviceId);
      delete this.profileErrors[deviceId];
      try {
        const profile = await invoke<DeviceProfile>("probe_storage_reliability", {
          deviceId,
          retryAfterPermissionsChanged,
        });
        this.profiles[deviceId] = profile;
        return profile;
      } catch (error) {
        this.profileErrors[deviceId] = String(error);
        return null;
      } finally {
        this.loadingProfiles = this.loadingProfiles.filter((id) => id !== deviceId);
      }
    },
    async stopMonitoring() {
      try {
        this.monitoring = await invoke<MonitoringStatus>("stop_monitoring");
        this.telemetry = [];
        this.telemetryWarmupReady = false;
        this.telemetryWarmupStartedAtUtc = null;
      } catch (error) {
        this.telemetryError = String(error);
      }
    },
  },
});
