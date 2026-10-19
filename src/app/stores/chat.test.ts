import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useChatStore, type ChatMessageRecord, type ChatThreadRecord } from "./chat";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const thread: ChatThreadRecord = {
  id: "thread-gpu", scope: "device", targetDeviceId: "gpu-1",
  createdAtUtc: "2026-10-14T12:00:00Z", updatedAtUtc: "2026-10-14T12:00:00Z",
};
const userMessage: ChatMessageRecord = {
  id: "message-user", threadId: thread.id, role: "user", body: "這張顯卡有溫度嗎？",
  evidenceSnapshotRef: "snapshot-1", citations: [], createdAtUtc: "2026-10-14T12:00:00Z",
  status: "COMPLETED", errorCode: null, modelSlug: null, source: null, snapshotCapturedAtUtc: "2026-10-14T12:00:00Z",
};
const assistantMessage: ChatMessageRecord = {
  ...userMessage, id: "message-assistant", role: "assistant", body: "本次快照中没有可读取的温度值。",
  citations: [{ claim: "本次快照没有温度值。", evidenceKeys: ["metric.device.gpu.temperature.celsius"] }],
  createdAtUtc: "2026-10-14T12:00:01Z", modelSlug: "model-one", source: "ChatGPT Responses API",
};

describe("hardware chat store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    invokeMock.mockReset().mockImplementation(async (command: string) => {
      if (command === "list_chat_threads") return [thread];
      if (command === "load_chat_messages") return [userMessage, assistantMessage];
      if (command === "create_chat_thread") return thread;
      if (command === "create_chat_preview") return {
        token: "preview-1", scope: "device", threadId: thread.id,
        snapshot: { snapshotId: "snapshot-1", scope: "device", capturedAtUtc: "2026-10-14T12:00:00Z", scanAtUtc: "2026-10-14T11:59:00Z", devices: [], metrics: [] },
        question: "這張顯卡有溫度嗎？", recentTurns: [],
      };
      if (command === "send_chat_message") return { status: "COMPLETED", userMessageId: userMessage.id, assistantMessage, errorCode: null };
      if (command === "cancel_ai_request") return true;
      if (command === "clear_chat_thread") return 2;
      return null;
    });
  });

  it("loads threads and messages for only the selected device context", async () => {
    const chat = useChatStore();
    await chat.setContext("device", "gpu-1");

    expect(invokeMock).toHaveBeenCalledWith("list_chat_threads", { scope: "device", targetDeviceId: "gpu-1" });
    expect(invokeMock).toHaveBeenCalledWith("load_chat_messages", { threadId: thread.id });
    expect(chat.messages.map((message) => message.role)).toEqual(["user", "assistant"]);
  });

  it("reviews the selected snapshot before sending and reloads the completed response", async () => {
    const chat = useChatStore();
    await chat.setContext("device", "gpu-1");
    await chat.createPreview("這張顯卡有溫度嗎？");
    expect(chat.preview?.snapshot.snapshotId).toBe("snapshot-1");
    expect(invokeMock).toHaveBeenCalledWith("create_chat_preview", {
      scope: "device", targetDeviceId: "gpu-1", threadId: thread.id, question: "這張顯卡有溫度嗎？",
    });

    const result = await chat.sendPreview("oaiapp_one", "model-one", "system_proxy");
    expect(result?.status).toBe("COMPLETED");
    expect(invokeMock).toHaveBeenCalledWith("send_chat_message", {
      previewToken: "preview-1", scope: "device", targetDeviceId: "gpu-1", threadId: thread.id,
      clientId: "oaiapp_one", modelSlug: "model-one", route: "system_proxy",
    });
    expect(chat.preview).toBeNull();
    expect(chat.messages[chat.messages.length - 1]?.source).toBe("ChatGPT Responses API");
  });

  it("creates a fresh local conversation for the current scope and cancels the shared AI request", async () => {
    const chat = useChatStore();
    await chat.setContext("system");
    await chat.createThread();
    expect(invokeMock).toHaveBeenCalledWith("create_chat_thread", { scope: "system" });
    expect(chat.selectedThreadId).toBe(thread.id);

    await chat.cancelRequest();
    expect(invokeMock).toHaveBeenCalledWith("cancel_ai_request");
  });

  it("clears only the selected local conversation", async () => {
    const chat = useChatStore();
    await chat.setContext("device", "gpu-1");
    const cleared = await chat.clearCurrentThread();

    expect(cleared).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("clear_chat_thread", { threadId: thread.id });
    expect(chat.threads).toHaveLength(1);
    expect(chat.messages).toEqual([]);
  });
});
