/**
 * 会话容器 Rust 真源模式契约测试：内存假 Rust（探测/装载/apply 意图转发/commit 提交/结果回填）
 * 驱动薄客户端与主窗口执行体。覆盖：探测接入、纯持久 op 直应用、执行 op 意图转发与变更提交、
 * 执行体回声跳过、外部折叠防回环、flush 请求真源写盘、多窗口经真源广播收敛。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => {
  type WireMessage = Record<string, unknown> & { id: string };
  type WireSession = {
    id: string;
    file: string;
    title?: string;
    agentId?: string;
    compaction?: unknown;
    createdAt: number;
    updatedAt: number;
    messages: WireMessage[];
  };
  type Frag = {
    metas: Array<Record<string, unknown>>;
    messages: Array<{ sessionId: string; upserts: WireMessage[]; keepCount?: number }>;
    status: Record<string, unknown> | null;
    messageSessionIds: string[];
    metaSessionIds: string[];
    deletedIds: string[];
  };
  type Acc = {
    touched: string[];
    created: string[];
    removed: string[];
    appended: Array<[string, WireMessage[]]>;
    patched: Array<[string, WireMessage]>;
    truncated: Array<[string, number]>;
    status: Record<string, unknown> | null;
  };
  type IntentResult =
    | { status: "ok"; value?: unknown; createdSessionId?: string }
    | { status: "error"; error: string };

  /** 进程内事件总线：emit 全体投递（含发送者自身），emitTo 定向投递。 */
  const listeners = new Map<string, Set<{ label: string; handler: (payload: unknown) => void }>>();
  /** 当前窗口 label（模块初始化时决定主窗口/撕裂窗口角色）。 */
  let currentLabel = "main";

  function deliver(target: string | null, event: string, payload: unknown): void {
    for (const l of [...(listeners.get(event) ?? [])]) {
      if (target === null || l.label === target) l.handler(payload);
    }
  }

  // ===== 内存假 Rust：与 chat_container.rs 的 apply/commit/publish 语义逐项对齐 =====

  const EXECUTION_KINDS = new Set(["send", "regenerate", "stop", "compact", "rename", "import", "append"]);

  const rust = {
    seq: 0,
    sessions: [] as WireSession[],
    streaming: false,
    compacting: null as string | null,
    vaultKey: "local:v1",
    intentWindows: [] as Array<{ intentId: string; requestId: string; fragments: Frag[] }>,
    pendingIntents: new Map<string, (r: IntentResult) => void>(),
    patchRevs: new Map<string, number>(),
    appliedOps: [] as Array<Record<string, unknown>>,
    commits: [] as Array<{ requestId: string; expectedRoot: string; batch: Record<string, unknown> }>,
    intentResults: [] as Array<{ intentId: string; result: IntentResult }>,
    bootCount: 0,
    flushCount: 0,
    probes: 0,
    loads: 0,
    deltas: [] as Array<Record<string, unknown>>,
    /** 提交命令注入失败（验证前端退避重试；`false` = 正常应用）。 */
    failCommit: false,
  };
  let intentSeq = 0;
  let createValue: unknown = null;

  function resetRust(): void {
    rust.seq = 0;
    rust.sessions = [];
    rust.streaming = false;
    rust.compacting = null;
    rust.vaultKey = "local:v1";
    rust.intentWindows = [];
    rust.pendingIntents.clear();
    rust.patchRevs.clear();
    rust.appliedOps = [];
    rust.commits = [];
    rust.intentResults = [];
    rust.bootCount = 0;
    rust.flushCount = 0;
    rust.probes = 0;
    rust.loads = 0;
    rust.deltas = [];
    rust.failCommit = false;
    intentSeq = 0;
    createValue = null;
  }

  const findSession = (id: string): WireSession | undefined => rust.sessions.find((s) => s.id === id);

  /** 元数据完整片段（title/agentId/compaction 恒在场：null = 清除）。 */
  function metaFull(s: WireSession): Record<string, unknown> {
    return {
      id: s.id,
      file: s.file,
      title: s.title ?? null,
      agentId: s.agentId ?? null,
      compaction: s.compaction ?? null,
      createdAt: s.createdAt,
      updatedAt: s.updatedAt,
    };
  }

  const newAcc = (): Acc => ({
    touched: [],
    created: [],
    removed: [],
    appended: [],
    patched: [],
    truncated: [],
    status: null,
  });

  /** 剥离消息附件运行时缓存（与 Rust strip_message 对齐：payload 只活在各窗口本地）。 */
  function stripMessage(m: WireMessage): WireMessage {
    const atts = m.attachments;
    if (!Array.isArray(atts)) return { ...m };
    return {
      ...m,
      attachments: atts.map((a) => {
        if (a && typeof a === "object" && "payload" in a) {
          const { payload: _payload, ...rest } = a as Record<string, unknown>;
          return rest;
        }
        return a;
      }),
    };
  }

  /** 片段产出（DiffAccumulator::finish）：触及会话全量 meta + 各消息增量分组（upserts 剥 payload）。 */
  function finishAcc(acc: Acc): Frag {
    const metas: Array<Record<string, unknown>> = [];
    const messages: Frag["messages"] = [];
    const messageSessionIds: string[] = [];
    const metaSessionIds: string[] = [];
    for (const id of acc.touched) {
      const s = findSession(id);
      if (!s) continue;
      metas.push(metaFull(s));
      metaSessionIds.push(id);
    }
    for (const id of acc.created) {
      const s = findSession(id);
      if (!s) continue;
      messages.push({ sessionId: id, upserts: s.messages.map(stripMessage) });
      messageSessionIds.push(id);
    }
    for (const [id, upserts] of acc.appended) {
      messages.push({ sessionId: id, upserts: upserts.map(stripMessage) });
      messageSessionIds.push(id);
    }
    for (const [id, m] of acc.patched) {
      messages.push({ sessionId: id, upserts: [stripMessage(m)] });
      messageSessionIds.push(id);
    }
    for (const [id, keep] of acc.truncated) {
      messages.push({ sessionId: id, upserts: [], keepCount: keep });
      messageSessionIds.push(id);
    }
    const deletedIds: string[] = [];
    for (const id of acc.removed) {
      metas.push({ id, removed: true });
      deletedIds.push(id);
    }
    return { metas, messages, status: acc.status, messageSessionIds, metaSessionIds, deletedIds };
  }

  function fragEmpty(f: Frag): boolean {
    return f.metas.length === 0 && f.messages.length === 0 && f.status === null;
  }

  /** publish：seq 推进 + 片段累积进在途意图窗口（opOwners 并入其 requestId）+ 全窗口广播。 */
  function publish(acc: Acc, requestId: string | null): void {
    const frag = finishAcc(acc);
    if (fragEmpty(frag)) return;
    const owners: string[] = requestId ? [requestId] : [];
    for (const w of rust.intentWindows) {
      if (!owners.includes(w.requestId)) owners.push(w.requestId);
      w.fragments.push(frag);
    }
    rust.seq += 1;
    const delta = { seq: rust.seq, opOwners: owners, ...frag };
    rust.deltas.push(delta);
    deliver(null, "chat-container-delta", delta);
  }

  /** 合并片段列表（意图窗口累积 → 响应载荷；与 Rust merge_fragment_list 对齐：数组拼接、
   *  id 集合去重、status 后到字段覆盖）。 */
  function mergeFragmentList(list: Frag[]): Frag {
    const out: Frag = {
      metas: [],
      messages: [],
      status: null,
      messageSessionIds: [],
      metaSessionIds: [],
      deletedIds: [],
    };
    for (const f of list) {
      out.metas.push(...f.metas);
      out.messages.push(...f.messages);
      if (f.status) out.status = { ...(out.status ?? {}), ...f.status };
      for (const id of f.messageSessionIds) {
        if (!out.messageSessionIds.includes(id)) out.messageSessionIds.push(id);
      }
      for (const id of f.metaSessionIds) {
        if (!out.metaSessionIds.includes(id)) out.metaSessionIds.push(id);
      }
      for (const id of f.deletedIds) {
        if (!out.deletedIds.includes(id)) out.deletedIds.push(id);
      }
    }
    return out;
  }

  /** 纯持久 op 直应用（rollback 守卫与前端一致：流式中/位置非法静默不动）。 */
  function applyPure(op: Record<string, unknown>): Acc {
    const acc = newAcc();
    switch (op.kind) {
      case "create": {
        const id = `created-${rust.seq}-${rust.sessions.length}`;
        const s: WireSession = {
          id,
          file: `.atelyx/对话历史/${id}.jsonl`,
          createdAt: 1000,
          updatedAt: 1000,
          messages: [],
        };
        rust.sessions.push(s);
        acc.created.push(id);
        acc.touched.push(id);
        createValue = { id };
        break;
      }
      case "setTitle": {
        const s = findSession(op.sessionId as string);
        // 与 Rust apply_pure 对齐：SetTitle 缺会话报错（其余元数据 op 缺失静默）
        if (!s) throw new Error(`会话不存在：${String(op.sessionId)}`);
        s.title = op.title as string;
        acc.touched.push(s.id);
        break;
      }
      case "setAgentId": {
        const s = findSession(op.sessionId as string);
        if (s) {
          s.agentId = op.agentId as string | undefined;
          acc.touched.push(s.id);
        }
        break;
      }
      case "rollback": {
        if (rust.streaming || rust.compacting !== null) break;
        const s = findSession(op.sessionId as string);
        const idx = s?.messages.findIndex((m) => m.id === op.messageId) ?? -1;
        if (!s || idx === -1 || idx === s.messages.length - 1) break;
        const keep = idx + 1;
        s.messages = s.messages.slice(0, keep);
        s.updatedAt = 1000;
        acc.truncated.push([s.id, keep]);
        acc.touched.push(s.id);
        break;
      }
      case "delete":
      case "deleteExternal": {
        const pos = rust.sessions.findIndex((s) => s.id === op.sessionId);
        if (pos >= 0) {
          const [s] = rust.sessions.splice(pos, 1);
          acc.removed.push(s.id);
        }
        break;
      }
      case "setModelOverride":
      case "setEffortOverride":
        break;
      default:
        throw new Error(`op 须由执行体处理：${String(op.kind)}`);
    }
    return acc;
  }

  /** 容器 op 统一入口：纯持久 op 直应用；执行 op 挂意图窗口并定向投递执行体。 */
  async function applyCmd(args: { requestId: string; op: Record<string, unknown> }): Promise<Record<string, unknown>> {
    const op = args.op;
    rust.appliedOps.push(op);
    createValue = null;
    if (!EXECUTION_KINDS.has(op.kind as string)) {
      const acc = applyPure(op);
      publish(acc, args.requestId);
      const createdSessionId = createValue !== null ? (createValue as { id: string }).id : undefined;
      return {
        value: createValue ?? null,
        ...(createdSessionId !== undefined ? { createdSessionId } : {}),
        fragments: finishAcc(acc),
      };
    }
    // 执行 op：意图窗口 + 定向投递主窗口执行体，intent_result 回填后以窗口累积片段响应
    const intentId = `intent-${++intentSeq}`;
    rust.intentWindows.push({ intentId, requestId: args.requestId, fragments: [] });
    deliver("main", "chat-container-intent", { intentId, requestId: args.requestId, op });
    const result = await new Promise<IntentResult>((resolve) => rust.pendingIntents.set(intentId, resolve));
    const idx = rust.intentWindows.findIndex((w) => w.intentId === intentId);
    const intentWindow = idx >= 0 ? rust.intentWindows.splice(idx, 1)[0] : null;
    if (result.status === "error") throw new Error(result.error);
    return {
      ...(result.value !== undefined ? { value: result.value } : {}),
      ...(result.createdSessionId !== undefined ? { createdSessionId: result.createdSessionId } : {}),
      fragments: mergeFragmentList(intentWindow?.fragments ?? []),
    };
  }

  /** 执行体提交：批次按契约顺序应用 + 差分广播。 */
  function commitCmd(args: { requestId: string; expectedRoot: string; batch: Record<string, unknown> }): Record<string, unknown> {
    rust.commits.push(args);
    if (rust.failCommit) throw new Error("提交通道暂时不可用");
    const acc = newAcc();
    const b = args.batch as {
      created: Array<WireSession>;
      metas: Array<{ id: string; title?: string; agentId?: string; compaction?: unknown; updatedAt?: number }>;
      appends: Array<{ sessionId: string; messages: WireMessage[] }>;
      patches: Array<{ sessionId: string; messageId: string; rev: number; content?: string; steps?: unknown }>;
      truncations: Array<{ sessionId: string; keepCount: number }>;
      status: { streaming?: boolean; compacting?: string | null };
    };
    for (const def of b.created) {
      if (findSession(def.id)) throw new Error(`会话已存在：${def.id}`);
      const s: WireSession = { ...def, messages: def.messages.map((m) => ({ ...m })) };
      rust.sessions.push(s);
      acc.created.push(s.id);
      acc.touched.push(s.id);
    }
    for (const patch of b.metas) {
      const s = findSession(patch.id);
      if (!s) continue;
      if (patch.title !== undefined) s.title = patch.title;
      if (patch.agentId !== undefined) s.agentId = patch.agentId;
      if (patch.compaction !== undefined) s.compaction = patch.compaction;
      if (patch.updatedAt !== undefined) s.updatedAt = patch.updatedAt;
      acc.touched.push(s.id);
    }
    for (const ap of b.appends) {
      const s = findSession(ap.sessionId);
      if (!s) continue;
      s.messages.push(...ap.messages.map((m) => ({ ...m })));
      acc.appended.push([s.id, ap.messages]);
      acc.touched.push(s.id);
    }
    for (const p of b.patches) {
      const key = `${p.sessionId}\u0000${p.messageId}`;
      const appliedRev = rust.patchRevs.get(key) ?? 0;
      if (p.rev < appliedRev) continue;
      const s = findSession(p.sessionId);
      const m = s?.messages.find((x) => x.id === p.messageId);
      if (!s || !m) continue;
      if (p.content !== undefined) m.content = p.content;
      if (p.steps !== undefined) m.steps = p.steps;
      rust.patchRevs.set(key, p.rev);
      acc.patched.push([s.id, m]);
      acc.touched.push(s.id);
    }
    for (const t of b.truncations) {
      const s = findSession(t.sessionId);
      if (!s || t.keepCount >= s.messages.length) continue;
      s.messages = s.messages.slice(0, t.keepCount);
      s.updatedAt = 1000;
      acc.truncated.push([s.id, t.keepCount]);
      acc.touched.push(s.id);
    }
    const status: Record<string, unknown> = {};
    if (b.status?.streaming !== undefined) {
      status.streaming = b.status.streaming;
      rust.streaming = b.status.streaming;
    }
    if (b.status?.compacting !== undefined) {
      status.compacting = b.status.compacting;
      rust.compacting = b.status.compacting;
    }
    if (Object.keys(status).length > 0) acc.status = status;
    publish(acc, args.requestId);
    return { fragments: finishAcc(acc) };
  }

  function snapshotPayload(): Record<string, unknown> {
    return {
      seq: rust.seq,
      sessionVaultKey: rust.vaultKey,
      // 快照出线剥 payload（与 Rust session_to_wire 对齐）
      sessions: rust.sessions.map((s) => ({ ...s, messages: s.messages.map(stripMessage) })),
      streaming: rust.streaming,
      compacting: rust.compacting,
      persistError: null,
      modelOverride: null,
      effortOverride: null,
    };
  }

  return {
    rust,
    resetRust,
    listeners,
    deliver,
    getLabel: () => currentLabel,
    setLabel: (label: string) => {
      currentLabel = label;
    },
    applyCmd,
    commitCmd,
    snapshotPayload,
    /** 预置一个真源会话（boot 前调用）。 */
    seedSession(id: string, messages: WireMessage[]): void {
      rust.sessions.push({
        id,
        file: `.atelyx/对话历史/${id}.jsonl`,
        createdAt: 1000,
        updatedAt: 1000,
        messages: messages.map((m) => ({ ...m })),
      });
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
    switch (cmd) {
      case "chat_container_snapshot":
        h.rust.probes += 1;
        return h.snapshotPayload();
      case "chat_container_load":
        h.rust.loads += 1;
        return h.snapshotPayload();
      case "chat_container_apply":
        return h.applyCmd(args as { requestId: string; op: Record<string, unknown> });
      case "chat_container_commit":
        return h.commitCmd(args as { requestId: string; expectedRoot: string; batch: Record<string, unknown> });
      case "chat_container_intent_result": {
        const a = args as { intentId: string; result: { status: "ok"; value?: unknown; createdSessionId?: string } | { status: "error"; error: string } };
        h.rust.intentResults.push({ intentId: a.intentId, result: a.result });
        const resolve = h.rust.pendingIntents.get(a.intentId);
        h.rust.pendingIntents.delete(a.intentId);
        resolve?.(a.result as never);
        return "";
      }
      case "chat_container_executor_boot":
        h.rust.bootCount += 1;
        return "";
      case "chat_container_flush":
        h.rust.flushCount += 1;
        return "";
      // 事件线腿（空间身份退离）的宿主读盘路径：空仓库语义
      case "list_chat_sessions":
        return [];
      case "read_chat_messages":
        return "";
      case "read_editor_chats_meta":
        return {};
      case "write_editor_chats_meta":
      case "append_chat_messages":
      case "write_chat_messages":
      case "delete_chat_messages":
      case "write_chat_session_meta":
      case "read_attachment_data_url":
      case "write_temp_attachment":
        return "";
      default:
        return "";
    }
  },
}));

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ label: h.getLabel() }),
}));

vi.mock("@tauri-apps/api/event", () => ({
  emit: async (event: string, payload: unknown) => {
    h.deliver(null, event, payload);
  },
  emitTo: async (target: string, event: string, payload: unknown) => {
    h.deliver(target, event, payload);
  },
  listen: async (event: string, handler: (e: { payload: unknown }) => void) => {
    const entry = { label: h.getLabel(), handler: (p: unknown) => handler({ payload: p }) };
    if (!h.listeners.has(event)) h.listeners.set(event, new Set());
    h.listeners.get(event)!.add(entry);
    return () => {
      h.listeners.get(event)?.delete(entry);
    };
  },
}));

type ChatStore = typeof import("./chatPanelStore");
type AppStore = typeof import("./appStore");

let main: ChatStore;

async function loadWindowGraph(label: string, fresh = false): Promise<{ chat: ChatStore; app: AppStore }> {
  if (fresh) vi.resetModules();
  h.setLabel(label);
  await import("./noteSessionStore");
  await import("./settingsStore");
  await import("./noteStore");
  await import("./pluginStore");
  const app = (await import("./appStore")) as AppStore;
  const chat = (await import("./chatPanelStore")) as ChatStore;
  await vi.advanceTimersByTimeAsync(0);
  return { chat, app };
}

/** 主窗口（执行体）激活仓库并完成容器装载。 */
async function bootMain(root = "v1"): Promise<AppStore> {
  const { chat, app } = await loadWindowGraph("main");
  main = chat;
  app.useAppStore.setState({ vaultIdentity: { kind: "local", root }, vaultRoot: root });
  await main.useChatPanelStore.getState().load(true);
  expect(main.useChatPanelStore.getState().sessionVaultKey).toBe(`local:${root}`);
  return app;
}

/** 注册一次性应答的对话运行时（send 意图执行依赖；返回注销函数）。 */
async function withRuntime<T>(run: () => Promise<T>): Promise<T> {
  const runtimeHost = await import("@/utils/chatRuntimeHost");
  const off = runtimeHost.registerChatRuntime({
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
    return await run();
  } finally {
    off();
  }
}

beforeEach(() => {
  vi.resetModules();
  vi.useFakeTimers();
  Object.assign(globalThis as Record<string, unknown>, {
    window: { __TAURI_INTERNALS__: {} },
  });
  h.resetRust();
  h.listeners.clear();
});

function sessionMessages(store: ChatStore, id: string): Array<{ id: string; content: string }> {
  return (store.useChatPanelStore.getState().sessions.find((s) => s.id === id)?.messages ?? []).map((m) => ({
    id: m.id,
    content: m.content,
  }));
}

describe("Rust 真源模式：薄客户端与执行体契约", () => {
  it("探测接入：快照来自真源（会话/仓库身份），执行体登记一次", async () => {
    h.seedSession("s1", [{ id: "m1", role: "user", content: "hi", createdAt: 1 }]);
    const wire = await import("@/services/chatContainerWire");
    await bootMain();
    expect(wire.isRustTruthMode()).toBe(true);
    const st = main.useChatPanelStore.getState();
    expect(st.loaded).toBe(true);
    expect(st.sessions.map((s) => s.id)).toEqual(["s1"]);
    expect(sessionMessages(main, "s1").map((m) => m.id)).toEqual(["m1"]);
    expect(h.rust.loads).toBe(1);
    expect(h.rust.probes).toBe(1);
    expect(h.rust.bootCount).toBe(1);
  });

  it("纯持久 op 直应用（rollback）：真源截断经响应片段折叠，不产生变更提交", async () => {
    h.seedSession("s1", [
      { id: "m1", role: "user", content: "q", createdAt: 1 },
      { id: "m2", role: "assistant", content: "a", createdAt: 2 },
    ]);
    await bootMain();
    main.useChatPanelStore.getState().openSession("s1");
    main.useChatPanelStore.getState().rollbackTo("m1");
    await vi.advanceTimersByTimeAsync(600);

    expect(sessionMessages(main, "s1").map((m) => m.id)).toEqual(["m1"]);
    expect(h.rust.sessions.find((s) => s.id === "s1")!.messages.map((m) => m.id)).toEqual(["m1"]);
    expect(h.rust.appliedOps).toContainEqual({ kind: "rollback", sessionId: "s1", messageId: "m1" });
    // 快照装载与外部折叠不回提交：执行体差分基准随外部变更推进
    expect(h.rust.commits).toHaveLength(0);
  });

  it("执行 op 意图转发（send 新会话）：执行体执行 + 编排变更提交 + 意图结果回填", async () => {
    await bootMain();
    await withRuntime(async () => {
      const ok = await main.useChatPanelStore.getState().send("问");
      expect(ok).toBe(true);
      // 执行体乐观应用：意图结果回填后激活态同步（读己之写）
      const sid = main.useChatPanelStore.getState().activeSessionId;
      expect(sid).not.toBeNull();
      expect(h.rust.intentResults).toHaveLength(1);
      expect(h.rust.intentResults[0].result).toMatchObject({ status: "ok", value: { ok: true } });

      // 编排变更（建会话 + 流式补丁）经节流提交进真源
      await vi.advanceTimersByTimeAsync(600);
      expect(h.rust.commits.length).toBeGreaterThanOrEqual(1);
      const truth = h.rust.sessions.find((s) => s.id === sid)!;
      expect(truth.messages.map((m) => [m.role, m.content])).toEqual([
        ["user", "问"],
        ["assistant", "答"],
      ]);
      expect(sessionMessages(main, sid!).map((m) => m.content)).toEqual(["问", "答"]);
    });
  });

  it("提交失败按有界退避重试：逐档拉长，达上限后放弃（不无限重试）", async () => {
    h.rust.failCommit = true;
    await bootMain();
    await withRuntime(async () => {
      await main.useChatPanelStore.getState().send("问");
      // 节流窗口后首次提交
      await vi.advanceTimersByTimeAsync(100);
      expect(h.rust.commits.length).toBe(1);
      // 第 1 档 500ms → 第 2 次尝试
      await vi.advanceTimersByTimeAsync(500);
      expect(h.rust.commits.length).toBe(2);
      // 再 500ms 不产生第 3 次（间隔已按退避拉长到 1s；固定间隔重试会在此失败）
      await vi.advanceTimersByTimeAsync(500);
      expect(h.rust.commits.length).toBe(2);
      await vi.advanceTimersByTimeAsync(500);
      expect(h.rust.commits.length).toBe(3);
      // 走完余下档位：共 1 次首发 + 5 档退避 = 6 次尝试后放弃
      await vi.advanceTimersByTimeAsync(60_000);
      expect(h.rust.commits.length).toBe(6);
      // 链条已断：继续推进时间不再有新的尝试
      await vi.advanceTimersByTimeAsync(60_000);
      expect(h.rust.commits.length).toBe(6);
    });
  });

  it("执行体回声跳过：自身提交 requestId 的广播不回退本地内容", async () => {
    await bootMain();
    await withRuntime(async () => {
      await main.useChatPanelStore.getState().send("问");
      await vi.advanceTimersByTimeAsync(600);
      const sid = main.useChatPanelStore.getState().activeSessionId!;
      const aid = sessionMessages(main, sid)[1].id;
      expect(h.rust.commits.length).toBeGreaterThanOrEqual(1);
      const ownCommitId = h.rust.commits[0].requestId;

      // 伪造携带自身提交 id 的广播（seq 前进至有效），内容为过期回声
      h.rust.seq += 1;
      h.deliver(null, "chat-container-delta", {
        seq: h.rust.seq,
        opOwners: [ownCommitId],
        metas: [],
        messages: [{ sessionId: sid, upserts: [{ id: aid, role: "assistant", content: "过期回声", createdAt: 2 }] }],
        status: null,
        messageSessionIds: [sid],
        metaSessionIds: [],
        deletedIds: [],
      });
      expect(sessionMessages(main, sid)[1].content).toBe("答");
    });
  });

  it("外部广播折叠防回环：折叠期间差分基准推进，不产生变更提交", async () => {
    h.seedSession("s1", [{ id: "m1", role: "user", content: "hi", createdAt: 1 }]);
    await bootMain();

    // 模拟另一窗口经真源产生的变更广播（本窗口无从知晓的外部追加）
    h.rust.seq += 1;
    h.deliver(null, "chat-container-delta", {
      seq: h.rust.seq,
      opOwners: [],
      metas: [],
      messages: [{ sessionId: "s1", upserts: [{ id: "m2", role: "assistant", content: "外部", createdAt: 2 }] }],
      status: null,
      messageSessionIds: ["s1"],
      metaSessionIds: [],
      deletedIds: [],
    });
    await vi.advanceTimersByTimeAsync(600);

    expect(sessionMessages(main, "s1").map((m) => m.id)).toEqual(["m1", "m2"]);
    expect(h.rust.commits).toHaveLength(0);
  });

  it("flush 请求真源写盘", async () => {
    await bootMain();
    await main.useChatPanelStore.getState().flush();
    expect(h.rust.flushCount).toBe(1);
  });

  it("多窗口薄客户端：撕裂窗口拉同一真源快照，写意图直连真源且不登记执行体", async () => {
    h.seedSession("s1", [
      { id: "m1", role: "user", content: "q", createdAt: 1 },
      { id: "m2", role: "assistant", content: "a", createdAt: 2 },
    ]);
    await bootMain();
    const bootsAfterMain = h.rust.bootCount;

    // 撕裂窗口独立模块图：探测接入为薄客户端（不登记执行体）
    const { chat: mirror } = await loadWindowGraph("panel-1", true);
    const app2 = (await import("./appStore")) as AppStore;
    app2.useAppStore.setState({ vaultIdentity: { kind: "local", root: "v1" }, vaultRoot: "v1" });
    await mirror.useChatPanelStore.getState().load(true);
    expect(mirror.useChatPanelStore.getState().sessions.map((s) => s.id)).toEqual(["s1"]);
    expect(h.rust.bootCount).toBe(bootsAfterMain);

    // 撕裂窗口 rollback 直连真源（不经主窗口事件线）：双端经广播收敛
    mirror.useChatPanelStore.getState().openSession("s1");
    mirror.useChatPanelStore.getState().rollbackTo("m1");
    await vi.advanceTimersByTimeAsync(600);
    expect(sessionMessages(mirror, "s1").map((m) => m.id)).toEqual(["m1"]);
    expect(h.rust.sessions.find((s) => s.id === "s1")!.messages.map((m) => m.id)).toEqual(["m1"]);
    expect(sessionMessages(main, "s1").map((m) => m.id)).toEqual(["m1"]);
  });

  it("模式随仓库身份重评估：空间身份退离事件线（真源不被污染），回到本地恢复真源", async () => {
    h.seedSession("s1", [{ id: "m1", role: "user", content: "hi", createdAt: 1 }]);
    const wire = await import("@/services/chatContainerWire");
    const app = await bootMain("v1");
    expect(wire.isRustTruthMode()).toBe(true);

    // 切到空间身份：资格不成立 → 事件线（主窗口 = 宿主，容器走内容 I/O；真源不被触碰）
    app.useAppStore.setState({
      vaultIdentity: { kind: "space", serverUrl: "http://s", spaceId: "sp1" },
      vaultRoot: null,
    });
    await main.useChatPanelStore.getState().load(true);
    await vi.advanceTimersByTimeAsync(600);
    expect(wire.isRustTruthMode()).toBe(false);
    expect(h.rust.commits).toHaveLength(0);
    expect(h.rust.appliedOps).toHaveLength(0);
    expect(h.rust.sessions).toHaveLength(1); // 空间期间真源仍持 v1 会话（未被宿主写盘污染）

    // 切回本地仓库：资格成立 → 真源模式恢复，快照仍为 v1 会话
    app.useAppStore.setState({ vaultIdentity: { kind: "local", root: "v1" }, vaultRoot: "v1" });
    await main.useChatPanelStore.getState().load(true);
    expect(wire.isRustTruthMode()).toBe(true);
    expect(main.useChatPanelStore.getState().sessions.map((s) => s.id)).toEqual(["s1"]);
    expect(sessionMessages(main, "s1").map((m) => m.id)).toEqual(["m1"]);
  });
});
