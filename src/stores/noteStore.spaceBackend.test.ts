/**
 * 笔记域读写链在空间形态后端上的压测：内容面激活 stub 后端后，
 * 真实 noteStore 读写链（缓存先行 + 按文件串行队列 + 失败不静默）
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

  it("写盘在途 + 切仓库：排队的旧仓库写被身份守卫作废，不落新仓库", async () => {
    const { activateContentVault } = await import("@/services/content/factory");
    const { saveNoteContent, reset } = noteStore.useNoteStore.getState();
    stub.writeDelayMs = 50;
    const first = saveNoteContent("已有.md", "旧仓库新正文"); // 排队即捕获归属（旧仓库）
    const second = saveNoteContent("已有.md", "排队的旧正文"); // 排在其后，同属旧仓库
    // 等第一笔真正进入在途窗口（后端写延迟 50ms），期间切换到新空间仓库
    // （与切仓库流程同序：激活新身份 + 同步 reset）；stub 默认身份两实例相同，
    // 须给 B 独立 spaceId 才能让身份键真正区分两仓库
    await new Promise((r) => setTimeout(r, 10));
    const stubB = stubMod.createSpaceStubBackend();
    activateContentVault(
      { kind: "space", serverUrl: "stub://space", spaceId: "stub-space-b" },
      stubB.backend,
    );
    reset();
    await first;
    // 在途第一笔在切换前已开写（后端已定为旧仓库）：归旧仓库落盘
    expect(stub.files.get("已有.md")).toBe("旧仓库新正文");
    // 排队项执行时激活身份已换：作废（written false），内容既不落新仓库也不重写旧仓库
    await expect(second).resolves.toEqual({ written: false, content: "排队的旧正文" });
    expect(stubB.files.get("已有.md")).toBeUndefined();
  });

  it("空间错误形态（SpaceApiError）写失败：会话错误状态可见、挂起输入保留", async () => {
    const sessionStore = await import("./noteSessionStore");
    const { SpaceApiError } = await import("@/services/space/client");
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    stub.backend.writeNote = () =>
      Promise.reject(
        new SpaceApiError({
          message: "协作服务器 http://s 返回 403：无编辑权限",
          status: 403,
          code: "http",
          serverMessage: "无编辑权限",
          url: "http://s/api/spaces/SP/file",
        }),
      );
    const session = sessionStore.noteSurfaceProvider.open("已有.md");
    session.applyBody("会失败的正文");
    await vi.waitFor(() => expect(session.getState().error).toBe(true), { timeout: 3000 });
    expect(noteStore.useNoteStore.getState().pendingNoteContent["已有.md"]).toBe("会失败的正文");
    consoleSpy.mockRestore();
  });
});
