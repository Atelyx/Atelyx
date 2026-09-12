/**
 * 仓库级配置写盘契约测试（stores/settingsStore.ts）。
 *
 * 两条契约：
 * 1) 「API key 随仓库保存」(syncKeys) 关闭时 key 只存本机 keychain：任何写盘路径都不得把
 *    config.json 里残留的明文 key 写回，也不得静默丢弃；开启时 key 必须随仓库落盘。
 * 2) 写盘是字段级补丁（`vault_config_patch`）：只发本次改动的字段，其余字段留给 Rust 侧
 *    按磁盘现有值合并——否则撕裂窗口的陈旧副本会把主窗口刚写入的字段整片覆盖。
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const h = vi.hoisted(() => ({
  vaultConfig: {} as Record<string, unknown>,
  /** read_vault_config 带回的损坏备份文件名（非 null = 后端已备份损坏原文并按空配置返回）。 */
  corruptBackup: null as string | null,
  /** vault_config_patch 带回的损坏备份文件名（写盘路径发现磁盘原文损坏）。 */
  patchCorruptBackup: null as string | null,
  keychain: new Map<string, string>(),
  /** 字段级补丁（vault_config_patch）载荷。 */
  patches: [] as Record<string, unknown>[],
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
        return { config: h.vaultConfig, corruptBackup: h.corruptBackup };
      case "vault_config_patch":
        h.patches.push(a.patch as Record<string, unknown>);
        return h.patchCorruptBackup;
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

/** 取某次补丁载荷里的 search 段（补丁未带 search 时返回 {}）。 */
function searchOf(write: Record<string, unknown>): Record<string, unknown> {
  return (write.search ?? {}) as Record<string, unknown>;
}

/**
 * 把合并补丁应用到配置对象上（RFC 7386 语义：`null` 删键、对象递归下钻、其余整体替换）。
 * 与 Rust 侧 `merge_vault_config` 同语义的最小实现——mock 的 invoke 是直通录制，不做合并，
 * 只有在本函数里模拟合并，才能断言「磁盘最终确实没有明文 key」而不是只断言补丁形状。
 */
function mergeInto(
  base: Record<string, unknown>,
  patch: Record<string, unknown>,
): Record<string, unknown> {
  const out: Record<string, unknown> = { ...base };
  for (const [key, value] of Object.entries(patch)) {
    if (value === null) {
      delete out[key];
      continue;
    }
    const existing = out[key];
    if (
      value !== undefined &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      existing &&
      typeof existing === "object" &&
      !Array.isArray(existing)
    ) {
      out[key] = mergeInto(
        existing as Record<string, unknown>,
        value as Record<string, unknown>,
      );
    } else if (value !== undefined) {
      out[key] = value;
    }
  }
  return out;
}

beforeEach(async () => {
  vi.resetModules();
  h.vaultConfig = {};
  h.corruptBackup = null;
  h.patchCorruptBackup = null;
  h.keychain = new Map();
  h.patches = [];
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
  it("persist 写盘路径（改供应商）顺手抹掉磁盘残留的明文 key", async () => {
    h.vaultConfig = {
      vaultId: "v1",
      providers: [{ id: "p1", name: "A", baseUrl: "u", models: [] }],
      search: { provider: "tavily", tavilyApiKey: "tvly-secret" },
    };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().updateProvider("p1", { name: "改名" });
    await settings.useSettingsStore.getState().flush();

    expect(h.patches).toHaveLength(1);
    const providers = h.patches[0].providers as Array<Record<string, unknown>>;
    expect(providers).toHaveLength(1);
    expect(providers[0].apiKey).toBeUndefined();
    // 残留明文 key 不在补丁里重写，而是显式删键（合并语义下省略 = 保留磁盘现值）
    expect(searchOf(h.patches[0]).tavilyApiKey).toBeNull();
    const merged = mergeInto(h.vaultConfig, h.patches[0]);
    expect((merged.search as Record<string, unknown>).tavilyApiKey).toBeUndefined();
    // provider 等其余 search 字段不在补丁里 → 由 Rust 侧按磁盘现值保留
    expect((merged.search as Record<string, unknown>).provider).toBe("tavily");
  });

  it("残留 key 只删一次：写盘后再改字段的补丁不再携带删键指令", async () => {
    h.vaultConfig = {
      vaultId: "v1",
      providers: [{ id: "p1", name: "A", baseUrl: "u", models: [] }],
      search: { provider: "tavily", tavilyApiKey: "tvly-secret" },
    };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().updateProvider("p1", { name: "改名" });
    await settings.useSettingsStore.getState().flush();
    await settings.useSettingsStore.getState().setFileExplorerSort("name-asc");

    expect(h.patches).toHaveLength(2);
    expect(Object.keys(h.patches[1])).toEqual(["fileExplorerSort"]);
  });

  it("改排序只发排序一个字段（磁盘无残留 key 时其余字段由 Rust 侧按磁盘合并保留）", async () => {
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily" } };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setFileExplorerSort("name-asc");

    expect(h.patches).toHaveLength(1);
    expect(h.patches[0]).toEqual({ fileExplorerSort: "name-asc" });
  });

  it("改 SearXNG 地址不会把 config.json 里残留的明文 key 写回（并显式删键）", async () => {
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-secret" } };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore
      .getState()
      .setSearchConfig({ provider: "searxng", searxngUrl: "http://127.0.0.1:8080" });

    expect(h.patches).toHaveLength(1);
    const search = searchOf(h.patches[0]);
    expect(search.tavilyApiKey).toBeNull();
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

    expect(searchOf(h.patches[0]).tavilyApiKey).toBe("tvly-secret");
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

    expect(h.patches).toHaveLength(1);
    // 补丁必须带 syncKeys/providers/search 三项：只写 syncKeys 会把明文 key 留在配置里
    expect(h.patches[0].syncKeys).toBe(false);
    // 关键：清空内层 key 只能靠显式 null（合并语义下「省略键」= 保留磁盘现值），
    // 发 undefined 会被 JSON 丢弃 → 文件里的明文 key 永远留着
    expect(searchOf(h.patches[0]).tavilyApiKey).toBeNull();
    expect(h.keychain.get("v1:search-tavily")).toBe("tvly-secret");
  });

  it("合并补丁语义：显式 null 删键、省略键保留磁盘现值", async () => {
    // 与 Rust 侧 merge_vault_config 同语义的最小实现：只要补丁语义不被破坏，本用例就能拦住回归
    h.vaultConfig = {
      vaultId: "v1",
      syncKeys: true,
      fileExplorerSort: "name-asc",
      search: { provider: "tavily", tavilyApiKey: "tvly-secret" },
    };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setSyncKeys(false);

    const disk = mergeInto(h.vaultConfig, h.patches[0]);
    const search = disk.search as Record<string, unknown>;
    expect("tavilyApiKey" in search).toBe(false);
    // 未出现在补丁里的字段保留磁盘值（这正是撕裂窗口不覆盖主窗口配置的依据）
    expect(disk.fileExplorerSort).toBe("name-asc");
    expect(disk.vaultId).toBe("v1");
  });

  it("清空 Tavily key（syncKeys 开启）也走显式 null 删键", async () => {
    h.vaultConfig = {
      vaultId: "v1",
      syncKeys: true,
      search: { provider: "tavily", tavilyApiKey: "tvly-secret" },
    };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().setTavilyKey("");

    const disk = mergeInto(h.vaultConfig, h.patches[0]);
    expect("tavilyApiKey" in (disk.search as Record<string, unknown>)).toBe(false);
  });

  it("本机 keychain 已有 key 时不覆盖", async () => {
    h.keychain.set("v1:search-tavily", "tvly-local");
    h.vaultConfig = { vaultId: "v1", search: { provider: "tavily", tavilyApiKey: "tvly-stray" } };
    await settings.useSettingsStore.getState().loadVaultConfig();

    expect(h.setKeyCalls).toEqual([]);
    expect(settings.useSettingsStore.getState().tavilyKey).toBe("tvly-local");
  });
});

describe("脏门控与字段级补丁（撕裂窗口覆盖防护）", () => {
  it("无变化时 flush 不写盘", async () => {
    h.vaultConfig = { vaultId: "v1", providers: [{ id: "p1", name: "A", baseUrl: "u", models: [] }] };
    await settings.useSettingsStore.getState().loadVaultConfig();

    await settings.useSettingsStore.getState().flush();

    expect(h.patches).toHaveLength(0);
  });

  it("仓库配置加载失败时不写盘（内存是默认值，落盘会抹掉磁盘配置）", async () => {
    const app = await import("./appStore");
    app.useAppStore.setState({ vaultId: "v2" });
    // 未对本仓库调用 loadVaultConfig：loadedForVaultId 仍是 null
    await settings.useSettingsStore.getState().updateProvider("p9", { name: "x" });
    await settings.useSettingsStore.getState().flush();

    expect(h.patches).toHaveLength(0);
  });

  it("重复改同一值时只在首次写盘", async () => {
    h.vaultConfig = { vaultId: "v1", providers: [{ id: "p1", name: "A", baseUrl: "u", models: [] }] };
    await settings.useSettingsStore.getState().loadVaultConfig();

    await settings.useSettingsStore.getState().updateProvider("p1", { name: "B" });
    await settings.useSettingsStore.getState().flush();
    // 再改回同值：内容与基线一致 → 不再写盘
    await settings.useSettingsStore.getState().updateProvider("p1", { name: "B" });
    await settings.useSettingsStore.getState().flush();

    expect(h.patches).toHaveLength(1);
  });

  it("改供应商只发 providers，不携带陈旧副本的其它字段", async () => {
    // 撕裂窗口场景：本窗口内存里 fileExplorerSort 陈旧（"mtime-desc"），主窗口已改成 "name-asc"。
    // 本窗口改供应商时补丁只带 providers，不得把陈旧的排序写回去。
    h.vaultConfig = {
      vaultId: "v1",
      fileExplorerSort: "mtime-desc",
      providers: [{ id: "p1", name: "A", baseUrl: "u", models: [] }],
    };
    await settings.useSettingsStore.getState().loadVaultConfig();
    await settings.useSettingsStore.getState().updateProvider("p1", { name: "B" });
    await settings.useSettingsStore.getState().flush();

    expect(h.patches).toHaveLength(1);
    expect(Object.keys(h.patches[0])).toEqual(["providers"]);
  });
});

describe("配置损坏的可见性", () => {
  it("后端报告损坏备份时弹出通知（用户要知道设置与 key 为何消失）", async () => {
    const notifications = await import("./notificationStore");
    h.vaultConfig = {};
    h.corruptBackup = "config.json.corrupt-abc";
    await settings.useSettingsStore.getState().loadVaultConfig();

    const items = notifications.useNotificationStore.getState().items;
    expect(items).toHaveLength(1);
    expect(items[0].level).toBe("error");
    expect(items[0].message).toContain("config.json.corrupt-abc");
  });

  it("正常读取不弹通知", async () => {
    const notifications = await import("./notificationStore");
    h.vaultConfig = { vaultId: "v1" };
    await settings.useSettingsStore.getState().loadVaultConfig();

    expect(notifications.useNotificationStore.getState().items).toHaveLength(0);
  });

  it("写盘路径发现原文损坏（后端已备份）也弹通知", async () => {
    const notifications = await import("./notificationStore");
    h.vaultConfig = { vaultId: "v1" };
    await settings.useSettingsStore.getState().loadVaultConfig();
    h.patchCorruptBackup = "config.json.corrupt-write";

    await settings.useSettingsStore.getState().setFileExplorerSort("name-asc");

    const items = notifications.useNotificationStore.getState().items;
    expect(items).toHaveLength(1);
    expect(items[0].level).toBe("error");
    expect(items[0].message).toContain("config.json.corrupt-write");
  });
});
