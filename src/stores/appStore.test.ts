/**
 * appStore 测试：画布 CRUD 失败契约 + 启动与建仓流程。
 *
 * 画布部分：新建/重命名/删除画布失败一律 reject，调用方据此提示失败；
 * 失败若被静默成「期望标题 / null」，UI 会把失败当成功（磁盘仍是旧名或什么都没建）。
 *
 * 启动部分：无仓库时不自动建仓（仓库完全由用户在文件面板以「打开文件夹」添加）；
 * selectVault 失败停留未激活态；切换重入由 switchingVaultRoot 守卫拒绝。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  failing: new Set<string>(),
  calls: [] as { cmd: string; args: Record<string, unknown> }[],
  results: new Map<string, unknown>(),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    h.calls.push({ cmd, args: args ?? {} });
    if (h.failing.has(cmd)) throw new Error(`${cmd} failed`);
    if (h.results.has(cmd)) return h.results.get(cmd);
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
  h.calls = [];
  h.results = new Map();
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

/** 种子全局配置（read_global_config 的磁盘形态）。 */
function seedGlobalConfig(recentVaults: unknown[]): void {
  h.results.set("read_global_config", {
    config: { recentVaults },
    corruptBackup: null,
  });
}

describe("启动与建仓流程", () => {
  it("无最近仓库：不自动建仓、不登记、不自动进入（仓库由用户在文件面板创建/打开）", async () => {
    seedGlobalConfig([]);
    const autoEnterRoot = await app.useAppStore.getState().init();
    expect(autoEnterRoot).toBeNull();
    expect(app.useAppStore.getState().recentVaults).toEqual([]);
    expect(h.calls.some((c) => c.cmd === "ensure_default_vault")).toBe(false);
    expect(h.calls.some((c) => c.cmd === "write_global_config")).toBe(false);
  });

  it("有最近仓库：返回最近仓库 root 供自动进入，不写 global.json", async () => {
    seedGlobalConfig([{ root: "E:/v1", name: "v1", lastOpenedAt: 1 }]);
    const autoEnterRoot = await app.useAppStore.getState().init();
    expect(autoEnterRoot).toBe("E:/v1");
    expect(app.useAppStore.getState().recentVaults[0]?.root).toBe("E:/v1");
    expect(h.calls.some((c) => c.cmd === "write_global_config")).toBe(false);
  });

  it("selectVault 失败：返回 false、停留在未激活态（仓库树据 vaultRoot 判定）", async () => {
    seedGlobalConfig([]);
    h.failing = new Set(["open_vault"]);
    await expect(app.useAppStore.getState().selectVault("E:/gone")).resolves.toBe(false);
    expect(app.useAppStore.getState().vaultRoot).toBeNull();
    // 失败必须用户可见
    const levels = notifications.useNotificationStore.getState().items.map((n) => n.level);
    expect(levels).toContain("error");
  });

  it("selectVault 重入：切换进行中的再次调用直接忽略（不再有整屏加载屏兜底）", async () => {
    seedGlobalConfig([]);
    let releaseOpen!: (v: unknown) => void;
    h.results.set(
      "open_vault",
      new Promise((resolve) => {
        releaseOpen = resolve;
      }),
    );
    const first = app.useAppStore.getState().selectVault("E:/v2");
    // 第一次切换仍在途（open_vault 未返回）→ 重入调用必须立即 false，且不再触发 open_vault
    await expect(app.useAppStore.getState().selectVault("E:/v3")).resolves.toBe(false);
    releaseOpen({ root: "E:/v2", name: "v2", configCorruptBackup: null });
    await expect(first).resolves.toBe(true);
    expect(app.useAppStore.getState().vaultRoot).toBe("E:/v2");
    expect(h.calls.filter((c) => c.cmd === "open_vault")).toHaveLength(1);
    // 切换收尾后守卫解除
    expect(app.useAppStore.getState().switchingVaultRoot).toBeNull();
  });

  it("selectVault：领域 flush 在换 root 之前执行且携带旧仓库 root（防跨仓库写盘）", async () => {
    seedGlobalConfig([]);
    h.results.set("open_vault", { root: "E:/v2", name: "v2", configCorruptBackup: null });
    const lifecycle = await import("@/utils/kernelLifecycle");
    const flushed: (string | null)[] = [];
    const off = lifecycle.registerDomainLifecycle({
      id: "test.flush-order",
      flush: async (ctx) => {
        flushed.push(ctx.vaultRoot);
      },
    });
    try {
      app.useAppStore.setState({ vaultRoot: "E:/v1" });
      await app.useAppStore.getState().selectVault("E:/v2");
      // flush 捕获的是旧 root（若 flush 晚于 set，会话写盘会被新仓库守卫丢弃 → 数据丢失）
      expect(flushed).toEqual(["E:/v1"]);
      expect(app.useAppStore.getState().vaultRoot).toBe("E:/v2");
    } finally {
      off();
    }
  });
});
