/**
 * 声明式覆盖（replace）桥测试：能力命名空间注册冲突的协商语义。
 *
 * 覆盖——默认 first-wins（拒绝 + 告警含持有者）、显式 replace+requires → last-wins 替换、
 * 替换后旧持有者卸载不误删新项、新持有者卸载能力消失、调用路由到新持有者实现。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { PluginManifest, PluginType } from "@/types";
import type { PluginTransport } from "./worker";
import {
  attachPlugin,
  pluginCapabilityOwner,
  unloadPlugin,
} from "./bridge";

/** 最小传输 mock：收到 invoke 时自动回包（模拟 worker 执行注册函数）。 */
class FakeTransport implements PluginTransport {
  posted: unknown[] = [];
  /** fnId → 返回值的工厂（模拟插件函数实现）。 */
  invokeResult: (fnId: string, args: unknown[]) => unknown = () => undefined;
  private handlers: Array<(m: unknown) => void> = [];
  post(message: unknown): void {
    this.posted.push(message);
    const m = message as { kind?: string; seq?: number; fnId?: string; args?: unknown[] };
    if (m.kind === "invoke" && typeof m.seq === "number") {
      // 模拟 worker 执行：异步回包
      setTimeout(() => {
        const result = this.invokeResult(m.fnId ?? "", m.args ?? []);
        this.receive({ kind: "reply", seq: m.seq, ok: true, result });
      }, 0);
    } else if (m.kind === "reply") {
      // 回包投递给监听者（与真实传输一致：消息双向流动）
      this.receive(message);
    }
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

const manifest = (id: string, extra?: Partial<PluginManifest>): PluginManifest => ({
  schemaVersion: 2,
  id,
  name: id,
  version: "1.0.0",
  type: "tool" as PluginType,
  main: "plugin.js",
  ...extra,
});

function spawnPlugin(id: string, extra?: Partial<PluginManifest>): Spawned {
  const transport = new FakeTransport();
  attachPlugin(manifest(id, extra), transport);
  spawned.push({ id, transport });
  return { id, transport };
}

function registerCapability(t: FakeTransport, seq: number, namespace: string): Promise<unknown> {
  const result = new Promise<unknown>((resolve) => {
    const h = (m: unknown): void => {
      const msg = m as { kind?: string; seq?: number; ok?: boolean; result?: unknown; error?: string };
      if (msg.kind === "reply" && msg.seq === seq) {
        unsub();
        resolve(msg.ok ? msg.result : new Error(msg.error ?? "注册失败"));
      }
    };
    const unsub = t.onMessage(h);
  });
  t.receive({
    kind: "call",
    seq,
    method: "registerCapability",
    args: [{ namespace, methodIds: { query: "f-query" } }],
  });
  return result;
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  vi.resetAllMocks();
});

afterEach(() => {
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("能力覆盖（replace）", () => {
  it("默认冲突：无替换意图的注册被拒绝，告警含持有者", async () => {
    const a = spawnPlugin("com.r.a");
    await registerCapability(a.transport, 1, "com.r.shared");
    const d = spawnPlugin("com.r.d");
    const err = await registerCapability(d.transport, 2, "com.r.shared");
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message).toContain("已被插件 com.r.a 占用");
    expect(pluginCapabilityOwner("com.r.shared")).toBe("com.r.a");
  });

  it("显式 replace + requires：last-wins 替换，调用路由到新持有者", async () => {
    const a = spawnPlugin("com.r.a");
    a.transport.invokeResult = (fnId) => (fnId === "f-query" ? "fromA" : undefined);
    await registerCapability(a.transport, 1, "com.r.shared");

    const b = spawnPlugin("com.r.b", { requires: ["com.r.shared"], replace: ["com.r.shared"] });
    b.transport.invokeResult = (fnId) => (fnId === "f-query" ? "fromB" : undefined);
    const res = await registerCapability(b.transport, 2, "com.r.shared");
    expect(res).toBe(true);
    expect(pluginCapabilityOwner("com.r.shared")).toBe("com.r.b");

    // 调用方 → 路由到新持有者（com.r.b）的实现
    const caller = spawnPlugin("com.r.caller");
    caller.transport.receive({ kind: "call", seq: 3, method: "call", args: ["com.r.shared", "query", []] });
    await tick();
    expect(caller.transport.posted).toContainEqual({ kind: "reply", seq: 3, ok: true, result: "fromB" });
  });

  it("替换后旧持有者卸载：不误删新持有者条目（按 owner 清理）", async () => {
    const a = spawnPlugin("com.r.a");
    await registerCapability(a.transport, 1, "com.r.shared");
    const b = spawnPlugin("com.r.b", { requires: ["com.r.shared"], replace: ["com.r.shared"] });
    await registerCapability(b.transport, 2, "com.r.shared");
    unloadPlugin("com.r.a");
    expect(pluginCapabilityOwner("com.r.shared")).toBe("com.r.b");
    // 替换后新持有者卸载：能力消失（无自动回退，符合无特权原则）
    unloadPlugin("com.r.b");
    expect(pluginCapabilityOwner("com.r.shared")).toBeUndefined();
  });

  it("requires 未声明 replace 目标：不满足替换条件，仍按冲突拒绝", async () => {
    const a = spawnPlugin("com.r.a");
    await registerCapability(a.transport, 1, "com.r.other");
    // replace 声明了但 requires 未声明 → 不满足 last-wins 前提
    const b = spawnPlugin("com.r.b", { replace: ["com.r.other"] });
    const err = await registerCapability(b.transport, 2, "com.r.other");
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message).toContain("已被插件 com.r.a 占用");
  });
});
