/**
 * 笔记正文编辑会话测试（stores/noteSessionStore.ts）。
 * 只覆盖不依赖真实仓库 I/O 的语义：引用计数、提交与订阅、外部修改转冲突、冲突跨会话保留。
 * 读写 .md 经 Tauri 命令，这里按调用顺序喂入返回值（`read_note`）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({ reads: [] as string[], failWrites: false, failReads: false }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string) => {
    if (cmd === "read_note") {
      if (h.failReads) throw new Error("read failed");
      return h.reads.shift() ?? "";
    }
    if (h.failWrites) throw new Error("write failed");
    return "";
  },
}));

type SessionStore = typeof import("./noteSessionStore");
type NoteStore = typeof import("./noteStore");
type NotificationStore = typeof import("./notificationStore");
type SettingsStore = typeof import("./settingsStore");
type CollabStore = typeof import("./collabStore");
type NoteCollabStore = typeof import("./noteCollabStore");

let store: SessionStore;
let noteStore: NoteStore;
let notifications: NotificationStore;
let settings: SettingsStore;
let collab: CollabStore;
let noteCollab: NoteCollabStore;

beforeEach(async () => {
  vi.resetModules();
  h.reads = [];
  h.failWrites = false;
  h.failReads = false;
  store = await import("./noteSessionStore");
  noteStore = await import("./noteStore");
  notifications = await import("./notificationStore");
  settings = await import("./settingsStore");
  collab = await import("./collabStore");
  noteCollab = await import("./noteCollabStore");
  // 默认非协作态：协作用例各自打开开关，避免相互影响
  settings.useSettingsStore.setState({ collabEnabled: false });
  collab.useCollabStore.setState({ connected: false, peers: [] });
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

describe("协作态绑定基线", () => {
  /** 打开协作开关并返回会话；reads 队列按 read_note 调用顺序喂入（缓存未命中/直读盘都走它）。 */
  function openCollab(file = "a.md") {
    settings.useSettingsStore.setState({ collabEnabled: true });
    collab.useCollabStore.setState({ connected: true });
    return { session: store.noteSurfaceProvider.open(file), file };
  }

  it("无未落盘输入：直读盘取基线（缓存滞后时以磁盘为准），并从磁盘内容对齐会话", async () => {
    // 缓存已有旧内容（未命中读盘），直读盘返回更新正文
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "缓存旧内容");
    h.reads.push("盘上较新内容");
    const { session } = openCollab();
    await vi.waitFor(() => expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined());
    expect(h.reads).toHaveLength(0); // 直读盘已消费
    await vi.waitFor(() => expect(session.getState().content).toBe("盘上较新内容"));
  });

  it("有未落盘输入：仍以磁盘正文为基线，会话正文按差量写回文档（不回退、不登记为已落盘）", async () => {
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "未落盘新内容");
    h.reads.push("盘上更旧内容");
    const { session } = openCollab();
    // 绑定前先本地输入（会话脏 + 挂起输入）：此时写盘仍在途，直读盘会读到更旧的正文
    session.applyBody("未落盘新内容");
    await vi.waitFor(() => expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined());
    // 基线仍取自磁盘（不跳过读盘），未落盘输入随后按差量写回共享基线
    expect(h.reads).toHaveLength(0);
    await vi.waitFor(() =>
      expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"].ytext.toString()).toBe(
        "未落盘新内容",
      ),
    );
    expect(session.getState().content).toBe("未落盘新内容");
  });

  it("直读盘失败：退回会话正文完成绑定（不阻塞协作）", async () => {
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "会话正文");
    h.failReads = true;
    const { session } = openCollab();
    await vi.waitFor(() => expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined());
    expect(session.getState().content).toBe("会话正文");
  });

  it("磁盘确实为空：以空正文为基线，会话对齐到空（外部清空即权威）", async () => {
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "缓存内容");
    h.reads.push(""); // 直读盘得到一个空文件
    const { session } = openCollab();
    await vi.waitFor(() => expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined());
    await vi.waitFor(() => expect(session.getState().content).toBe(""));
  });

  it("非协作态不建立协作文档", async () => {
    store.noteSurfaceProvider.open("a.md");
    await vi.waitFor(() => expect(store.noteSurfaceProvider.get("a.md")).not.toBeNull());
    expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeUndefined();
  });

  /** 对端更高序基线通告（触发采纳重建）。 */
  async function adoptPeerBaseline(file: string, text: string): Promise<void> {
    const { receiveSyncMessage } = await import("@/services/noteCollab/noteDoc");
    const { baselineIdOf, encodeNoteBaseline } = await import("@/services/noteCollab/frame");
    const tag = { seq: 99, author: "对端", id: baselineIdOf(text) };
    receiveSyncMessage(file, encodeNoteBaseline(tag, text, text), 1);
  }

  it("编辑面打开期间文档被采纳重建：绑定刷新到新文档", async () => {
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "本端正文");
    openCollab();
    await vi.waitFor(() => expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined());
    const before = noteCollab.useNoteCollabStore.getState().bindings["a.md"];
    await adoptPeerBaseline("a.md", "对端新正文");
    const after = noteCollab.useNoteCollabStore.getState().bindings["a.md"];
    expect(after).toBeDefined();
    expect(after).not.toBe(before);
    expect(after.ytext.toString()).toBe("对端新正文");
  });

  it("会话关闭（无编辑面）的保留文档被重建：不凭空复活绑定", async () => {
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "本端正文");
    openCollab();
    await vi.waitFor(() => expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined());
    store.noteSurfaceProvider.close("a.md");
    expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeUndefined();

    // 保留文档仍参与房间收敛（refcount=0 也会采纳更高序基线并重建），但不得生出无编辑面的绑定
    await adoptPeerBaseline("a.md", "对端新正文");
    expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeUndefined();
  });
});
