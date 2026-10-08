/**
 * 会话容器跨窗口契约测试（宿主-镜像模型）：宿主 = 主窗口单写者（独占写盘链），镜像窗口 = 薄客户端
 * （boot 拉 seq 戳快照 + op 转发 + 增量折叠）。覆盖 AGENTS §2.4 时序矩阵：写盘在途 + 用户继续输入 +
 * 广播到达的收敛性；核心复现 = 镜像流式全量重写不得抹掉宿主已落盘的并发追加。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => {
  const state = {
    /** 内存会话文件系统：file → .jsonl 内容 / file → 侧车 JSON（宿主单写者的磁盘真源）。 */
    chatFiles: new Map<string, string>(),
    chatMetas: new Map<string, string>(),
    appends: [] as Array<{ file: string; ids: string[] }>,
    messageWrites: [] as unknown[],
    metaWrites: [] as unknown[],
    tempWrites: [] as Array<{ canvasId: string; fileName: string }>,
    attachmentDataUrl: "",
    /** 进程内事件总线：emit 全体投递（含发送者自身，同 Tauri emit），emitTo 定向投递。 */
    listeners: new Map<string, Set<{ label: string; handler: (payload: unknown) => void }>>(),
    /** 当前窗口 label（模块初始化时经 getCurrentWindow().label 决定宿主/镜像角色）。 */
    currentLabel: "main",
    gate: null as null | {
      armed: Promise<void>;
      resolveArmed: () => void;
      release: Promise<void>;
      resolveRelease: () => void;
    },
  };
  function deliver(target: string | null, event: string, payload: unknown): void {
    for (const l of [...(state.listeners.get(event) ?? [])]) {
      if (target === null || l.label === target) l.handler(payload);
    }
  }
  /** 挂起下一次写盘类 invoke（写盘在途模拟）：armed 在 invoke 到达时置位，release 放行提交。 */
  function gatedInvoke(commit: () => void): string {
    if (state.gate) {
      const gate = state.gate;
      gate.resolveArmed();
      return gate.release.then(() => {
        commit();
        return "";
      }) as unknown as string;
    }
    commit();
    return "";
  }
  return {
    state,
    deliver,
    gatedInvoke,
    /** 挂起下一次写盘类 invoke（追加与全量重写都走闸：写盘在途不分写法）。 */
    gateMessageWrite() {
      let resolveArmed!: () => void;
      const armed = new Promise<void>((r) => (resolveArmed = r));
      let resolveRelease!: () => void;
      const release = new Promise<void>((r) => (resolveRelease = r));
      state.gate = { armed, resolveArmed, release, resolveRelease };
    },
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
    if (cmd === "chat_container_snapshot") {
      // 传输探测命令：内存 mock 无 Rust 容器命令，抛错使探测回落宿主-镜像事件线（本文件测事件线契约）
      throw new Error("命令不存在");
    }
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
      return rows;
    }
    if (cmd === "read_chat_messages") {
      const content = h.state.chatFiles.get((args as { file: string }).file);
      if (content === undefined) throw new Error("会话消息不存在");
      return content;
    }
    if (cmd === "append_chat_messages") {
      const a = args as { file: string; records: Array<{ id: string }> };
      return h.gatedInvoke(() => {
        const existing = h.state.chatFiles.get(a.file) ?? "";
        const lines = a.records.map((r) => JSON.stringify(r));
        h.state.chatFiles.set(a.file, existing ? `${existing}\n${lines.join("\n")}` : lines.join("\n"));
        h.state.appends.push({ file: a.file, ids: a.records.map((r) => r.id) });
      });
    }
    if (cmd === "write_chat_messages") {
      const a = args as { file: string; content: string };
      return h.gatedInvoke(() => {
        h.state.messageWrites.push(args);
        h.state.chatFiles.set(a.file, a.content);
      });
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
    if (cmd === "read_attachment_data_url") return h.state.attachmentDataUrl;
    if (cmd === "write_temp_attachment") {
      h.state.tempWrites.push(args as { canvasId: string; fileName: string });
      return ".atelyx/temp/0123456789abcdef/att-1.txt";
    }
    return "";
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ label: h.state.currentLabel }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  emit: async (event: string, payload: unknown) => {
    h.deliver(null, event, payload);
  },
  emitTo: async (target: string, event: string, payload: unknown) => {
    h.deliver(target, event, payload);
  },
  listen: async (event: string, handler: (e: { payload: unknown }) => void) => {
    const entry = { label: h.state.currentLabel, handler: (p: unknown) => handler({ payload: p }) };
    if (!h.state.listeners.has(event)) h.state.listeners.set(event, new Set());
    h.state.listeners.get(event)!.add(entry);
    return () => {
      h.state.listeners.get(event)?.delete(entry);
    };
  },
}));

type ChatStore = typeof import("./chatPanelStore");
type AppStore = typeof import("./appStore");

let host: ChatStore;
let mirror: ChatStore;

/**
 * 以指定 label 装载一整套窗口模块图（每窗口 = 独立模块注册表 = 独立 store 实例）。
 * 取模顺序与 chatPanelStore.test.ts 一致（先起环上前置模块再进 chatPanelStore）。
 * `fresh`：先重置模块注册表（同测试内装载第二个窗口图时必须，否则拿到的是第一份缓存实例）。
 */
async function loadWindowGraph(label: string, fresh = false): Promise<{ chat: ChatStore; app: AppStore }> {
  if (fresh) vi.resetModules();
  h.state.currentLabel = label;
  await import("./noteSessionStore");
  await import("./settingsStore");
  await import("./noteStore");
  await import("./pluginStore");
  const app = (await import("./appStore")) as AppStore;
  const chat = (await import("./chatPanelStore")) as ChatStore;
  // wire 监听注册是异步微任务：等注册落定再进入用例
  await vi.advanceTimersByTimeAsync(0);
  return { chat, app };
}

/** 宿主激活仓库并完成容器加载。 */
async function bootHostInVault(root: string): Promise<void> {
  const { chat, app } = await loadWindowGraph("main");
  host = chat;
  app.useAppStore.setState({ vaultIdentity: { kind: "local", root }, vaultRoot: root });
  await host.useChatPanelStore.getState().load(true);
  expect(host.useChatPanelStore.getState().sessionVaultKey).toBe(`local:${root}`);
}

/** 镜像窗口装载 + 快照拉取（load = 向宿主要基线）；镜像窗口经上下文广播持有同一仓库身份。 */
async function bootMirror(root = "v1"): Promise<void> {
  const { chat, app } = await loadWindowGraph("panel-1", true);
  mirror = chat;
  app.useAppStore.setState({ vaultIdentity: { kind: "local", root }, vaultRoot: root });
  await mirror.useChatPanelStore.getState().load(true);
}

beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  // wire 只在 Tauri 运行时安装（hasTauriRuntime 判定 window.__TAURI_INTERNALS__）：
  // node 测试环境无 window，注入最小全局标记放行安装
  Object.assign(globalThis as Record<string, unknown>, {
    window: { __TAURI_INTERNALS__: {} },
  });
  h.state.chatFiles.clear();
  h.state.chatMetas.clear();
  h.state.appends = [];
  h.state.messageWrites = [];
  h.state.metaWrites = [];
  h.state.tempWrites = [];
  h.state.attachmentDataUrl = "";
  h.state.listeners.clear();
  h.state.currentLabel = "main";
  h.state.gate = null;
});

/** 解析 .jsonl 磁盘内容为消息 id 序列。 */
function diskMessageIds(file: string): string[] {
  return (h.state.chatFiles.get(file) ?? "")
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line) => (JSON.parse(line) as { id: string }).id);
}

describe("跨窗口并发写同一会话（§2.4 时序矩阵）", () => {
  it("核心复现：镜像基于陈旧副本的截断写盘不得绕过宿主真源（宿主已落盘消息不得被静默抹掉）", async () => {
    await bootHostInVault("v1");
    const { id } = await host.useChatPanelStore.getState().importSession([
      { id: "m1", role: "user", content: "hi" },
    ]);
    await host.useChatPanelStore.getState().flush();
    // 镜像先 boot（持 [m1] 陈旧副本），宿主随后追加并落盘（镜像不知情）
    await bootMirror();
    mirror.useChatPanelStore.getState().openSession(id);
    await host.useChatPanelStore.getState().appendMessages(id, [
      { id: "m2", role: "assistant", content: "宿主追加" },
    ]);
    await host.useChatPanelStore.getState().flush();
    await vi.advanceTimersByTimeAsync(200);

    // 宿主-镜像模型：增量先折叠，镜像在知情状态下操作（当前架构此处即失败：副本停留在 m1）
    expect(
      mirror.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.messages.map((m) => m.id),
    ).toContain("m2");

    // 镜像回滚到 m1（截断其后消息）：写意图必须经宿主真源应用，三端收敛
    mirror.useChatPanelStore.getState().rollbackTo("m1");
    await vi.advanceTimersByTimeAsync(600);

    const file = host.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.file;
    expect(diskMessageIds(file)).toEqual(["m1"]);
    expect(
      host.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.messages.map((m) => m.id),
    ).toEqual(["m1"]);
    expect(
      mirror.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.messages.map((m) => m.id),
    ).toEqual(["m1"]);
  });

  it("写盘在途 + 用户继续输入：op 到达不丢，宿主下一轮落盘收敛", async () => {
    await bootHostInVault("v1");
    const { id } = await host.useChatPanelStore.getState().importSession([
      { id: "m1", role: "user", content: "hi" },
    ]);
    const file = host.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.file;
    await host.useChatPanelStore.getState().flush();
    await bootMirror();

    // 宿主写盘在途（invoke 挂起）
    await host.useChatPanelStore.getState().appendMessages(id, [
      { id: "m2", role: "assistant", content: "宿主追加" },
    ]);
    h.gateMessageWrite();
    const flushing = host.useChatPanelStore.getState().flush();
    await h.waitGateArmed();

    // 在途期间镜像追加（op 转发）
    await mirror.useChatPanelStore.getState().appendMessages(id, [
      { id: "m3", role: "user", content: "镜像在途追加" },
    ]);
    h.releaseGate();
    await flushing;

    // 本轮写盘快照未含 m3：盘上只有已落盘的 m1/m2，宿主保留脏标记下一轮带上 m3（不丢）
    expect(diskMessageIds(file)).toEqual(["m1", "m2"]);
    await vi.advanceTimersByTimeAsync(600);

    expect(diskMessageIds(file)).toEqual(["m1", "m2", "m3"]);
    expect(
      host.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.messages.map((m) => m.id),
    ).toEqual(["m1", "m2", "m3"]);
    expect(
      mirror.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.messages.map((m) => m.id),
    ).toEqual(["m1", "m2", "m3"]);
  });

  it("镜像 boot 快照：读到宿主内存真源（含未落盘改动）", async () => {
    await bootHostInVault("v1");
    const { id } = await host.useChatPanelStore.getState().importSession([
      { id: "m1", role: "user", content: "未落盘消息" },
    ]);
    await bootMirror();

    const mirrored = mirror.useChatPanelStore.getState().sessions.find((s) => s.id === id);
    expect(mirrored?.messages.map((m) => m.id)).toEqual(["m1"]);
    expect(mirror.useChatPanelStore.getState().loaded).toBe(true);
  });

  it("镜像 send 经 op 转发：读己之写，双端立即可见", async () => {
    await bootHostInVault("v1");
    const hostRuntimeHost = await import("@/utils/chatRuntimeHost");
    const off = hostRuntimeHost.registerChatRuntime({
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
      await bootMirror();
      const ok = await mirror.useChatPanelStore.getState().send("镜像发送");
      expect(ok).toBe(true);
      // 助手终稿在轮转收尾时入容器，经增量广播到镜像（op 响应只含 send 落定的前缀）
      await vi.advanceTimersByTimeAsync(200);

      // 读己之写：镜像与宿主立即看到同一会话与消息
      const mirrorActive = mirror.useChatPanelStore.getState().activeSessionId;
      expect(mirrorActive).not.toBeNull();
      expect(
        mirror.useChatPanelStore.getState().sessions.find((s) => s.id === mirrorActive)!.messages.map((m) => [m.role, m.content]),
      ).toEqual([
        ["user", "镜像发送"],
        ["assistant", "答"],
      ]);
      expect(
        host.useChatPanelStore.getState().sessions.find((s) => s.id === mirrorActive)!.messages.map((m) => [m.role, m.content]),
      ).toEqual([
        ["user", "镜像发送"],
        ["assistant", "答"],
      ]);
      // 镜像新建会话不改变宿主的激活会话（激活态每窗口本地）
      expect(host.useChatPanelStore.getState().activeSessionId).toBeNull();
    } finally {
      off();
    }
  });

  it("过期增量（seq ≤ 已应用）被忽略，新增量正常折叠", async () => {
    await bootHostInVault("v1");
    const { id } = await host.useChatPanelStore.getState().importSession([
      { id: "m1", role: "user", content: "hi" },
    ]);
    await bootMirror();
    const before = mirror.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.messages.length;

    // 伪造过期增量（seq 0 ≤ 快照基线）与新增量（seq 足够大）
    h.deliver(null, "chat-container-delta", {
      seq: 0,
      opOwners: [],
      metas: [],
      messages: [{ sessionId: id, upserts: [{ id: "stale", role: "user", content: "过期", createdAt: 1 }] }],
      status: null,
      messageSessionIds: [id],
      metaSessionIds: [],
      deletedIds: [],
    });
    h.deliver(null, "chat-container-delta", {
      seq: 999,
      opOwners: [],
      metas: [],
      messages: [{ sessionId: id, upserts: [{ id: "fresh", role: "user", content: "新增", createdAt: 2 }] }],
      status: null,
      messageSessionIds: [id],
      metaSessionIds: [],
      deletedIds: [],
    });
    const ids = mirror.useChatPanelStore.getState().sessions.find((s) => s.id === id)!.messages.map((m) => m.id);
    expect(ids).not.toContain("stale");
    expect(ids).toContain("fresh");
    expect(ids).toHaveLength(before + 1);
  });

  it("宿主切换仓库：镜像经增量重同步（旧会话移除、激活态回落）", async () => {
    const { app } = await loadWindowGraph("main");
    host = (await import("./chatPanelStore")) as ChatStore;
    app.useAppStore.setState({ vaultIdentity: { kind: "local", root: "v1" }, vaultRoot: "v1" });
    await host.useChatPanelStore.getState().load(true);
    await host.useChatPanelStore.getState().importSession([{ id: "m1", role: "user", content: "hi" }]);
    await bootMirror();
    expect(mirror.useChatPanelStore.getState().sessions).toHaveLength(1);
    // 镜像激活该会话：切换后应随会话消失回落新对话态（激活态每窗口本地，靠折叠收敛）
    const v1SessionId = mirror.useChatPanelStore.getState().sessions[0].id;
    mirror.useChatPanelStore.getState().openSession(v1SessionId);
    expect(mirror.useChatPanelStore.getState().activeSessionId).toBe(v1SessionId);

    // 宿主切换仓库（force 重载 → 容器清空重建）；mock FS 不分仓库，先清空模拟新仓库无会话
    h.state.chatFiles.clear();
    h.state.chatMetas.clear();
    app.useAppStore.setState({ vaultIdentity: { kind: "local", root: "v2" }, vaultRoot: "v2" });
    await host.useChatPanelStore.getState().load(true);
    await vi.advanceTimersByTimeAsync(200);

    expect(mirror.useChatPanelStore.getState().sessions).toEqual([]);
    expect(mirror.useChatPanelStore.getState().activeSessionId).toBeNull();
  });

  it("宿主写入 → 镜像折叠后向本窗口插件转发 chat:sessions-changed", async () => {
    await bootHostInVault("v1");
    await bootMirror();
    const cordisEvents = await import("@/services/cordis/events");
    const received: Array<{ name: string; payload: unknown }> = [];
    cordisEvents.setKernelRef({
      ctx: { events: { emit: (name: string, payload: unknown) => received.push({ name, payload }) } },
    } as never);
    try {
      const { id } = await host.useChatPanelStore.getState().importSession([
        { id: "m1", role: "user", content: "hi" },
      ]);
      await vi.advanceTimersByTimeAsync(200);
      expect(received).toContainEqual({
        name: "chat:sessions-changed",
        payload: { messages: [id], metas: [id], deleted: [] },
      });
    } finally {
      cordisEvents.setKernelRef(null);
    }
  });
});
