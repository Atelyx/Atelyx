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
    tempWrites: [] as Array<{ canvasId: string; fileName: string; base64Data: string }>,
    tempWriteFails: 0,
    attachmentDataUrl: "",
    /** 内存会话文件系统：file → .jsonl 内容（list/read/write/delete 共用，模拟重启重读）。 */
    chatFiles: new Map<string, string>(),
    chatMetas: new Map<string, string>(),
    /** 追加命令留痕（file + 追加的消息 id 序列；纯增长路径断言用）。 */
    appends: [] as Array<{ file: string; ids: string[] }>,
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
      const rows = [...h.state.chatFiles.keys()].map((file) => {
        const id = file.split("/").pop()!.replace(/\.jsonl$/, "");
        const metaRaw = h.state.chatMetas.get(file.replace(/\.jsonl$/, ".meta.json"));
        return {
          id,
          file,
          meta: metaRaw ? (JSON.parse(metaRaw) as Record<string, unknown>) : null,
        };
      });
      if (h.state.gate) {
        const gate = h.state.gate;
        gate.resolveArmed();
        return gate.release.then(() => rows);
      }
      return rows;
    }
    if (cmd === "read_chat_messages") {
      const content = h.state.chatFiles.get((args as { file: string }).file);
      if (content === undefined) throw new Error("会话消息不存在");
      return content;
    }
    if (cmd === "append_chat_messages") {
      const a = args as { file: string; records: Array<{ id: string }> };
      const existing = h.state.chatFiles.get(a.file) ?? "";
      const lines = a.records.map((r) => JSON.stringify(r));
      h.state.chatFiles.set(a.file, existing ? `${existing}\n${lines.join("\n")}` : lines.join("\n"));
      h.state.appends.push({ file: a.file, ids: a.records.map((r) => r.id) });
      return "";
    }
    if (cmd === "delete_chat_messages") {
      h.state.chatFiles.delete((args as { file: string }).file);
      return "";
    }
    if (cmd === "write_chat_session_meta") {
      const a = args as { file: string; meta: unknown };
      h.state.chatMetas.set(a.file, JSON.stringify(a.meta));
      return "";
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
      h.state.chatFiles.set((args as { file: string }).file, (args as { content: string }).content);
      return "";
    }
    if (cmd === "read_attachment_data_url") return h.state.attachmentDataUrl;
    if (cmd === "write_temp_attachment") {
      if (h.state.tempWriteFails > 0) {
        h.state.tempWriteFails--;
        throw new Error("附件写入失败");
      }
      h.state.tempWrites.push(args as { canvasId: string; fileName: string; base64Data: string });
      return ".atelyx/temp/0123456789abcdef/att-1-笔记.txt";
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
  h.state.tempWrites = [];
  h.state.tempWriteFails = 0;
  h.state.appends = [];
  h.state.attachmentDataUrl = "";
  h.state.chatFiles.clear();
  h.state.chatMetas.clear();
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
  /** 等待在途轮收尾（send 只等预检、轮转 fire-and-forget：轮末状态靠微任务推进）。 */
  async function settleTurn(): Promise<void> {
    for (let i = 0; i < 50 && chat.useChatPanelStore.getState().streaming; i++) {
      await vi.advanceTimersByTimeAsync(1);
    }
  }

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
      await settleTurn();
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

describe("面板附件通道（临时区落盘 + 引用持久化）", () => {
  /** 注册对话核心（本轮立即完成），返回取消注册。 */
  async function withRuntime(): Promise<() => void> {
    const { registerChatRuntime } = await import("@/utils/chatRuntimeHost");
    return registerChatRuntime({
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
  }

  it("发送带附件：字节按会话 id 落临时区，消息持久化剥离 payload 只留路径引用", async () => {
    await loadedInVault("v1");
    const off = await withRuntime();
    try {
      const blob = new File([new TextEncoder().encode("正文内容")], "笔记.txt", { type: "text/plain" });
      const pending = {
        id: "att-1",
        kind: "file" as const,
        mime: "text/plain",
        filename: "笔记.txt",
        payload: "正文内容",
        blob,
      };
      const ok = await chat.useChatPanelStore.getState().send("看这个", [], [pending]);
      expect(ok).toBe(true);

      // 字节落临时区：目录归属 = 会话 id（发送时已确定，新会话亦然）
      expect(h.state.tempWrites).toHaveLength(1);
      expect(h.state.tempWrites[0].fileName).toBe("笔记.txt");
      const s = chat.useChatPanelStore.getState();
      expect(s.sessions[0].messages[0].attachments?.[0]?.file).toBe(
        ".atelyx/temp/0123456789abcdef/att-1-笔记.txt",
      );

      // 持久化记录：附件按 file 引用落盘，运行时缓存 payload 不得进 .jsonl
      await chat.useChatPanelStore.getState().flush();
      expect(h.state.messageWrites).toHaveLength(1);
      const record = JSON.parse(
        (h.state.messageWrites[0] as { content: string }).content.split("\n")[0],
      ) as { attachments?: Array<{ file?: string; payload?: string }> };
      expect(record.attachments?.[0]?.file).toBe(".atelyx/temp/0123456789abcdef/att-1-笔记.txt");
      expect(record.attachments?.[0]).not.toHaveProperty("payload");
    } finally {
      off();
    }
  });

  it("附件落盘失败：send 返回 false，不创建消息、新会话不残留（草稿可保留待重试）", async () => {
    await loadedInVault("v1");
    h.state.tempWriteFails = 1;
    const off = await withRuntime();
    try {
      const blob = new File(["x"], "a.png", { type: "image/png" });
      const ok = await chat.useChatPanelStore.getState().send("看图", [], [
        { id: "att-1", kind: "image", mime: "image/png", filename: "a.png", blob },
      ]);
      expect(ok).toBe(false);
      const s = chat.useChatPanelStore.getState();
      expect(s.sessions).toEqual([]);
      expect(s.activeSessionId).toBeNull();
      expect(h.state.messageWrites).toHaveLength(0);
    } finally {
      off();
    }
  });

  it("重启后打开会话：附件按引用读回 payload（水合恢复图片显示）", async () => {
    await loadedInVault("v1");
    const off = await withRuntime();
    try {
      h.state.attachmentDataUrl = "data:image/png;base64,aGVsbG8=";
      const blob = new File(["x"], "a.png", { type: "image/png" });
      await chat.useChatPanelStore.getState().send("看图", [], [
        {
          id: "att-1",
          kind: "image",
          mime: "image/png",
          filename: "a.png",
          payload: "data:image/png;base64,old",
          blob,
        },
      ]);
      const sessionId = chat.useChatPanelStore.getState().activeSessionId!;
      // 模拟重启：落盘后强制重读（记录里 payload 已剥离），附件无运行时缓存
      await chat.useChatPanelStore.getState().flush();
      await chat.useChatPanelStore.getState().load(true);
      const restored = chat.useChatPanelStore
        .getState()
        .sessions.find((s) => s.id === sessionId);
      expect(restored?.messages[0].attachments?.[0]?.payload).toBeUndefined();
      // 打开会话 → 水合按引用读回
      chat.useChatPanelStore.getState().openSession(sessionId);
      for (let i = 0; i < 20; i++) {
        await vi.advanceTimersByTimeAsync(1);
        const msg = chat.useChatPanelStore
          .getState()
          .sessions.find((s) => s.id === sessionId)
          ?.messages[0];
        if (msg?.attachments?.[0]?.payload) break;
      }
      const msg = chat.useChatPanelStore
        .getState()
        .sessions.find((s) => s.id === sessionId)
        ?.messages[0];
      expect(msg?.attachments?.[0]?.payload).toBe("data:image/png;base64,aGVsbG8=");
    } finally {
      off();
    }
  });
});

describe("插件侧会话写入（importSession / appendMessages）", () => {
  it("importSession：校验转换 + 落盘（不改激活会话，id 撞车重生成，payload 剥离）", async () => {
    await loadedInVault("v1");
    const { id } = await chat.useChatPanelStore.getState().importSession(
      [
        { id: "m1", role: "user", content: "hi", displayContent: "提问" },
        // 撞车 id：宿主重生成，面板会话内唯一
        { id: "m1", role: "assistant", content: "答" },
      ],
      { title: "插件会话" },
    );
    const s = chat.useChatPanelStore.getState();
    expect(s.sessions).toHaveLength(1);
    expect(s.activeSessionId).toBeNull();
    const session = s.sessions.find((x) => x.id === id);
    expect(session?.title).toBe("插件会话");
    expect(session?.messages).toHaveLength(2);
    expect(session?.messages[0].id).toBe("m1");
    expect(session?.messages[1].id).not.toBe("m1");
    // createdAt 按序派生（时序保证）
    expect(session?.messages[1].createdAt).toBeGreaterThan(session?.messages[0].createdAt ?? 0);

    await chat.useChatPanelStore.getState().flush();
    expect(h.state.messageWrites).toHaveLength(1);
    // 元数据侧车随登记落盘（write_chat_session_meta 进 chatMetas 内存表）
    const metaRaw = [...h.state.chatMetas.values()][0];
    expect(metaRaw).toBeDefined();
    expect((JSON.parse(metaRaw!) as { title?: string }).title).toBe("插件会话");
  });

  it("importSession：角色/内容/附件校验失败抛错，不产生会话", async () => {
    await loadedInVault("v1");
    const store = chat.useChatPanelStore.getState();
    await expect(
      store.importSession([{ id: "a", role: "system" as never, content: "x" }]),
    ).rejects.toThrow("role");
    await expect(
      store.importSession([{ id: "a", role: "user", content: 1 as never }]),
    ).rejects.toThrow("content");
    await expect(
      store.importSession([
        { id: "a", role: "user", content: "x", attachments: [{ kind: "image", payload: "data:" } as never] },
      ]),
    ).rejects.toThrow("file");
    expect(chat.useChatPanelStore.getState().sessions).toEqual([]);
  });

  it("appendMessages：向既有会话追加并落盘；会话不存在抛错", async () => {
    await loadedInVault("v1");
    const { id } = await chat.useChatPanelStore.getState().importSession([
      { id: "m1", role: "user", content: "hi" },
    ]);
    await chat.useChatPanelStore.getState().appendMessages(id, [
      { id: "m2", role: "assistant", content: "答" },
    ]);
    const session = chat.useChatPanelStore.getState().sessions.find((s) => s.id === id);
    expect(session?.messages.map((m) => [m.role, m.content])).toEqual([
      ["user", "hi"],
      ["assistant", "答"],
    ]);
    await expect(
      chat.useChatPanelStore.getState().appendMessages("no-such", [{ id: "x", role: "user", content: "y" }]),
    ).rejects.toThrow("会话不存在");
  });
});

describe("跨窗口对账（reconcileExternalChatWrites）", () => {
  it("收到其他窗口的消息写盘广播：内存副本对齐磁盘，基线随磁盘重置", async () => {
    await loadedInVault("v1");
    const { id } = await chat.useChatPanelStore.getState().importSession([
      { id: "m1", role: "user", content: "hi" },
    ]);
    await chat.useChatPanelStore.getState().flush();
    // 模拟另一窗口写盘：磁盘上多出一条本窗口不知道的消息
    const file = chat.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.file;
    const line = JSON.stringify({
      id: "other-1",
      role: "assistant",
      content: "来自另一窗口",
      createdAt: Date.now() + 5000,
    });
    h.state.chatFiles.set(file, h.state.chatFiles.get(file) + "\n" + line);

    await chat.reconcileExternalChatWrites({
      origin: "other-window",
      messages: [id],
      metas: [],
      deleted: [],
    });
    const session = chat.useChatPanelStore.getState().sessions.find((s) => s.id === id);
    expect(session?.messages.map((m) => m.id)).toEqual(["m1", "other-1"]);

    // 对账后本窗口继续追加：以磁盘为基线走纯增长（只追加新行，不整文件重写其他窗口的消息）
    await chat.useChatPanelStore.getState().appendMessages(id, [
      { id: "m2", role: "user", content: "本地追加" },
    ]);
    await chat.useChatPanelStore.getState().flush();
    expect(h.state.appends.at(-1)).toMatchObject({ file, ids: ["m2"] });
    const lines = (h.state.chatFiles.get(file) ?? "").split("\n");
    expect(lines).toHaveLength(3);
    expect(h.state.chatFiles.get(file)).toContain("other-1");
  });

  it("本地有在途写的会话跳过对账；广播删除会移除内存副本并回落激活态", async () => {
    await loadedInVault("v1");
    const { id } = await chat.useChatPanelStore.getState().importSession([
      { id: "m1", role: "user", content: "hi" },
    ]);
    await chat.useChatPanelStore.getState().flush();
    const file = chat.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.file;

    // 本地在途写：append 后未 flush（脏集合非空）
    await chat.useChatPanelStore.getState().appendMessages(id, [
      { id: "m2", role: "assistant", content: "本地未落盘" },
    ]);
    h.state.chatFiles.set(file, JSON.stringify({ id: "m1", role: "user", content: "hi", createdAt: 1 }));
    await chat.reconcileExternalChatWrites({
      origin: "other-window",
      messages: [id],
      metas: [],
      deleted: [],
    });
    // 本地未落盘的 m2 仍在（在途写优先）
    expect(
      chat.useChatPanelStore.getState().sessions.find((s) => s.id === id)?.messages.map((m) => m.id),
    ).toEqual(["m1", "m2"]);

    // flush 后收到删除广播：内存副本移除、激活会话回落
    await chat.useChatPanelStore.getState().flush();
    chat.useChatPanelStore.setState({ activeSessionId: id });
    await chat.reconcileExternalChatWrites({
      origin: "other-window",
      messages: [],
      metas: [],
      deleted: [id],
    });
    expect(chat.useChatPanelStore.getState().sessions.some((s) => s.id === id)).toBe(false);
    expect(chat.useChatPanelStore.getState().activeSessionId).toBeNull();
  });
});
