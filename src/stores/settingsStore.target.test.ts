/**
 * 仓库设置的目标作用域契约测试（stores/settingsStore.ts 的设置会话）。
 *
 * 覆盖：目标是激活仓库时不建会话（写入仍走激活链路）；非激活目标走独立会话读改写
 * （本地 `*_at` 命令、空间按目标身份）；读取失败/只读会话一律拒绝写入且通知可见；
 * keychain 条目按目标身份落点；关闭会话时在途写不丢、之后写入不再落盘。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => {
  const state = {
    /** 本地仓库配置（root → 配置对象）。 */
    configs: {} as Record<string, Record<string, unknown>>,
    agents: {} as Record<string, unknown[]>,
    patchCalls: [] as Array<{ root?: string; patch: Record<string, unknown> }>,
    globalPatches: [] as Array<Record<string, unknown>>,
    spaceConfigPatches: [] as Array<{ serverKey: string; patch: Record<string, unknown> }>,
    keychain: new Map<string, string>(),
    readCalls: [] as string[],

    /** 读取即失败的 root（模拟目录不可达）。 */
    failReadRoots: new Set<string>(),
    /** 挂起 read_vault_config_at（读取在途模拟）。 */
    gate: null as null | {
      armed: Promise<void>;
      resolveArmed: () => void;
      release: Promise<void>;
      resolveRelease: () => void;
    },
  };
  return {
    state,
    gateRead() {
      let resolveArmed!: () => void;
      const armed = new Promise<void>((r) => (resolveArmed = r));
      let resolveRelease!: () => void;
      const release = new Promise<void>((r) => (resolveRelease = r));
      state.gate = { armed, resolveArmed, release, resolveRelease };
    },
    releaseRead() {
      h.state.gate?.resolveRelease();
      h.state.gate = null;
    },
  };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const a = args ?? {};
    switch (cmd) {
      case "read_vault_config":
        return { config: h.state.configs.v1 ?? {}, corruptBackup: null };
      case "read_vault_config_at": {
        const root = String(a.root);
        h.state.readCalls.push(root);
        if (h.state.failReadRoots.has(root)) throw new Error("仓库路径不可达：目录不存在");
        if (h.state.gate) {
          h.state.gate.resolveArmed();
          await h.state.gate.release;
        }
        return { config: h.state.configs[root] ?? {}, corruptBackup: null };
      }
      case "vault_config_patch":
        h.state.patchCalls.push({ patch: a.patch as Record<string, unknown> });
        return null;
      case "vault_config_patch_at":
        h.state.patchCalls.push({
          root: String(a.root),
          patch: a.patch as Record<string, unknown>,
        });
        return null;
      case "space_config_patch":
        h.state.spaceConfigPatches.push({
          serverKey: String(a.serverKey),
          patch: a.patch as Record<string, unknown>,
        });
        return null;
      case "read_global_config":
        return { config: {}, corruptBackup: null };
      case "patch_global_config":
        h.state.globalPatches.push(a.patch as Record<string, unknown>);
        return { config: {}, corruptBackup: null };
      case "read_agents":
        return h.state.agents.v1 ?? [];
      case "read_agents_at":
        return h.state.agents[String(a.root)] ?? [];
      case "write_agents":
        h.state.agents.v1 = a.agents as unknown[];
        return null;
      case "write_agents_at":
        h.state.agents[String(a.root)] = a.agents as unknown[];
        return null;
      case "read_prompt_notes":
      case "read_prompt_notes_at":
        return [];
      case "read_folder_colors":
        return {};
      case "get_api_key":
        return h.state.keychain.get(`${String(a.vaultRoot)}:${String(a.providerId)}`) ?? "";
      case "set_api_key":
        h.state.keychain.set(`${String(a.vaultRoot)}:${String(a.providerId)}`, String(a.key));
        return null;
      case "delete_api_key":
        h.state.keychain.delete(`${String(a.vaultRoot)}:${String(a.providerId)}`);
        return null;
      default:
        return null;
    }
  },
}));

const space = vi.hoisted(() => {
  const state = {
    teamValues: {} as Record<string, string>,
    calls: [] as string[],
    /** 非空时团队元数据写入抛该消息（模拟服务端按角色拒绝）。 */
    failPatch: null as string | null,
  };
  return { state };
});

vi.mock("@/services/space/client", () => ({
  createSpaceClient: () => ({
    auth: {},
    spaces: {},
    content: {},
    meta: {
      getSpaceMeta: async () => {
        space.state.calls.push("getSpaceMeta");
        return { values: { ...space.state.teamValues } };
      },
      patchSpaceMeta: async (_spaceId: string, body: { values: Record<string, string> }) => {
        space.state.calls.push("patchSpaceMeta");
        if (space.state.failPatch) throw new Error(space.state.failPatch);
        Object.assign(space.state.teamValues, body.values);
        return {};
      },
      deleteSpaceMeta: async (_spaceId: string, key: string) => {
        space.state.calls.push("deleteSpaceMeta");
        delete space.state.teamValues[key];
      },
      getMyMeta: async () => ({ values: {} }),
      patchMyMeta: async () => ({}),
      deleteMyMeta: async () => ({}),
    },
  }),
}));

type SettingsStore = typeof import("./settingsStore");
type AppStore = typeof import("./appStore");
type NotificationStore = typeof import("./notificationStore");
type ContentFactory = typeof import("@/services/content/factory");
type VaultSettingsTarget = import("@/types").VaultSettingsTarget;

/** 目标的本地 root（空间目标返回 null），供断言用。 */
function rootOf(target: VaultSettingsTarget | undefined): string | null {
  return target?.kind === "local" ? target.root : null;
}

let settings: SettingsStore;
let app: AppStore;
let notifications: NotificationStore;
let factory: ContentFactory;
let spaceDirectory: typeof import("./spaceDirectoryStore");

const ACTIVE = { kind: "local", root: "v1", name: "当前仓库" } as const;
const OTHER = { kind: "local", root: "v2", name: "另一个仓库" } as const;

beforeEach(async () => {
  vi.resetModules();
  h.state.configs = {};
  h.state.agents = {};
  h.state.patchCalls = [];
  h.state.spaceConfigPatches = [];
  h.state.keychain = new Map();
  h.state.readCalls = [];

  h.state.failReadRoots = new Set();
  h.state.gate = null;
  space.state.teamValues = {};
  space.state.calls = [];
  space.state.failPatch = null;
  await import("./noteSessionStore");
  await import("./pluginStore");
  factory = await import("@/services/content/factory");
  app = await import("./appStore");
  spaceDirectory = await import("./spaceDirectoryStore");
  notifications = await import("./notificationStore");
  notifications.useNotificationStore.setState({ items: [] });
  settings = await import("./settingsStore");
  app.useAppStore.setState({
    vaultRoot: "v1",
    vaultIdentity: { kind: "local", root: "v1" },
    vaultName: "当前仓库",
    recentVaults: [
      { root: "v1", name: "当前仓库", lastOpenedAt: 2 },
      { root: "v2", name: "另一个仓库", lastOpenedAt: 1 },
    ],
    recentSpaces: [],
  });
});

describe("打开仓库设置的目标", () => {
  it("目标是激活仓库：不建会话（编辑的就是激活态与运行时同一份数据）", async () => {
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...ACTIVE });
    expect(settings.useSettingsStore.getState().settingsSession).toBeNull();
    expect(h.state.readCalls).toEqual([]);
  });

  it("非激活本地目标：读取走 *_at 命令，会话带着目标身份", async () => {
    h.state.configs.v2 = { excludeFolders: ["旧夹"], model: "m1" };
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });

    const session = settings.useSettingsStore.getState().settingsSession;
    expect(h.state.readCalls).toEqual(["v2"]);
    expect(session?.loaded).toBe(true);
    expect(rootOf(session?.target)).toBe("v2");
    expect(session?.vaultConfig.excludeFolders).toEqual(["旧夹"]);
    // 激活态不受影响（会话不污染运行时读取的配置）
    expect(settings.useSettingsStore.getState().vaultConfig).toBeNull();
  });

  it("非激活空间按目标身份读取，无需先把该空间设为激活", async () => {
    space.state.teamValues.sort = JSON.stringify("name-desc");
    await settings.useSettingsStore.getState().openVaultSettingsSession({
      kind: "space",
      serverUrl: "http://s2",
      spaceId: "sp2",
      name: "外部空间",
      role: "editor",
    });
    const session = settings.useSettingsStore.getState().settingsSession;
    expect(session?.loaded).toBe(true);
    expect(session?.vaultConfig.fileExplorerSort).toBe("name-desc");
    expect(session?.readOnly).toBe(false);
  });
});

describe("会话写入", () => {
  it("标量配置：内存即时生效 + 按目标 root 落盘（不动激活仓库）", async () => {
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    await settings.useSettingsStore.getState().setExcludeFolders(["草稿"]);
    await settings.useSettingsStore.getState().flush();

    expect(settings.useSettingsStore.getState().settingsSession?.vaultConfig.excludeFolders).toEqual([
      "草稿",
    ]);
    expect(h.state.patchCalls).toEqual([
      { root: "v2", patch: { excludeFolders: ["草稿"] } },
    ]);
  });

  it("providers：内存即时生效 + keychain 条目按目标身份 + 写入落在目标 root", async () => {
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    const id = await settings.useSettingsStore.getState().addProvider();
    await settings.useSettingsStore.getState().updateProvider(id, { apiKey: "sk-1" });
    await settings.useSettingsStore.getState().flush();

    const session = settings.useSettingsStore.getState().settingsSession;
    expect(session?.config.providers.map((p) => p.id)).toEqual([id]);
    expect(h.state.keychain.get(`v2:${id}`)).toBe("sk-1");
    expect(h.state.keychain.has(`v1:${id}`)).toBe(false);
    // providers 以整数组补丁发出（把 key 一并写好，syncKeys 关时由 cleanVaultPatch 剥离）
    const writes = h.state.patchCalls.filter((c) => c.patch.providers !== undefined);
    expect(writes.length).toBeGreaterThan(0);
    expect(writes.every((c) => c.root === "v2")).toBe(true);
  });

  it("Agent 增改按目标落盘（不写激活仓库的 agents.json）", async () => {
    h.state.configs.v2 = {};
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    await settings.useSettingsStore.getState().addAgent();

    expect(h.state.agents.v2?.length).toBeGreaterThan(0);
    expect(h.state.agents.v1).toBeUndefined();
    expect(settings.useSettingsStore.getState().settingsSession?.agents.length).toBe(
      h.state.agents.v2?.length,
    );
  });

  it("目标是激活仓库时写盘仍走激活链路（vault_config_patch，不带 root）", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...ACTIVE });
    await settings.useSettingsStore.getState().setAttachmentFolder("附件");

    expect(settings.useSettingsStore.getState().vaultConfig?.attachmentFolder).toBe("附件");
    expect(h.state.patchCalls).toEqual([{ patch: { attachmentFolder: "附件" } }]);
  });

  it("应用级显示偏好改走 global.json，不落仓库配置", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    await settings.useSettingsStore.getState().setSoftLineBreak(false);
    await settings.useSettingsStore.getState().setInlineTitle(true);

    expect(settings.useSettingsStore.getState().softLineBreak).toBe(false);
    expect(settings.useSettingsStore.getState().inlineTitle).toBe(true);
    expect(h.state.patchCalls).toEqual([]);
    expect(h.state.globalPatches).toEqual([{ softLineBreak: false }, { inlineTitle: true }]);
  });
});

describe("会话的失败与只读路径", () => {
  it("目标读取失败：会话标记失败、写入被拒并通知（不产生补丁）", async () => {
    h.state.failReadRoots.add("v2");
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });

    const session = settings.useSettingsStore.getState().settingsSession;
    expect(session?.loaded).toBe(false);
    expect(session?.error).toContain("仓库路径不可达");

    await settings.useSettingsStore.getState().setExcludeFolders(["草稿"]);
    expect(h.state.patchCalls).toEqual([]);
    expect(settings.useSettingsStore.getState().settingsSession?.vaultConfig.excludeFolders).toBeUndefined();
    expect(
      notifications.useNotificationStore.getState().items.some((n) => n.message.includes("不会保存")),
    ).toBe(true);
  });

  it("只读会话（空间 viewer）：写入被拒并提示，不产生任何写", async () => {
    await settings.useSettingsStore.getState().openVaultSettingsSession({
      kind: "space",
      serverUrl: "http://s2",
      spaceId: "sp2",
      name: "只读空间",
      role: "viewer",
    });
    const session = settings.useSettingsStore.getState().settingsSession;
    expect(session?.readOnly).toBe(true);

    await settings.useSettingsStore.getState().setExcludeFolders(["草稿"]);
    await settings.useSettingsStore.getState().addAgent();
    expect(space.state.calls).not.toContain("patchSpaceMeta");
    expect(h.state.spaceConfigPatches).toEqual([]);
    expect(
      notifications.useNotificationStore.getState().items.some((n) => n.message.includes("查看者")),
    ).toBe(true);
  });

  it("空间目标（editor）：排除夹与默认模型各落自己的团队键", async () => {
    await settings.useSettingsStore.getState().openVaultSettingsSession({
      kind: "space",
      serverUrl: "http://s2",
      spaceId: "sp2",
      name: "外部空间",
      role: "editor",
    });
    await settings.useSettingsStore.getState().setExcludeFolders(["草稿"]);
    await settings.useSettingsStore.getState().setVaultModel({ providerId: "p1", model: "m1" });
    // 会话写盘在串行链上：flush 等所有在途写落地（关窗/切仓库前的 flush 同语义）
    await settings.useSettingsStore.getState().flush();

    expect(space.state.teamValues.exclusions).toBe(JSON.stringify(["草稿"]));
    // 空间 AI 配置按字段落团队元数据（供应商键不因改模型被动），不再落本机 spaceConfigs
    expect(JSON.parse(space.state.teamValues["ai-model"])).toBe("m1");
    expect(JSON.parse(space.state.teamValues["ai-model-provider"])).toBe("p1");
  });
});

describe("空间内的团队层写入", () => {
  /** 进入协作空间（激活态）并加载其配置：内容面身份也要激活（元数据层按身份分流）。 */
  async function enterSpace() {
    factory.activateContentIdentity({ kind: "space", serverUrl: "http://s2", spaceId: "sp2" });
    app.useAppStore.setState({
      vaultRoot: null,
      vaultIdentity: { kind: "space", serverUrl: "http://s2", spaceId: "sp2" },
      vaultName: "团队空间",
    });
    await settings.useSettingsStore.getState().loadVaultConfig();
  }

  it("改 Agent 落团队元数据（所有者/编辑者可写，不再由客户端拒绝）", async () => {
    await enterSpace();
    await settings.useSettingsStore.getState().addAgent();

    const written = JSON.parse(space.state.teamValues.agents) as Array<{
      name: string;
      builtin?: boolean;
    }>;
    expect(written.some((a) => a.name === "新 Agent")).toBe(true);
    // 预置 Agent 随整表落团队层（其他成员读到同一份列表）
    expect(written.some((a) => a.builtin)).toBe(true);
    expect(settings.useSettingsStore.getState().agents.length).toBe(written.length);
  });

  it("写入被服务端拒绝（查看者/掉线）：不产生虚假可编辑态", async () => {
    await enterSpace();
    space.state.failPatch = "需要所有者或编辑者权限";
    const before = settings.useSettingsStore.getState().agents.map((a) => a.id);

    await settings.useSettingsStore.getState().addAgent();

    expect(settings.useSettingsStore.getState().agents.map((a) => a.id)).toEqual(before);
    expect(space.state.teamValues.agents).toBeUndefined();
  });

  it("激活空间 + 查看者：写入口先被拒（不产生虚假已保存态）", async () => {
    await enterSpace();
    spaceDirectory.useSpaceDirectoryStore.setState({
      spacesByServer: {
        "http://s2": [
          { spaceId: "sp2", name: "团队空间", role: "viewer", ownerUserId: "u1", createdAt: 1 },
        ],
      },
    });

    await settings.useSettingsStore.getState().setExcludeFolders(["草稿"]);
    await settings.useSettingsStore.getState().addProvider();
    await settings.useSettingsStore.getState().flush();

    expect(settings.useSettingsStore.getState().vaultConfig?.excludeFolders).toBeUndefined();
    expect(settings.useSettingsStore.getState().config.providers).toEqual([]);
    expect(space.state.calls).not.toContain("patchSpaceMeta");
    expect(
      notifications.useNotificationStore.getState().items.some((n) =>
        n.message.includes("查看者"),
      ),
    ).toBe(true);
  });
});

describe("会话生命周期", () => {
  it("Tavily key（syncKeys 开）：key 随目标仓库配置落盘，不进 keychain", async () => {
    h.state.configs.v2 = { syncKeys: true };
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    await settings.useSettingsStore.getState().setTavilyKey("tvly-x");
    await settings.useSettingsStore.getState().flush();

    const last = h.state.patchCalls.at(-1);
    expect(last?.root).toBe("v2");
    expect((last?.patch.search as { tavilyApiKey?: string }).tavilyApiKey).toBe("tvly-x");
    expect(h.state.keychain.size).toBe(0);
  });

  it("syncKeys 关闭：配置补丁与 keychain 回写同任务按序完成，关闭会话会等完", async () => {
    // 开启状态：明文 key 在目标仓库的配置里
    h.state.configs.v2 = {
      syncKeys: true,
      providers: [{ id: "p1", name: "A", baseUrl: "u", models: [], apiKey: "sk-1" }],
    };
    await settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    // 不等写盘立刻关闭会话：链上的任务必须被等完（否则这次 key 落盘策略切换会丢）
    const toggling = settings.useSettingsStore.getState().setSyncKeys(false);
    await settings.useSettingsStore.getState().closeVaultSettingsSession();
    await toggling;

    const last = h.state.patchCalls.at(-1);
    expect(last?.root).toBe("v2");
    expect(last?.patch.syncKeys).toBe(false);
    // key 从配置剥离（补丁不含 apiKey）并落到目标身份的 keychain 条目
    expect((last?.patch.providers as Array<{ apiKey?: string }>)[0]?.apiKey).toBeUndefined();
    expect(h.state.keychain.get("v2:p1")).toBe("sk-1");
    expect(settings.useSettingsStore.getState().settingsSession).toBeNull();
  });

  it("关闭会话：链上的在途写照常落目标，销毁后的写入回归激活链路", async () => {
    h.gateRead();
    const opening = settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    await h.state.gate?.armed;
    // 读取在途时写入被拒（配置尚未读到，落盘会抹掉磁盘上的真实配置）
    await settings.useSettingsStore.getState().setExcludeFolders(["草稿"]);
    expect(h.state.patchCalls).toEqual([]);

    h.releaseRead();
    await opening;
    // 发起写入后立即关闭：写盘在会话串行链上，关闭会先把它等完（关窗/切仓库前的 flush 同理）
    const writing = settings.useSettingsStore.getState().setExcludeFolders(["草稿"]);
    await settings.useSettingsStore.getState().closeVaultSettingsSession();
    await writing;
    expect(h.state.patchCalls).toEqual([{ root: "v2", patch: { excludeFolders: ["草稿"] } }]);
    expect(settings.useSettingsStore.getState().settingsSession).toBeNull();

    // 会话已销毁：再调写入走激活链路（读到的是激活层的空配置，不产生第二个目标）
    await settings.useSettingsStore.getState().setExcludeFolders(["再改"]);
    expect(h.state.patchCalls.at(-1)).toEqual({ patch: { excludeFolders: ["再改"] } });
  });

  it("读取在途期间换成别的目标：旧结果不装进新会话", async () => {
    h.state.configs.v3 = { model: "m3" };
    h.gateRead();
    const first = settings.useSettingsStore.getState().openVaultSettingsSession({ ...OTHER });
    await h.state.gate?.armed;
    // 第一个会话的读取仍在途：此时换成第三个目标
    h.releaseRead();
    const second = settings.useSettingsStore.getState().openVaultSettingsSession({
      kind: "local",
      root: "v3",
      name: "第三个仓库",
    });
    await Promise.all([first, second]);

    const session = settings.useSettingsStore.getState().settingsSession;
    expect(rootOf(session?.target)).toBe("v3");
    expect(session?.vaultConfig.model).toBe("m3");
  });
});
