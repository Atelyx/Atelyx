/**
 * 笔记域读写链在空间形态后端上的压测：内容面激活 stub 后端后，
 * 真实 noteStore 读写链（缓存先行 + 按文件串行队列 + canWrite 落盘许可 + 失败不静默）
 * 全部经契约落在 stub 内存树上——验证契约能表达空间语义，且既有写盘语义不因后端改变。
 * 不 mock Tauri invoke：链路上出现任何契约外 I/O 即失败（invoke mock 记录并拒绝）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Record<string, unknown> }[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    h.calls.push({ cmd, args: args ?? {} });
    throw new Error(`契约外命令调用：${cmd}`);
  },
}));

type NoteStore = typeof import("./noteStore");
type StubFactory = typeof import("@/services/content/stubSpaceBackend");

let noteStore: NoteStore;
let stubMod: StubFactory;
let stub: ReturnType<StubFactory["createSpaceStubBackend"]>;

beforeEach(async () => {
  // 模块重置回收工厂激活态与 stub 状态：每个用例从干净的后端注册表开始
  vi.resetModules();
  h.calls.length = 0;
  await import("./noteSessionStore");
  noteStore = await import("./noteStore");
  stubMod = await import("@/services/content/stubSpaceBackend");
  const { activateContentVault } = await import("@/services/content/factory");
  stub = stubMod.createSpaceStubBackend();
  await stub.write("已有.md", "预置正文");
  activateContentVault(stub.identity, stub.backend);
});

describe("笔记读写链 × 空间 stub 后端", () => {
  it("读：命中 stub 内存树并写入缓存；二次读不再触后端", async () => {
    const { readNoteContent } = noteStore.useNoteStore.getState();
    expect(await readNoteContent("已有.md")).toBe("预置正文");
    expect(stub.reads).toBe(1);
    expect(await readNoteContent("已有.md")).toBe("预置正文");
    expect(stub.reads).toBe(1);
    expect(h.calls).toEqual([]);
  });

  it("写：落 stub 内存树 + 缓存先行（写后立即读缓存即得新内容）", async () => {
    const { saveNoteContent, readNoteContent } = noteStore.useNoteStore.getState();
    const result = await saveNoteContent("已有.md", "新正文");
    expect(result).toEqual({ written: true, content: "新正文" });
    expect(stub.files.get("已有.md")).toBe("新正文");
    expect(await readNoteContent("已有.md")).toBe("新正文");
    expect(stub.reads).toBe(0);
  });

  it("新文件写自动出现在 stub 树（写自动建父目录语义由后端承载）", async () => {
    const { saveNoteContent } = noteStore.useNoteStore.getState();
    await saveNoteContent("子目录/新建.md", "内容");
    expect(stub.files.get("子目录/新建.md")).toBe("内容");
  });

  it("同文件并发保存串行完成，最终内容 = 最后一次调用", async () => {
    stub.writeDelayMs = 5;
    const { saveNoteContent } = noteStore.useNoteStore.getState();
    await Promise.all([
      saveNoteContent("已有.md", "第一版"),
      saveNoteContent("已有.md", "第二版"),
      saveNoteContent("已有.md", "第三版"),
    ]);
    expect(stub.files.get("已有.md")).toBe("第三版");
    // 末三条 = 三次保存按调用序完成（首条是 beforeEach 的预置种子写入）
    expect(stub.writeOrder.slice(-3)).toEqual(["已有.md", "已有.md", "已有.md"]);
  });

  it("canWrite 撤销落盘许可：不落盘且缓存作废（重读走后端）", async () => {
    const { saveNoteContent, readNoteContent } = noteStore.useNoteStore.getState();
    let permit = true;
    const saving = saveNoteContent("已有.md", "被取消的正文", () => permit);
    permit = false;
    await expect(saving).resolves.toEqual({ written: false, content: "被取消的正文" });
    expect(stub.files.get("已有.md")).toBe("预置正文");
    expect(await readNoteContent("已有.md")).toBe("预置正文");
    expect(stub.reads).toBe(1);
  });

  it("后端写失败如实抛出（不静默），前序失败不阻断本序", async () => {
    stub.writeDelayMs = 5;
    stub.failContents.add("坏版本");
    const { saveNoteContent } = noteStore.useNoteStore.getState();
    const failed = saveNoteContent("已有.md", "坏版本");
    const next = saveNoteContent("已有.md", "好版本");
    await expect(failed).rejects.toThrow("后端写失败");
    await next;
    expect(stub.files.get("已有.md")).toBe("好版本");
  });

  it("挂起输入 flush：全部经契约落盘到 stub 树", async () => {
    const { setPendingNoteContent, flushPendingNotes } = noteStore.useNoteStore.getState();
    const { useVaultStore } = await import("./vaultStore");
    useVaultStore.setState({ noteList: [{ file: "已有.md", name: "已有" }] });
    setPendingNoteContent("已有.md", "挂起的正文");
    await flushPendingNotes();
    expect(stub.files.get("已有.md")).toBe("挂起的正文");
    expect(h.calls).toEqual([]);
  });

  it("切仓库交叉：切后 reset 同步清缓存，新读写路由到新后端（旧内容不串味）", async () => {
    const { activateContentVault } = await import("@/services/content/factory");
    const { readNoteContent, saveNoteContent, reset } = noteStore.useNoteStore.getState();
    // 仓库 A：读入缓存并落盘
    expect(await readNoteContent("已有.md")).toBe("预置正文");
    await saveNoteContent("已有.md", "A 仓库正文");
    expect(stub.files.get("已有.md")).toBe("A 仓库正文");
    // 切到仓库 B（另一空间身份 + 另一内存树）+ 同步 reset：与 appStore 切仓库同序
    const stubB = stubMod.createSpaceStubBackend();
    activateContentVault(stubB.identity, stubB.backend);
    reset();
    // A 的缓存/内容不得串到 B：B 树为空，读取如实报缺（不走 A 后端、不落 A 缓存残留）
    await expect(readNoteContent("已有.md")).rejects.toThrow("文件不存在");
    // 切换后的写入只落 B
    await saveNoteContent("已有.md", "B 仓库正文");
    expect(stubB.files.get("已有.md")).toBe("B 仓库正文");
    expect(stub.files.get("已有.md")).toBe("A 仓库正文");
  });
});
