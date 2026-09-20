/**
 * 日历 store 写盘契约测试（stores/calendarStore.ts）。
 *
 * 写盘按「已加载仓库身份键」归属校验 + 脏门控：手动日程 CRUD 每次都调度防抖写盘，
 * 但内存日程与上次落盘一致时不得写盘（同一值重复提交、切面板重挂都会重复调度）。
 * 身份判别用身份键（identityKeyOf）：空间模式下 vaultRoot 恒 null，root 比对无法区分空间 A/B。
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
  };
});

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

let calendar: CalendarStore;
let app: AppStore;

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
  app = await import("./appStore");
  await activate("v1");
  calendar = await import("./calendarStore");
});

/** 落定防抖窗口，触发一次写盘。 */
async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(500);
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
