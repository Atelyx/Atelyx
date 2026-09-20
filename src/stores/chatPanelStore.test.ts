/**
 * AI 对话面板写盘契约测试（stores/chatPanelStore.ts 的 flush → persistNow）。
 * 覆盖不依赖真实仓库 I/O 的语义：flush 按仓库身份键校验归属（空间模式下 root 恒 null，
 * 身份判别不得用 root）；在途读 + 切换身份时旧读被丢弃；写盘失败按指数退避重试且失败可见。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { CHAT_UNAVAILABLE_TEXT } from "@/constants/chat";

const h = vi.hoisted(() => {
  const state = {
    metaWrites: [] as unknown[],
    messageWriteFails: 0,
    messageWrites: [] as unknown[],
    gate: null as null | {
      armed: Promise<void>;
      resolveArmed: () => void;
      release: Promise<void>;
      resolveRelease: () => void;
    },
  };
  return {
    state,
    /** 挂起下一次 list_chat_sessions（在途读模拟）。 */
    gateListSessions() {
      let resolveArmed!: () => void;
      const armed = new Promise<void>((r) => (resolveArmed = r));
      let resolveRelease!: () => void;
      const release = new Promise<void>((r) => (resolveRelease = r));
      state.gate = { armed, resolveArmed, release, resolveRelease };
    },
    /** 等 load 已挂到门上（invoke 已消费 gate 并返回挂起 promise）。 */
    async waitGateArmed() {
      await h.state.gate?.armed;
    },
    releaseGate() {
      h.state.gate?.resolveRelease();
      h.state.gate = null;
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args: unknown) => {
    if (cmd === "list_chat_sessions") {
      if (h.state.gate) {
        const gate = h.state.gate;
        gate.resolveArmed();
        return gate.release.then(() => []);
      }
      return [];
    }
    if (cmd === "read_editor_chats_meta") return {};
    if (cmd === "write_editor_chats_meta") {
      h.state.metaWrites.push(args);
      return "";
    }
    if (cmd === "write_chat_messages") {
      if (h.state.messageWriteFails > 0) {
        h.state.messageWriteFails--;
        throw new Error("磁盘已满");
      }
      h.state.messageWrites.push(args);
      return "";
    }
    return "";
  },
}));

type ChatStore = typeof import("./chatPanelStore");
type AppStore = typeof import("./appStore");

let chat: ChatStore;
let app: AppStore;

/** 激活身份 root 并加载（load 后 sessionVaultKey = local:<root>、loaded = true）。 */
async function loadedInVault(root: string): Promise<void> {
  app.useAppStore.setState({ vaultIdentity: { kind: "local", root }, vaultRoot: root });
  await chat.useChatPanelStore.getState().load(true);
  expect(chat.useChatPanelStore.getState().sessionVaultKey).toBe(`local:${root}`);
}

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  h.state.metaWrites = [];
  h.state.messageWrites = [];
  h.state.messageWriteFails = 0;
  h.state.gate = null;
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

describe("flush 的仓库归属守卫（身份键语义）", () => {
  it("当前身份与内存会话一致 → 落盘", async () => {
    await loadedInVault("v1");
    chat.useChatPanelStore.getState().setModelOverride({ providerId: "p", model: "m" });
    await chat.useChatPanelStore.getState().flush();
    expect(h.state.metaWrites).toHaveLength(1);
  });

  it("在途期间身份已切换（空间 A→B：root 恒 null 无法按 root 判别）→ 不落盘", async () => {
    await loadedInVault("v1");
    // 模拟切换完成后（身份已变）才到达的迟到 flush
    app.useAppStore.setState({ vaultIdentity: { kind: "local", root: "v2" } });
    chat.useChatPanelStore.getState().setModelOverride({ providerId: "p", model: "m" });
    await chat.useChatPanelStore.getState().flush();
    expect(h.state.metaWrites).toHaveLength(0);
  });

  it("未加载（loaded=false）时不落盘，防空态覆盖磁盘历史", async () => {
    // 只置 sessionVaultKey：让 loaded 成为唯一拦截条件（否则身份守卫会先拦下，测不到该分支）
    chat.useChatPanelStore.setState({ sessionVaultKey: "local:v1" });
    chat.useChatPanelStore.getState().setModelOverride({ providerId: "p", model: "m" });
    await chat.useChatPanelStore.getState().flush();
    expect(h.state.metaWrites).toHaveLength(0);
  });
});

describe("load 的在途读竞态守卫（身份键语义）", () => {
  it("在途读 + 切换身份：旧仓库读取结果被丢弃，不覆盖新身份的空态", async () => {
    app.useAppStore.setState({ vaultIdentity: { kind: "local", root: "v1" }, vaultRoot: "v1" });
    h.gateListSessions();
    const loading = chat.useChatPanelStore.getState().load(true);
    await h.waitGateArmed();
    // 读取在途期间切换到 v2（真实切换会先 flush 旧仓库，这里只关注读竞态守卫）
    app.useAppStore.setState({ vaultIdentity: { kind: "local", root: "v2" } });
    h.releaseGate();
    await loading;
    const s = chat.useChatPanelStore.getState();
    // 旧仓库（v1）的加载结果不得落地：loaded 未置位、会话归属键不是 v1
    expect(s.loaded).toBe(false);
    expect(s.sessionVaultKey).not.toBe("local:v1");
    expect(s.sessions).toEqual([]);
  });
});

describe("写盘失败的退避重试与可见性", () => {
  /** 注册对话核心并发送一条消息（落进会话容器 + 置脏待落盘）。 */
  async function sendOne(): Promise<void> {
    const { registerChatRuntime } = await import("@/utils/chatRuntimeHost");
    const off = registerChatRuntime({
      resolveTarget: () => ({
        ok: true,
        provider: { id: "p1", name: "P", baseUrl: "http://x", apiKey: "k", models: [] },
        model: "m1",
      }),
      runTurn: async (req) => {
        req.sink.finish({ content: "答", steps: [], removed: false, timedOut: false, aborted: false });
      },
      compact: async () => ({ ok: false, aborted: false, message: "不应被调用" }),
      autoName: async () => "skipped",
    });
    try {
      await chat.useChatPanelStore.getState().send("你好");
    } finally {
      off();
    }
  }

  it("消息写盘失败：persistError 置位并按退避重试，成功后清空且消息不丢", async () => {
    await loadedInVault("v1");
    // persistNow 对单条写失败会先原地回落全量重写一次（幂等重建），连续失败才判定本轮失败
    h.state.messageWriteFails = 3;
    await sendOne();
    // debounce 500ms 到点：首写失败 → persistError 置位（失败可见）+ 脏保留
    await vi.advanceTimersByTimeAsync(500);
    expect(chat.useChatPanelStore.getState().persistError).not.toBeNull();
    expect(h.state.messageWrites).toHaveLength(0);
    // 首轮退避 500ms 后重试成功：persistError 清空、消息落盘
    await vi.advanceTimersByTimeAsync(500);
    expect(chat.useChatPanelStore.getState().persistError).toBeNull();
    expect(h.state.messageWrites).toHaveLength(1);
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
