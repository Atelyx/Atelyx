/**
 * 仓库切换门契约测试（两阶段切换协议）：主窗口切换前广播 prepare，撕裂窗口遮罩禁写 →
 * flush 全部领域挂起写入 → ack 上行；主窗口收齐全部存活窗口的 ack 才放行（无限等待，
 * 等待期间窗口消失 = 从等待集剔除），任一 ack 失败即广播 abort 并返回失败清单。
 * 用内存事件线驱动（emit 全体投递 / emitTo 定向，同 Tauri 语义），按窗口 label 模拟多窗口。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => {
  const state = {
    /** 内存事件线：event → 按窗口 label 注册的 handler 集。 */
    listeners: new Map<string, Set<{ label: string; handler: (payload: unknown) => void }>>(),
    /** 当前窗口 label（模拟主窗口/撕裂窗口角色）。 */
    currentLabel: "main",
  };
  function deliver(target: string | null, event: string, payload: unknown): void {
    for (const l of [...(state.listeners.get(event) ?? [])]) {
      if (target === null || l.label === target) l.handler(payload);
    }
  }
  return {
    state,
    deliver,
    /** 以当前 label 注册监听（模拟 listen）。 */
    subscribe(event: string, handler: (payload: unknown) => void): () => void {
      const entry = { label: state.currentLabel, handler };
      let set = state.listeners.get(event);
      if (!set) state.listeners.set(event, (set = new Set()));
      set.add(entry);
      return () => set!.delete(entry);
    },
    setLabel(label: string): void {
      state.currentLabel = label;
    },
  };
});

vi.mock("@tauri-apps/api/event", () => ({
  // emit/emitTo 异步投递（microtask，贴近真实 IPC 往返），测试用 flushMicro 驱动
  emit: async (event: string, payload: unknown) => {
    await Promise.resolve();
    h.deliver(null, event, payload);
  },
  emitTo: async (target: string, event: string, payload: unknown) => {
    await Promise.resolve();
    h.deliver(target, event, payload);
  },
  listen: (event: string, cb: (e: { payload: unknown }) => void) => {
    const un = h.subscribe(event, (payload) => cb({ payload }));
    return Promise.resolve(un);
  },
}));

vi.mock("@/services/window", () => ({
  getCurrentWindowLabel: () => h.state.currentLabel,
}));

type GateMod = typeof import("./vaultSwitchGate");
let gate: GateMod;

/** 等待 microtask 队列排空（emit 投递 + listen 回调链）。 */
const flushMicro = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

/** 撕裂窗口替身：安装面板面依赖并捕获遮罩状态与 flush 调用。 */
function makePanel(windowId: string, flush?: () => Promise<void>) {
  h.setLabel(`panel-${windowId}`);
  const masks: Array<{ phase: string; switchId: string } | null> = [];
  let flushCount = 0;
  const controlled = flush ?? (async () => {});
  const deps = {
    flush: async (): Promise<void> => {
      flushCount += 1;
      await controlled();
    },
    onGateState: (s: { phase: "preparing" | "switching"; switchId: string } | null) => {
      masks.push(s);
    },
  };
  return {
    windowId,
    deps,
    masks,
    flushCalls: () => flushCount,
    /** 安装（listen 注册为 microtask 生效）。 */
    install: () => gate.installVaultSwitchGatePanel(deps),
    get mask() {
      return masks[masks.length - 1] ?? null;
    },
  };
}

/** 主窗口存活查询替身：可变的存活名单。 */
function makeAliveSource(initial: string[]) {
  const source = { alive: initial, fail: false };
  return {
    source,
    query: async (): Promise<string[]> => {
      if (source.fail) throw new Error("快照不可用");
      return [...source.alive];
    },
  };
}

beforeEach(async () => {
  vi.resetModules();
  h.state.listeners.clear();
  h.setLabel("main");
  gate = await import("./vaultSwitchGate");
});

describe("主窗口面（runVaultSwitchGate）", () => {
  it("无撕裂窗口：立即放行且不广播 prepare", async () => {
    const panel = makePanel("w1");
    await panel.install();
    h.setLabel("main");
    const alive = makeAliveSource([]);
    const r = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    expect(r.ok).toBe(true);
    expect(r.failures).toEqual([]);
    await flushMicro();
    expect(panel.flushCalls()).toBe(0);
  });

  it("存活快照查询失败：降级放行（不阻塞切换；Rust 不可达时 open_vault 随后也会失败）", async () => {
    const alive = makeAliveSource(["w1"]);
    alive.source.fail = true;
    const r = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    expect(r.ok).toBe(true);
  });

  it("prepare → 撕裂窗口遮罩 + flush + ack，主窗口收齐后放行", async () => {
    const panel = makePanel("w1");
    await panel.install();
    h.setLabel("main");
    const alive = makeAliveSource(["w1"]);
    const r = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    expect(r.ok).toBe(true);
    expect(panel.flushCalls()).toBe(1);
    expect(panel.mask).toEqual({ phase: "preparing", switchId: r.switchId });
  });

  it("无限等待：ack 迟到仍收敛，不超时放弃", async () => {
    let release!: () => void;
    const panel = makePanel("w1", () => new Promise<void>((r) => (release = r)));
    await panel.install();
    h.setLabel("main");
    const alive = makeAliveSource(["w1"]);
    const pending = gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    await flushMicro();
    await flushMicro();
    // flush 未完成 = 无 ack：门保持等待（无限等待语义）
    let settled = false;
    void pending.then(() => (settled = true));
    await flushMicro();
    expect(settled).toBe(false);
    release();
    const r = await pending;
    expect(r.ok).toBe(true);
    expect(panel.flushCalls()).toBe(1);
  });

  it("等待期间窗口关闭：从等待集剔除，不再等它的 ack", async () => {
    const panelA = makePanel("w1");
    await panelA.install();
    // w2 不安装面板面（模拟已关闭但快照曾列出）——首次查询含 w2，其后被剔除
    h.setLabel("main");
    const alive = makeAliveSource(["w1", "w2"]);
    const pending = gate.runVaultSwitchGate({
      queryAliveWindows: async () => {
        const list = await alive.query();
        // 首轮后 w2 消失（窗口关闭 → webview 消失）
        alive.source.alive = ["w1"];
        return list;
      },
      pollMs: 1,
    });
    const r = await pending;
    expect(r.ok).toBe(true);
  });

  it("boot 中窗口漏过首帧 prepare：重发补达，接好监听后 ack 放行", async () => {
    // w2 初始存活（已被枚举）但监听未接好（boot 中）；一轮重发后才 install
    const alive = makeAliveSource(["w1", "w2"]);
    const panelA = makePanel("w1");
    await panelA.install();
    h.setLabel("main");
    const pending = gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    // 第一轮 prepare 已发（w2 漏过）：短暂等待后接好 w2 的监听，下一轮重发补达
    await new Promise((r) => setTimeout(r, 5));
    const panelB = makePanel("w2");
    await panelB.install();
    const r = await pending;
    expect(r.ok).toBe(true);
    expect(panelA.flushCalls()).toBeGreaterThanOrEqual(1);
    expect(panelB.flushCalls()).toBe(1);
  });

  it("ack 失败：返回失败清单 + 广播 abort 撤遮罩", async () => {
    const panel = makePanel("w1", async () => {
      throw new Error("磁盘写满");
    });
    await panel.install();
    h.setLabel("main");
    const alive = makeAliveSource(["w1"]);
    const r = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    expect(r.ok).toBe(false);
    expect(r.failures).toEqual([{ windowId: "w1", error: "磁盘写满" }]);
    await flushMicro();
    // abort 到达撕裂窗口：遮罩撤销
    expect(panel.mask).toBeNull();
  });

  it("switchId 不匹配的 abort 不清遮罩（上一轮收尾帧不干扰新一轮门）", async () => {
    const panel = makePanel("w1");
    await panel.install();
    h.setLabel("main");
    await gate.abortVaultSwitchGate("not-a-real-id");
    await flushMicro();
    expect(panel.mask).toBeNull(); // 从未建门，保持空
    const alive = makeAliveSource(["w1"]);
    const r = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    await flushMicro();
    expect(panel.mask).toEqual({ phase: "preparing", switchId: r.switchId });
    await gate.abortVaultSwitchGate("not-a-real-id");
    await flushMicro();
    expect(panel.mask).toEqual({ phase: "preparing", switchId: r.switchId });
  });

  it("二次 prepare（新一轮切换）：重新 flush 并更新门 id", async () => {
    const panel = makePanel("w1");
    await panel.install();
    h.setLabel("main");
    const alive = makeAliveSource(["w1"]);
    const r1 = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    const r2 = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    expect(r1.switchId).not.toBe(r2.switchId);
    expect(panel.flushCalls()).toBe(2);
    expect(panel.mask).toEqual({ phase: "preparing", switchId: r2.switchId });
  });
});

describe("撕裂窗口面（状态机）", () => {
  it("preparing → markApplying（switching）→ complete（清空）", async () => {
    const panel = makePanel("w1");
    await panel.install();
    h.setLabel("main");
    const alive = makeAliveSource(["w1"]);
    const r = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    expect(gate.currentVaultSwitchGate()).toEqual({ phase: "preparing", switchId: r.switchId });
    gate.markVaultSwitchApplying();
    expect(gate.currentVaultSwitchGate()).toEqual({ phase: "switching", switchId: r.switchId });
    // 旧 id 的 complete 不清新状态；匹配 id 才清
    gate.completeVaultSwitchGate("other-id");
    expect(gate.currentVaultSwitchGate()).toEqual({ phase: "switching", switchId: r.switchId });
    gate.completeVaultSwitchGate(r.switchId);
    expect(gate.currentVaultSwitchGate()).toBeNull();
  });

  it("主窗口 label 安装面板面：prepare 不消费（主窗口是发送方）", async () => {
    const { emit } = await import("@tauri-apps/api/event");
    const panel = makePanel("w1");
    h.setLabel("main");
    await panel.install();
    await emit("vault-switch-prepare", { kind: "prepare", switchId: "sid" });
    await flushMicro();
    expect(panel.flushCalls()).toBe(0);
    expect(panel.mask).toBeNull();
  });

  it("abort 不清 switching 阶段（加载链 finally 负责收尾，防中间态暴露）", async () => {
    const panel = makePanel("w1");
    await panel.install();
    h.setLabel("main");
    const alive = makeAliveSource(["w1"]);
    const r = await gate.runVaultSwitchGate({ queryAliveWindows: alive.query, pollMs: 1 });
    gate.markVaultSwitchApplying();
    await gate.abortVaultSwitchGate(r.switchId);
    await flushMicro();
    expect(panel.mask).toEqual({ phase: "switching", switchId: r.switchId });
    gate.completeVaultSwitchGate(r.switchId);
    expect(panel.mask).toBeNull();
  });
});
