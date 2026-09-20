/**
 * 切换激活仓库清理总断言（数据边界）。
 *
 * 驱动真实的同步清理点 `notifyVaultLeaving()`（selectVault 在 openVault 后、下一个 await 前调用），
 * 断言上一个仓库的 per-file 状态全部为空：笔记内容缓存 / 挂起输入 / 冲突与保存状态 / 外部修改序号、
 * 笔记撤销栈、协作文档绑定（Y.Doc 上下文）、编辑会话、画布运行时、表格运行时与图片显示缓存、
 * 画布视口交接缓存。任何一类残留都会让新仓库同路径文件串到旧仓库内容（冲突条 / 陈旧正文 / 陈旧图片）。
 *
 * 清理钩子必须由领域 store 在模块加载时自注册：插件停用期间内核照常切仓库，清理不得依赖插件生命周期
 * （与本文件不含任何插件模块互为印证——钩子若仍挂在插件层，本文件的全部断言会失败）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  /** 假磁盘：read_note 读回、write_note 落盘。 */
  disk: {} as Record<string, string>,
  /** 按命令名记录的调用清单（断言图片缓存清理 = 同路径二次解析必须再次发 IPC）。 */
  calls: [] as string[],
  /** 按命令名固定的返回值（open_vault 等需要结构化结果时使用）。 */
  results: new Map<string, unknown>(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    h.calls.push(cmd);
    if (h.results.has(cmd)) return h.results.get(cmd);
    const file = String(args?.file ?? "");
    if (cmd === "read_note") return h.disk[file] ?? "";
    if (cmd === "write_note") {
      h.disk[file] = String(args?.content ?? "");
      return "";
    }
    if (cmd === "read_attachment_data_url") return "data:image/png;base64,QUJD";
    return "";
  },
}));

/** 空间内容后端的传输层替身：树恒为空，成功语义（本文件只关心清理时序，不关心树内容）。 */
vi.mock("@/services/space/client", () => ({
  createSpaceClient: () => ({
    auth: { listDevices: async () => [] },
    content: { getTree: async () => [] },
  }),
}));

type KernelLifecycle = typeof import("@/utils/kernelLifecycle");
type NoteStore = typeof import("./noteStore");
type NoteUndoStore = typeof import("./noteUndoStore");
type NoteSessionStore = typeof import("./noteSessionStore");
type NoteCollabStore = typeof import("./noteCollabStore");
type CanvasStore = typeof import("./canvasStore");
type TableStore = typeof import("./tableStore");
type TableImageCache = typeof import("@/services/tableImageCache");
type ViewHandoff = typeof import("@/services/viewHandoff");

let kernel: KernelLifecycle;
let noteStore: NoteStore;
let noteUndoStore: NoteUndoStore;
let noteSessionStore: NoteSessionStore;
let noteCollabStore: NoteCollabStore;
let canvasStore: CanvasStore;
let tableStore: TableStore;
let tableImageCache: TableImageCache;
let viewHandoff: ViewHandoff;

beforeEach(async () => {
  vi.resetModules();
  h.disk = {};
  h.calls = [];
  h.results = new Map();
  kernel = await import("@/utils/kernelLifecycle");
  // 各领域 store 在模块加载时自注册 onVaultLeaving——导入即完成注册。
  // 次序约束：noteSessionStore 须最先（其模块级 subscribe 依赖 noteStore 完成求值，
  // 先导入可让后续经插件宿主环回进来的再导入命中缓存，见 noteSessionStore.test 同款次序）
  noteSessionStore = await import("./noteSessionStore");
  noteStore = await import("./noteStore");
  noteUndoStore = await import("./noteUndoStore");
  noteCollabStore = await import("./noteCollabStore");
  canvasStore = await import("./canvasStore");
  tableStore = await import("./tableStore");
  tableImageCache = await import("@/services/tableImageCache");
  viewHandoff = await import("@/services/viewHandoff");
});

/** 种子协作空间登录态（绕开 restore 的 keychain/网络路径；restored=true 时 restore 幂等跳过）。 */
async function seedSpaceSession(): Promise<void> {
  const auth = await import("./spaceAuthStore");
  auth.useSpaceAuthStore.setState({
    restored: true,
    servers: [{ serverUrl: "http://s", userId: "u1", username: "alice", displayName: "Alice" }],
  });
}

/** 全部 per-file 状态为空的总断言（与上方逐条用例同口径，供真实切换流程复用）。 */
function assertAllPerFileStateCleared(): void {
  const s = noteStore.useNoteStore.getState();
  expect(s.noteContents).toEqual({});
  expect(s.pendingNoteContent).toEqual({});
  expect(s.noteConflicts).toEqual({});
  expect(s.noteSaveStates).toEqual({});
  expect(s.externalNoteEdits).toEqual({});
  expect(Object.keys(noteUndoStore.useNoteUndoStore.getState().stacks)).toEqual([]);
  expect(noteCollabStore.useNoteCollabStore.getState().bindings).toEqual({});
  expect(noteSessionStore.openNoteSessionFiles()).toEqual([]);
  const c = canvasStore.useCanvasStore.getState();
  expect(c.canvasFile).toBeNull();
  expect(c.dirty).toBe(false);
  expect(c.baseUpdatedAt).toBe(0);
  const t = tableStore.useTableStore.getState();
  expect(t.tableFile).toBeNull();
  expect(t.dirty).toBe(false);
  expect(t.baseUpdatedAt).toBe(0);
  expect(t.saving).toBe(false);
  expect(t.conflictPending).toBe(false);
  expect(viewHandoff.getCachedCanvasViewport("a.atlx")).toBeNull();
}

/** 把全部领域的 per-file 状态种子化（模拟旧仓库内编辑过的现场）。 */
async function seedAllDomains(): Promise<void> {
  // 笔记：内容缓存 + 挂起输入 + 冲突/保存状态 + 外部修改序号
  const note = noteStore.useNoteStore.getState();
  note.stageNoteContent("a.md", "正文");
  note.setPendingNoteContent("a.md", "未落盘输入");
  note.setNoteConflict("a.md", true);
  note.setNoteSaveState("a.md", { state: "edited", loadError: false });
  note.markNoteExternallyEdited("a.md");
  // 笔记撤销栈
  noteUndoStore.useNoteUndoStore.getState().stackOf("a.md");
  // 编辑会话（含运行时表与冲突集合）
  noteSessionStore.noteSurfaceProvider.open("a.md");
  // 协作文档绑定（真实 Y.Doc，离线无网络）
  noteCollabStore.useNoteCollabStore.getState().bind("a.md", "正文", { name: "本端", color: "#000000" });
  // 画布运行时（不同时种 canvasId+canvasFile：临时附件回收是 fire-and-forget I/O，不在本测试断言面）
  canvasStore.useCanvasStore.setState({ canvasFile: "a.atlx", dirty: true, baseUpdatedAt: 5 });
  // 表格运行时
  tableStore.useTableStore.setState({
    tableFile: "a.atb",
    id: "t1",
    dirty: true,
    baseUpdatedAt: 5,
    saving: true,
    conflictPending: true,
  });
  // 表格图片显示缓存（真实读取一次入缓存；空间后端无附件读取，种子失败可容忍——
  // 缓存保持为空不影响「已清」断言）
  await tableImageCache
    .resolveTableImageUrl(".atelyx/attachments/t1/pic.png")
    .catch(() => undefined);
  // 画布视口交接缓存
  viewHandoff.cacheCanvasViewport("a.atlx", { x: 1, y: 2, zoom: 1 });
}

describe("切换激活仓库清理（notifyVaultLeaving 总断言）", () => {
  beforeEach(seedAllDomains);

  it("切换后笔记运行时态全部为空（含外部修改序号）", () => {
    kernel.notifyVaultLeaving();
    const s = noteStore.useNoteStore.getState();
    expect(s.noteContents).toEqual({});
    expect(s.pendingNoteContent).toEqual({});
    expect(s.noteConflicts).toEqual({});
    expect(s.noteSaveStates).toEqual({});
    expect(s.externalNoteEdits).toEqual({});
  });

  it("切换后笔记撤销栈与协作文档绑定为空", () => {
    kernel.notifyVaultLeaving();
    expect(Object.keys(noteUndoStore.useNoteUndoStore.getState().stacks)).toEqual([]);
    expect(noteCollabStore.useNoteCollabStore.getState().bindings).toEqual({});
  });

  it("切换后编辑会话全部关闭", () => {
    kernel.notifyVaultLeaving();
    expect(noteSessionStore.openNoteSessionFiles()).toEqual([]);
  });

  it("切换后画布与表格运行时为空", () => {
    kernel.notifyVaultLeaving();
    const c = canvasStore.useCanvasStore.getState();
    expect(c.canvasFile).toBeNull();
    expect(c.dirty).toBe(false);
    expect(c.baseUpdatedAt).toBe(0);
    const t = tableStore.useTableStore.getState();
    expect(t.tableFile).toBeNull();
    expect(t.dirty).toBe(false);
    expect(t.baseUpdatedAt).toBe(0);
    expect(t.saving).toBe(false);
    expect(t.conflictPending).toBe(false);
  });

  it("切换后同路径图片再次解析必须重新读盘（显示缓存已清）", async () => {
    expect(h.calls.filter((c) => c === "read_attachment_data_url")).toHaveLength(1);
    kernel.notifyVaultLeaving();
    await tableImageCache.resolveTableImageUrl(".atelyx/attachments/t1/pic.png");
    expect(h.calls.filter((c) => c === "read_attachment_data_url")).toHaveLength(2);
  });

  it("切换后画布视口交接缓存为空", () => {
    kernel.notifyVaultLeaving();
    expect(viewHandoff.getCachedCanvasViewport("a.atlx")).toBeNull();
  });
});

/** 图片显示缓存清理断言：切换后同路径图片必须重新发 IPC（缓存已清）。
 *  仅适用于激活后端支持附件读取的场景（local）。 */
async function assertImageCacheCleared(): Promise<void> {
  const callsBefore = h.calls.filter((c) => c === "read_attachment_data_url").length;
  await tableImageCache.resolveTableImageUrl(".atelyx/attachments/t1/pic.png");
  expect(h.calls.filter((c) => c === "read_attachment_data_url").length).toBe(callsBefore + 1);
}

/** 图片显示缓存清理断言（空间目标）：空间后端没有附件读取，再次解析触达后端即拒绝；
 *  缓存若未清会直接命中返回、不会触达后端。 */
async function assertImageCacheClearedForSpace(): Promise<void> {
  await expect(
    tableImageCache.resolveTableImageUrl(".atelyx/attachments/t1/pic.png"),
  ).rejects.toThrow();
}

describe("三向切换清理（selectSpace/selectVault 真实流程）", () => {
  it("local → space：切换后全部 per-file 状态为空", async () => {
    await seedAllDomains();
    await seedSpaceSession();
    const app = await import("./appStore");
    const result = await app.useAppStore.getState().selectSpace({
      serverUrl: "http://s",
      spaceId: "sp1",
      name: "空间",
    });
    expect(result).toBe("ok");
    assertAllPerFileStateCleared();
    await assertImageCacheClearedForSpace();
  });

  it("space → space：切换后全部 per-file 状态为空", async () => {
    await seedSpaceSession();
    const app = await import("./appStore");
    const enter = await app.useAppStore.getState().selectSpace({
      serverUrl: "http://s",
      spaceId: "sp1",
      name: "空间",
    });
    expect(enter).toBe("ok");
    await seedAllDomains();
    const result = await app.useAppStore.getState().selectSpace({
      serverUrl: "http://s",
      spaceId: "sp2",
      name: "空间二",
    });
    expect(result).toBe("ok");
    expect(app.useAppStore.getState().vaultIdentity).toEqual({
      kind: "space",
      serverUrl: "http://s",
      spaceId: "sp2",
    });
    assertAllPerFileStateCleared();
    await assertImageCacheClearedForSpace();
  });

  it("space → local：切换后全部 per-file 状态为空", async () => {
    await seedSpaceSession();
    const app = await import("./appStore");
    h.results.set("open_vault", { root: "E:/v1", name: "v1", configCorruptBackup: null });
    const enter = await app.useAppStore.getState().selectSpace({
      serverUrl: "http://s",
      spaceId: "sp1",
      name: "空间",
    });
    expect(enter).toBe("ok");
    await seedAllDomains();
    h.results.set("open_vault", { root: "E:/v2", name: "v2", configCorruptBackup: null });
    const result = await app.useAppStore.getState().selectVault("E:/v2");
    expect(result).toBe(true);
    expect(app.useAppStore.getState().vaultIdentity).toEqual({ kind: "local", root: "E:/v2" });
    assertAllPerFileStateCleared();
    await assertImageCacheCleared();
  });
});

describe("撕裂窗口身份激活（activateContentIdentity）", () => {
  it("收到空间身份后 getActiveContentBackend 返回空间后端（listTree 走 mock client）", async () => {
    const factory = await import("@/services/content/factory");
    factory.activateContentIdentity({ kind: "space", serverUrl: "http://s", spaceId: "sp1" });
    await expect(factory.getActiveContentBackend().listTree()).resolves.toEqual([]);
  });

  it("本地身份 = localBackend；null = 退出激活回落 localBackend", async () => {
    const factory = await import("@/services/content/factory");
    const { localBackend } = await import("@/services/content/local");
    factory.activateContentIdentity({ kind: "local", root: "E:/v1" });
    expect(factory.getActiveContentBackend()).toBe(localBackend);
    factory.activateContentIdentity(null);
    expect(factory.getActiveContentBackend()).toBe(localBackend);
  });
});
