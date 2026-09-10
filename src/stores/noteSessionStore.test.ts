/**
 * 笔记正文编辑会话测试（stores/noteSessionStore.ts）。
 * 只覆盖不依赖真实仓库 I/O 的语义：引用计数、提交与订阅、外部修改转冲突、冲突跨会话保留。
 * 读写 .md 经 Tauri 命令，这里按调用顺序喂入返回值（`read_note`）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({ reads: [] as string[], failWrites: false }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string) => {
    if (cmd === "read_note") return h.reads.shift() ?? "";
    if (h.failWrites) throw new Error("write failed");
    return "";
  },
}));

type SessionStore = typeof import("./noteSessionStore");
type NoteStore = typeof import("./noteStore");
type NotificationStore = typeof import("./notificationStore");

let store: SessionStore;
let noteStore: NoteStore;
let notifications: NotificationStore;

beforeEach(async () => {
  vi.resetModules();
  h.reads = [];
  h.failWrites = false;
  store = await import("./noteSessionStore");
  noteStore = await import("./noteStore");
  notifications = await import("./notificationStore");
});

describe("会话引用计数", () => {
  it("同文件重复打开复用同一会话，引用计数归零才关闭", () => {
    const first = store.noteSurfaceProvider.open("a.md");
    expect(store.noteSurfaceProvider.open("a.md")).toBe(first);
    expect(store.openNoteSessionFiles()).toEqual(["a.md"]);

    store.noteSurfaceProvider.close("a.md");
    expect(store.noteSurfaceProvider.get("a.md")).toBe(first);

    store.noteSurfaceProvider.close("a.md");
    expect(store.noteSurfaceProvider.get("a.md")).toBeNull();
    expect(store.openNoteSessionFiles()).toEqual([]);
  });

  it("关闭全部会话清空注册表（切仓库/插件停用路径）", () => {
    store.noteSurfaceProvider.open("a.md");
    store.noteSurfaceProvider.open("b.md");
    store.closeAllNoteSessions();
    expect(store.openNoteSessionFiles()).toEqual([]);
  });
});

describe("提交与订阅", () => {
  it("applyBody 提交正文、置脏并通知订阅者", () => {
    const session = store.noteSurfaceProvider.open("a.md");
    const listener = vi.fn();
    const off = session.subscribe(listener);

    session.applyBody("hello");
    expect(session.getState().content).toBe("hello");
    expect(session.getState().dirty).toBe(true);
    const notified = listener.mock.calls.length;
    expect(notified).toBeGreaterThan(0);

    off();
    session.applyBody("hello again");
    expect(listener).toHaveBeenCalledTimes(notified);
  });
});

describe("落盘状态", () => {
  it("写盘失败置 error，下次保存成功后复位", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    h.failWrites = true;
    session.applyBody("local");
    await vi.waitFor(() => expect(session.getState().error).toBe(true), { timeout: 3000 });

    h.failWrites = false;
    session.applyBody("local again");
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(session.getState().error).toBe(false);
  });
});

describe("外部修改与冲突", () => {
  it("本地有未落盘编辑时外部改盘：转冲突、提示一次、关会话仍保留冲突标志", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("local");
    expect(session.getState().conflict).toBe(false);

    h.reads.push("external");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().conflict).toBe(true));
    expect(noteStore.useNoteStore.getState().noteConflicts["a.md"]).toBe(true);
    expect(notifications.useNotificationStore.getState().items).toHaveLength(1);

    // 同一冲突再次改盘：不再重复提示（去重集合生效）
    h.reads.push("external-2");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");
    await vi.waitFor(() => expect(h.reads).toHaveLength(0));
    expect(notifications.useNotificationStore.getState().items).toHaveLength(1);

    // 关闭会话不丢冲突与挂起输入：flushPendingNotes 仍会跳过该文件，等用户在笔记面板决策
    store.noteSurfaceProvider.close("a.md");
    expect(noteStore.useNoteStore.getState().noteConflicts["a.md"]).toBe(true);
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("local");
    // 画布节点徽标取会话注册表的冲突集合：会话已关仍为真
    expect(store.noteSurfaceProvider.isConflicted("a.md")).toBe(true);
  });

  it("冲突未决时重开会话恢复本地内容，保留本地后清挂起输入", async () => {
    const first = store.noteSurfaceProvider.open("a.md");
    first.applyBody("local");
    h.reads.push("external");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");
    await vi.waitFor(() => expect(first.getState().conflict).toBe(true));
    store.noteSurfaceProvider.close("a.md");

    // 重开：以磁盘为基准，但未决冲突的本地内容回到会话里，等用户选择
    h.reads.push("external");
    const reopened = store.noteSurfaceProvider.open("a.md");
    await vi.waitFor(() => expect(reopened.getState().content).toBe("local"));
    expect(reopened.getState().conflict).toBe(true);

    reopened.saveLocalOverExternal();
    await vi.waitFor(() =>
      expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBeUndefined(),
    );
    expect(reopened.getState().conflict).toBe(false);
  });
});
