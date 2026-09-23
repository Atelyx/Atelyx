/**
 * 笔记正文编辑会话测试（stores/noteSessionStore.ts）。
 * 只覆盖不依赖真实仓库 I/O 的语义：引用计数、提交与订阅、外部修改处理（采纳/保留本地）、
 * 落盘竞态（在途写盘）。
 * 读写 .md 经 Tauri 命令：`read_note` 按调用顺序喂入返回值（缺省读假磁盘），
 * `write_note` 落假磁盘并可按序注入延迟/失败（模拟慢盘与写盘失败的在途窗口）。
 */
import { describe, it, expect, afterEach, beforeEach, vi } from "vitest";
import type { Context } from "@atelyx/cordis";
import type { Kernel } from "@/services/cordis/kernel";

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
  /** history/note 侧文件的 mock 返回（默认空串 = 空历史；veto 回滚用例注入版本）。 */
  historyJson: "",
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
    if (cmd === "read_vault_file" && file.includes("history/note")) {
      return h.historyJson;
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
type SettingsStore = typeof import("./settingsStore");
type CollabStore = typeof import("./collabStore");
type NoteCollabStore = typeof import("./noteCollabStore");

let store: SessionStore;
let noteStore: NoteStore;
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
  h.historyJson = "";
  store = await import("./noteSessionStore");
  noteStore = await import("./noteStore");
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

describe("外部修改处理", () => {
  it("本地有未落盘输入时外部改盘：保留本地输入、不采纳，下一次自动保存按整文件写覆盖磁盘", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("local");

    h.disk["a.md"] = "external";
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    // 外部感知读到磁盘上的 external：与本地正文不同且本地有未落盘输入 → 不采纳
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(session.getState().content).toBe("local");
    // 自动保存照常落盘（无基准检查，本地后写者胜）：磁盘最终 = 本地正文
    expect(h.disk["a.md"]).toBe("local");
    expect(h.writes).toEqual(["local"]);
    expect(noteStore.useNoteStore.getState().noteSaveStates["a.md"]?.state).toBe("saved");
    expect(session.getState().error).toBe(false);
  });

  it("写盘失败留下的挂起输入：重开会话时恢复为本地脏输入并继续写盘", async () => {
    silenceSaveErrorLog();
    const session = store.noteSurfaceProvider.open("a.md");
    h.failWrites = true;
    session.applyBody("未落盘输入");
    await vi.waitFor(() => expect(session.getState().error).toBe(true), { timeout: 3000 });
    store.noteSurfaceProvider.close("a.md");
    // 关闭后挂起输入仍在（切仓库/关窗 flush 用它兜底）
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("未落盘输入");

    h.failWrites = false;
    const again = store.noteSurfaceProvider.open("a.md");
    // 恢复的挂起输入落到盘上，而不是被磁盘内容顶掉后静默丢弃
    await vi.waitFor(() => expect(h.disk["a.md"]).toBe("未落盘输入"), { timeout: 3000 });
    expect(again.getState().content).toBe("未落盘输入");
    expect(again.getState().dirty).toBe(false);
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBeUndefined();
  });

  it("外部改盘且本地无未落盘输入：采纳磁盘内容（静默刷新），不触发写盘", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("local");
    await vi.waitFor(() => expect(h.disk["a.md"]).toBe("local"), { timeout: 3000 });
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false));

    h.disk["a.md"] = "别人改的内容";
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().content).toBe("别人改的内容"));
    expect(session.getState().dirty).toBe(false);
    expect(noteStore.useNoteStore.getState().noteSaveStates["a.md"]?.state).toBe("idle");
    // 采纳不写盘：只落了第一次本地保存
    expect(h.writes).toEqual(["local"]);
    expect(h.disk["a.md"]).toBe("别人改的内容");
  });

  /**
   * 应用内其它写者（画布文本节点 / AI 文件工具 / Rust 链接改写）落盘的内容成为磁盘当前内容：
   * 本地有未落盘输入时磁盘内容不被采纳，本地输入由下一次自动保存按整文件写落盘（后写者胜）。
   */
  it("应用内写者落盘的正文与本地未落盘输入不同：保留本地输入，自动保存按整文件写覆盖磁盘", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("本地未落盘");

    await noteStore.useNoteStore.getState().saveNoteContent("a.md", "应用自写内容");
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(session.getState().content).toBe("本地未落盘");
    expect(h.writes).toEqual(["应用自写内容", "本地未落盘"]);
    expect(h.disk["a.md"]).toBe("本地未落盘");
  });

  /**
   * 磁盘持有的正是本地正文（关窗/切仓库/AI 操作前的挂起输入落盘走的就是这份内容）：内容已在盘上，
   * 基线跟上并清脏即可——这是逐字节的内容事实，不是对写出者的推断，故不再补一次相同写盘。
   */
  it("磁盘内容与本地正文一致：对齐基线并清脏（不再补一次写盘）", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("本地未落盘");
    // 应用内 flush 链把同一份正文写盘（不经会话写盘链，会话基线仍落后）
    await noteStore.useNoteStore.getState().saveNoteContent("a.md", "本地未落盘");
    const writesBefore = h.writes.length;

    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    await vi.waitFor(() => expect(session.getState().dirty).toBe(false));
    expect(session.getState().content).toBe("本地未落盘");
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBeUndefined();
    expect(noteStore.useNoteStore.getState().noteSaveStates["a.md"]?.state).toBe("saved");
    // 内容已在盘上：不再补一次相同写盘
    await new Promise((r) => setTimeout(r, 600));
    expect(h.writes).toHaveLength(writesBefore);
  });

  /**
   * `save()` 不做写盘前磁盘基准比对：磁盘被应用内写者改过（本会话未参与）也照常把本地正文写盘，
   * 结果 = 本地后写者胜（不会因基准不符拒绝落盘）。
   */
  it("save 不做写盘前磁盘基准检查：磁盘被应用内写者改过也照常写盘", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("本地未落盘");
    // 应用内另一次写入改动了磁盘，会话的 lastSaved 未参与其中
    await noteStore.useNoteStore.getState().saveNoteContent("a.md", "应用内另一次写入");

    // 防抖落点：无基准比对，本地正文直接落盘覆盖磁盘
    await vi.waitFor(() => expect(h.disk["a.md"]).toBe("本地未落盘"), { timeout: 3000 });
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(noteStore.useNoteStore.getState().noteSaveStates["a.md"]?.state).toBe("saved");
    expect(session.getState().error).toBe(false);
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
   * 写盘在途 + 继续输入 + 外部同时变化（三者交叉）：无写盘许可可取消，排在慢写之后的本地写盘
   * 照常落盘，磁盘最终 = 本地最新正文（本地后写者胜，外部那次改动被覆盖）。
   */
  it("写盘在途期间外部改盘：本地未落盘输入不采纳磁盘，排队的写盘照常落盘", async () => {
    const session = store.noteSurfaceProvider.open("a.md");
    h.writeDelays = [900, 0];
    session.applyBody("本地第一版");
    await vi.waitFor(() => expect(h.writeStarted).toHaveLength(1), { timeout: 3000 });
    // 继续输入 → 第二次保存（排在慢写之后）
    session.applyBody("本地第二版");
    // 等第二次保存已发起并排在队列里（第一次写盘仍在途）
    await new Promise((r) => setTimeout(r, 750));

    h.disk["a.md"] = "外部内容";
    noteStore.useNoteStore.getState().markNoteExternallyEdited("a.md");

    // 两次写盘依次落地（第二次不作废），磁盘 = 本地最新正文
    await vi.waitFor(() => expect(h.writes).toEqual(["本地第一版", "本地第二版"]), {
      timeout: 3000,
    });
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(session.getState().content).toBe("本地第二版");
    expect(h.disk["a.md"]).toBe("本地第二版");
    expect(noteStore.useNoteStore.getState().noteSaveStates["a.md"]?.state).toBe("saved");
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
    // 等两次在途写盘依次收尾，避免迟到结果落到下一个用例
    await vi.waitFor(() => expect(h.writes).toEqual(["本地第一版", "本地第二版"]), {
      timeout: 3000,
    });
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(h.disk["a.md"]).toBe("本地第二版");
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

describe("保存前钩子（note:before-save serial veto/改写）", () => {
  let kernel: Kernel | null = null;

  /** 挂载一个注册 note:before-save 监听器的插件（apply 自定），返回卸载回调。 */
  async function mountSaveHook(apply: (ctx: Context) => void): Promise<() => Promise<void>> {
    const { createKernel } = await import("@/services/cordis/kernel");
    const { setKernelRef } = await import("@/services/cordis/events");
    const { mountPlugin, unmountAll } = await import("@/services/cordis/loader");
    kernel = createKernel();
    setKernelRef(kernel);
    const result = await mountPlugin(kernel, { id: "com.test.save-hook", apply });
    expect(result.ok).toBe(true);
    return () => unmountAll(kernel!);
  }

  afterEach(async () => {
    if (kernel) {
      const { unmountAll } = await import("@/services/cordis/loader");
      await unmountAll(kernel);
      const { setKernelRef } = await import("@/services/cordis/events");
      setKernelRef(null);
      kernel.dispose();
      kernel = null;
    }
  });

  it("改写落盘内容：磁盘与返回 = 改写后；会话基线跟随实际落盘内容", async () => {
    const unmount = await mountSaveHook((ctx) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", (p) => ({ content: `改写:${p.content}` })),
      );
    });
    // 会话保存：磁盘 = 改写后，会话基线跟随（lastSaved = 实际落盘内容）
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("正文");
    await vi.waitFor(() => expect(h.disk["a.md"]).toBe("改写:正文"), { timeout: 3000 });

    // 二次编辑保存：磁盘 = 第二次改写后的内容（基线跟随落盘内容，不与磁盘分歧）
    session.applyBody("正文 v2");
    await vi.waitFor(() => expect(h.disk["a.md"]).toBe("改写:正文 v2"), { timeout: 3000 });
    await vi.waitFor(() => expect(session.getState().dirty).toBe(false), { timeout: 3000 });
    expect(h.disk["a.md"]).toBe("改写:正文 v2");
    await unmount();
  });

  it("veto 保存：不落盘、不写内容缓存、返回 written:false；挂起输入保留待重试", async () => {
    await mountSaveHook((ctx) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", () => ({ veto: true })),
      );
    });
    const result = await noteStore.useNoteStore.getState().saveNoteContent("a.md", "正文");
    expect(result.written).toBe(false);
    expect(result.content).toBe("正文");
    expect(h.writes).toEqual([]);
    // veto 在缓存先行之前：未落盘内容不得进缓存（否则重开会话会把它当磁盘基线）
    expect(noteStore.useNoteStore.getState().noteContents["a.md"]).toBeUndefined();
    // 会话路径：内容保留在挂起输入，可再次触发保存
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("正文");
    await vi.waitFor(() =>
      expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("正文"),
      { timeout: 3000 },
    );
  });

  it("监听器抛错：异常隔离，保存照常（原内容落盘）", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    await mountSaveHook((ctx) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", () => {
          throw new Error("插件处理失败");
        }),
      );
    });
    const result = await noteStore.useNoteStore.getState().saveNoteContent("a.md", "正文");
    expect(result).toEqual({ written: true, content: "正文" });
    expect(h.writes).toEqual(["正文"]);
    errorSpy.mockRestore();
  });

  it("veto 时 flushPendingNotes 保留挂起输入（内容未落盘不清除）", async () => {
    await mountSaveHook((ctx) => {
      ctx.effect(() => ctx.events.on("note:before-save", () => ({ veto: true })));
    });
    // 注入 noteList 让 flush 走真实写盘链（veto 分支）；否则文件已删守卫会在写盘前跳过
    const { useVaultStore } = await import("./vaultStore");
    useVaultStore.setState({ noteList: [{ name: "a", file: "a.md" }] });
    noteStore.useNoteStore.getState().setPendingNoteContent("a.md", "正文");

    await noteStore.useNoteStore.getState().flushPendingNotes();

    expect(h.writes).toEqual([]);
    // veto 后内容未落盘：挂起输入必须保留（否则关窗/切仓库时被当已落盘清掉 → 内容丢失）
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("正文");
  });

  it("veto 时 noteHistoryRollback 返回 null，不记 restore 版本、不落盘", async () => {
    await mountSaveHook((ctx) => {
      ctx.effect(() => ctx.events.on("note:before-save", () => ({ veto: true })));
    });
    h.historyJson = JSON.stringify({
      versions: [
        {
          seq: 1,
          ts: 1,
          author: { id: "", name: "", device: "" },
          action: "edit",
          content: "旧版",
        },
      ],
    });

    const result = await noteStore.useNoteStore.getState().noteHistoryRollback("a.md", 1);

    expect(result).toBeNull();
    expect(h.writes).toEqual([]);
  });

  it("veto 时 closeSession（flushPending）不清除挂起输入（未落盘内容保留）", async () => {
    await mountSaveHook((ctx) => {
      ctx.effect(() => ctx.events.on("note:before-save", () => ({ veto: true })));
    });
    const session = store.noteSurfaceProvider.open("a.md");
    session.applyBody("正文");
    await vi.waitFor(() =>
      expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("正文"),
      { timeout: 3000 },
    );

    // 关会话触发 flushPending 落盘挂起输入：veto 后不得清挂起（会话已关，清了 = 未落盘内容丢失）
    store.noteSurfaceProvider.close("a.md");
    await vi.waitFor(() => expect(store.noteSurfaceProvider.get("a.md")).toBeNull());
    await vi.waitFor(() => expect(h.writes).toEqual([]), { timeout: 3000 });
    expect(noteStore.useNoteStore.getState().pendingNoteContent["a.md"]).toBe("正文");
  });

  it("卸载插件后钩子不再生效：保存恢复原样", async () => {
    const unmount = await mountSaveHook((ctx) => {
      ctx.effect(() =>
        ctx.events.on("note:before-save", (p) => ({ content: `改写:${p.content}` })),
      );
    });
    expect(
      (await noteStore.useNoteStore.getState().saveNoteContent("a.md", "正文")).content,
    ).toBe("改写:正文");

    await unmount();
    const result = await noteStore.useNoteStore.getState().saveNoteContent("a.md", "正文");
    expect(result).toEqual({ written: true, content: "正文" });
    expect(h.writes).toEqual(["改写:正文", "正文"]);
  });
});
