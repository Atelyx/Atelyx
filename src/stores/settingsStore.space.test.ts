/**
 * settingsStore 协作空间分流契约测试（stores/settingsStore.ts 空间分支）。
 *
 * 覆盖：loadVaultConfig 按身份取数（local = config.json + keychain root 条目；
 * space = 服务端团队元数据：AI 配置按字段分键（含 key）+ sort/exclusions + Agent/提示词）；
 * local→space→local 配置互不串味；persist 空间路径落团队元数据（不写本机 spaceConfigs/keychain、
 * 不碰本地 config.json）；「API key 随仓库保存」在空间无意义（不产生任何写）；
 * 写盘在途期间身份切换不污染新身份的存储。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

// ===== 空间客户端替身 =====

const space = vi.hoisted(() => {
  const state = {
    teamValues: {} as Record<string, string>,
    myValues: {} as Record<string, string>,
    patchSpaceCalls: [] as Array<{ values: Record<string, string> }>,
    /** 挂起 patchSpaceMeta 应答（在途写守卫测试）。 */
    holdPatch: null as Promise<void> | null,
    releasePatch: null as (() => void) | null,
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
          if (state.holdPatch) await state.holdPatch;
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
    /** space_config_patch 收到的补丁（空间 AI 配置已不落本机，正常路径应为 0 条）。 */
    spaceConfigPatches: [] as Array<{ serverKey: string; patch: Record<string, unknown> }>,
    /** vault_config_patch 收到的补丁（空间下不得出现）。 */
    vaultPatches: [] as Record<string, unknown>[],
    /** patch_global_config 收到的补丁（应用级显示偏好落点）。 */
    globalPatches: [] as Record<string, unknown>[],
    keychain: new Map<string, string>(),
    keyWrites: [] as string[],
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
      case "patch_global_config":
        h.state.globalPatches.push(a.patch as Record<string, unknown>);
        return { config: h.state.globalConfig, corruptBackup: null };
      case "write_global_config":
        return null;
      case "space_config_patch":
        h.state.spaceConfigPatches.push({
          serverKey: String(a.serverKey),
          patch: a.patch as Record<string, unknown>,
        });
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
const SPACE_KEYCHAIN = "space:http://s1#sp1";

/** 读团队 AI 配置（按字段分键）。 */
function teamAi(): {
  providers?: Array<Record<string, unknown>>;
  model?: string;
  modelProviderId?: string;
  search?: { tavilyApiKey?: string };
} {
  const read = (key: string): unknown => {
    const raw = space.state.teamValues[key];
    return raw === undefined ? undefined : JSON.parse(raw);
  };
  return {
    providers: read("ai-providers") as Array<Record<string, unknown>> | undefined,
    model: read("ai-model") as string | undefined,
    modelProviderId: read("ai-model-provider") as string | undefined,
    search: read("ai-search") as { tavilyApiKey?: string } | undefined,
  };
}

beforeEach(async () => {
  vi.resetModules();
  space.state.teamValues = {};
  space.state.myValues = {};
  space.state.patchSpaceCalls = [];
  space.state.holdPatch = null;
  space.state.releasePatch = null;
  h.state.vaultConfig = {};
  h.state.globalConfig = {};
  h.state.spaceConfigPatches = [];
  h.state.vaultPatches = [];
  h.state.globalPatches = [];
  h.state.keychain = new Map();
  h.state.keyWrites = [];
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
  it("space = 团队元数据 AI 配置本体（含 key）+ sort/exclusions + 提示词标记", async () => {
    enterSpace();
    space.state.teamValues["ai-providers"] = JSON.stringify([
      { id: "p1", name: "空间供应商", baseUrl: "u", models: [], apiKey: "sk-team" },
    ]);
    space.state.teamValues["ai-model"] = JSON.stringify("m1");
    space.state.teamValues["ai-model-provider"] = JSON.stringify("p1");
    space.state.teamValues.sort = JSON.stringify("name-asc");
    space.state.teamValues.exclusions = JSON.stringify(["草稿"]);
    space.state.teamValues["prompt-notes"] = JSON.stringify(["笔记/团队提示词.md"]);
    // 本机 keychain 里的同名条目不该被采用（空间 key 由团队层承载）
    h.state.keychain.set(`${SPACE_KEYCHAIN}:p1`, "sk-local");

    await settings.useSettingsStore.getState().loadVaultConfig();

    const s = settings.useSettingsStore.getState();
    expect(s.config.providers[0].name).toBe("空间供应商");
    expect(s.config.providers[0].apiKey).toBe("sk-team");
    expect(s.vaultConfig?.model).toBe("m1");
    expect(s.vaultConfig?.fileExplorerSort).toBe("name-asc");
    expect(s.vaultConfig?.excludeFolders).toEqual(["草稿"]);
    expect(s.promptNotes).toEqual(["笔记/团队提示词.md"]);
  });

  it("团队层尚无 AI 配置 = 空配置（不报错、可写盘）", async () => {
    enterSpace();
    await settings.useSettingsStore.getState().loadVaultConfig();

    expect(settings.useSettingsStore.getState().config.providers).toEqual([]);
    await settings.useSettingsStore.getState().addProvider();
    await settings.useSettingsStore.getState().flush();
    expect(space.state.patchSpaceCalls).toHaveLength(1);
    expect(teamAi().providers).toHaveLength(1);
    expect(h.state.spaceConfigPatches).toHaveLength(0);
  });

  it("local→space→local 配置互不串味（两套预置数据断言）", async () => {
    h.state.vaultConfig = {
      providers: [{ id: "pl", name: "本地供应商", baseUrl: "u", models: [] }],
      model: "local-m",
    };
    space.state.teamValues["ai-providers"] = JSON.stringify([
      { id: "ps", name: "空间供应商", baseUrl: "u", models: [], apiKey: "key-space" },
    ]);
    space.state.teamValues["ai-model"] = JSON.stringify("space-m");
    h.state.keychain.set("E:\\v1:pl", "key-local");

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
    space.state.teamValues["ai-providers"] = JSON.stringify([
      { id: "p1", name: "A", baseUrl: "u", models: [], apiKey: "sk-team" },
    ]);
    space.state.teamValues["ai-model"] = JSON.stringify("m1");
    space.state.teamValues.sort = JSON.stringify("mtime-desc");
  });

  it("供应商改动落团队元数据（含 key），不写本机 spaceConfigs 与本地 config.json", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().updateProvider("p1", { name: "B" });
    await settings.useSettingsStore.getState().flush();

    expect(teamAi().providers).toEqual([
      { id: "p1", name: "B", baseUrl: "u", models: [], apiKey: "sk-team" },
    ]);
    expect(h.state.spaceConfigPatches).toHaveLength(0);
    expect(h.state.vaultPatches).toHaveLength(0);
    expect(h.state.keyWrites).toEqual([]);
  });

  it("AI 配置按字段分键：改默认模型只写模型键，供应商键不动", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setVaultModel({ providerId: "p1", model: "m2" });

    const ai = teamAi();
    expect(ai.model).toBe("m2");
    expect(ai.modelProviderId).toBe("p1");
    expect(ai.providers).toEqual([
      { id: "p1", name: "A", baseUrl: "u", models: [], apiKey: "sk-team" },
    ]);
  });

  it("排序改动落 sort 键，不产生 spaceConfigs 补丁", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setFileExplorerSort("name-asc");

    expect(space.state.patchSpaceCalls.at(-1)?.values).toEqual({
      sort: JSON.stringify("name-asc"),
    });
    expect(h.state.spaceConfigPatches).toHaveLength(0);
  });

  it("Tavily key 落团队元数据（不进本机 keychain）", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setTavilyKey("tvly-123");
    await settings.useSettingsStore.getState().flush();

    expect(teamAi().search?.tavilyApiKey).toBe("tvly-123");
    expect(h.state.keyWrites).toEqual([]);
  });

  it("「API key 随仓库保存」在空间无意义：开关不产生任何写", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setSyncKeys(true);
    await settings.useSettingsStore.getState().flush();

    expect(space.state.patchSpaceCalls).toHaveLength(0);
    expect(h.state.spaceConfigPatches).toHaveLength(0);
    expect(h.state.keyWrites).toEqual([]);
    expect(settings.useSettingsStore.getState().vaultConfig?.syncKeys).toBeUndefined();
  });

  it("写盘在途期间身份切换：写落旧空间身份，不污染新身份的存储", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();
    space.state.holdPatch = new Promise<void>((r) => (space.state.releasePatch = r));

    void settings.useSettingsStore.getState().updateProvider("p1", { name: "B" });
    const flushP = settings.useSettingsStore.getState().flush();
    // 等写盘真正在途（team meta 已收到补丁并被挂起）
    await vi.waitFor(() => expect(space.state.patchSpaceCalls.length).toBeGreaterThan(0));
    // 写盘在途：切到本地仓库
    enterLocal("E:\\v2");
    space.state.releasePatch!();
    await flushP;

    expect(teamAi().providers).toEqual([
      { id: "p1", name: "B", baseUrl: "u", models: [], apiKey: "sk-team" },
    ]);
    // 新仓库身份零写入：无本地 config 补丁、无新身份 keychain 条目
    expect(h.state.vaultPatches).toHaveLength(0);
    expect(h.state.keyWrites.every((k) => !k.startsWith("E:\\v2:"))).toBe(true);
  });
});

describe("应用级显示偏好在空间内不被吞", () => {
  beforeEach(() => {
    enterSpace();
  });

  it("宽松换行/页面内标题在空间下写 global.json（不落团队元数据、不丢）", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();

    await settings.useSettingsStore.getState().setSoftLineBreak(false);
    await settings.useSettingsStore.getState().setInlineTitle(true);

    // 应用级落盘：两条 patch_global_config，且不产生任何仓库级写
    expect(h.state.globalPatches).toEqual([{ softLineBreak: false }, { inlineTitle: true }]);
    expect(space.state.patchSpaceCalls).toHaveLength(0);
    expect(h.state.vaultPatches).toHaveLength(0);
    // 内存态即时生效（跨仓库共享）
    expect(settings.useSettingsStore.getState().softLineBreak).toBe(false);
    expect(settings.useSettingsStore.getState().inlineTitle).toBe(true);
  });
});
