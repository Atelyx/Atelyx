/**
 * 笔记正文编辑会话测试（stores/noteSessionStore.ts）。
 * 只覆盖不依赖真实仓库 I/O 的语义：引用计数、提交与订阅、外部修改转冲突、冲突跨会话保留、
 * 落盘竞态（在途写盘）。
 * 读写 .md 经 Tauri 命令：`read_note` 按调用顺序喂入返回值（缺省读假磁盘），
 * `write_note` 落假磁盘并可按序注入延迟/失败（模拟慢盘与写盘失败的在途窗口）。
 */
import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  reads: [] as string[],
  failWrites: false,
  failReads: false,
  /** 假磁盘（write_note 落盘、read_note 缺省读回）：断言撤销是否真的落盘。 */
  disk: {} as Record<string, string>,
  /** 各次 write_note 发起时的正文（按调用顺序；发起即记录，不受延迟影响）。 */
  writeStarted: [] as string[],
  /** 各次 write_note 完成后的正文（按完成顺序）。 */
  writes: [] as string[],
  /** 各次 read_note 的延迟（毫秒，按调用顺序消费）：构造「读盘在途期间又来一次外部变化」。 */
  readDelays: [] as number[],
  /** 各次 write_note 的延迟（毫秒）与失败计划（按调用顺序消费，缺省 0/false）。 */
  writeDelays: [] as number[],
  writeFails: [] as boolean[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const file = String(args?.file ?? "");
    if (cmd === "read_note") {
      if (h.failReads) throw new Error("read failed");
      const delay = h.readDelays.shift() ?? 0;
      const value = h.reads.shift() ?? h.disk[file] ?? "";
      if (delay) await new Promise((r) => setTimeout(r, delay));
      return value;
    }
    if (cmd === "write_note") {
      const content = String(args?.content ?? "");
      const delay = h.writeDelays.shift() ?? 0;
      const fails = h.writeFails.shift() ?? h.failWrites;
      h.writeStarted.push(content);
      if (delay) await new Promise((r) => setTimeout(r, delay));
      if (fails) throw new Error("write failed");
      h.disk[file] = content;
      h.writes.push(content);
      return "";
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

/** 注入写盘失败的用例：静音期望内的失败日志（断言的是状态与落盘结果，不是日志输出）。 */
function silenceSaveErrorLog(): void {
  vi.spyOn(console, "error").mockImplementation(() => {});
}

afterEach(() => {
  vi.restoreAllMocks();
});

beforeEach(async () => {
  vi.resetModules();
  h.reads = [];
  h.readDelays = [];
  h.failWrites = false;
  h.failReads = false;
  h.disk = {};
  h.writeStarted = [];
  h.writes = [];
  h.writeDelays = [];
  h.writeFails = [];
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
    silenceSaveErrorLog();
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

  /**
   * 应用内其它写者（画布文本节点 / AI 文件工具 / Rust 链接改写）落盘的内容与本地未落盘正文
   * 不同时，与真实外部修改同口径转冲突：磁盘内容看不出「谁写的」，放行等于让尚未落盘的本地
   * 输入静默盖掉刚写进去的正文（无冲突条、无历史、无用户可见信号）。
   */
  it("应用内写者落盘的内容与本地正文不同：转冲突、不静默覆盖", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("本地未落盘");
    expect(session.getState().conflict).toBe(false);

    await noteStore.useNoteStore.getState().saveNoteContent("a.md", "应用自写内容");
    h.reads.push("应用自写内容");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().conflict).toBe(true));
    expect(session.getState().content).toBe("本地未落盘"); // 本地输入保留，等用户决策
    expect(h.disk["a.md"]).toBe("应用自写内容"); // 磁盘上是应用写者的正文，未被覆盖
    expect(noteStore.useNoteStore.getState().noteConflicts["a.md"]).toBe(true);
    expect(notifications.useNotificationStore.getState().items).toHaveLength(1);
  });

  /**
   * 磁盘持有的正是本地正文（关窗/切仓库/AI 操作前的挂起输入落盘走的就是这份内容）：内容已在盘上，
   * 基线跟上并清脏即可——这是逐字节的内容事实，不是对写出者的推断，故不弹冲突条。
   */
  it("磁盘内容与本地正文一致：基线对齐、清脏、不弹冲突条", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("本地未落盘");
    // 应用内 flush 链把同一份正文写盘（不经会话写盘链，会话基线仍落后）
    await noteStore.useNoteStore.getState().saveNoteContent("a.md", "本地未落盘");
    const writesBefore = h.writes.length;

    h.reads.push("本地未落盘");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().dirty).toBe(false));
    expect(session.getState().conflict).toBe(false);
    expect(session.getState().content).toBe("本地未落盘");
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBeUndefined();
    expect(notifications.useNotificationStore.getState().items).toHaveLength(0);
    // 内容已在盘上：不再补一次相同写盘
    await new Promise((r) => setTimeout(r, 600));
    expect(h.writes).toHaveLength(writesBefore);
  });

  it("真正的外部修改仍转冲突（内容比对不得放宽成「一概不冲突」）", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("本地未落盘");
    await noteStore.useNoteStore.getState().saveNoteContent("a.md", "应用自写内容");

    // 外部把磁盘改成别的内容：与本地正文也不相等 → 按外部修改处理
    h.disk["a.md"] = "别人改的内容";
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().conflict).toBe(true));
    expect(noteStore.useNoteStore.getState().noteConflicts["a.md"]).toBe(true);
  });

  /**
   * `save()` 的写前基准只认本会话确认过的落盘内容：磁盘被应用内写者改过（本会话未参与）时
   * 转冲突让用户决策，不得用尚未落盘的本地输入覆盖它。
   */
  it("save 的写前基准不放行「应用内写者刚写的内容」：转冲突而非静默覆盖", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("本地未落盘");
    // 应用内另一次写入改动了磁盘，会话的 lastSaved 未参与其中
    await noteStore.useNoteStore.getState().saveNoteContent("a.md", "应用内另一次写入");

    // 触发会话保存（防抖落点）：写前应发现磁盘 ≠ lastSaved 并转冲突
    await vi.waitFor(() => expect(session.getState().conflict).toBe(true), { timeout: 3000 });
    expect(noteStore.useNoteStore.getState().noteConflicts["a.md"]).toBe(true);
    // 磁盘上仍是应用内写者的内容，未被尚未落盘的本地输入覆盖
    expect(h.disk["a.md"]).toBe("应用内另一次写入");
  });

  it("保留本地写盘失败：冲突态与挂起登记都保留（可重试），不静默当成功", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("local");
    h.reads.push("external");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");
    await vi.waitFor(() => expect(session.getState().conflict).toBe(true));

    silenceSaveErrorLog();
    h.writeFails.push(true);
    session.saveLocalOverExternal();
    await vi.waitFor(() => expect(session.getState().error).toBe(true));

    // 冲突未决 + 挂起输入在册：关窗 flush 仍会跳过该文件，用户可再次选择
    expect(session.getState().conflict).toBe(true);
    expect(noteStore.useNoteStore.getState().noteConflicts["a.md"]).toBe(true);
    expect(session.getState().dirty).toBe(true);
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("local");
    expect(h.writes).toEqual([]);
  });

  it("读盘在途又有新变化：丢弃本次读到的旧快照，不回退视图", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    // 第一次外部变化：读盘慢（在途期间第二次变化到达）
    h.readDelays.push(60);
    h.reads.push("旧快照");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");
    // 第二次：读盘立即返回，先完成并采纳
    h.reads.push("最新内容");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().content).toBe("最新内容"), { timeout: 3000 });
    // 等第一次（延迟）读返回：不得把视图回退到旧快照
    await new Promise((r) => setTimeout(r, 150));
    expect(session.getState().content).toBe("最新内容");
  });

  /**
   * 写盘在途 + 继续输入 + 外部同时变化（三者交叉）：排在慢写之后的本地写盘在排队期间必须被作废，
   * 否则它落地时会覆盖刚被识别出来的外部内容（用户看到冲突条、磁盘却已被本端改写）。
   */
  it("写盘在途期间外部改盘转冲突：排队中的本地写盘作废，不把未落盘输入写下去", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    h.writeDelays = [900, 0];
    session.applyBody("本地第一版");
    await vi.waitFor(() => expect(h.writeStarted).toHaveLength(1), { timeout: 3000 });
    // 继续输入 → 第二次保存（排在慢写之后）
    session.applyBody("本地第二版");
    // 等第二次保存已发起并排在队列里（第一次写盘仍在途）
    await new Promise((r) => setTimeout(r, 750));

    h.disk["a.md"] = "外部内容";
    h.reads.push("外部内容");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");
    await vi.waitFor(() => expect(session.getState().conflict).toBe(true));

    // 等在途写盘落地（只落第一次；排队中的第二次被取消），再做断言——
    // 不等它收尾会让迟到的写盘落到下一个用例的假磁盘上
    await vi.waitFor(() => expect(h.writes).toHaveLength(1), { timeout: 3000 });
    expect(h.writes).not.toContain("本地第二版");
    expect(session.getState().conflict).toBe(true);
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("本地第二版");
  });

  /**
   * 冲突在「写盘已落地」之后被本端写收尾时收口：冲突条此刻已无决策意义（两个按钮都只会读到
   * 本次写入的内容），必须就地清除并提示——否则冲突态会让后续键入不再自动保存、关窗又被
   * flush 跳过，那些键入只留在内存里。
   */
  it("写盘落地后才被置冲突：收尾时清除冲突并提示（不留悬空的冲突条）", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    h.writeDelays = [500];
    session.applyBody("本地正文");
    await vi.waitFor(() => expect(h.writeStarted).toHaveLength(1), { timeout: 3000 });

    // 写盘仍在途时外部改盘（watcher 回波先到）、随后本端写落地
    h.reads.push("外部内容");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");
    await vi.waitFor(() => expect(session.getState().conflict).toBe(true));

    // 写落地后收尾读取的磁盘内容 = 本次写入 → 冲突收口（清冲突 + 提示），状态回到一致
    h.reads.push("本地正文");
    await vi.waitFor(() => expect(session.getState().conflict).toBe(false), { timeout: 3000 });
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(noteStore.useNoteStore.getState().noteConflicts["a.md"]).toBeUndefined();
    expect(h.disk["a.md"]).toBe("本地正文");
    expect(
      notifications.useNotificationStore.getState().items.some((n) => n.message.includes("覆盖")),
    ).toBe(true);
  });

  it("采纳磁盘内容时本端仍有旧写在途：不采纳（避免清脏后正文与磁盘静默分歧）", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    h.writeDelays = [400];
    session.applyBody("本地第一版");
    await vi.waitFor(() => expect(h.writeStarted).toHaveLength(1), { timeout: 3000 });
    session.applyBody("本地第二版");
    // 应用内写者（flush 链）写下与本地正文相同的内容：watcher 回波会看到 disk === current.content
    h.disk["a.md"] = "本地第二版";
    h.reads.push("本地第二版");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    // 在途写盘未收尾前不得清脏（那次写落盘后磁盘会变回更旧内容，清脏就成了静默分歧）
    await new Promise((r) => setTimeout(r, 100));
    expect(session.getState().dirty).toBe(true);
    // 等这次在途写盘收尾，避免迟到结果落到下一个用例
    await vi.waitFor(() => expect(h.writes).toHaveLength(1), { timeout: 3000 });
  });
});

describe("写盘许可（排队期间可取消）", () => {
  it("许可为假：不写盘，且不污染内容缓存", async () => {
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "旧内容");

    const written = await noteStore.useNoteStore
      .getState()
      .saveNoteContent("a.md", "新内容", () => false);

    expect(written).toBe(false);
    expect(h.writes).toEqual([]);
    expect(noteStore.useNoteStore.getState().noteContents["a.md"]).toBe("旧内容");
  });

  it("排队期间许可转假：排在慢写之后的这次写盘被跳过，磁盘保留前一次内容", async () => {
    h.writeDelays = [200, 0];
    const first = noteStore.useNoteStore.getState().saveNoteContent("a.md", "第一次");
    await vi.waitFor(() => expect(h.writeStarted).toHaveLength(1), { timeout: 3000 });
    const second = noteStore.useNoteStore
      .getState()
      .saveNoteContent("a.md", "第二次", () => false);

    expect(await first).toBe(true);
    expect(await second).toBe(false);
    expect(h.writes).toEqual(["第一次"]);
    expect(h.disk["a.md"]).toBe("第一次");
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
    // 采纳正文与直读盘正文一致：只对齐视图，不重复写盘
    expect(h.writes).toEqual([]);
  });

  it("采纳时 frontmatter 以直读盘为准：会话对齐到磁盘全文且不写盘", async () => {
    // 缓存带旧 frontmatter，磁盘 frontmatter 已被外部改过：CRDT 只承载正文，frontmatter 只能来自磁盘
    noteStore.useNoteStore.getState().stageNoteContent("a.md", "---\ntitle: 旧\n---\nhello\n");
    h.reads.push("---\ntitle: 新\n---\nhello\n");
    const { session } = openCollab();
    await vi.waitFor(() => expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined());
    await vi.waitFor(() => expect(session.getState().content).toBe("---\ntitle: 新\n---\nhello\n"));
    expect(h.writes).toEqual([]);
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

describe("在途写盘竞态", () => {
  /** 协作态会话（协作分支不直读盘，基线/在途内容的分歧只由本模块组织）。 */
  function openCollab(file = "a.md", disk = "hello\n") {
    h.disk[file] = disk;
    settings.useSettingsStore.setState({ collabEnabled: true });
    collab.useCollabStore.setState({ connected: true });
    return { session: store.noteSurfaceProvider.open(file), file };
  }

  it("改动后立刻删回原样：撤销后的正文必须落盘，不因上次写盘在途被判为无改动", async () => {
    h.writeDelays = [700, 0];
    const { session } = openCollab();
    await vi.waitFor(() => expect(session.getState().content).toBe("hello\n"));

    session.applyBody("hello world\n");
    // 首次写盘已发起但未落盘（延迟 700ms）：此刻删回原样
    await vi.waitFor(() => expect(h.writeStarted).toHaveLength(1), { timeout: 3000 });
    session.applyBody("hello\n");

    await vi.waitFor(() => expect(h.writes).toHaveLength(2), { timeout: 3000 });
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(h.writes).toEqual(["hello world\n", "hello\n"]);
    expect(h.disk["a.md"]).toBe("hello\n");
    expect(noteStore.useNoteStore.getState().noteSaveStates["a.md"]?.state).not.toBe("error");
  });

  it("写盘失败但内容已被后续输入取代：不置「保存失败」（当前正文另有写盘接管）", async () => {
    silenceSaveErrorLog();
    h.writeDelays = [800, 0];
    h.writeFails = [true];
    const { session } = openCollab();
    await vi.waitFor(() => expect(session.getState().content).toBe("hello\n"));
    const errors: boolean[] = [];
    const off = session.subscribe(() => errors.push(!!session.getState().error));

    session.applyBody("hello world\n");
    await vi.waitFor(() => expect(h.writeStarted).toHaveLength(1), { timeout: 3000 });
    session.applyBody("hello\n");

    // 首次写盘迟到的失败落在撤销之后：撤销后的正文由第二次写盘落盘，全程不得出现「保存失败」
    await vi.waitFor(() => expect(h.writes).toEqual(["hello\n"]), { timeout: 3000 });
    off();
    expect(errors).not.toContain(true);
    expect(noteStore.useNoteStore.getState().noteSaveStates["a.md"]?.state).toBe("saved");
  });

  it("当前正文的写盘失败：如实置「保存失败」并保留挂起登记待重试", async () => {
    silenceSaveErrorLog();
    h.writeFails = [true];
    const { session } = openCollab();
    await vi.waitFor(() => expect(session.getState().content).toBe("hello\n"));

    session.applyBody("hello world\n");
    await vi.waitFor(() => expect(session.getState().error).toBe(true), { timeout: 3000 });
    expect(session.getState().dirty).toBe(true);
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("hello world\n");
  });
});

describe("协作采纳落盘", () => {
  it("采纳对端已前进的正文：磁盘随之收敛，且不被登记为已落盘基线", async () => {
    h.disk["a.md"] = "hello\n";
    settings.useSettingsStore.setState({ collabEnabled: true });
    collab.useCollabStore.setState({ connected: true });
    const session = store.noteSurfaceProvider.open("a.md");
    await vi.waitFor(() => expect(session.getState().content).toBe("hello\n"));
    await vi.waitFor(() =>
      expect(noteCollab.useNoteCollabStore.getState().bindings["a.md"]).toBeDefined(),
    );

    // 保留文档已收到对端帧：ytext 前进到磁盘还没有的正文 → 采纳并按内容变更落盘
    session.handleCollabDivergence("peer content\n");
    await vi.waitFor(() => expect(h.disk["a.md"]).toBe("peer content\n"), { timeout: 3000 });
    expect(h.writes).toEqual(["peer content\n"]);

    // 磁盘已持有该正文：「改一下又删回原样」判无内容可写（不重复写盘），正文不回退
    session.applyBody("peer content\nx");
    session.applyBody("peer content\n");
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(h.writes).toEqual(["peer content\n"]);
    expect(h.disk["a.md"]).toBe("peer content\n");
  });
});
