import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";

export interface NativeAppInfo {
  productName: string;
  version: string;
  databaseReady: boolean;
}

export const useAppStore = defineStore("app", {
  state: () => ({
    info: null as NativeAppInfo | null,
    startupError: "" as string,
    isLoading: false,
  }),
  actions: {
    async loadInfo() {
      this.isLoading = true;
      this.startupError = "";
      try {
        this.info = await invoke<NativeAppInfo>("app_info");
      } catch (error) {
        this.startupError = String(error);
      } finally {
        this.isLoading = false;
      }
    },
  },
});
