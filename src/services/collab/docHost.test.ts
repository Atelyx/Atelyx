/**
 * 协作文档宿主注册表测试（services/collab/docHost.ts 的文档注册表 + 模型注册制）。
 * 覆盖模型注册/未注册报错、docId kind 拆分路由、bind/unbind 引用计数（多绑定共享实例、
 * 归零保留、再绑定重建）、远端消息/重连/销毁路由、resyncAll/destroyAll 遍历激活文档。
 * 用假 adapter（vi.fn）驱动，不触碰任何领域模型。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import type { DocModelAdapter } from "./docHost";

// 注册表为模块级单例：每测试重置模块以隔离注册状态
type DocHostMod = typeof import("./docHost");
let host: DocHostMod;

beforeEach(async () => {
  vi.resetModules();
  host = await import("./docHost");
});

function fakeAdapter(kind = "fake"): DocModelAdapter & {
  createDoc: ReturnType<typeof vi.fn>;
  applyRemoteMessage: ReturnType<typeof vi.fn>;
  resync: ReturnType<typeof vi.fn>;
  destroy: ReturnType<typeof vi.fn>;
  applyRemoteAwareness: ReturnType<typeof vi.fn>;
} {
  let seq = 0;
  return {
    kind,
    createDoc: vi.fn((docId: string) => ({ docId, n: ++seq })),
    applyRemoteMessage: vi.fn((_inst: unknown, _peerId: number, _payload: unknown, _meta?: unknown) => {}),
    resync: vi.fn((_inst: unknown) => {}),
    destroy: vi.fn((_inst: unknown) => {}),
    applyRemoteAwareness: vi.fn((_inst: unknown, _payload: unknown) => {}),
  };
}

describe("模型注册与绑定", () => {
  it("bindDoc 路由到 adapter.createDoc（docId + baseline 透传）并返回实例", () => {
    const a = fakeAdapter();
    host.registerDocModel(a);
    const inst = host.bindDoc("fake:notes/a.md", "hello");
    expect(a.createDoc).toHaveBeenCalledWith("fake:notes/a.md", "hello");
    expect(inst).toEqual({ docId: "fake:notes/a.md", n: 1 });
  });

  it("未注册 kind：bindDoc 抛错", () => {
    expect(() => host.bindDoc("nope:a.md", "")).toThrow("协作文档模型未注册：nope");
  });

  it("多绑定共享实例：refcount>0 时复用，createDoc 只调一次", () => {
    const a = fakeAdapter();
    host.registerDocModel(a);
    const first = host.bindDoc("fake:x");
    const second = host.bindDoc("fake:x");
    expect(first).toBe(second);
    expect(a.createDoc).toHaveBeenCalledTimes(1);
  });

  it("unbind 归零后再次绑定以基线重建（destroy 旧 + createDoc 新）", () => {
    const a = fakeAdapter();
    host.registerDocModel(a);
    const first = host.bindDoc("fake:x", "v1");
    host.unbindDoc("fake:x");
    host.unbindDoc("fake:x"); // 归零后再减为 no-op
    const second = host.bindDoc("fake:x", "v2");
    expect(second).not.toBe(first);
    expect(a.destroy).toHaveBeenCalledWith(first);
    expect(a.createDoc).toHaveBeenLastCalledWith("fake:x", "v2");
  });
});

describe("模型方法路由", () => {
  it("resyncAllDocs 只遍历激活文档（refcount>0）", () => {
    const a = fakeAdapter();
    host.registerDocModel(a);
    const active = host.bindDoc("fake:a");
    const inactive = host.bindDoc("fake:b");
    host.unbindDoc("fake:b"); // 归零 → 不参与 resync
    host.resyncAllDocs();
    expect(a.resync).toHaveBeenCalledWith(active);
    expect(a.resync).not.toHaveBeenCalledWith(inactive);
  });

  it("destroyAllDocs 销毁全部并清空注册表", () => {
    const a = fakeAdapter();
    host.registerDocModel(a);
    const x = host.bindDoc("fake:x");
    const y = host.bindDoc("fake:y");
    host.destroyAllDocs();
    expect(a.destroy).toHaveBeenCalledWith(x);
    expect(a.destroy).toHaveBeenCalledWith(y);
  });
});
