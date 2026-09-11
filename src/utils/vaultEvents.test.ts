/**
 * 仓库文件事件发射器测试：
 * 订阅/撤销/按 kind 分发/同步保序/未注册静默丢弃/订阅方异常逐个隔离。
 */
import { describe, it, expect } from "vitest";
import { emitVaultEvent, onVaultEvent } from "./vaultEvents";
import type { VaultEvent } from "./vaultEvents";

describe("仓库文件事件发射器", () => {
  it("按注册序同步分发同 kind 事件；载荷透传", () => {
    const seen: string[] = [];
    const off1 = onVaultEvent("note:changed", (e) => seen.push(`a:${e.path}`));
    const off2 = onVaultEvent("note:changed", (e) => seen.push(`b:${e.path}`));
    emitVaultEvent({ kind: "note:changed", path: "n.md" });
    expect(seen).toEqual(["a:n.md", "b:n.md"]);
    off1();
    off2();
  });

  it("按 kind 隔离：其他 kind 不触发；未注册 kind 静默丢弃", () => {
    let called = 0;
    const off = onVaultEvent("table:changed", () => {
      called++;
    });
    emitVaultEvent({ kind: "note:changed", path: "n.md" });
    emitVaultEvent({ kind: "table:changed", path: "t.atb" });
    expect(called).toBe(1);
    off();
    emitVaultEvent({ kind: "table:changed", path: "t.atb" });
    expect(called).toBe(1);
  });

  it("撤销幂等；全部撤销后注册表清空", () => {
    let called = 0;
    const off = onVaultEvent("chat:changed", () => {
      called++;
    });
    off();
    off();
    emitVaultEvent({ kind: "chat:changed", path: ".atelyx/x.jsonl" });
    expect(called).toBe(0);
  });

  it("动作事件载荷透传（重命名含 old/new；文件夹含 oldDir/newDir）", () => {
    const got: VaultEvent[] = [];
    const off1 = onVaultEvent("note:renamed", (e) => got.push(e));
    const off2 = onVaultEvent("folder:renamed", (e) => got.push(e));
    emitVaultEvent({ kind: "note:renamed", oldPath: "a.md", newPath: "b.md", newTitle: "b" });
    emitVaultEvent({ kind: "folder:renamed", oldDir: "x", newDir: "y" });
    expect(got).toEqual([
      { kind: "note:renamed", oldPath: "a.md", newPath: "b.md", newTitle: "b" },
      { kind: "folder:renamed", oldDir: "x", newDir: "y" },
    ]);
    off1();
    off2();
  });

  it("订阅方同步抛错被隔离：后续订阅方仍收到，且不外传给调用方", () => {
    const after: string[] = [];
    const off1 = onVaultEvent("canvas:changed", () => {
      throw new Error("handler 失败");
    });
    const off2 = onVaultEvent("canvas:changed", () => after.push("after"));
    expect(() => emitVaultEvent({ kind: "canvas:changed", path: "c.atlx" })).not.toThrow();
    expect(after).toEqual(["after"]);
    off1();
    off2();
  });

  it("订阅方返回被拒 Promise 也不外传给调用方", () => {
    const off = onVaultEvent("table:changed", async () => {
      throw new Error("async handler 失败");
    });
    expect(() => emitVaultEvent({ kind: "table:changed", path: "t.atb" })).not.toThrow();
    off();
  });
});
