import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";

export interface CacheSummary {
  databaseBytes: number;
  scanCount: number;
  deviceCount: number;
  metricHistoryCount: number;
  aiAnalysisCount: number;
  chatMessageCount: number;
  chatThreadCount: number;
  modelKnowledgeCount: number;
  priceReferenceCount: number;
  clearableAiItems: number;
}

export interface MetricHistoryRecord {
  deviceId: string;
  metricKey: string;
  value: unknown | null;
  unit: string | null;
  state: string;
  observedAtUtc: string | null;
  source: string;
  evidenceLevel: string | null;
  reasonCode: string | null;
}

export const useSettingsStore = defineStore("settings", {
  state: () => ({
    cacheSummary: null as CacheSummary | null,
    historyByDevice: {} as Record<string, MetricHistoryRecord[]>,
    historyErrors: {} as Record<string, string>,
    loadingCache: false,
    loadingHistory: [] as string[],
    isClearingCache: false,
    isExportingDiagnostics: false,
    operationStatus: "",
    error: "",
  }),
  actions: {
    async loadCacheSummary() {
      this.loadingCache = true;
      this.error = "";
      try {
        this.cacheSummary = await invoke<CacheSummary>("get_cache_summary");
      } catch (error) {
        this.error = String(error);
      } finally {
        this.loadingCache = false;
      }
    },
    async loadDeviceHistory(deviceId: string) {
      if (this.loadingHistory.includes(deviceId)) return;
      this.loadingHistory.push(deviceId);
      delete this.historyErrors[deviceId];
      try {
        this.historyByDevice[deviceId] = await invoke<MetricHistoryRecord[]>("load_metric_history", { deviceId });
      } catch (error) {
        this.historyErrors[deviceId] = String(error);
        this.error = String(error);
      } finally {
        this.loadingHistory = this.loadingHistory.filter((id) => id !== deviceId);
      }
    },
    async clearAiCache() {
      this.isClearingCache = true;
      this.error = "";
      this.operationStatus = "";
      try {
        const removed = await invoke<number>("clear_ai_cache");
        await this.loadCacheSummary();
        this.operationStatus = removed > 0 ? "AI 与问答缓存已清理" : "没有可清理的 AI 缓存";
        return removed;
      } catch (error) {
        this.error = String(error);
        return null;
      } finally {
        this.isClearingCache = false;
      }
    },
    async exportDiagnostics(path: string) {
      this.isExportingDiagnostics = true;
      this.error = "";
      this.operationStatus = "";
      try {
        await invoke("export_sanitized_diagnostics", { path });
        this.operationStatus = "脱敏诊断已导出";
        return true;
      } catch (error) {
        this.error = String(error);
        return false;
      } finally {
        this.isExportingDiagnostics = false;
      }
    },
  },
});
