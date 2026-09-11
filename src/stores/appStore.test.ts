/**
 * 画布 CRUD 失败契约测试（stores/appStore.ts）。
 * 新建/重命名/删除画布失败一律 reject，调用方据此提示失败；
 * 失败若被静默成「期望标题 / null」，UI 会把失败当成功（磁盘仍是旧名或什么都没建）。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({ failing: new Set<string>() }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string) => {
    if (h.failing.has(cmd)) throw new Error(`${cmd} failed`);
    if (cmd === "create_canvas_vault") return { id: "c9", file: "新画布.atlx" };
    if (cmd === "list_canvases_vault") return [];
    return "";
  },
}));

type AppStore = typeof import("./appStore");
type NotificationStore = typeof import("./notificationStore");

/** 画布行占位（列表为空时调用方按 file 定位的兜底形态）。 */
const row = { id: "c1", file: "画布.atlx", title: "画布", updatedAt: 0 };

let app: AppStore;
let notifications: NotificationStore;

beforeEach(async () => {
  vi.resetModules();
  h.failing = new Set();
  await import("./noteSessionStore");
  await import("./pluginStore");
  app = await import("./appStore");
  notifications = await import("./notificationStore");
});

describe("画布 CRUD 失败契约", () => {
  it("createCanvas 失败 reject（失败信号必须显式，不得返回伪成功值）", async () => {
    h.failing = new Set(["create_canvas_vault"]);
    await expect(app.useAppStore.getState().createCanvas("新画布", "")).rejects.toThrow();
  });

  it("renameCanvas 失败 reject（不得返回期望标题冒充成功）", async () => {
    h.failing = new Set(["rename_canvas_vault"]);
    await expect(app.useAppStore.getState().renameCanvas(row, "新名")).rejects.toThrow();
  });

  it("deleteCanvas 失败 reject（不得静默吞错）", async () => {
    h.failing = new Set(["delete_canvas_vault"]);
    await expect(app.useAppStore.getState().deleteCanvas(row)).rejects.toThrow();
  });

  it("createCanvas 成功返回实际 id/file/title（同名去重后的标题）", async () => {
    const created = await app.useAppStore.getState().createCanvas("新画布", "");
    expect(created).toEqual({ id: "c9", file: "新画布.atlx", title: "新画布" });
  });

  it("订阅方同步抛错不影响改名成败，也不给用户报失败", async () => {
    // 事件总线已逐个隔离订阅方异常（见 utils/vaultEvents.test.ts）；此处锁「画布操作成败只由落盘决定」
    const { subscribeVaultEvent } = await import("@/utils/vaultEvents");
    const off = subscribeVaultEvent({
      kind: "canvas:renamed",
      handler: () => {
        throw new Error("订阅方抛错");
      },
    });
    try {
      await expect(app.useAppStore.getState().renameCanvas(row, "新名")).resolves.toBe("新名");
      const levels = notifications.useNotificationStore.getState().items.map((n) => n.level);
      expect(levels).not.toContain("error");
      expect(levels).not.toContain("warning");
    } finally {
      off();
    }
  });

  it("订阅方同步抛错不影响删除成败，也不给用户报失败", async () => {
    const { subscribeVaultEvent } = await import("@/utils/vaultEvents");
    const off = subscribeVaultEvent({
      kind: "canvas:deleted",
      handler: () => {
        throw new Error("订阅方抛错");
      },
    });
    try {
      await expect(app.useAppStore.getState().deleteCanvas(row)).resolves.toBeUndefined();
      const levels = notifications.useNotificationStore.getState().items.map((n) => n.level);
      expect(levels).not.toContain("error");
      expect(levels).not.toContain("warning");
    } finally {
      off();
    }
  });
});
