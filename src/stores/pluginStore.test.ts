/**
 * 插件行状态编排测试（stores/pluginStore）：挂载失败的分段诊断。
 *
 * store 侧独有的两个阶段（manifest / compat）在此覆盖；read / transpile / eval / apply 四段
 * 在 loader / packageMount 测试里覆盖。插件运行时与 Rust 命令以替身替代，只验证阶段归类。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InstalledPlugin, PluginPackageJson } from "@/types";

vi.mock("@/services/plugins", () => ({
  pluginInstall: vi.fn(),
  pluginInstallLocal: vi.fn(),
  pluginList: vi.fn(async () => []),
  pluginSeedDefault: vi.fn(),
  pluginSetEnabled: vi.fn(async () => {}),
  pluginUninstall: vi.fn(),
  pluginUpdate: vi.fn(),
}));

vi.mock("@/services/app", () => ({ getAppVersion: async () => "0.0.0" }));

vi.mock("@/stores/appStore", () => ({
  useAppStore: {
    getState: () => ({ entryLoading: false, reportLoad: () => {}, openPluginPage: () => {} }),
  },
}));

import { pluginList } from "@/services/plugins";
import { usePluginStore } from "@/stores/pluginStore";

function row(over: Partial<InstalledPlugin> & { id: string }): InstalledPlugin {
  return {
    manifest: { id: over.id, name: over.id, version: "1.0.0", type: "panel" },
    scope: "app",
    installDir: "",
    sourceKind: "builtin",
    enabled: false,
    phase: "pending",
    ...over,
  };
}

beforeEach(() => {
  usePluginStore.setState({ plugins: {}, initialized: false });
  vi.mocked(pluginList).mockResolvedValue([]);
});

describe("插件行失败诊断", () => {
  it("实现随应用编译但缺实现定义 → 阶段 manifest", async () => {
    usePluginStore.setState({ plugins: { "com.test.nope": row({ id: "com.test.nope" }) } });
    await usePluginStore.getState().setEnabled("com.test.nope", true);
    const p = usePluginStore.getState().plugins["com.test.nope"];
    expect(p.phase).toBe("failed");
    expect(p.failure).toEqual({ phase: "manifest", message: "实现随应用编译但缺少对应实现定义" });
  });

  it("契约版本不兼容 → 阶段 compat", async () => {
    const manifest: PluginPackageJson = {
      name: "com.test.old",
      version: "1.0.0",
      main: "main.js",
      atelyx: { name: "Old", type: "panel", hostApiVersion: 999 },
    };
    vi.mocked(pluginList).mockResolvedValue([
      {
        id: "com.test.old",
        name: "Old",
        version: "1.0.0",
        type: "panel",
        scope: "app",
        installDir: "/tmp/old",
        sourceKind: "market",
        enabled: true,
        manifest,
      },
    ]);
    await usePluginStore.getState().load();
    const p = usePluginStore.getState().plugins["com.test.old"];
    expect(p.phase).toBe("failed");
    expect(p.failure?.phase).toBe("compat");
    expect(p.failure?.message).toContain("999");
  });

  it("手动启用不兼容插件同样停在 compat 阶段", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.old": row({
          id: "com.test.old",
          installDir: "/tmp/old",
          sourceKind: "market",
          manifest: {
            id: "com.test.old",
            name: "Old",
            version: "1.0.0",
            type: "panel",
            main: "main.js",
            hostApiVersion: 999,
          },
        }),
      },
    });

    await usePluginStore.getState().setEnabled("com.test.old", true);
    const p = usePluginStore.getState().plugins["com.test.old"];
    expect(p.enabled).toBe(true);
    expect(p.phase).toBe("failed");
    expect(p.failure?.phase).toBe("compat");
    expect(p.failure?.message).toContain("999");
  });
});
