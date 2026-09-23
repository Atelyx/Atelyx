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
  /** 空间内容后端的树数据/错误与 getTree 调用计数（mock createSpaceClient）。 */
  spaceTree: [] as unknown[],
  spaceTreeError: null as Error | null,
  spaceGetTreeCalls: 0,
  /** 空间内容文件（相对路径 → 文本），供 listCanvases/createCanvas/补丁链读写。 */
  spaceFiles: new Map<string, string>(),
  /** 空间写入记录（PUT /file 载荷）。 */
  spaceWrites: [] as Record<string, unknown>[],
  /** 空间画布补丁记录（POST /patches/canvas 载荷）。 */
  spacePatchBodies: [] as Record<string, unknown>[],
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    h.calls.push({ cmd, args: args ?? {} });
    if (h.failing.has(cmd)) throw new Error(`${cmd} failed`);
    if (h.results.has(cmd)) return h.results.get(cmd);
    if (cmd === "create_canvas_vault") return { id: "c9", file: "新画布.atlx" };
    if (cmd === "list_canvases_vault") return [];
    // keychain 应用秘密：空间会话恢复用（token 条目 → 令牌；user 条目 → 用户 JSON）
    if (cmd === "get_app_secret") {
      const name = String(args?.name ?? "");
      if (name.startsWith("space-token-")) return "tok";
      if (name.startsWith("space-user-")) {
        return JSON.stringify({ userId: "u1", username: "alice", displayName: "Alice" });
      }
      return "";
    }
    return "";
  },
}));

vi.mock("@/services/space/client", () => {
  class SpaceApiError extends Error {
    readonly status: number;
    constructor(status: number, message: string) {
      super(message);
      this.status = status;
    }
  }
  return {
    SpaceApiError,
    createSpaceClient: () => ({
      auth: {
        listDevices: async () => [],
      },
      content: {
        getTree: async () => {
          h.spaceGetTreeCalls += 1;
          if (h.spaceTreeError) throw h.spaceTreeError;
          return h.spaceTree;
        },
        readFile: async (_spaceId: string, path: string) => {
          const content = h.spaceFiles.get(path);
          if (content === undefined) {
            throw new SpaceApiError(404, `文件不存在：${path}`);
          }
          return { content, updatedAt: 100 };
        },
        writeFile: async (_spaceId: string, body: Record<string, unknown>) => {
          h.spaceWrites.push(body);
          h.spaceFiles.set(String(body.path), String(body.content));
          return { updatedAt: 101, path: body.path };
        },
        patchCanvas: async (_spaceId: string, body: Record<string, unknown>) => {
          h.spacePatchBodies.push(body);
          return { updatedAt: 102, file: body.path };
        },
        glob: async (_spaceId: string, body: { pattern: string }) => {
          // 仅测试用 patterns（**/*.atlx 形态）：取最后一段扩展名过滤
          const ext = body.pattern.slice(body.pattern.lastIndexOf("."));
          const paths = [...h.spaceFiles.keys()].filter((p) => p.endsWith(ext));
          return { paths, total: paths.length };
        },
      },
    }),
  };
});

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
  h.spaceTree = [];
  h.spaceTreeError = null;
  h.spaceGetTreeCalls = 0;
  h.spaceFiles = new Map();
  h.spaceWrites = [];
  h.spacePatchBodies = [];
  await import("./noteSessionStore");
  await import("./pluginStore");
  app = await import("./appStore");
  notifications = await import("./notificationStore");
});

/** 种子空间登录态（绕开 restore 的 keychain/网络路径；restored=true 时 restore 幂等跳过）。 */
async function seedSpaceSession(serverUrl = "http://s"): Promise<void> {
  const auth = await import("./spaceAuthStore");
  auth.useSpaceAuthStore.setState({
    restored: true,
    servers: [{ serverUrl, userId: "u1", username: "alice", displayName: "Alice" }],
  });
}

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
    const autoEnter = await app.useAppStore.getState().init();
    expect(autoEnter).toEqual({ kind: "local", root: "E:/v1" });
    expect(app.useAppStore.getState().recentVaults[0]?.root).toBe("E:/v1");
    expect(h.calls.some((c) => c.cmd === "patch_global_config")).toBe(false);
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

/** 种子含协作空间的全局配置。 */
function seedGlobalConfigWithSpaces(spaces: unknown[], spaceServers: string[] = []): void {
  h.results.set("read_global_config", {
    config: { recentVaults: [], spaces, spaceServers },
    corruptBackup: null,
  });
}

describe("boot 空间自动进入", () => {
  it("最近条目为空间且会话失效：静默跳过自动进入、无错误通知", async () => {
    // 无 spaceServers 清单 → restore 无会话可恢复 → getServer 未命中
    seedGlobalConfigWithSpaces([{ serverUrl: "http://s", spaceId: "sp1", name: "空间", openedAt: 200 }]);
    const target = await app.useAppStore.getState().init();
    expect(target).toBeNull();
    const levels = notifications.useNotificationStore.getState().items.map((n) => n.level);
    expect(levels).not.toContain("error");
    // 未发起任何空间内容请求
    expect(h.spaceGetTreeCalls).toBe(0);
  });

  it("最近条目为空间且会话有效：返回空间条目供自动进入", async () => {
    seedGlobalConfigWithSpaces(
      [{ serverUrl: "http://s", spaceId: "sp1", name: "空间", openedAt: 200 }],
      ["http://s"],
    );
    const target = await app.useAppStore.getState().init();
    expect(target).toEqual({
      kind: "space",
      entry: { serverUrl: "http://s", spaceId: "sp1", name: "空间", openedAt: 200 },
    });
  });

  it("本地仓库更新时优先本地：空间更旧不改变自动进入目标", async () => {
    h.results.set("read_global_config", {
      config: {
        recentVaults: [{ root: "E:/v1", name: "v1", lastOpenedAt: 300 }],
        spaces: [{ serverUrl: "http://s", spaceId: "sp1", name: "空间", openedAt: 200 }],
        spaceServers: ["http://s"],
      },
      corruptBackup: null,
    });
    const target = await app.useAppStore.getState().init();
    expect(target).toEqual({ kind: "local", root: "E:/v1" });
  });
});

describe("selectSpace 激活分流", () => {
  const entry = { serverUrl: "http://s", spaceId: "sp1", name: "空间" };

  it("会话无效：返回 need-login、不激活、无空间 I/O、不调 openVault", async () => {
    seedGlobalConfig([]);
    const factory = await import("@/services/content/factory");
    const before = factory.getActiveContentBackend();
    const result = await app.useAppStore.getState().selectSpace(entry);
    expect(result).toBe("need-login");
    expect(app.useAppStore.getState().vaultIdentity).toBeNull();
    expect(app.useAppStore.getState().vaultRoot).toBeNull();
    expect(factory.getActiveContentBackend()).toBe(before);
    expect(h.spaceGetTreeCalls).toBe(0);
    expect(h.calls.some((c) => c.cmd === "open_vault")).toBe(false);
  });

  it("服务端不可达：停留未激活态 + 用户可见通知（不静默）", async () => {
    await seedSpaceSession();
    h.spaceTreeError = new Error("无法连接协作服务器 http://s");
    const factory = await import("@/services/content/factory");
    const before = factory.getActiveContentBackend();
    const result = await app.useAppStore.getState().selectSpace(entry);
    expect(result).toBe("error");
    const levels = notifications.useNotificationStore.getState().items.map((n) => n.level);
    expect(levels).toContain("error");
    expect(app.useAppStore.getState().vaultIdentity).toBeNull();
    expect(factory.getActiveContentBackend()).toBe(before);
  });

  it("成功路径：激活空间身份 + 树与画布列表经空间后端加载 + recentSpaces 置顶落盘", async () => {
    await seedSpaceSession();
    h.spaceTree = [{ name: "a.md", path: "a.md", isDir: false, updatedAt: 1, children: [] }];
    // 磁盘上已有一块画布：selectSpace 的画布列表加载（glob → 逐个读）应把它填进 canvases
    h.spaceFiles.set(
      "画布.atlx",
      JSON.stringify({ schema: 1, id: "c1", title: "画布", nodes: [], edges: [], createdAt: 0, updatedAt: 5 }),
    );
    const result = await app.useAppStore.getState().selectSpace(entry);
    expect(result).toBe("ok");
    expect(app.useAppStore.getState().vaultIdentity).toEqual({
      kind: "space",
      serverUrl: "http://s",
      spaceId: "sp1",
    });
    expect(app.useAppStore.getState().vaultName).toBe("空间");
    expect(app.useAppStore.getState().vaultRoot).toBeNull();
    // 文件树经空间内容后端加载（预检 + loadFiles 共两次 getTree）
    const vaultStore = await import("./vaultStore");
    expect(vaultStore.useVaultStore.getState().tree[0]?.path).toBe("a.md");
    // 画布列表经空间后端 listCanvases 加载（selectSpace 后 canvases 非空）
    expect(app.useAppStore.getState().canvases).toEqual([
      { id: "c1", title: "画布", file: "画布.atlx", updatedAt: 5 },
    ]);
    // 激活后端确为空间后端：再拉一次树走 mock client 而非本地 IPC
    const factory = await import("@/services/content/factory");
    const callsBefore = h.spaceGetTreeCalls;
    await factory.getActiveContentBackend().listTree();
    expect(h.spaceGetTreeCalls).toBe(callsBefore + 1);
    // recentSpaces 置顶 + 落盘 global.json（patch_global_config 补丁携带 spaces）
    expect(app.useAppStore.getState().recentSpaces[0]?.spaceId).toBe("sp1");
    const write = h.calls.filter((c) => c.cmd === "patch_global_config").at(-1);
    const patch = write?.args.patch as { spaces?: { spaceId: string }[] };
    expect(patch.spaces?.[0]?.spaceId).toBe("sp1");
  });

  it("空间内新建画布：createCanvas 落盘空间 → 列表/树刷新 → 增量保存链走空间补丁端点", async () => {
    await seedSpaceSession();
    await app.useAppStore.getState().selectSpace(entry);
    const created = await app.useAppStore.getState().createCanvas("新画布", "");
    // 新建走空间后端（PUT /file 直写最小磁盘 JSON，不经本地 Tauri 命令）
    expect(h.calls.some((c) => c.cmd === "create_canvas_vault")).toBe(false);
    expect(h.spaceWrites).toHaveLength(1);
    const disk = JSON.parse(String(h.spaceWrites[0].content)) as { id: string; title: string };
    expect(disk.title).toBe("新画布");
    expect(created).toEqual({ id: disk.id, file: "新画布.atlx", title: "新画布" });
    // 列表/树已刷新：画布出现在 canvases（updatedAt 取 readFile 响应的文件 mtime）
    expect(app.useAppStore.getState().canvases.map((c) => c.file)).toContain("新画布.atlx");
    // 增量保存链：patchCanvasVault → 空间补丁端点（title 变化随补丁携带）
    const { patchCanvasVault } = await import("@/services/vault");
    const result = await patchCanvasVault({
      file: created.file,
      canvasId: created.id,
      title: "改名",
      nodes: [],
      edges: [],
      messagesByConv: {},
      lastSaved: { nodes: [], edges: [], messagesByConv: {}, title: "新画布" },
    });
    expect(result).toEqual({ updatedAt: 102, file: "新画布.atlx" });
    expect(h.spacePatchBodies).toHaveLength(1);
    expect(h.spacePatchBodies[0].path).toBe("新画布.atlx");
  });
});
