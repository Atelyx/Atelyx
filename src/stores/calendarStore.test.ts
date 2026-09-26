/**
 * 日历 store 写盘契约测试（stores/calendarStore.ts）。
 *
 * 写盘按「已加载仓库身份键」归属校验 + 脏门控：手动日程 CRUD 每次都调度防抖写盘，
 * 但内存日程与上次落盘一致时不得写盘（同一值重复提交、切面板重挂都会重复调度）。
 * 身份判别用身份键（identityKeyOf）：空间模式下 vaultRoot 恒 null，root 比对无法区分空间 A/B。
 * 共享日历（协作空间团队 meta）：meta-changed 帧驱动远端刷新——本地干净才采纳磁盘真源，
 * 本地脏（未落盘改动/写盘在途）忽略推送，自回声幂等。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => {
  const state = {
    /** `.atelyx/calendar.json` 磁盘内容（readVaultFile 返回）。 */
    disk: "",
    /** writeVaultFile 调用次数与载荷。 */
    writes: [] as Array<{ file: string; content: string }>,
    gate: null as null | {
      armed: Promise<void>;
      resolveArmed: () => void;
      release: Promise<void>;
      resolveRelease: () => void;
    },
    /** metadata 层模式：local = 本地文件语义；space = 团队 meta 语义。 */
    mode: "local" as "local" | "space",
    /** 团队 meta 键值（space 模式的磁盘真源）。 */
    teamValues: {} as Record<string, string>,
    /** space 模式写盘载荷记录。 */
    spaceWrites: [] as string[],
    /** 读日历失败注入（同步共享场景的网络抖动）。 */
    readFail: false,
    /** 挂起下一次 readCalendarRaw（在途读模拟）。 */
    metaGate: null as null | {
      armed: Promise<void>;
      resolveArmed: () => void;
      release: Promise<void>;
      resolveRelease: () => void;
    },
  };
  return {
    state,
    /** 挂起下一次 list_dated_notes（在途读模拟）。 */
    gateDatedNotes() {
      let resolveArmed!: () => void;
      const armed = new Promise<void>((r) => (resolveArmed = r));
      let resolveRelease!: () => void;
      const release = new Promise<void>((r) => (resolveRelease = r));
      state.gate = { armed, resolveArmed, release, resolveRelease };
    },
    async waitGateArmed() {
      await h.state.gate?.armed;
    },
    releaseGate() {
      h.state.gate?.resolveRelease();
      h.state.gate = null;
    },
    /** 挂起下一次 readCalendarRaw（共享日历在途读模拟）。 */
    gateMetaRead() {
      let resolveArmed!: () => void;
      const armed = new Promise<void>((r) => (resolveArmed = r));
      let resolveRelease!: () => void;
      const release = new Promise<void>((r) => (resolveRelease = r));
      state.metaGate = { armed, resolveArmed, release, resolveRelease };
    },
    async waitMetaArmed() {
      await h.state.metaGate?.armed;
    },
    releaseMetaRead() {
      h.state.metaGate?.resolveRelease();
      h.state.metaGate = null;
    },
  };
});

vi.mock("@/services/metadata", () => ({
  readCalendarRaw: async () => {
    // 门控读：发起时刻捕获磁盘快照、释放后返回快照——模拟「读发起早于后续写入」的在途旧值
    let captured: string | null = null;
    if (h.state.metaGate) {
      const gate = h.state.metaGate;
      captured = h.state.mode === "space" ? (h.state.teamValues["calendar"] ?? null) : h.state.disk || null;
      gate.resolveArmed();
      await gate.release;
    }
    if (h.state.readFail) throw new Error("网络失败");
    if (h.state.metaGate) return captured;
    if (h.state.mode === "space") return h.state.teamValues["calendar"] ?? null;
    return h.state.disk || null;
  },
  writeCalendarRaw: async (content: string) => {
    if (h.state.mode === "space") {
      h.state.spaceWrites.push(content);
      h.state.teamValues["calendar"] = content;
      return;
    }
    h.state.writes.push({ file: ".atelyx/calendar.json", content });
    h.state.disk = content;
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const a = args ?? {};
    switch (cmd) {
      case "read_vault_file":
        if (!h.state.disk) throw new Error("文件不存在");
        return h.state.disk;
      case "write_vault_file":
        h.state.writes.push({ file: String(a.file), content: String(a.content) });
        h.state.disk = String(a.content);
        return null;
      case "list_dated_notes": {
        if (h.state.gate) {
          const gate = h.state.gate;
          gate.resolveArmed();
          return gate.release.then(() => []);
        }
        return [];
      }
      default:
        return null;
    }
  },
}));

type CalendarStore = typeof import("./calendarStore");
type AppStore = typeof import("./appStore");
type CollabHost = typeof import("@/utils/collabHost");

let calendar: CalendarStore;
let app: AppStore;
/** 与被测 store 同一模块实例（vi.resetModules 后动态引入）：帧分发改到同一注册表。 */
let collabHost: CollabHost;

/** 激活身份 root 并加载。 */
async function activate(root: string): Promise<void> {
  app.useAppStore.setState({ vaultIdentity: { kind: "local", root }, vaultRoot: root });
}

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  h.state.disk = "";
  h.state.writes = [];
  h.state.gate = null;
  h.state.mode = "local";
  h.state.teamValues = {};
  h.state.spaceWrites = [];
  h.state.readFail = false;
  h.state.metaGate = null;
  app = await import("./appStore");
  collabHost = await import("@/utils/collabHost");
  await activate("v1");
  calendar = await import("./calendarStore");
});

/** 落定防抖窗口，触发一次写盘。 */
async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(500);
}

/** 排空微任务（帧分发的 void 异步处理落定）。 */
async function flush(): Promise<void> {
  await vi.advanceTimersByTimeAsync(0);
  await vi.advanceTimersByTimeAsync(0);
}

/** 模拟服务端 meta-changed 帧到达（经同一模块实例的注册表分发）。 */
function dispatchCollab(key: string): void {
  collabHost.dispatchCollabMetaChanged(key);
}

describe("日历日程写盘脏门控", () => {
  it("加载后未改动不写盘", async () => {
    h.state.disk = JSON.stringify({ schema: "atelyx-calendar/v1", items: [] });
    await calendar.useCalendarStore.getState().load();

    await settle();

    expect(h.state.writes).toHaveLength(0);
  });

  it("改一项只写一次", async () => {
    await calendar.useCalendarStore.getState().load();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "日程");
    await settle();

    expect(h.state.writes).toHaveLength(1);
    expect(h.state.writes[0].file).toBe(".atelyx/calendar.json");
  });

  it("同一值重复提交不产生第二次写盘", async () => {
    await calendar.useCalendarStore.getState().load();
    const id = "fixed-id";
    calendar.useCalendarStore.setState({
      items: [{ id, date: "2026-01-01", title: "日程", createdAt: 1 }],
    });
    calendar.useCalendarStore.getState().updateItem(id, { title: "改名" });
    await settle();
    expect(h.state.writes).toHaveLength(1);
    // 再提交同样的值：内存内容与落盘基线一致 → 不再写
    calendar.useCalendarStore.getState().updateItem(id, { title: "改名" });
    await settle();

    expect(h.state.writes).toHaveLength(1);
  });

  it("切仓库重置基线：新仓库首次改动照常写盘", async () => {
    await calendar.useCalendarStore.getState().load();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "第一仓库");
    await settle();
    expect(h.state.writes).toHaveLength(1);

    await activate("v2");
    await calendar.useCalendarStore.getState().load();
    calendar.useCalendarStore.getState().addItem("2026-02-02", "第二仓库");
    await settle();

    expect(h.state.writes).toHaveLength(2);
  });
});

describe("load 的在途读竞态守卫（身份键语义）", () => {
  it("在途读 + 切换身份：旧仓库读取结果被丢弃", async () => {
    h.gateDatedNotes();
    const loading = calendar.useCalendarStore.getState().load();
    await h.waitGateArmed();
    // 读取在途期间切到 v2（空间 A→B 场景下 root 恒 null，只有身份键能判别）
    await activate("v2");
    h.releaseGate();
    await loading;

    const s = calendar.useCalendarStore.getState();
    // 旧仓库（v1）的加载结果不得落地：loadedFor 未置位、日程未写入
    expect(s.loadedFor).toBeNull();
    expect(s.items).toEqual([]);
  });
});

describe("共享日历远端刷新（meta-changed 帧）", () => {
  const schema = "atelyx-calendar/v1";

  /** 激活协作空间身份并加载（磁盘真源 = h.state.teamValues["calendar"]）；
   *  注册帧接线（产线由 builtin.calendar 的 collabWiring 注册，此处同口径装配）。 */
  async function activateSpaceAndLoad(): Promise<void> {
    h.state.mode = "space";
    app.useAppStore.setState({
      vaultIdentity: { kind: "space", serverUrl: "http://s", spaceId: "sp1" },
      vaultRoot: null,
    });
    calendar.registerCalendarCollabWiring();
    await calendar.useCalendarStore.getState().load();
  }

  const item = (id: string, title: string) => ({ id, date: "2026-01-01", title, createdAt: 1 });

  it("本地干净时收到推送：采纳磁盘真源，不产生写盘", async () => {
    await activateSpaceAndLoad();
    h.state.teamValues["calendar"] = JSON.stringify({ schema, items: [item("a", "远端新增")] });

    dispatchCollab("calendar");
    await flush();

    const s = calendar.useCalendarStore.getState();
    expect(s.items).toEqual([item("a", "远端新增")]);
    expect(h.state.spaceWrites).toHaveLength(0);
  });

  it("自回声幂等：推送内容与落盘基线一致时不重渲染", async () => {
    await activateSpaceAndLoad();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "本地");
    await settle();
    const before = calendar.useCalendarStore.getState().items;

    // 服务端内容 = 本端刚写盘的内容（自回声）
    h.state.teamValues["calendar"] = h.state.spaceWrites[0];
    dispatchCollab("calendar");
    await flush();

    expect(calendar.useCalendarStore.getState().items).toEqual(before);
    expect(h.state.spaceWrites).toHaveLength(1);
  });

  it("本地有未落盘改动时收到推送：忽略，保留本地输入，随后落盘后写者胜", async () => {
    await activateSpaceAndLoad();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "本地输入");
    // 防抖窗口内远端写入并广播
    h.state.teamValues["calendar"] = JSON.stringify({ schema, items: [item("b", "远端")] });
    dispatchCollab("calendar");
    await flush();

    // 本地输入保留（远端快照未采纳），防抖落盘覆盖远端
    expect(calendar.useCalendarStore.getState().items.map((it) => it.title)).toEqual(["本地输入"]);
    await settle();
    expect(h.state.spaceWrites).toHaveLength(1);
    expect(h.state.teamValues["calendar"]).toContain("本地输入");
  });

  it("采纳在途期间产生本地编辑：不采纳（保留编辑），落盘包含编辑", async () => {
    await activateSpaceAndLoad();
    h.state.teamValues["calendar"] = JSON.stringify({ schema, items: [item("a", "远端")] });

    h.gateMetaRead();
    dispatchCollab("calendar");
    await h.waitMetaArmed();
    // 在途读期间用户编辑
    calendar.useCalendarStore.getState().addItem("2026-01-02", "编辑中");
    h.releaseMetaRead();
    await flush();

    expect(calendar.useCalendarStore.getState().items.map((it) => it.title)).toEqual(["编辑中"]);
    await settle();
    expect(h.state.spaceWrites).toHaveLength(1);
    expect(h.state.spaceWrites[0]).toContain("编辑中");
  });

  it("在途读期间编辑并落盘完成：丢弃本次读（旧值不得覆盖刚落盘内容），基线推进后由新帧自愈", async () => {
    await activateSpaceAndLoad();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "本地E");
    await settle();
    expect(h.state.spaceWrites).toHaveLength(1);

    // 读发起时刻磁盘上还是旧远端快照（模拟读发起早于本地落盘到达服务端）
    h.state.teamValues["calendar"] = JSON.stringify({ schema, items: [item("old", "旧远端")] });
    h.gateMetaRead();
    dispatchCollab("calendar");
    await h.waitMetaArmed();
    // 在途读期间：新编辑防抖落盘完成（基线推进，磁盘真源变为含编辑内容）
    calendar.useCalendarStore.getState().addItem("2026-01-02", "编辑落盘");
    await settle();
    expect(h.state.spaceWrites).toHaveLength(2);
    h.releaseMetaRead();
    await flush();

    // 旧读被丢弃：视图保持刚落盘内容，不回退到旧远端快照，也无额外写盘
    expect(calendar.useCalendarStore.getState().items.map((it) => it.title)).toEqual([
      "本地E",
      "编辑落盘",
    ]);
    expect(h.state.spaceWrites).toHaveLength(2);

    // 自愈：基线推进后的新帧（自回声）以干净状态采纳磁盘真源（内容一致 → 无变化）
    dispatchCollab("calendar");
    await flush();
    expect(calendar.useCalendarStore.getState().items.map((it) => it.title)).toEqual([
      "本地E",
      "编辑落盘",
    ]);
  });

  it("读失败保持现状：不清空视图", async () => {
    await activateSpaceAndLoad();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "本地");
    await settle();

    h.state.readFail = true;
    dispatchCollab("calendar");
    await flush();

    expect(calendar.useCalendarStore.getState().items.map((it) => it.title)).toEqual(["本地"]);
  });

  it("非 calendar 键与未加载状态不触发采纳", async () => {
    dispatchCollab("calendar");
    await flush();
    expect(calendar.useCalendarStore.getState().loadedFor).toBeNull();

    await activateSpaceAndLoad();
    h.state.teamValues["calendar"] = JSON.stringify({ schema, items: [item("a", "远端")] });
    dispatchCollab("别的键");
    await flush();
    expect(calendar.useCalendarStore.getState().items).toEqual([]);

    dispatchCollab("calendar");
    await flush();
    expect(calendar.useCalendarStore.getState().items).toEqual([item("a", "远端")]);
  });
});
