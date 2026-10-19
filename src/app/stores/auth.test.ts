import { createPinia, setActivePinia } from "pinia";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAuthStore, type ChatGptAuthState } from "./auth";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

const disconnected: ChatGptAuthState = {
  accounts: [], activeClientId: null, connected: false, planUsageAuthorized: false,
};
const connected: ChatGptAuthState = {
  accounts: [{ clientId: "oaiapp_one", email: "one@example.invalid", displayName: "One", connected: true, planUsageAuthorized: true }],
  activeClientId: "oaiapp_one", connected: true, planUsageAuthorized: true,
};

describe("ChatGPT authorization store", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    localStorage.clear();
    invokeMock.mockReset().mockImplementation(async (command: string) => {
      if (command === "get_chatgpt_auth_state") return disconnected;
      if (command === "list_chatgpt_models") return [];
      return null;
    });
  });

  it("separates disconnected, connected, and plan scope states without carrying token fields", async () => {
    const auth = useAuthStore();
    await auth.initialize();
    expect(auth.connection?.connected).toBe(false);
    expect(auth.connection?.planUsageAuthorized).toBe(false);
    expect("accessToken" in auth).toBe(false);
    expect("refreshToken" in auth).toBe(false);

    invokeMock.mockResolvedValueOnce({ ...connected, planUsageAuthorized: false });
    await auth.refreshState();
    expect(auth.connection?.connected).toBe(true);
    expect(auth.connection?.planUsageAuthorized).toBe(false);
  });

  it("updates connection state immediately after sign in and loads that account's models", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "connect_chatgpt") return connected;
      if (command === "list_chatgpt_models") return [{ slug: "model-a", displayName: "Model A" }];
      return null;
    });
    const auth = useAuthStore();
    auth.route = "direct";
    await auth.connect();

    expect(auth.connection?.activeClientId).toBe("oaiapp_one");
    expect(auth.models.map((model) => model.slug)).toEqual(["model-a"]);
    expect(invokeMock).toHaveBeenCalledWith("connect_chatgpt", { clientId: null, route: "direct" });
    expect(invokeMock).toHaveBeenCalledWith("list_chatgpt_models", { clientId: "oaiapp_one", route: "direct" });
  });

  it("reuses the current account for authorization instead of creating or switching accounts", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "connect_chatgpt") return connected;
      if (command === "list_chatgpt_models") return [{ slug: "model-b", displayName: "Model B" }];
      return null;
    });
    const auth = useAuthStore();
    auth.connection = connected;
    await auth.connect();
    auth.setModel("model-b");

    expect(auth.connection?.activeClientId).toBe("oaiapp_one");
    expect(auth.models[0].slug).toBe("model-b");
    expect(auth.selectedModelSlug).toBe("model-b");
    expect(invokeMock).toHaveBeenCalledWith("connect_chatgpt", { clientId: "oaiapp_one", route: "system_proxy" });
    expect(invokeMock).not.toHaveBeenCalledWith("set_active_chatgpt_account", expect.anything());
  });

  it("keeps inference explicit, records only a completed result, and leaves hardware commands untouched", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_chatgpt_auth_state") return connected;
      if (command === "list_chatgpt_models") return [{ slug: "model-a", displayName: "Model A" }];
      if (command === "verify_chatgpt_inference") return {
        modelSlug: "model-a", responseText: "verified", completedAtUtc: "2026-10-14T10:00:00Z",
      };
      return null;
    });
    const auth = useAuthStore();
    await auth.initialize();
    auth.setModel("model-a");
    expect(auth.verification).toBeNull();
    await auth.verifyInference();

    expect(auth.verification?.responseText).toBe("verified");
    expect(invokeMock).toHaveBeenCalledWith("verify_chatgpt_inference", {
      clientId: "oaiapp_one", modelSlug: "model-a", route: "system_proxy",
    });
    expect(invokeMock).not.toHaveBeenCalledWith("scan_devices", expect.anything());
    expect(invokeMock).not.toHaveBeenCalledWith("start_monitoring", expect.anything());
  });

  it("keeps the last completed verification visible when a later stream fails", async () => {
    const auth = useAuthStore();
    auth.connection = connected;
    auth.models = [{ slug: "model-a", displayName: "Model A" }];
    auth.setModel("model-a");
    auth.verification = { modelSlug: "model-a", responseText: "previous", completedAtUtc: "2026-10-14T10:00:00Z" };
    invokeMock.mockRejectedValueOnce("AUTH_SSE_STREAM_INTERRUPTED");

    await auth.verifyInference();

    expect(auth.verification?.responseText).toBe("previous");
    expect(auth.errorCode).toBe("AUTH_SSE_STREAM_INTERRUPTED");
  });

  it("distinguishes an account model that is no longer available from a completed inference", async () => {
    const auth = useAuthStore();
    auth.connection = connected;
    auth.models = [{ slug: "model-a", displayName: "Model A" }];
    auth.setModel("model-a");
    invokeMock.mockRejectedValueOnce("AUTH_MODEL_NOT_AVAILABLE");

    await auth.verifyInference();

    expect(auth.verification).toBeNull();
    expect(auth.errorCode).toBe("AUTH_MODEL_NOT_AVAILABLE");
  });

  it("disconnects local credentials without clearing hardware inventory", async () => {
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_chatgpt_auth_state") return connected;
      if (command === "list_chatgpt_models") return [{ slug: "model-a", displayName: "Model A" }];
      if (command === "disconnect_chatgpt") return { state: disconnected, remoteRevocationConfirmed: false };
      return null;
    });
    const auth = useAuthStore();
    await auth.initialize();
    const result = await auth.disconnect();

    expect(result).toBe(false);
    expect(auth.connection?.connected).toBe(false);
    expect(invokeMock).toHaveBeenCalledWith("disconnect_chatgpt", { clientId: "oaiapp_one", route: "system_proxy" });
    expect(invokeMock).not.toHaveBeenCalledWith("clear_ai_cache");
  });

  it("cancels a pending browser sign in and allows network route selection", async () => {
    const auth = useAuthStore();
    auth.setRoute("direct");
    expect(auth.route).toBe("direct");
    expect(localStorage.getItem("corescope.auth.route")).toBe("direct");
    await auth.cancelConnect();
    expect(invokeMock).toHaveBeenCalledWith("cancel_chatgpt_sign_in");
  });
});
