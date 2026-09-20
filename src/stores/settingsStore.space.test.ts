/**
 * settingsStore 协作空间分流契约测试（stores/settingsStore.ts 空间分支）。
 *
 * 覆盖：loadVaultConfig 按身份取数（local = config.json + keychain root 条目；
 * space = global.json spaceConfigs + 服务端 team meta + keychain 空间条目）；
 * local→space→local 配置互不串味；persist 空间路径落 spaceConfigs / team meta；
 * 写盘在途期间身份切换不污染新身份的存储。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

// ===== 空间客户端替身 =====

const space = vi.hoisted(() => {
  const state = {
    teamValues: {} as Record<string, string>,
    myValues: {} as Record<string, string>,
    patchSpaceCalls: [] as Array<{ values: Record<string, string> }>,
  };
  function makeClient(_serverUrl: string) {
    return {
      auth: {},
      spaces: {},
      content: {},
      meta: {
        getSpaceMeta: async () => ({ values: { ...state.teamValues } }),
        patchSpaceMeta: async (_spaceId: string, body: { values: Record<string, string> }) => {
          state.patchSpaceCalls.push(body);
          Object.assign(state.teamValues, body.values);
          return {};
        },
        deleteSpaceMeta: async (_spaceId: string, key: string) => {
          delete state.teamValues[key];
        },
        getMyMeta: async () => ({ values: { ...state.myValues } }),
        patchMyMeta: async (_spaceId: string, body: { values: Record<string, string> }) => {
          Object.assign(state.myValues, body.values);
          return {};
        },
        deleteMyMeta: async (_spaceId: string, key: string) => {
          delete state.myValues[key];
        },
      },
    };
  }
  return { state, makeClient };
});

vi.mock("@/services/space/client", () => ({
  createSpaceClient: (_serverUrl: string) => space.makeClient(_serverUrl),
}));

// ===== 本地命令 / global.json / keychain 替身 =====

const h = vi.hoisted(() => {
  const state = {
    /** 个人仓库 .atelyx/config.json 内容（read_vault_config 返回）。 */
    vaultConfig: {} as Record<string, unknown>,
    /** global.json 内容（read_global_config 返回）。 */
    globalConfig: {} as Record<string, unknown>,
    /** space_config_patch 收到的补丁。 */
    spaceConfigPatches: [] as Array<{ serverKey: string; patch: Record<string, unknown> }>,
    /** vault_config_patch 收到的补丁（空间下不得出现）。 */
    vaultPatches: [] as Record<string, unknown>[],
    /** write_global_config 收到的整文件（空间分流不得整文件覆盖）。 */
    globalWrites: [] as Record<string, unknown>[],
    keychain: new Map<string, string>(),
    keyWrites: [] as string[],
    /** 挂起 space_config_patch 应答（在途写守卫测试）。 */
    holdSpacePatch: null as Promise<void> | null,
    releaseSpacePatch: null as (() => void) | null,
  };
  return { state };
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const a = args ?? {};
    switch (cmd) {
      case "read_vault_config":
        return { config: h.state.vaultConfig, corruptBackup: null };
      case "vault_config_patch":
        h.state.vaultPatches.push(a.patch as Record<string, unknown>);
        return null;
      case "read_global_config":
        return { config: h.state.globalConfig, corruptBackup: null };
      case "write_global_config":
        h.state.globalWrites.push(a.config as Record<string, unknown>);
        return null;
      case "space_config_patch":
        h.state.spaceConfigPatches.push({
          serverKey: String(a.serverKey),
          patch: a.patch as Record<string, unknown>,
        });
        if (h.state.holdSpacePatch) await h.state.holdSpacePatch;
        return null;
      case "get_api_key":
        return h.state.keychain.get(`${String(a.vaultRoot)}:${String(a.providerId)}`) ?? "";
      case "set_api_key":
        h.state.keyWrites.push(`${String(a.vaultRoot)}:${String(a.providerId)}`);
        h.state.keychain.set(`${String(a.vaultRoot)}:${String(a.providerId)}`, String(a.key));
        return null;
      case "delete_api_key":
        h.state.keychain.delete(`${String(a.vaultRoot)}:${String(a.providerId)}`);
        return null;
      case "read_prompt_notes":
      case "read_agents":
        return [];
      case "read_folder_colors":
        return {};
      default:
        return null;
    }
  },
}));

type SettingsStore = typeof import("./settingsStore");

let settings: SettingsStore;
let app: typeof import("./appStore");
let factory: typeof import("@/services/content/factory");

const SPACE = { kind: "space" as const, serverUrl: "http://s1", spaceId: "sp1" };
const SERVER_KEY = "http://s1#sp1";
const SPACE_KEYCHAIN = "space:http://s1#sp1";

beforeEach(async () => {
  vi.resetModules();
  space.state.teamValues = {};
  space.state.myValues = {};
  space.state.patchSpaceCalls = [];
  h.state.vaultConfig = {};
  h.state.globalConfig = {};
  h.state.spaceConfigPatches = [];
  h.state.vaultPatches = [];
  h.state.globalWrites = [];
  h.state.keychain = new Map();
  h.state.keyWrites = [];
  h.state.holdSpacePatch = null;
  h.state.releaseSpacePatch = null;
  await import("./noteSessionStore");
  await import("./pluginStore");
  app = await import("./appStore");
  factory = await import("@/services/content/factory");
  settings = await import("./settingsStore");
});

function enterSpace(): void {
  factory.activateContentIdentity(SPACE);
  app.useAppStore.setState({ vaultIdentity: SPACE, vaultRoot: null });
}

function enterLocal(root: string): void {
  factory.activateContentIdentity({ kind: "local", root });
  app.useAppStore.setState({ vaultIdentity: { kind: "local", root }, vaultRoot: root });
}

describe("loadVaultConfig 空间分流", () => {
  it("space = spaceConfigs 本体 + team meta sort/exclusions + keychain 空间条目", async () => {
    enterSpace();
    h.state.globalConfig = {
      spaceConfigs: {
        [SERVER_KEY]: {
          providers: [{ id: "p1", name: "空间供应商", baseUrl: "u", models: [] }],
          model: "m1",
          modelProviderId: "p1",
        },
      },
    };
    space.state.teamValues.sort = JSON.stringify("name-asc");
    space.state.teamValues.exclusions = JSON.stringify(["草稿"]);
    space.state.teamValues["prompt-notes"] = JSON.stringify(["笔记/团队提示词.md"]);
    h.state.keychain.set(`${SPACE_KEYCHAIN}:p1`, "sk-space");

    await settings.useSettingsStore.getState().loadVaultConfig();

    const s = settings.useSettingsStore.getState();
    expect(s.config.providers[0].name).toBe("空间供应商");
    expect(s.config.providers[0].apiKey).toBe("sk-space");
    expect(s.vaultConfig?.model).toBe("m1");
    expect(s.vaultConfig?.fileExplorerSort).toBe("name-asc");
    expect(s.vaultConfig?.excludeFolders).toEqual(["草稿"]);
    expect(s.promptNotes).toEqual(["笔记/团队提示词.md"]);
  });

  it("spaceConfigs 缺失条目 = 默认配置（不报错、可写盘）", async () => {
    enterSpace();
    await settings.useSettingsStore.getState().loadVaultConfig();

    const s = settings.useSettingsStore.getState();
    expect(s.config.providers).toEqual([]);
    await settings.useSettingsStore.getState().addProvider();
    await settings.useSettingsStore.getState().flush();
    expect(h.state.spaceConfigPatches).toHaveLength(1);
  });

  it("local→space→local 配置互不串味（两套预置数据断言）", async () => {
    h.state.vaultConfig = {
      providers: [{ id: "pl", name: "本地供应商", baseUrl: "u", models: [] }],
      model: "local-m",
    };
    h.state.globalConfig = {
      spaceConfigs: {
        [SERVER_KEY]: {
          providers: [{ id: "ps", name: "空间供应商", baseUrl: "u", models: [] }],
          model: "space-m",
        },
      },
    };
    h.state.keychain.set("E:\\v1:pl", "key-local");
    h.state.keychain.set(`${SPACE_KEYCHAIN}:ps`, "key-space");

    enterLocal("E:\\v1");
    await settings.useSettingsStore.getState().loadVaultConfig();
    let s = settings.useSettingsStore.getState();
    expect(s.config.providers.map((p) => p.name)).toEqual(["本地供应商"]);
    expect(s.config.providers[0].apiKey).toBe("key-local");
    expect(s.vaultConfig?.model).toBe("local-m");

    enterSpace();
    await settings.useSettingsStore.getState().loadVaultConfig();
    s = settings.useSettingsStore.getState();
    expect(s.config.providers.map((p) => p.name)).toEqual(["空间供应商"]);
    expect(s.config.providers[0].apiKey).toBe("key-space");
    expect(s.vaultConfig?.model).toBe("space-m");

    enterLocal("E:\\v1");
    await settings.useSettingsStore.getState().loadVaultConfig();
    s = settings.useSettingsStore.getState();
    expect(s.config.providers.map((p) => p.name)).toEqual(["本地供应商"]);
    expect(s.config.providers[0].apiKey).toBe("key-local");
  });
});

describe("persist 空间分流", () => {
  beforeEach(() => {
    enterSpace();
    h.state.globalConfig = {
      spaceConfigs: {
        [SERVER_KEY]: {
          providers: [{ id: "p1", name: "A", baseUrl: "u", models: [] }],
          model: "m1",
          fileExplorerSort: "mtime-desc",
        },
      },
    };
  });

  it("供应商改动落 spaceConfigs（space_config_patch），不整文件写 global.json、不碰本地 config.json", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().updateProvider("p1", { name: "B" });
    await settings.useSettingsStore.getState().flush();

    expect(h.state.spaceConfigPatches).toHaveLength(1);
    expect(h.state.spaceConfigPatches[0].serverKey).toBe(SERVER_KEY);
    expect(h.state.spaceConfigPatches[0].patch).toEqual({
      providers: [{ id: "p1", name: "B", baseUrl: "u", models: [] }],
    });
    expect(h.state.vaultPatches).toHaveLength(0);
    expect(h.state.globalWrites).toHaveLength(0);
  });

  it("排序改动落 team meta（sort 键），不产生 spaceConfigs 补丁", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setFileExplorerSort("name-asc");

    expect(space.state.patchSpaceCalls).toHaveLength(1);
    expect(space.state.patchSpaceCalls[0].values).toEqual({ sort: JSON.stringify("name-asc") });
    expect(h.state.spaceConfigPatches).toHaveLength(0);
  });

  it("keychain 条目按空间身份隔离（space:<serverUrl>#<spaceId>）", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setTavilyKey("tvly-123");

    expect(h.state.keyWrites).toContain(`${SPACE_KEYCHAIN}:search-tavily`);
    expect(h.state.keychain.get(`${SPACE_KEYCHAIN}:search-tavily`)).toBe("tvly-123");
  });

  it("写盘在途期间身份切换：写落旧空间身份，不污染新身份的存储", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    h.state.holdSpacePatch = new Promise<void>((r) => (h.state.releaseSpacePatch = r));

    void settings.useSettingsStore.getState().updateProvider("p1", { name: "B" });
    const flushP = settings.useSettingsStore.getState().flush();
    // 等写盘真正在途（space_config_patch 已收到补丁并被挂起）
    await vi.waitFor(() => expect(h.state.spaceConfigPatches).toHaveLength(1));
    // 写盘在途：切到本地仓库
    enterLocal("E:\\v2");
    h.state.releaseSpacePatch!();
    await flushP;
    // 让后续 keychain 写入落地
    await new Promise((r) => setTimeout(r, 0));

    expect(h.state.spaceConfigPatches).toHaveLength(1);
    expect(h.state.spaceConfigPatches[0].serverKey).toBe(SERVER_KEY);
    // 新仓库身份零写入：无本地 config 补丁、无新身份 keychain 条目
    expect(h.state.vaultPatches).toHaveLength(0);
    expect(h.state.keyWrites.every((k) => !k.startsWith("E:\\v2:"))).toBe(true);
  });
});
