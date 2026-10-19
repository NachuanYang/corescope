import { invoke } from "@tauri-apps/api/core";
import { defineStore } from "pinia";

export type NetworkRoute = "system_proxy" | "direct";

export interface AuthAccount {
  clientId: string;
  email: string | null;
  displayName: string | null;
  connected: boolean;
  planUsageAuthorized: boolean;
}

export interface ChatGptAuthState {
  accounts: AuthAccount[];
  activeClientId: string | null;
  connected: boolean;
  planUsageAuthorized: boolean;
}

export interface AvailableModel {
  slug: string;
  displayName: string;
}

export interface InferenceVerification {
  modelSlug: string;
  responseText: string;
  completedAtUtc: string;
}

interface DisconnectResult {
  state: ChatGptAuthState;
  remoteRevocationConfirmed: boolean;
}

const ROUTE_KEY = "corescope.auth.route";
const MODELS_KEY = "corescope.auth.models";

function storedRoute(): NetworkRoute {
  return localStorage.getItem(ROUTE_KEY) === "direct" ? "direct" : "system_proxy";
}

function storedModels(): Record<string, string> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(MODELS_KEY) ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).filter(([key, slug]) =>
      key.startsWith("oaiapp_") && typeof slug === "string" && slug.length <= 160,
    ));
  } catch {
    return {};
  }
}

export const useAuthStore = defineStore("auth", {
  state: () => ({
    connection: null as ChatGptAuthState | null,
    models: [] as AvailableModel[],
    route: storedRoute(),
    selectedModelByClient: storedModels(),
    verification: null as InferenceVerification | null,
    isInitializing: false,
    isConnecting: false,
    isLoadingModels: false,
    isVerifying: false,
    isDisconnecting: false,
    errorCode: "",
    operationStatus: "",
  }),
  getters: {
    activeAccount: (state): AuthAccount | null => state.connection?.accounts.find(
      (account) => account.clientId === state.connection?.activeClientId,
    ) ?? null,
    selectedModelSlug: (state): string => {
      const clientId = state.connection?.activeClientId;
      return clientId ? state.selectedModelByClient[clientId] ?? "" : "";
    },
  },
  actions: {
    async initialize() {
      if (this.isInitializing) return;
      this.isInitializing = true;
      try {
        await this.refreshState();
        if (this.connection?.connected && this.connection.planUsageAuthorized) await this.loadModels();
      } finally {
        this.isInitializing = false;
      }
    },

    async refreshState() {
      this.errorCode = "";
      try {
        this.connection = await invoke<ChatGptAuthState>("get_chatgpt_auth_state");
        if (!this.connection.connected || !this.connection.planUsageAuthorized) {
          this.models = [];
          this.verification = null;
        }
      } catch (error) {
        this.errorCode = errorCode(error, "AUTH_STATE_LOAD_FAILED");
      }
    },

    async connect(clientId?: string | null) {
      const selectedClientId = clientId === undefined ? this.connection?.activeClientId ?? null : clientId;
      this.errorCode = "";
      this.operationStatus = "";
      this.isConnecting = true;
      try {
        this.connection = await invoke<ChatGptAuthState>("connect_chatgpt", {
          clientId: selectedClientId,
          route: this.route,
        });
        this.verification = null;
        this.models = [];
        this.operationStatus = "ChatGPT 授权已更新";
        if (this.connection.connected && this.connection.planUsageAuthorized) await this.loadModels();
      } catch (error) {
        const code = errorCode(error, "AUTH_CONNECT_FAILED");
        if (code === "AUTH_SIGN_IN_CANCELLED") this.operationStatus = "授权已取消";
        else if (code === "AUTH_USER_DENIED") this.operationStatus = "授权未完成";
        else this.errorCode = code;
      } finally {
        this.isConnecting = false;
      }
    },

    async cancelConnect() {
      try {
        await invoke<boolean>("cancel_chatgpt_sign_in");
      } catch (error) {
        this.errorCode = errorCode(error, "AUTH_CANCEL_FAILED");
      }
    },

    async loadModels(force = false) {
      const clientId = this.connection?.activeClientId;
      if (!clientId || !this.connection?.planUsageAuthorized) {
        this.models = [];
        return;
      }
      this.isLoadingModels = true;
      this.errorCode = "";
      try {
        this.models = await invoke<AvailableModel[]>("list_chatgpt_models", {
          clientId,
          route: this.route,
          ...(force ? { forceRefresh: true } : {}),
        });
        const selected = this.selectedModelByClient[clientId];
        if (!selected || !this.models.some((model) => model.slug === selected)) {
          if (this.models[0]) this.selectedModelByClient[clientId] = this.models[0].slug;
          else delete this.selectedModelByClient[clientId];
          persistModels(this.selectedModelByClient);
        }
        this.operationStatus = this.models.length ? `已读取 ${this.models.length} 个可用模型` : "当前账号没有可用模型";
      } catch (error) {
        this.models = [];
        this.errorCode = errorCode(error, "AUTH_MODELS_LOAD_FAILED");
      } finally {
        this.isLoadingModels = false;
      }
    },

    setModel(slug: string) {
      const clientId = this.connection?.activeClientId;
      if (!clientId || !this.models.some((model) => model.slug === slug)) return;
      this.selectedModelByClient[clientId] = slug;
      persistModels(this.selectedModelByClient);
      this.verification = null;
    },

    setRoute(route: NetworkRoute) {
      this.route = route === "direct" ? "direct" : "system_proxy";
      localStorage.setItem(ROUTE_KEY, this.route);
      this.models = [];
      this.verification = null;
      if (this.connection?.connected && this.connection.planUsageAuthorized) void this.loadModels(true);
    },

    async verifyInference() {
      const clientId = this.connection?.activeClientId;
      const modelSlug = this.selectedModelSlug;
      if (!clientId || !this.connection?.planUsageAuthorized) {
        this.errorCode = "AUTH_PLAN_USAGE_SCOPE_REQUIRED";
        return;
      }
      if (!modelSlug) {
        this.errorCode = "AUTH_MODEL_NOT_SELECTED";
        return;
      }
      this.isVerifying = true;
      this.errorCode = "";
      try {
        this.verification = await invoke<InferenceVerification>("verify_chatgpt_inference", {
          clientId,
          modelSlug,
          route: this.route,
        });
        this.operationStatus = "模型流式响应已完成";
      } catch (error) {
        this.errorCode = errorCode(error, "AUTH_INFERENCE_FAILED");
      } finally {
        this.isVerifying = false;
      }
    },

    async disconnect(): Promise<boolean | null> {
      const clientId = this.connection?.activeClientId;
      if (!clientId) return null;
      this.isDisconnecting = true;
      this.errorCode = "";
      this.operationStatus = "";
      try {
        const result = await invoke<DisconnectResult>("disconnect_chatgpt", {
          clientId,
          route: this.route,
        });
        this.connection = result.state;
        this.models = [];
        this.verification = null;
        delete this.selectedModelByClient[clientId];
        persistModels(this.selectedModelByClient);
        this.operationStatus = result.remoteRevocationConfirmed
          ? "已断开 ChatGPT 授权"
          : "本机凭据已清除；远端撤销未确认，可在 ChatGPT 用量设置中检查授权";
        return result.remoteRevocationConfirmed;
      } catch (error) {
        this.errorCode = errorCode(error, "AUTH_DISCONNECT_FAILED");
        return null;
      } finally {
        this.isDisconnecting = false;
      }
    },

  },
});

function persistModels(models: Record<string, string>) {
  localStorage.setItem(MODELS_KEY, JSON.stringify(models));
}

function errorCode(error: unknown, fallback: string): string {
  return typeof error === "string" && /^[A-Z0-9_.:-]{1,120}$/.test(error) ? error : fallback;
}
