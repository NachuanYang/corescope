import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";
import type { NetworkRoute } from "./auth";
import type { EvidenceSnapshot } from "./analysis";

export type ChatScope = "system" | "device";
export type ChatMessageStatus = "PENDING" | "COMPLETED" | "FAILED" | "INTERRUPTED";

export interface ChatThreadRecord {
  id: string;
  scope: ChatScope;
  targetDeviceId: string | null;
  createdAtUtc: string;
  updatedAtUtc: string;
}

export interface ChatCitation {
  claim: string;
  evidenceKeys: string[];
}

export interface ChatMessageRecord {
  id: string;
  threadId: string;
  role: "user" | "assistant";
  body: string;
  evidenceSnapshotRef: string | null;
  citations: ChatCitation[];
  createdAtUtc: string;
  status: ChatMessageStatus;
  errorCode: string | null;
  modelSlug: string | null;
  source: string | null;
  snapshotCapturedAtUtc: string | null;
}

export interface ChatPreview {
  token: string;
  scope: ChatScope;
  threadId: string | null;
  snapshot: EvidenceSnapshot;
  question: string;
  recentTurns: Array<{ role: string; status: string; body: string }>;
}

export interface ChatSendOutcome {
  status: ChatMessageStatus;
  userMessageId: string;
  assistantMessage: ChatMessageRecord | null;
  errorCode: string | null;
}

const SAFE_ERROR_CODE = /^[A-Z0-9_.:-]{1,120}$/;

function errorCode(error: unknown, fallback: string): string {
  return typeof error === "string" && SAFE_ERROR_CODE.test(error) ? error : fallback;
}

export const useChatStore = defineStore("chat", {
  state: () => ({
    scope: "system" as ChatScope,
    targetDeviceId: null as string | null,
    threads: [] as ChatThreadRecord[],
    selectedThreadId: null as string | null,
    messages: [] as ChatMessageRecord[],
    preview: null as ChatPreview | null,
    isLoading: false,
    isCreatingThread: false,
    isCreatingPreview: false,
    isSending: false,
    errorCode: "",
    operationStatus: "",
  }),
  getters: {
    selectedThread: (state): ChatThreadRecord | null => state.threads.find(
      (thread) => thread.id === state.selectedThreadId,
    ) ?? null,
    canSend: (state): boolean => Boolean(state.selectedThreadId && !state.isCreatingPreview && !state.isSending),
  },
  actions: {
    async setContext(scope: ChatScope, targetDeviceId?: string) {
      this.scope = scope;
      this.targetDeviceId = scope === "device" ? targetDeviceId ?? null : null;
      this.preview = null;
      this.selectedThreadId = null;
      this.messages = [];
      return this.loadThreads();
    },

    async loadThreads() {
      this.isLoading = true;
      this.errorCode = "";
      try {
        const threads = await invoke<ChatThreadRecord[]>("list_chat_threads", {
          scope: this.scope,
          ...(this.targetDeviceId ? { targetDeviceId: this.targetDeviceId } : {}),
        });
        this.threads = Array.isArray(threads) ? threads : [];
        const selected = this.threads.find((thread) => thread.id === this.selectedThreadId) ?? this.threads[0] ?? null;
        this.selectedThreadId = selected?.id ?? null;
        if (selected) await this.loadMessages(selected.id);
        else this.messages = [];
        return this.threads;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_CHAT_THREAD_LOAD_FAILED");
        return [];
      } finally {
        this.isLoading = false;
      }
    },

    async loadMessages(threadId: string) {
      if (!this.threads.some((thread) => thread.id === threadId)) return [];
      this.isLoading = true;
      this.errorCode = "";
      try {
        const messages = await invoke<ChatMessageRecord[]>("load_chat_messages", { threadId });
        this.messages = Array.isArray(messages) ? messages : [];
        this.selectedThreadId = threadId;
        return this.messages;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_CHAT_MESSAGES_LOAD_FAILED");
        return [];
      } finally {
        this.isLoading = false;
      }
    },

    async createThread() {
      if (this.isCreatingThread) return null;
      this.isCreatingThread = true;
      this.errorCode = "";
      this.preview = null;
      try {
        const thread = await invoke<ChatThreadRecord>("create_chat_thread", {
          scope: this.scope,
          ...(this.targetDeviceId ? { targetDeviceId: this.targetDeviceId } : {}),
        });
        this.threads = [thread, ...this.threads.filter((item) => item.id !== thread.id)];
        this.selectedThreadId = thread.id;
        this.messages = [];
        this.operationStatus = "已新建本地对话";
        return thread;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_CHAT_THREAD_CREATE_FAILED");
        return null;
      } finally {
        this.isCreatingThread = false;
      }
    },

    async openThread(threadId: string) {
      if (!this.threads.some((thread) => thread.id === threadId)) return false;
      this.preview = null;
      this.selectedThreadId = threadId;
      await this.loadMessages(threadId);
      return true;
    },

    async clearCurrentThread() {
      const threadId = this.selectedThreadId;
      if (!threadId || this.isSending) return false;
      this.errorCode = "";
      this.preview = null;
      try {
        await invoke<number>("clear_chat_thread", { threadId });
        this.messages = [];
        this.operationStatus = "当前本地对话已清空";
        return true;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_CHAT_CLEAR_FAILED");
        return false;
      }
    },

    async createPreview(question: string) {
      if (this.isCreatingPreview || this.isSending) return null;
      if (!this.selectedThreadId) {
        const thread = await this.createThread();
        if (!thread) return null;
      }
      const threadId = this.selectedThreadId;
      if (!threadId) return null;
      this.isCreatingPreview = true;
      this.errorCode = "";
      this.operationStatus = "正在生成本次发送摘要";
      try {
        this.preview = await invoke<ChatPreview>("create_chat_preview", {
          scope: this.scope,
          ...(this.targetDeviceId ? { targetDeviceId: this.targetDeviceId } : {}),
          threadId,
          question,
        });
        this.operationStatus = "请核对本次提问将发送的设备摘要与对话上下文";
        return this.preview;
      } catch (error) {
        this.errorCode = errorCode(error, "AI_CHAT_PREVIEW_FAILED");
        this.operationStatus = "未能生成提问摘要";
        return null;
      } finally {
        this.isCreatingPreview = false;
      }
    },

    async sendPreview(clientId: string, modelSlug: string, route: NetworkRoute) {
      const preview = this.preview;
      const threadId = this.selectedThreadId;
      if (!preview || !threadId || this.isSending) {
        this.errorCode = "AI_CHAT_PREVIEW_REQUIRED";
        return null;
      }
      this.isSending = true;
      this.errorCode = "";
      this.operationStatus = "正在等待 ChatGPT 完整回复；本机采集继续运行";
      try {
        const outcome = await invoke<ChatSendOutcome>("send_chat_message", {
          previewToken: preview.token,
          scope: this.scope,
          ...(this.targetDeviceId ? { targetDeviceId: this.targetDeviceId } : {}),
          threadId,
          clientId,
          modelSlug,
          route,
        });
        this.preview = null;
        this.errorCode = outcome.errorCode ?? "";
        this.operationStatus = outcome.status === "COMPLETED"
          ? "回答已保存到本地对话"
          : outcome.status === "INTERRUPTED" ? "提问已中断，可重新发送" : "提问未完成，可检查授权或重试";
        await this.loadMessages(threadId);
        return outcome;
      } catch (error) {
        this.preview = null;
        this.errorCode = errorCode(error, "AI_CHAT_SEND_FAILED");
        this.operationStatus = "提问未发送，请查看诊断码后重试";
        return null;
      } finally {
        this.isSending = false;
      }
    },

    clearPreview() {
      this.preview = null;
    },

    async cancelRequest() {
      try {
        return await invoke<boolean>("cancel_ai_request");
      } catch (error) {
        this.errorCode = errorCode(error, "AI_CHAT_CANCEL_FAILED");
        return false;
      }
    },
  },
});
