/**
 * collab 能力（协作在线状态：读 peers + 上报 presence）桥路由测试（services/plugins/bridge 注册的宿主 `collab` 命名空间）。
 *
 * 覆盖——方法路由与参数透传、参数类型错误/未知方法、未接线错误、审计。
 * store 数据源经 `setPluginCollabAccess` 注入假实现（bridge 不 import store），只验证桥的路由与校验。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { CollabPeer, PluginManifest, PluginType } from "@/types";
import type { PluginTransport } from "./worker";
import {
  attachPlugin,
  hostCapabilityNames,
  runtimeSnapshot,
  setPluginCollabAccess,
  unloadPlugin,
  type PluginCollabAccess,
} from "./bridge";

class FakeTransport implements PluginTransport {
  posted: unknown[] = [];
  private handlers: Array<(m: unknown) => void> = [];
  post(message: unknown): void {
    this.posted.push(message);
  }
  onMessage(handler: (m: unknown) => void): () => void {
    this.handlers.push(handler);
    return () => {
      this.handlers = this.handlers.filter((h) => h !== handler);
    };
  }
  receive(message: unknown): void {
    for (const h of this.handlers) h(message);
  }
  dispose(): void {}
}

interface Spawned {
  id: string;
  transport: FakeTransport;
}
const spawned: Spawned[] = [];

const manifest = (id: string): PluginManifest => ({
  schemaVersion: 2,
  id,
  name: id,
  version: "1.0.0",
  type: "tool" as PluginType,
  main: "plugin.js",
});

function spawnPlugin(id: string): Spawned {
  const transport = new FakeTransport();
  attachPlugin(manifest(id), transport);
  spawned.push({ id, transport });
  return { id, transport };
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

function makeAccess(): PluginCollabAccess {
  return {
    peers: vi.fn<() => CollabPeer[]>(() => []),
    setPresence: vi.fn(),
  };
}

beforeEach(() => {
  vi.resetAllMocks();
  setPluginCollabAccess(makeAccess());
});

afterEach(() => {
  setPluginCollabAccess(null);
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("collab 能力路由", () => {
  it("peers：透传并回包", async () => {
    const peers: CollabPeer[] = [
      { peerId: 1, nickname: "甲", color: "#f00", deviceName: "d1", presence: null },
    ];
    const access = makeAccess();
    access.peers = vi.fn(() => peers);
    setPluginCollabAccess(access);
    const b = spawnPlugin("com.test.cl1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["collab", "peers", []] });
    await tick();
    expect(access.peers).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 1, ok: true, result: peers });
  });

  it("setPresence：view/file 透传（含 null）", async () => {
    const access = makeAccess();
    setPluginCollabAccess(access);
    const b = spawnPlugin("com.test.cl2");
    b.transport.receive({
      kind: "call",
      seq: 2,
      method: "call",
      args: ["collab", "setPresence", ["com.acme.timeline", "t.atb"]],
    });
    await tick();
    expect(access.setPresence).toHaveBeenCalledWith("com.acme.timeline", "t.atb");
    b.transport.receive({ kind: "call", seq: 3, method: "call", args: ["collab", "setPresence", [null, null]] });
    await tick();
    expect(access.setPresence).toHaveBeenCalledWith(null, null);
  });

  it("参数类型错误与未知方法：友好 error reply", async () => {
    const b = spawnPlugin("com.test.cl3");
    b.transport.receive({ kind: "call", seq: 10, method: "call", args: ["collab", "setPresence", [123, "a.atlx"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 10,
      ok: false,
      error: "collab.setPresence 需要 view 或 null",
    });
    b.transport.receive({ kind: "call", seq: 11, method: "call", args: ["collab", "nonexistent", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 11, ok: false, error: "collab 无方法 nonexistent" });
  });

  it("未接线：友好 error reply", async () => {
    setPluginCollabAccess(null);
    const b = spawnPlugin("com.test.cl4");
    b.transport.receive({ kind: "call", seq: 12, method: "call", args: ["collab", "peers", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 12, ok: false, error: "协作能力未就绪" });
  });

  it("审计记录 collab 命名空间；注册表含 collab", async () => {
    const b = spawnPlugin("com.test.cl5");
    b.transport.receive({ kind: "call", seq: 13, method: "call", args: ["collab", "peers", []] });
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.test.cl5")?.used).toContain("collab");
    expect(hostCapabilityNames()).toContain("collab");
  });
});
