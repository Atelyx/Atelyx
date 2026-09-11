/**
 * 仓库级配置写盘契约测试（stores/settingsStore.ts）。
 *
 * 「API key 随仓库保存」(syncKeys) 关闭时 key 只存本机 keychain：任何写盘路径都不得把
 * config.json 里残留的明文 key 写回，也不得静默丢弃；开启时 key 必须随仓库落盘。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  vaultConfig: {} as Record<string, unknown>,
  keychain: new Map<string, string>(),
  writes: [] as Record<string, unknown>[],
  setKeyCalls: [] as string[],
  failGetKey: false,
  failSetKey: false,
  keyOf: (a: Record<string, unknown>) => `${String(a.vaultId)}:${String(a.providerId)}`,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Record<string, unknown>) => {
    const a = args ?? {};
    switch (cmd) {
      case "read_vault_config":
        return h.vaultConfig;
      case "write_vault_config":
        h.writes.push(a.config as Record<string, unknown>);
        return null;
      case "read_prompt_notes":
      case "read_agents":
        return [];
      case "read_folder_colors":
        return {};
      case "get_api_key":
        if (h.failGetKey) throw new Error("keychain 不可用");
        return h.keychain.get(h.keyOf(a)) ?? "";
      case "set_api_key":
        if (h.failSetKey) throw new Error("keychain 不可用");
        h.setKeyCalls.push(h.keyOf(a));
        h.keychain.set(h.keyOf(a), String(a.key));
        return null;
      case "delete_api_key":
        h.keychain.delete(h.keyOf(a));
        return null;
      default:
        return null;
    }
  },
}));

type SettingsStore = typeof import("./settingsStore");

let settings: SettingsStore;

/** 取某次 write_vault_config 载荷里的 search 段。 */
function searchOf(write: Record<string, unknown>): Record<string, unknown> {
  return (write.search ?? {}) as Record<string, unknown>;
}

beforeEach(async () => {
  vi.resetModules();
  h.vaultConfig = {};
  h.keychain = new Map();
  h.writes = [];
  h.setKeyCalls = [];
  h.failGetKey = false;
  h.failSetKey = false;
  await import("./noteSessionStore");
  await import("./pluginStore");
  const app = await import("./appStore");
  settings = await import("./settingsStore");
  app.useAppStore.setState({ vaultId: "v1" });
});

describe("syncKeys 关闭时的明文 key 边界", () => {
  it("persist 写盘路径（改供应商）不写回磁盘残留的明文 key", async () => {
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-secret" } };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().updateProvider("p1", { name: "改名" });
    await settings.useSettingsStore.getState().flush();

    expect(h.writes).toHaveLength(1);
    expect(searchOf(h.writes[0]).tavilyApiKey).toBeUndefined();
  });

  it("commitVault 写盘路径（改排序等无关字段）不写回明文 key", async () => {
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-secret" } };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setFileExplorerSort("name-asc");

    expect(h.writes).toHaveLength(1);
    expect(searchOf(h.writes[0]).tavilyApiKey).toBeUndefined();
    // 剥离 key 不得连带丢其它字段（vaultId 丢失会让下次进仓重生成 ID、keychain 条目失配）
    expect(h.writes[0].vaultId).toBe("v1");
    expect(h.writes[0].fileExplorerSort).toBe("name-asc");
  });

  it("改 SearXNG 地址不会把 config.json 里残留的明文 key 写回", async () => {
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-secret" } };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore
      .getState()
      .setSearchConfig({ provider: "searxng", searxngUrl: "http://127.0.0.1:8080" });

    expect(h.writes).toHaveLength(1);
    const search = searchOf(h.writes[0]);
    expect(search.tavilyApiKey).toBeUndefined();
    expect(search.provider).toBe("searxng");
    expect(search.searxngUrl).toBe("http://127.0.0.1:8080");
  });

  it("syncKeys 开启时 key 随仓库落盘（不得误剥离）", async () => {
    h.vaultConfig = {
      vaultId: "v1",
      syncKeys: true,
      search: { provider: "tavily", tavilyApiKey: "tvly-secret" },
    };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore
      .getState()
      .setSearchConfig({ searxngUrl: "http://192.168.1.10:8888" });

    expect(searchOf(h.writes[0]).tavilyApiKey).toBe("tvly-secret");
  });

  it("关闭时残留 key 采纳进本机 keychain（不静默丢弃）", async () => {
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-stray" } };
    await settings.useSettingsStore.getState().loadVaultConfig();

    expect(h.setKeyCalls).toEqual(["v1:search-tavily"]);
    expect(h.keychain.get("v1:search-tavily")).toBe("tvly-stray");
    expect(settings.useSettingsStore.getState().tavilyKey).toBe("tvly-stray");
  });

  it("keychain 读失败时仍能采纳残留 key，且不阻断加载", async () => {
    h.failGetKey = true;
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-stray" } };

    await settings.useSettingsStore.getState().loadVaultConfig();

    expect(settings.useSettingsStore.getState().tavilyKey).toBe("tvly-stray");
  });

  it("keychain 回写失败只记日志，不阻断加载也不丢内存 key", async () => {
    h.failSetKey = true;
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-stray" } };

    await settings.useSettingsStore.getState().loadVaultConfig();

    expect(settings.useSettingsStore.getState().tavilyKey).toBe("tvly-stray");
  });

  it("关闭 syncKeys 开关时写盘剥离 key 并回写 keychain", async () => {
    h.vaultConfig = {
      vaultId: "v1",
      syncKeys: true,
      search: { provider: "tavily", tavilyApiKey: "tvly-secret" },
    };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setSyncKeys(false);

    expect(searchOf(h.writes[0]).tavilyApiKey).toBeUndefined();
    expect(h.keychain.get("v1:search-tavily")).toBe("tvly-secret");
  });

  it("本机 keychain 已有 key 时不覆盖", async () => {
    h.keychain.set("v1:search-tavily", "tvly-local");
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-stray" } };
    await settings.useSettingsStore.getState().loadVaultConfig();

    expect(h.setKeyCalls).toEqual([]);
    expect(settings.useSettingsStore.getState().tavilyKey).toBe("tvly-local");
  });
});
