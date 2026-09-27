/**
 * AI 文件工具写盘后的笔记域直连通知：写 .md 落盘即投递 `note:changed`
 * （经 utils/vaultEvents 总线，笔记域据此作废内容缓存并收敛编辑面）。
 * 非笔记文本文件不投递；edit/append 经 writeVaultFile 单点、同样只投一次。
 * 后端为空间 stub（内存树），不 mock Tauri invoke：契约外命令一律失败。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

type AiFiles = typeof import("@/services/vault/aiFiles");
type VaultIndex = typeof import("@/services/vault");
type VaultEvents = typeof import("@/utils/vaultEvents");
type StubFactory = typeof import("@/services/content/stubSpaceBackend");
type Factory = typeof import("@/services/content/factory");

let aiFiles: AiFiles;
let vaultIndex: VaultIndex;
let vaultEvents: VaultEvents;
let stub: ReturnType<StubFactory["createSpaceStubBackend"]>;

beforeEach(async () => {
  vi.resetModules();
  aiFiles = await import("@/services/vault/aiFiles");
  vaultIndex = await import("@/services/vault");
  vaultEvents = await import("@/utils/vaultEvents");
  const stubMod = await import("@/services/content/stubSpaceBackend");
  const { activateContentVault } = await import("@/services/content/factory") as Factory;
  stub = stubMod.createSpaceStubBackend();
  activateContentVault(stub.identity, stub.backend);
});

/** 订阅 note:changed 并收集载荷（返回收集器）。 */
async function collectNoteChanged(): Promise<{ paths: string[] }> {
  const collected = { paths: [] as string[] };
  vaultEvents.onVaultEvent("note:changed", (e) => {
    collected.paths.push(e.path);
  });
  return collected;
}

describe("aiFiles 写盘 → note:changed 直连通知", () => {
  it("writeVaultFile 写 .md 投递一次 note:changed", async () => {
    const collected = await collectNoteChanged();
    await aiFiles.writeVaultFile("笔记.md", "正文");
    expect(collected.paths).toEqual(["笔记.md"]);
  });

  it("writeVaultFile 写非 .md 文本文件不投递", async () => {
    const collected = await collectNoteChanged();
    await aiFiles.writeVaultFile("数据.json", "{}");
    expect(collected.paths).toEqual([]);
  });

  it("editVaultFile 修改 .md 投递一次（经 writeVaultFile 单点）", async () => {
    await stub.write("笔记.md", "第一行\n第二行");
    const collected = await collectNoteChanged();
    const result = await aiFiles.editVaultFile("笔记.md", [
      { oldText: "第二行", newText: "改过的行" },
    ]);
    expect(result.ok).toBe(true);
    expect(collected.paths).toEqual(["笔记.md"]);
  });

  it("editVaultFile 校验失败未写盘不投递", async () => {
    await stub.write("笔记.md", "第一行");
    const collected = await collectNoteChanged();
    const result = await aiFiles.editVaultFile("笔记.md", [
      { oldText: "不存在的片段", newText: "x" },
    ]);
    expect(result.ok).toBe(false);
    expect(collected.paths).toEqual([]);
  });

  it("appendVaultFile 追加 .md 投递一次（经 writeVaultFile 单点）", async () => {
    await stub.write("笔记.md", "已有");
    const collected = await collectNoteChanged();
    const result = await aiFiles.appendVaultFile("笔记.md", "追加");
    expect(result.ok).toBe(true);
    expect(collected.paths).toEqual(["笔记.md"]);
  });

  it("rebuildInternalLinks 对改写清单逐个投递 note:changed", async () => {
    await stub.write("a.md", "[b](b.md)");
    stub.backend.rebuildLinks = async () => ({
      scanned: 1,
      modified: 1,
      links: 1,
      rewritten: ["a.md", "子/一个 笔记.md"],
    });
    const collected = await collectNoteChanged();
    const result = await vaultIndex.rebuildInternalLinks();
    expect(result.rewritten).toEqual(["a.md", "子/一个 笔记.md"]);
    expect(collected.paths).toEqual(["a.md", "子/一个 笔记.md"]);
  });
});
