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
  pluginList: vi.fn(async () => ({ rows: [] })),
  pluginSeedDefault: vi.fn(),
  pluginSetEnabled: vi.fn(async () => {}),
  pluginUninstall: vi.fn(async () => {}),
  pluginUpdate: vi.fn(),
  pluginRollback: vi.fn(),
  onPluginChanged: vi.fn(async () => () => {}),
}));

vi.mock("@/services/app", () => ({ getAppVersion: vi.fn(async () => "0.0.0") }));

vi.mock("@/stores/appStore", () => ({
  useAppStore: {
    getState: () => ({ entryLoading: false, reportLoad: () => {}, openPluginPage: () => {} }),
  },
}));

import { pluginInstall, pluginList, pluginRollback, pluginUninstall, pluginUpdate } from "@/services/plugins";
import { getAppVersion } from "@/services/app";
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
  vi.clearAllMocks();
  usePluginStore.setState({ plugins: {}, initialized: false, stateError: null });
  vi.mocked(pluginList).mockResolvedValue({ rows: [] });
  vi.mocked(pluginUpdate).mockReset();
  vi.mocked(pluginRollback).mockReset();
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
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
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
      ],
    });
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

  it("跨作用域同 id 冲突行：强制停用、标失败且不进入装配", async () => {    const conflict = "app 与仓库作用域存在同 id 插件，双方已停用：请卸载其一后重新启用";
    const conflictRow = (scope: "app" | "vault") => ({
      id: "com.test.dupe",
      name: "Dupe",
      version: "1.0.0",
      type: "panel",
      scope,
      installDir: `/tmp/dupe-${scope}`,
      sourceKind: "market",
      enabled: true,
      manifest: {
        name: "com.test.dupe",
        version: "1.0.0",
        main: "main.js",
        atelyx: { name: "Dupe", type: "panel" },
      },
      conflict,
    });
    vi.mocked(pluginList).mockResolvedValue({
      rows: [conflictRow("app"), conflictRow("vault")],
    } as never);

    await usePluginStore.getState().load();
    const p = usePluginStore.getState().plugins["com.test.dupe"];
    expect(p.enabled).toBe(false);
    expect(p.phase).toBe("failed");
    expect(p.failure?.phase).toBe("manifest");
    expect(p.failure?.message).toContain("同 id");
  });

  it("状态降级：stateError 落到 store，停用行不进入装配", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
        {
          id: "com.test.degraded",
          name: "Degraded",
          version: "1.0.0",
          type: "panel",
          scope: "app",
          installDir: "/tmp/degraded",
          sourceKind: "market",
          enabled: false,
          manifest: { name: "com.test.degraded", version: "1.0.0", main: "main.js", atelyx: { name: "D", type: "panel" } },
        },
      ],
      stateError: "插件状态文件损坏（原文保留，修复或删除后重试）",
    });
    await usePluginStore.getState().load();
    expect(usePluginStore.getState().stateError).toContain("损坏");
    expect(usePluginStore.getState().plugins["com.test.degraded"]?.enabled).toBe(false);
    expect(usePluginStore.getState().plugins["com.test.degraded"]?.phase).toBe("pending");
  });
});

describe("插件版本操作运行时恢复", () => {
  it("更新失败仍按磁盘重载，避免启用行悬空停机", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.update": row({ id: "com.test.update", installDir: "/tmp/update", sourceKind: "git" }),
      },
    });
    vi.mocked(pluginUpdate).mockRejectedValueOnce(new Error("更新失败"));

    await expect(usePluginStore.getState().update("com.test.update")).rejects.toThrow("更新失败");
    expect(pluginList).toHaveBeenCalledTimes(1);
  });

  it("回退携带确认的目标版本并在成功后重载", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.rollback": row({
          id: "com.test.rollback",
          installDir: "/tmp/rollback",
          sourceKind: "git",
          previousVersion: "1.0.0",
        }),
      },
    });
    vi.mocked(pluginRollback).mockResolvedValueOnce({ id: "com.test.rollback" } as never);

    await usePluginStore.getState().rollback("com.test.rollback");
    expect(pluginRollback).toHaveBeenCalledWith("com.test.rollback", "1.0.0");
    expect(pluginList).toHaveBeenCalledTimes(1);
  });

  it("回退失败仍按磁盘重载，错误原样透传", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.rollback": row({
          id: "com.test.rollback",
          installDir: "/tmp/rollback",
          sourceKind: "git",
          previousVersion: "1.0.0",
        }),
      },
    });
    vi.mocked(pluginRollback).mockRejectedValueOnce(new Error("回退失败"));

    await expect(usePluginStore.getState().rollback("com.test.rollback")).rejects.toThrow("回退失败");
    expect(pluginList).toHaveBeenCalledTimes(1);
  });

  it("回退与重载双失败 → 合成错误同时含两类原因", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.rollback": row({
          id: "com.test.rollback",
          installDir: "/tmp/rollback",
          sourceKind: "git",
          previousVersion: "1.0.0",
        }),
      },
    });
    vi.mocked(pluginRollback).mockRejectedValueOnce(new Error("目录不可用"));
    vi.mocked(pluginList).mockRejectedValueOnce(new Error("状态文件忙"));

    await expect(usePluginStore.getState().rollback("com.test.rollback")).rejects.toThrow(
      "回退失败：目录不可用；恢复运行时失败：状态文件忙",
    );
  });

  it("更新成功后重载且不抛错", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.update": row({ id: "com.test.update", installDir: "/tmp/update", sourceKind: "git" }),
      },
    });
    vi.mocked(pluginUpdate).mockResolvedValueOnce({ id: "com.test.update" } as never);

    await usePluginStore.getState().update("com.test.update");
    expect(pluginList).toHaveBeenCalledTimes(1);
  });

  it("无可回退版本时给出可见错误且不触达命令", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.norollback": row({ id: "com.test.norollback", installDir: "/tmp/nr", sourceKind: "git" }),
      },
    });

    await expect(usePluginStore.getState().rollback("com.test.norollback")).rejects.toThrow("没有可回退版本");
    expect(pluginRollback).not.toHaveBeenCalled();
    expect(pluginList).not.toHaveBeenCalled();
  });
});

describe("安装收尾的宿主兼容强制", () => {
  function installedRow(id: string, manifest: PluginPackageJson) {
    return {
      id,
      name: id,
      version: "1.0.0",
      type: "panel",
      scope: "app",
      installDir: `/tmp/${id}`,
      sourceKind: "market",
      enabled: true,
      manifest,
    };
  }

  it("清单 atelyx 块约束不满足 → 回滚卸载并报错（约束不因未归一化而漏判）", async () => {
    const raw: PluginPackageJson = {
      name: "com.test.constrained",
      version: "1.0.0",
      main: "main.js",
      atelyx: { name: "C", type: "panel", hostApiVersion: 999 },
    };
    vi.mocked(pluginInstall).mockResolvedValueOnce(installedRow("com.test.constrained", raw) as never);

    await expect(usePluginStore.getState().install("com/example", "app")).rejects.toThrow("999");
    expect(pluginUninstall).toHaveBeenCalledWith("com.test.constrained", "app");
  });

  it("宿主版本读取失败 → 不安装（fail-closed），回滚并给出可读错误", async () => {
    vi.mocked(getAppVersion).mockRejectedValueOnce(new Error("ipc down"));
    const raw: PluginPackageJson = {
      name: "com.test.noversion",
      version: "1.0.0",
      main: "main.js",
      atelyx: { name: "N", type: "panel" },
    };
    vi.mocked(pluginInstall).mockResolvedValueOnce(installedRow("com.test.noversion", raw) as never);

    await expect(usePluginStore.getState().install("com/example", "app")).rejects.toThrow("宿主版本");
    expect(pluginUninstall).toHaveBeenCalledWith("com.test.noversion", "app");
  });
});
