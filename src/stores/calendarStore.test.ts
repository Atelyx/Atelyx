/**
 * 日历 store 写盘契约测试（stores/calendarStore.ts）。
 *
 * 写盘按「已加载仓库」归属校验 + 脏门控：手动日程 CRUD 每次都调度防抖写盘，
 * 但内存日程与上次落盘一致时不得写盘（同一值重复提交、切面板重挂都会重复调度）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  /** `.atelyx/calendar.json` 磁盘内容（readVaultFile 返回）。 */
  disk: "",
  /** writeVaultFile 调用次数与载荷。 */
  writes: [] as Array<{ file: string; content: string }>,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const a = args ?? {};
    switch (cmd) {
      case "read_vault_file":
        if (!h.disk) throw new Error("文件不存在");
        return h.disk;
      case "write_vault_file":
        h.writes.push({ file: String(a.file), content: String(a.content) });
        h.disk = String(a.content);
        return null;
      case "list_dated_notes":
        return [];
      default:
        return null;
    }
  },
}));

type CalendarStore = typeof import("./calendarStore");

let calendar: CalendarStore;

beforeEach(async () => {
  vi.resetModules();
  vi.useFakeTimers();
  h.disk = "";
  h.writes = [];
  const app = await import("./appStore");
  app.useAppStore.setState({ vaultId: "v1" });
  calendar = await import("./calendarStore");
});

/** 落定防抖窗口，触发一次写盘。 */
async function settle(): Promise<void> {
  await vi.advanceTimersByTimeAsync(500);
}

describe("日历日程写盘脏门控", () => {
  it("加载后未改动不写盘", async () => {
    h.disk = JSON.stringify({ schema: "atelyx-calendar/v1", items: [] });
    await calendar.useCalendarStore.getState().load();

    await settle();

    expect(h.writes).toHaveLength(0);
  });

  it("改一项只写一次", async () => {
    await calendar.useCalendarStore.getState().load();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "日程");
    await settle();

    expect(h.writes).toHaveLength(1);
    expect(h.writes[0].file).toBe(".atelyx/calendar.json");
  });

  it("同一值重复提交不产生第二次写盘", async () => {
    await calendar.useCalendarStore.getState().load();
    const id = "fixed-id";
    calendar.useCalendarStore.setState({
      items: [{ id, date: "2026-01-01", title: "日程", createdAt: 1 }],
    });
    calendar.useCalendarStore.getState().updateItem(id, { title: "改名" });
    await settle();
    expect(h.writes).toHaveLength(1);
    // 再提交同样的值：内存内容与落盘基线一致 → 不再写
    calendar.useCalendarStore.getState().updateItem(id, { title: "改名" });
    await settle();

    expect(h.writes).toHaveLength(1);
  });

  it("切仓库重置基线：新仓库首次改动照常写盘", async () => {
    await calendar.useCalendarStore.getState().load();
    calendar.useCalendarStore.getState().addItem("2026-01-01", "第一仓库");
    await settle();
    expect(h.writes).toHaveLength(1);

    const app = await import("./appStore");
    app.useAppStore.setState({ vaultId: "v2" });
    await calendar.useCalendarStore.getState().load();
    calendar.useCalendarStore.getState().addItem("2026-02-02", "第二仓库");
    await settle();

    expect(h.writes).toHaveLength(2);
  });
});
