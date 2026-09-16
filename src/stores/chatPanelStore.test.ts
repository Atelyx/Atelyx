/**
 * AI 对话面板写盘契约测试（stores/chatPanelStore.ts 的 flush → persistNow + 回启动页分发）。
 * 只覆盖不依赖真实仓库 I/O 的语义：flush 传入的期望仓库必须与内存会话所属仓库一致才落盘；
 * 回启动页的领域分发必须携带置空前的 vaultId。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { CHAT_UNAVAILABLE_TEXT } from "@/constants/chat";

const h = vi.hoisted(() => ({ metaWrites: [] as unknown[] }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args: unknown) => {
    if (cmd === "list_chat_sessions") return [];
    if (cmd === "read_editor_chats_meta") return {};
    if (cmd === "write_editor_chats_meta") {
      h.metaWrites.push(args);
      return "";
    }
    return "";
  },
}));

type ChatStore = typeof import("./chatPanelStore");
type AppStore = typeof import("./appStore");

let chat: ChatStore;
let app: AppStore;

/** 进仓并让内存会话归属 v1（load 后 sessionVaultId = v1、loaded = true）。 */
async function loadedInVault(vaultId: string): Promise<void> {
  app.useAppStore.setState({ vaultId });
  await chat.useChatPanelStore.getState().load(vaultId, true);
  expect(chat.useChatPanelStore.getState().sessionVaultId).toBe(vaultId);
}

beforeEach(async () => {
  vi.resetModules();
  h.metaWrites = [];
  // 先起 noteSessionStore 再取目标模块：chatPanelStore 经 pluginStore→builtins 与
  // noteSessionStore/settingsStore 成环，从其它模块进入会在环上取到尚未初始化的 export
  // （同 noteSessionStore.test.ts 的取模顺序）
  await import("./noteSessionStore");
  await import("./settingsStore");
  await import("./noteStore");
  await import("./pluginStore");
  app = await import("./appStore");
  chat = await import("./chatPanelStore");
});

describe("flush 的仓库归属守卫", () => {
  it("期望仓库与内存会话一致 → 落盘", async () => {
    await loadedInVault("v1");
    chat.useChatPanelStore.getState().setModelOverride({ providerId: "p", model: "m" });
    await chat.useChatPanelStore.getState().flush("v1");
    expect(h.metaWrites).toHaveLength(1);
  });

  it("期望仓库为 null（置空后回读 store 的结果）→ 不落盘，防跨仓库覆盖", async () => {
    await loadedInVault("v1");
    chat.useChatPanelStore.getState().setModelOverride({ providerId: "p", model: "m" });
    await chat.useChatPanelStore.getState().flush(null);
    expect(h.metaWrites).toHaveLength(0);
  });

  it("期望仓库与内存会话不一致 → 不落盘", async () => {
    await loadedInVault("v1");
    chat.useChatPanelStore.getState().setModelOverride({ providerId: "p", model: "m" });
    await chat.useChatPanelStore.getState().flush("v2");
    expect(h.metaWrites).toHaveLength(0);
  });

  it("未加载（loaded=false）时不落盘，防空态覆盖磁盘历史", async () => {
    // 只置 sessionVaultId：让 loaded 成为唯一拦截条件（否则仓库守卫会先拦下，测不到该分支）
    chat.useChatPanelStore.setState({ sessionVaultId: "v1" });
    chat.useChatPanelStore.getState().setModelOverride({ providerId: "p", model: "m" });
    await chat.useChatPanelStore.getState().flush("v1");
    expect(h.metaWrites).toHaveLength(0);
  });
});

describe("回启动页的 flush 契约（appStore.backToVaultSelect → 领域钩子）", () => {
  it("钩子收到置空前的 vaultId（分发晚于 store 置空，不得回读 store）", async () => {
    const lifecycle = await import("@/utils/kernelLifecycle");
    app.useAppStore.setState({ vaultId: "v1" });
    const got: (string | null)[] = [];
    const off = lifecycle.registerDomainLifecycle({
      id: "test.exit-flush",
      onVaultExit: async (ctx) => {
        got.push(ctx.vaultId);
      },
    });
    try {
      app.useAppStore.getState().backToVaultSelect();
      await vi.waitFor(() => expect(got).toEqual(["v1"]));
      expect(app.useAppStore.getState().vaultId).toBeNull();
    } finally {
      off();
    }
  });
});

describe("对话能力缺失/存在时的一致性（运行时判空降级 + 写入器回写）", () => {
  it("对话核心未启用：发送被拦下并给出可操作提示，不创建空会话", async () => {
    await loadedInVault("v1");
    await chat.useChatPanelStore.getState().send("你好");
    const s = chat.useChatPanelStore.getState();
    expect(s.sessions).toEqual([]);
    expect(s.error).toBe(CHAT_UNAVAILABLE_TEXT);
  });

  it("对话核心启用：本轮产出经写入器落进会话容器，流式态随收尾复位", async () => {
    const { registerChatRuntime } = await import("@/utils/chatRuntimeHost");
    await loadedInVault("v1");
    const off = registerChatRuntime({
      resolveTarget: () => ({
        ok: true,
        provider: { id: "p1", name: "P", baseUrl: "http://x", apiKey: "k", models: [] },
        model: "m1",
      }),
      runTurn: async (req) => {
        req.sink.update({ content: "答", steps: [] });
        req.sink.finish({ content: "答", steps: [], removed: false, timedOut: false, aborted: false });
      },
      compact: async () => ({ ok: false, aborted: false, message: "不应被调用" }),
      autoName: async () => "skipped",
    });
    try {
      await chat.useChatPanelStore.getState().send("你好");
      const s = chat.useChatPanelStore.getState();
      expect(s.sessions).toHaveLength(1);
      expect(s.sessions[0].messages.map((m) => [m.role, m.content])).toEqual([
        ["user", "你好"],
        ["assistant", "答"],
      ]);
      expect(s.streaming).toBe(false);
    } finally {
      off();
    }
  });
});
