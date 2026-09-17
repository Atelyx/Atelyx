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
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    h.calls.push(cmd);
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
  // 表格图片显示缓存（真实读取一次入缓存）
  await tableImageCache.resolveTableImageUrl(".atelyx/attachments/t1/pic.png");
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
