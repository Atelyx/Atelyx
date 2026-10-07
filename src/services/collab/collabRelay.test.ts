/**
 * 协作连接宿主化契约测试（单连接 + 转发传输）：宿主单点持连，撕裂窗口 = proxy 传输（上行转发/下行分发/宿主回环）。
 * 用内存事件线驱动（emit 全体投递 / emitTo 定向，同 Tauri 语义），按窗口 label 模拟多窗口；投递为 microtask 异步，贴近真实 IPC。
 * 覆盖时序契约：snapshot 前下行帧先到（乱序收敛）、宿主断连期间出站静默丢弃、attach 在途的 demand 上报仍可达。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { CollabPeer, CollabPresence } from "@/types";

const h = vi.hoisted(() => {
  const state = {
    /** 内存事件线：event → 按窗口 label 注册的 handler 集。 */
    listeners: new Map<string, Set<{ label: string; handler: (payload: unknown) => void }>>(),
    /** 当前窗口 label（模拟宿主/撕裂窗口切换角色）。 */
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
  // emit/emitTo 异步投递（microtask，贴近真实 IPC 往返）：connect 内同步发出的 attach 响应
  // 不会同步到达，测试用 flushMicro 驱动，可构造「attach 在途」的乱序时序
  emit: async (event: string, payload: unknown) => {
    await Promise.resolve();
    h.deliver(null, event, payload);
  },
  emitTo: async (target: string, event: string, payload: unknown) => {
    await Promise.resolve();
    h.deliver(target, event, payload);
  },
  listen: (event: string, cb: (e: { payload: unknown }) => void) => {
    // 同步注册（真实 listen 的注册异步生效，但测试的 deliver 是同步投递——注册必须即时可见）
    const un = h.subscribe(event, (payload) => cb({ payload }));
    return Promise.resolve(un);
  },
}));

// getCurrentWindowLabel 决定角色判定（宿主 = "main"）
vi.mock("@/services/window", () => ({
  getCurrentWindowLabel: () => h.state.currentLabel,
}));

import type { CollabChannel } from "@/services/collab/transport";

// 模块级状态（hostInstalled/demand/presence 表）随 resetModules 重建，用例间互不污染
type RelayMod = typeof import("./collabRelay");
type ProxyMod = typeof import("./proxyTransport");
let relay: RelayMod;
let proxyMod: ProxyMod;

// ---------- fixtures ----------

function peer(id: number): CollabPeer {
  return { peerId: id, nickname: `p${id}`, color: "#fff", deviceName: `d${id}`, presence: null };
}

function presence(patch: Partial<CollabPresence> = {}): CollabPresence {
  return { file: null, selection: null, view: null, ...patch };
}

/** 宿主依赖替身：捕获出站与聚合 presence 输出。 */
function makeHostDeps() {
  const sent: Array<{ channel: CollabChannel; file: string; payload: unknown; targetPeerId?: number }> = [];
  const presenceOut: CollabPresence[] = [];
  let demandChanged = 0;
  let nextPeerId: number | null = 7;
  let hostUp = true;
  return {
    sent,
    presenceOut,
    demandChangedCount: () => demandChanged,
    deps: {
      send: (channel: CollabChannel, file: string, payload: unknown, targetPeerId?: number) => {
        if (!hostUp) return false;
        sent.push({ channel, file, payload, ...(targetPeerId !== undefined ? { targetPeerId } : {}) });
        return true;
      },
      myPeerId: () => nextPeerId,
      hostState: () => ({ connected: hostUp, peers: [peer(1), peer(2)], myPeerId: nextPeerId }),
      onDemandChanged: () => {
        demandChanged += 1;
      },
      onPresenceOut: (p: CollabPresence) => {
        presenceOut.push(p);
      },
    },
    setPeerId(id: number | null): void {
      nextPeerId = id;
    },
    setUp(up: boolean): void {
      hostUp = up;
    },
  };
}

/** 撕裂窗口 proxy 替身：connect 并捕获回调面。 */
function makeProxy(label: string) {
  h.setLabel(label);
  const channels: Array<[number, CollabChannel, string, unknown]> = [];
  const statuses: boolean[] = [];
  const peersFrames: CollabPeer[][] = [];
  const helloAcks: number[] = [];
  const resyncs: number[] = [];
  const handle = proxyMod.proxyCollabTransport.connect({
    url: "",
    hello: { nickname: "", color: "", deviceName: "" },
    onHelloAck: (id) => helloAcks.push(id),
    onPeers: (peers) => peersFrames.push(peers),
    onPeerPresence: () => {},
    onChannelMessage: (peerId, channel, file, payload) => channels.push([peerId, channel, file, payload]),
    onMetaChanged: () => {},
    onRenamed: () => {},
    onResync: () => {
      resyncs.push(1);
    },
    onServerError: () => {},
    onStatusChange: (connected) => statuses.push(connected),
  });
  return { handle, channels, statuses, peersFrames, helloAcks, resyncs };
}

beforeEach(async () => {
  vi.useFakeTimers();
  vi.resetModules();
  h.state.listeners.clear();
  h.state.currentLabel = "main";
  relay = await import("./collabRelay");
  proxyMod = await import("./proxyTransport");
});

afterEach(() => {
  vi.useRealTimers();
});

/** 排空 mock 事件线的 microtask 投递队列（emit/emitTo 异步生效；多轮覆盖链式转发）。 */
async function flushMicro(): Promise<void> {
  for (let i = 0; i < 4; i++) await Promise.resolve();
}

// ---------- presence 聚合 ----------

describe("presence 聚合（宿主单 peer 上报）", () => {
  it("两窗口合并：焦点取最后活跃窗口，openFiles 并集去重且最后活跃窗口在前，锁/生成/编辑清单取并集", () => {
    const host = makeHostDeps(); const { deps } = host;
    h.setLabel("main");
    relay.installCollabRelayHost(deps);

    // 主窗口上报：画布 A 聚焦 + 锁节点 n1（本用例只走上行 presence）
    h.deliver("main", "collab-relay-up", {
      kind: "presence",
      from: "main",
      presence: presence({
        file: "a.atlx",
        view: "canvas",
        selection: null,
        openFiles: [{ file: "a.atlx", view: "canvas" }],
        lockedNodes: [{ id: "n1", since: 1 }],
        streamingNodeIds: ["c1"],
      }),
    });
    // 撕裂窗口上报：表格 B 聚焦 + 编辑笔记 note.md
    h.deliver("main", "collab-relay-up", {
      kind: "presence",
      from: "panel-1",
      presence: presence({
        file: "b.atb",
        view: "table",
        selection: { kind: "cell", rowId: "r1", fieldId: "f1" },
        openFiles: [{ file: "b.atb", view: "table" }, { file: "note.md", view: "note" }],
        editingNotes: ["note.md"],
      }),
    });
    vi.advanceTimersByTime(100);

    expect(host.presenceOut).toHaveLength(1);
    const merged = host.presenceOut[0];
    // 焦点 = 最后活跃（panel-1 后到）
    expect(merged.file).toBe("b.atb");
    expect(merged.view).toBe("table");
    // openFiles 并集：最后活跃窗口在前，重复文件去重
    expect(merged.openFiles).toEqual([
      { file: "b.atb", view: "table" },
      { file: "note.md", view: "note" },
      { file: "a.atlx", view: "canvas" },
    ]);
    expect(merged.lockedNodes).toEqual([{ id: "n1", since: 1 }]);
    expect(merged.streamingNodeIds).toEqual(["c1"]);
    expect(merged.editingNotes).toEqual(["note.md"]);
  });

  it("节流窗口内多次更新只 flush 一次（输出最新合并值）", () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    h.deliver("main", "collab-relay-up", {
      kind: "presence",
      from: "panel-1",
      presence: presence({ file: "x.md", view: "note" }),
    });
    h.deliver("main", "collab-relay-up", {
      kind: "presence",
      from: "panel-1",
      presence: presence({ file: "y.md", view: "note" }),
    });
    vi.advanceTimersByTime(100);
    expect(host.presenceOut).toHaveLength(1);
    expect(host.presenceOut[0].file).toBe("y.md");
  });

  it("收缩：窗口 presence 清空后并集随之收缩；detach 移除条目", () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    h.deliver("main", "collab-relay-up", {
      kind: "presence",
      from: "panel-1",
      presence: presence({ file: "a.atb", view: "table", openFiles: [{ file: "a.atb", view: "table" }] }),
    });
    vi.advanceTimersByTime(100);
    expect(host.presenceOut[0].openFiles).toEqual([{ file: "a.atb", view: "table" }]);

    // panel-1 清空（关闭表格）
    h.deliver("main", "collab-relay-up", { kind: "presence", from: "panel-1", presence: presence() });
    vi.advanceTimersByTime(100);
    const merged = host.presenceOut[1];
    expect(merged.file).toBeNull();
    expect(merged.openFiles).toBeUndefined();

    // detach 移除条目：main 空闲 presence 照发（服务端覆盖语义：清空远端高亮）
    h.deliver("main", "collab-relay-up", { kind: "detach", from: "panel-1" });
    vi.advanceTimersByTime(100);
    expect(host.presenceOut[2]).toEqual(presence());
  });
});

// ---------- demand 聚合 ----------

describe("demand 聚合（协作意愿声明汇聚宿主评估）", () => {
  it("上报累加 / 释放递减，每次变化触发 onDemandChanged", () => {
    const host = makeHostDeps(); const { deps, demandChangedCount } = host;
    relay.installCollabRelayHost(deps);
    h.deliver("main", "collab-relay-up", { kind: "demand", from: "panel-1", delta: 1 });
    expect(relay.remoteCollabDemandTotal()).toBe(1);
    expect(demandChangedCount()).toBe(1);
    h.deliver("main", "collab-relay-up", { kind: "demand", from: "panel-1", delta: 1 });
    expect(relay.remoteCollabDemandTotal()).toBe(2);
    h.deliver("main", "collab-relay-up", { kind: "demand", from: "panel-1", delta: -1 });
    expect(relay.remoteCollabDemandTotal()).toBe(1);
    expect(demandChangedCount()).toBe(3);
  });

  it("prune 清理消失窗口的条目（计数回落触发 onDemandChanged），存活窗口保留", () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    h.deliver("main", "collab-relay-up", { kind: "demand", from: "panel-1", delta: 1 });
    h.deliver("main", "collab-relay-up", { kind: "demand", from: "panel-2", delta: 1 });
    expect(relay.remoteCollabDemandTotal()).toBe(2);
    relay.pruneCollabRelayWindows(["main", "panel-2"]);
    expect(relay.remoteCollabDemandTotal()).toBe(1);
    expect(host.demandChangedCount()).toBe(3);
  });

  it("detach 清 demand 与 presence 条目", () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    h.deliver("main", "collab-relay-up", { kind: "demand", from: "panel-1", delta: 1 });
    h.deliver("main", "collab-relay-up", {
      kind: "presence",
      from: "panel-1",
      presence: presence({ file: "a.md", view: "note" }),
    });
    vi.advanceTimersByTime(100);
    h.deliver("main", "collab-relay-up", { kind: "detach", from: "panel-1" });
    expect(relay.remoteCollabDemandTotal()).toBe(0);
    vi.advanceTimersByTime(100);
    // panel-1 移除后合并值回落到空（无剩余条目的扩展字段）
    expect(host.presenceOut[1]).toEqual(presence());
  });
});

// ---------- plugin-msg 回环 ----------

describe("plugin-msg 宿主回环（同应用窗口间广播可达）", () => {
  it("宿主 loopback 合成 plugin-msg 入站帧广播，撕裂窗口 proxy 喂 onChannelMessage；域帧不回环", async () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    await flushMicro();

    relay.loopbackPluginMsg("com.a:ch", { n: 1 });
    await flushMicro();
    expect(p.channels).toEqual([[7, "plugin-msg", "com.a:ch", { n: 1 }]]);

    // 服务端域帧经转发面照常下行；不回环的是宿主自身发出的域帧（sendPluginMessage 仅回环 plugin-msg）
    relay.forwardCollabInbound({ kind: "channel", peerId: 9, channel: "table-patch", file: "t.atb", payload: {} });
    await flushMicro();
    expect(p.channels).toHaveLength(2);
    expect(p.channels[1][1]).toBe("table-patch");
  });

  it("撕裂窗口上行 plugin-msg：宿主发出 + 回环广播排除来源窗口", async () => {
    const host = makeHostDeps(); const { deps, sent } = host;
    relay.installCollabRelayHost(deps);
    const p1 = makeProxy("panel-1");
    h.setLabel("main");
    const p2 = makeProxy("panel-2");
    await flushMicro();

    h.setLabel("panel-1");
    expect(p1.handle.sendMessage("plugin-msg", "com.a:ch", { n: 1 })).toBe(true);
    await flushMicro();
    // 宿主出站收到原样帧
    expect(sent).toEqual([{ channel: "plugin-msg", file: "com.a:ch", payload: { n: 1 } }]);
    // 回环广播：来源 panel-1 排除，panel-2 收到
    expect(p1.channels).toHaveLength(0);
    expect(p2.channels).toEqual([[7, "plugin-msg", "com.a:ch", { n: 1 }]]);
  });
});

// ---------- 全链路（attach/snapshot/出站/入站/时序） ----------

describe("全链路：撕裂窗口 proxy ↔ 宿主中继", () => {
  it("attach 拉状态快照：定向响应按 hello-ack → peers → status 序喂回调面", async () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    await flushMicro();

    expect(p.statuses).toEqual([false, true]);
    expect(p.helloAcks).toEqual([7]);
    expect(p.peersFrames).toEqual([[peer(1), peer(2)]]);
  });

  it("出站：presence 上行进宿主聚合；域帧转发宿主 deps.send 原样投递", async () => {
    const host = makeHostDeps(); const { deps, sent } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    await flushMicro();

    p.handle.sendPresence(presence({ file: "a.atb", view: "table" }));
    await flushMicro(); // 上行帧经事件线异步抵达宿主，先排空再推进聚合节流时钟
    vi.advanceTimersByTime(100);
    expect(host.presenceOut[0].file).toBe("a.atb");

    expect(p.handle.sendMessage("canvas-patch", "c.atlx", { ops: [] })).toBe(true);
    await flushMicro();
    expect(sent).toEqual([{ channel: "canvas-patch", file: "c.atlx", payload: { ops: [] } }]);
  });

  it("入站：宿主 forwardInbound 各帧全量广播，撕裂窗口回调面按帧型收到", async () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    await flushMicro();

    relay.forwardCollabInbound({ kind: "status", connected: false });
    relay.forwardCollabInbound({ kind: "peers", peers: [peer(3)] });
    relay.forwardCollabInbound({ kind: "meta-changed", key: "agents.json" });
    relay.forwardCollabInbound({ kind: "renamed", oldPath: "a.md", newPath: "b.md" });
    relay.forwardCollabInbound({ kind: "resync" });
    await flushMicro();

    expect(p.statuses).toEqual([false, true, false]);
    expect(p.peersFrames).toEqual([[peer(1), peer(2)], [peer(3)]]);
    expect(p.resyncs).toHaveLength(1);
  });

  it("宿主断连期间出站返回 false（静默丢弃语义），恢复后 true", async () => {
    const host = makeHostDeps(); const { deps, setUp } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    await flushMicro();

    relay.forwardCollabInbound({ kind: "status", connected: false });
    await flushMicro();
    expect(p.handle.sendMessage("plugin-msg", "ch", {})).toBe(false);
    relay.forwardCollabInbound({ kind: "status", connected: true });
    await flushMicro();
    expect(p.handle.sendMessage("plugin-msg", "ch", {})).toBe(true);
    setUp(false);
    // 宿主连接状态变化经 status 下行同步（setUp 只影响宿主 send 返回值，此处验证恢复路径）
    relay.forwardCollabInbound({ kind: "status", connected: false });
    await flushMicro();
    expect(p.handle.sendMessage("plugin-msg", "ch", {})).toBe(false);
  });

  it("乱序收敛：snapshot 前下行 peers 帧先到，snapshot 到达后状态被全量校正", async () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    // attach 在途（emit 队列未排空）时，先同步直达一帧 peers（模拟宿主先广播、snapshot 后到）
    h.deliver("panel-1", "collab-relay-down", { kind: "peers", peers: [peer(9)] });
    expect(p.peersFrames).toEqual([[peer(9)]]);
    await flushMicro();
    // snapshot 到达：全量覆盖收敛
    expect(p.peersFrames[p.peersFrames.length - 1]).toEqual([peer(1), peer(2)]);
    expect(p.helloAcks).toEqual([7]);
    expect(p.statuses).toEqual([false, true]);
  });

  it("二进制插件载荷：撕裂窗口出站经 base64 编码过事件线，宿主解码原样发出", async () => {
    const host = makeHostDeps(); const { deps, sent } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    await flushMicro();

    const bytes = new Uint8Array([1, 2, 3, 255]);
    expect(p.handle.sendMessage("plugin-msg", "com.a:ch", bytes)).toBe(true);
    await flushMicro();
    expect(sent).toHaveLength(1);
    expect(sent[0].payload).toBeInstanceOf(Uint8Array);
    expect(Array.from(sent[0].payload as Uint8Array)).toEqual([1, 2, 3, 255]);
  });

  it("disconnect：detach 上行清条目、isClosed 置位、后续下行不再喂回调", async () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    const p = makeProxy("panel-1");
    await flushMicro();

    p.handle.disconnect();
    expect(p.handle.isClosed()).toBe(true);
    expect(relay.remoteCollabDemandTotal()).toBe(0);

    relay.forwardCollabInbound({ kind: "peers", peers: [peer(5)] });
    await flushMicro();
    expect(p.peersFrames).toEqual([[peer(1), peer(2)]]);
  });

  it("attach 在途的 demand 上报仍可达（事件线异步投递不丢）", async () => {
    const host = makeHostDeps(); const { deps } = host;
    relay.installCollabRelayHost(deps);
    h.setLabel("panel-1");
    // connect（attach 上行在途）后立即上报 demand
    const p = makeProxy("panel-1");
    h.deliver("main", "collab-relay-up", { kind: "demand", from: "panel-1", delta: 1 });
    expect(relay.remoteCollabDemandTotal()).toBe(1);
    await flushMicro();
    expect(p.statuses).toEqual([false, true]);
  });
});
