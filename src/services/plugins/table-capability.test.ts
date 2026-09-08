/**
 * table 能力（当前打开的表格：读快照 + 写操作）桥路由测试（services/plugins/bridge 注册的宿主 `table` 命名空间）。
 *
 * 覆盖——方法路由与参数透传、参数类型错误/未知方法、未接线错误、审计。
 * store 数据源经 `setPluginTableRuntimeAccess` 注入假实现（bridge 不 import store），只验证桥的路由与校验。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { PluginManifest, PluginTableSnapshot, PluginType } from "@/types";
import type { PluginTransport } from "./worker";
import {
  attachPlugin,
  hostCapabilityNames,
  runtimeSnapshot,
  setPluginTableRuntimeAccess,
  unloadPlugin,
  type PluginTableRuntimeAccess,
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

const emptySnapshot = (): PluginTableSnapshot => ({
  tableFile: null,
  fields: [],
  rows: [],
  selectedRowId: null,
  peerColorByRowId: {},
});

function makeAccess(): PluginTableRuntimeAccess {
  return {
    snapshot: vi.fn(emptySnapshot),
    updateCell: vi.fn(),
    addRow: vi.fn(),
    removeRow: vi.fn(),
    selectRow: vi.fn(),
  };
}

beforeEach(() => {
  vi.resetAllMocks();
  setPluginTableRuntimeAccess(makeAccess());
});

afterEach(() => {
  setPluginTableRuntimeAccess(null);
  for (const s of spawned) unloadPlugin(s.id);
  spawned.length = 0;
});

describe("table 能力路由", () => {
  it("snapshot：透传并回包", async () => {
    const snap = emptySnapshot();
    const access = makeAccess();
    access.snapshot = vi.fn(() => snap);
    setPluginTableRuntimeAccess(access);
    const b = spawnPlugin("com.test.tb1");
    b.transport.receive({ kind: "call", seq: 1, method: "call", args: ["table", "snapshot", []] });
    await tick();
    expect(access.snapshot).toHaveBeenCalledWith();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 1, ok: true, result: snap });
  });

  it("updateCell / addRow / removeRow / selectRow：参数透传", async () => {
    const access = makeAccess();
    setPluginTableRuntimeAccess(access);
    const b = spawnPlugin("com.test.tb2");
    b.transport.receive({
      kind: "call",
      seq: 2,
      method: "call",
      args: ["table", "updateCell", ["r1", "f1", "值"]],
    });
    await tick();
    expect(access.updateCell).toHaveBeenCalledWith("r1", "f1", "值");
    b.transport.receive({ kind: "call", seq: 3, method: "call", args: ["table", "addRow", []] });
    await tick();
    expect(access.addRow).toHaveBeenCalledWith();
    b.transport.receive({ kind: "call", seq: 4, method: "call", args: ["table", "removeRow", ["r1"]] });
    await tick();
    expect(access.removeRow).toHaveBeenCalledWith("r1");
    b.transport.receive({ kind: "call", seq: 5, method: "call", args: ["table", "selectRow", [null]] });
    await tick();
    expect(access.selectRow).toHaveBeenCalledWith(null);
  });

  it("参数类型错误与未知方法：友好 error reply", async () => {
    const b = spawnPlugin("com.test.tb3");
    b.transport.receive({ kind: "call", seq: 10, method: "call", args: ["table", "updateCell", ["r1"]] });
    await tick();
    expect(b.transport.posted).toContainEqual({
      kind: "reply",
      seq: 10,
      ok: false,
      error: "table.updateCell 需要 rowId 与 fieldId",
    });
    b.transport.receive({ kind: "call", seq: 11, method: "call", args: ["table", "removeRow", [123]] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 11, ok: false, error: "table.removeRow 需要 rowId" });
    b.transport.receive({ kind: "call", seq: 12, method: "call", args: ["table", "nonexistent", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 12, ok: false, error: "table 无方法 nonexistent" });
  });

  it("未接线：友好 error reply", async () => {
    setPluginTableRuntimeAccess(null);
    const b = spawnPlugin("com.test.tb4");
    b.transport.receive({ kind: "call", seq: 13, method: "call", args: ["table", "snapshot", []] });
    await tick();
    expect(b.transport.posted).toContainEqual({ kind: "reply", seq: 13, ok: false, error: "表格能力未就绪" });
  });

  it("审计记录 table 命名空间；注册表含 table", async () => {
    const b = spawnPlugin("com.test.tb5");
    b.transport.receive({ kind: "call", seq: 14, method: "call", args: ["table", "snapshot", []] });
    await tick();
    expect(runtimeSnapshot().find((e) => e.id === "com.test.tb5")?.used).toContain("table");
    expect(hostCapabilityNames()).toContain("table");
  });
});
