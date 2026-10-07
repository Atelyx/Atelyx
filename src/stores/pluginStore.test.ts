/**
 * 插件行状态编排测试（stores/pluginStore）：挂载失败的分段诊断 + 磁盘包入口选择。
 *
 * store 侧独有的阶段（manifest / compat）与入口优先级（宿主产物 → 清单 main → 声明式）在此覆盖；
 * read / transpile / eval / apply 四段在 loader / packageMount 测试里覆盖。插件运行时与 Rust 命令
 * 以替身替代，只验证编排结果。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { InstalledPlugin, PluginPackageJson } from "@/types";

vi.mock("@/services/plugins", () => ({
  pluginInstall: vi.fn(),
  pluginInstallLocal: vi.fn(),
  pluginList: vi.fn(async () => ({ rows: [], assemblyVersion: 0 })),
  pluginSeedDefault: vi.fn(),
  pluginSetEnabled: vi.fn(async () => {}),
  pluginUninstall: vi.fn(async () => {}),
  pluginUpdate: vi.fn(),
  pluginRollback: vi.fn(),
  pluginApplyDefaultLayout: vi.fn(async () => {}),
  getAssemblyVersion: vi.fn(async () => 0),
  onPluginChanged: vi.fn(async (handler: (payload: { id: string; version: number }) => void) => {
    assemblyBroadcast.pluginChanged.push(handler);
    return () => {};
  }),
}));

vi.mock("@/services/app", () => ({ getAppVersion: vi.fn(async () => "0.0.0") }));
vi.mock("@/services/dialog", () => ({ pickDirectory: vi.fn() }));
// 广播 handler 抓取：store 的监听器在首次 load 时注册（ensure* 有模块级守卫，全文件仅注册一次），
// 测试经此直接派发跨窗口装配广播（plugin-changed / composition-changed）。
const assemblyBroadcast = vi.hoisted(() => ({
  pluginChanged: [] as Array<(payload: { id: string; version: number }) => void>,
  compositionChanged: [] as Array<(version: number) => void>,
}));
// 组合接管的用户层读写与跨窗口广播：本文件只验证装配编排，配置层与广播机制各自单测。
vi.mock("@/services/global", () => ({
  readGlobalConfig: vi.fn(async () => ({ config: { recentVaults: [] }, corruptBackup: null })),
  updateGlobalConfig: vi.fn(async () => ({ config: { recentVaults: [] }, corruptBackup: null })),
}));
vi.mock("@/services/windowBus", () => ({
  emitCompositionChanged: vi.fn(async () => {}),
  onCompositionChanged: vi.fn(async (handler: (version: number) => void) => {
    assemblyBroadcast.compositionChanged.push(handler);
    return () => {};
  }),
}));
// 事件发射以替身替代：本文件只验证接线「plugin-msg 入站 → 插件频道订阅注册表投递」，投递本身在 collabHost/kernel 测试覆盖
vi.mock("@/services/cordis/events", () => ({ emitPluginEvent: vi.fn() }));

// 挂载链路以替身替代：本文件只验证 store 侧的编排（入口选择、阶段归类），内核不参与。
// 内核对象必须**稳定**（同一 ctx 引用）：插件进程登记表以内核上下文为键，每次返回新对象会让
// 登记与结束落在不同表上，测不出真实行为。`vi.hoisted` 让 mock 工厂能引用到它。
const fakeKernel = vi.hoisted(() => ({ ctx: {} }));
vi.mock("@/services/cordis/kernel", () => ({ getKernel: () => fakeKernel }));
// stopPlugin 的快捷键注销收口走 Tauri invoke（node 环境无 window），按 no-op 桩掉防噪音
vi.mock("@/services/globalShortcut", () => ({
  registerGlobalShortcut: vi.fn(async () => {}),
  unregisterGlobalShortcut: vi.fn(async () => {}),
  releasePluginGlobalShortcuts: vi.fn(async () => {}),
  onGlobalShortcutTriggered: vi.fn(async () => () => {}),
}));
vi.mock("@/services/cordis/loader", () => ({
  mountPlugin: vi.fn(async () => ({ ok: true })),
  unmountPlugin: vi.fn(async () => {}),
  unmountAll: vi.fn(async () => {}),
  mountedPluginIds: vi.fn(() => []),
  pluginIdOf: vi.fn(() => undefined),
}));
vi.mock("@/services/cordis/packageMount", () => ({
  mountPluginFromPackage: vi.fn(async () => ({ ok: true })),
}));
// 结束插件进程会打到 Tauri：替换为替身（进程编排本身在 pluginProcesses 测试里覆盖）。
vi.mock("@/services/shell", () => ({
  killProcessTree: vi.fn(async () => {}),
}));

vi.mock("@/stores/appStore", () => ({
  useAppStore: {
    getState: () => ({ entryLoading: false, reportLoad: () => {}, openPluginPage: () => {} }),
  },
}));

import {
  getAssemblyVersion,
  pluginInstall,
  pluginInstallLocal,
  pluginList,
  pluginRollback,
  pluginUninstall,
  pluginUpdate,
} from "@/services/plugins";
import type { PluginRow } from "@/services/plugins";
import { getAppVersion } from "@/services/app";
import { pickDirectory } from "@/services/dialog";
import { mountPluginFromPackage } from "@/services/cordis/packageMount";
import { registerViewSlot } from "@/services/cordis/slots";
import { mountPlugin, unmountPlugin } from "@/services/cordis/loader";
import { readGlobalConfig, updateGlobalConfig } from "@/services/global";
import { emitCompositionChanged } from "@/services/windowBus";
import { killProcessTree } from "@/services/shell";
import { mountedPluginIds } from "@/services/cordis/loader";
import { trackPluginProcess } from "@/services/cordis/pluginProcesses";
import { emitPluginEvent } from "@/services/cordis/events";
import { dispatchCollabChannel, registerPluginChannel } from "@/utils/collabHost";
import { usePluginStore } from "@/stores/pluginStore";
import { useNotificationStore } from "@/stores/notificationStore";

function row(over: Partial<InstalledPlugin> & { id: string }): InstalledPlugin {
  return {
    manifest: { id: over.id, name: over.id, version: "1.0.0", type: "panel" },
    installDir: "",
    sourceKind: "builtin",
    enabled: false,
    phase: "pending",
    ...over,
  };
}

/** 随应用分发的行（无磁盘目录，清单 main 恒为占位串）。 */
const builtinRow = (id: string): PluginRow => ({
  id,
  name: id,
  version: "1.0.0",
  type: "panel",
  installDir: "",
  sourceKind: "builtin",
  enabled: true,
  manifest: { name: id, version: "1.0.0", main: "builtin", atelyx: { name: id, type: "panel" } },
});

/** 磁盘包行，声明接管给定的目标行。 */
const providerRow = (id: string, patch: unknown[]): PluginRow => ({
  id,
  name: id,
  version: "1.0.0",
  type: "background",
  installDir: "/tmp/provider",
  sourceKind: "market",
  enabled: true,
  manifest: {
    name: id,
    version: "1.0.0",
    main: "src/index.ts",
    atelyx: { name: id, type: "background", compositionPatch: patch },
  },
});

beforeEach(() => {
  vi.clearAllMocks();
  // assemblyBroadcast 的 handler 数组不清：监听器注册是模块级一次性（ensure* 守卫），
  // 清空后广播派发将空转（用例假绿或 waitFor 卡死）。
  usePluginStore.setState({
    plugins: {},
    initialized: false,
    stateError: null,
    compositionPatches: {},
    composition: null,
    assemblyCursor: 0,
  });
  vi.mocked(pluginList).mockResolvedValue({ rows: [], assemblyVersion: 0 });
  // 装配版本 mock 同源：默认 0 与 pluginList 缺省一致（漏发追平用例依赖两处同源）；
  // mockReset 清掉上个用例残留的 mockResolvedValue(Once) 队列，防版本计数器跨用例泄漏。
  vi.mocked(getAssemblyVersion).mockReset();
  vi.mocked(getAssemblyVersion).mockResolvedValue(0);
  vi.mocked(mountedPluginIds).mockReturnValue([]);
  vi.mocked(readGlobalConfig).mockResolvedValue({ config: { recentVaults: [] }, corruptBackup: null });
  vi.mocked(updateGlobalConfig).mockResolvedValue({ config: { recentVaults: [] }, corruptBackup: null });
  vi.mocked(emitCompositionChanged).mockResolvedValue(undefined);
  vi.mocked(pluginUpdate).mockReset();
  vi.mocked(pluginRollback).mockReset();
});

describe("插件行失败诊断", () => {
  it("实现随应用编译但缺实现定义 → 阶段 manifest", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
        {
          id: "com.test.nope",
          name: "Nope",
          version: "1.0.0",
          type: "panel",
          installDir: "",
          sourceKind: "builtin",
          enabled: true,
          manifest: { name: "com.test.nope", version: "1.0.0", type: "panel" },
        },
      ],
    });
    await usePluginStore.getState().load();
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
    // 启停走全量重建（组合裁决会随之变化）：Rust 返回的行带新启用态，与真实路径一致
    usePluginStore.setState({
      plugins: {
        "com.test.old": row({
          id: "com.test.old",
          installDir: "/tmp/old",
          sourceKind: "market",
          manifest: { id: "com.test.old", name: "Old", version: "1.0.0", type: "panel", main: "main.js" },
        }),
      },
    });
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
        {
          id: "com.test.old",
          name: "Old",
          version: "1.0.0",
          type: "panel",
          installDir: "/tmp/old",
          sourceKind: "market",
          enabled: true,
          manifest: {
            name: "com.test.old",
            version: "1.0.0",
            main: "main.js",
            atelyx: { name: "Old", type: "panel", hostApiVersion: 999 },
          },
        },
      ],
    });

    await usePluginStore.getState().setEnabled("com.test.old", true);
    const p = usePluginStore.getState().plugins["com.test.old"];
    expect(p.enabled).toBe(true);
    expect(p.phase).toBe("failed");
    expect(p.failure?.phase).toBe("compat");
    expect(p.failure?.message).toContain("999");
  });

  it("同 id 冲突行：强制停用、标失败且不进入装配", async () => {
    const conflict = "同 id 插件存在冲突行，双方已停用：请卸载其一后重新启用";
    const conflictRow = {
      id: "com.test.dupe",
      name: "Dupe",
      version: "1.0.0",
      type: "panel",
      installDir: "/tmp/dupe",
      sourceKind: "market",
      enabled: true,
      manifest: {
        name: "com.test.dupe",
        version: "1.0.0",
        main: "main.js",
        atelyx: { name: "Dupe", type: "panel" },
      },
      conflict,
    };
    vi.mocked(pluginList).mockResolvedValue({
      rows: [conflictRow],
    } as never);

    await usePluginStore.getState().load();
    const p = usePluginStore.getState().plugins["com.test.dupe"];
    expect(p.enabled).toBe(false);
    expect(p.phase).toBe("failed");
    expect(p.failure?.phase).toBe("manifest");
    expect(p.failure?.message).toContain("同 id");
  });

  it("随仓库插件目录仍在：汇总提示且不进入列表", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [],
      legacyVaultPluginRoots: ["E:/vaults/notes", "E:/vaults/work"],
    });
    const notify = vi.spyOn(useNotificationStore.getState(), "notify");

    await usePluginStore.getState().load();

    expect(notify).toHaveBeenCalledWith(
      expect.objectContaining({
        level: "warning",
        message: expect.stringContaining("E:/vaults/notes"),
      }),
    );
    notify.mockRestore();
  });

  it("无随仓库插件目录：不发汇总提示", async () => {
    const notify = vi.spyOn(useNotificationStore.getState(), "notify");
    await usePluginStore.getState().load();
    expect(notify).not.toHaveBeenCalled();
    notify.mockRestore();
  });

  it("状态降级：stateError 落到 store，停用行不进入装配", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
        {
          id: "com.test.degraded",
          name: "Degraded",
          version: "1.0.0",
          type: "panel",
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

    await expect(usePluginStore.getState().install("com/example")).rejects.toThrow("999");
    // 回滚卸载保留数据：安装命令可能已把保留区数据搬回新目录的 data/，直接删会丢用户数据
    expect(pluginUninstall).toHaveBeenCalledWith("com.test.constrained", true);
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

    await expect(usePluginStore.getState().install("com/example")).rejects.toThrow("宿主版本");
    expect(pluginUninstall).toHaveBeenCalledWith("com.test.noversion", true);
  });
});

describe("磁盘包入口选择（宿主产物优先）", () => {
  const diskRow = (id: string, over: Partial<PluginRow> = {}): PluginRow => ({
    id,
    name: id,
    version: "1.0.0",
    type: "panel",
    installDir: "/tmp/plugin",
    sourceKind: "market",
    enabled: true,
    manifest: {
      name: id,
      version: "1.0.0",
      main: "src/index.ts",
      atelyx: { name: id, type: "panel" },
    },
    ...over,
  });

  it("有打包产物时用产物入口（不再回落清单 main）", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [diskRow("com.test.dep", { entry: ".atelyx-dist/entry.js" })],
    });
    await usePluginStore.getState().load();
    expect(mountPluginFromPackage).toHaveBeenCalledWith(
      expect.anything(),
      "com.test.dep",
      ".atelyx-dist/entry.js",
      "com.test.dep",
    );
    expect(usePluginStore.getState().plugins["com.test.dep"].phase).toBe("active");
  });

  it("无产物时回落清单 main（未声明依赖的插件行为不变）", async () => {
    vi.mocked(pluginList).mockResolvedValue({ rows: [diskRow("com.test.plain")] });
    await usePluginStore.getState().load();
    expect(mountPluginFromPackage).toHaveBeenCalledWith(
      expect.anything(),
      "com.test.plain",
      "src/index.ts",
      "com.test.plain",
    );
  });

  it("产物与 main 都缺 → 声明式插件，直接置 active 且不挂载", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
        diskRow("com.test.theme", {
          type: "theme",
          manifest: { name: "com.test.theme", version: "1.0.0", atelyx: { name: "t", type: "theme" } },
        }),
      ],
    });
    await usePluginStore.getState().load();
    expect(mountPluginFromPackage).not.toHaveBeenCalled();
    expect(usePluginStore.getState().plugins["com.test.theme"].phase).toBe("active");
  });
});

describe("组合接管装配", () => {
  it("插件声明接管内置行：内置行的位置跑提供者入口，提供者自身行不独立装配", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [builtinRow("builtin.search"), providerRow("com.test.provider", [{ target: "builtin.search" }])],
    });
    await usePluginStore.getState().load();

    // 内置行的位置装配提供者的入口（读盘按提供者定位，挂载归属仍是内置行）
    expect(mountPluginFromPackage).toHaveBeenCalledWith(
      expect.anything(),
      "builtin.search",
      "src/index.ts",
      "com.test.provider",
    );
    // 提供者的自身行不独立装配（同一份 apply 只跑一次）
    expect(mountPluginFromPackage).not.toHaveBeenCalledWith(
      expect.anything(),
      "com.test.provider",
      expect.anything(),
      expect.anything(),
    );
    // 内置实现不再装配
    expect(mountPlugin).not.toHaveBeenCalledWith(
      expect.anything(),
      expect.objectContaining({ id: "builtin.search" }),
    );
    expect(usePluginStore.getState().composition?.bindings["builtin.search"]).toMatchObject({
      implId: "com.test.provider",
      source: "plugin",
    });
  });

  it("用户层钉住 default：内置行恢复自身实现，提供者自身行恢复独立装配", async () => {
    vi.mocked(readGlobalConfig).mockResolvedValue({
      config: { recentVaults: [], compositionPatches: { "builtin.search": "default" } },
      corruptBackup: null,
    });
    vi.mocked(pluginList).mockResolvedValue({
      rows: [builtinRow("builtin.search"), providerRow("com.test.provider", [{ target: "builtin.search" }])],
    });
    await usePluginStore.getState().load();

    expect(mountPlugin).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ id: "builtin.search" }));
    expect(mountPluginFromPackage).toHaveBeenCalledWith(
      expect.anything(),
      "com.test.provider",
      "src/index.ts",
      "com.test.provider",
    );
    expect(usePluginStore.getState().composition?.bindings["builtin.search"]).toMatchObject({
      implId: "builtin.search",
      source: "user",
    });
  });

  it("用户层指定的实现不可用（未安装）：回退本行默认实现并给出可读原因", async () => {
    vi.mocked(readGlobalConfig).mockResolvedValue({
      config: { recentVaults: [], compositionPatches: { "builtin.search": "com.absent.provider" } },
      corruptBackup: null,
    });
    vi.mocked(pluginList).mockResolvedValue({ rows: [builtinRow("builtin.search")] });
    await usePluginStore.getState().load();

    const binding = usePluginStore.getState().composition?.bindings["builtin.search"];
    expect(binding?.implId).toBe("builtin.search");
    expect(binding?.problem).toContain("com.absent.provider");
    expect(mountPlugin).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ id: "builtin.search" }));
  });

  it("setCompositionImpl：整表落 global.json + 带版本广播 + 定向重挂；清空整字段删除", async () => {
    // 真实替身：写盘后读回同一份（返回写后完整读取结果，含装配版本），镜像由加载阶段从磁盘重建
    let stored: Record<string, string> = {};
    vi.mocked(readGlobalConfig).mockImplementation(async () => ({
      config: { recentVaults: [], compositionPatches: stored },
      corruptBackup: null,
    }));
    vi.mocked(updateGlobalConfig).mockImplementation(async (patch) => {
      stored = patch.compositionPatches ? { ...patch.compositionPatches } : {};
      return {
        config: { recentVaults: [], compositionPatches: stored },
        corruptBackup: null,
        assemblyVersion: 11,
      };
    });
    vi.mocked(getAssemblyVersion).mockResolvedValue(11);

    await usePluginStore.getState().setCompositionImpl("builtin.search", "com.test.provider");

    expect(updateGlobalConfig).toHaveBeenCalledWith({
      compositionPatches: { "builtin.search": "com.test.provider" },
    });
    // 广播载荷 = 写后装配版本；本窗口随后按新裁决定向重挂（不跑无条件全量重载）
    expect(emitCompositionChanged).toHaveBeenCalledWith(11);
    expect(pluginList).toHaveBeenCalledTimes(1);
    expect(usePluginStore.getState().compositionPatches).toEqual({ "builtin.search": "com.test.provider" });

    await usePluginStore.getState().setCompositionImpl("builtin.search", null);
    expect(updateGlobalConfig).toHaveBeenLastCalledWith({ compositionPatches: null });
    expect(usePluginStore.getState().compositionPatches).toEqual({});
  });

  it("setCompositionImpl 写盘失败：不落地、原样上抛", async () => {
    vi.mocked(updateGlobalConfig).mockRejectedValue(new Error("写盘失败"));
    await expect(
      usePluginStore.getState().setCompositionImpl("builtin.search", "com.test.provider"),
    ).rejects.toThrow("写盘失败");
    expect(usePluginStore.getState().compositionPatches).toEqual({});
    expect(emitCompositionChanged).not.toHaveBeenCalled();
  });
});

describe("装配版本追平", () => {
  /** 基线：版本 10 的行集合（provider 接管 builtin.search）。监听器在首次 load 注册，
   *  之后测试经 assemblyBroadcast 直接派发跨窗口广播。 */
  beforeEach(async () => {
    // handler 必须已注册（注册发生在文件内首次 load；数组被清空会让派发空转、用例假绿）
    expect(assemblyBroadcast.pluginChanged.length).toBeGreaterThan(0);
    expect(assemblyBroadcast.compositionChanged.length).toBeGreaterThan(0);
    vi.mocked(pluginList).mockResolvedValue({
      rows: [builtinRow("builtin.search"), providerRow("com.test.provider", [{ target: "builtin.search" }])],
      assemblyVersion: 10,
    });
    vi.mocked(getAssemblyVersion).mockResolvedValue(10);
    await usePluginStore.getState().load();
    vi.mocked(pluginList).mockClear();
    vi.mocked(mountPlugin).mockClear();
    vi.mocked(mountPluginFromPackage).mockClear();
    vi.mocked(unmountPlugin).mockClear();
    vi.mocked(getAssemblyVersion).mockClear();
  });

  /** provider 行状态翻转为停用（装配输入变更的模拟）。 */
  const rowsWithProviderDisabled = (version: number) => ({
    rows: [
      builtinRow("builtin.search"),
      { ...providerRow("com.test.provider", [{ target: "builtin.search" }]), enabled: false },
    ],
    assemblyVersion: version,
  });

  it("同版本重复广播：不触发任何重载", async () => {
    for (const h of assemblyBroadcast.compositionChanged) h(10);
    for (const h of assemblyBroadcast.pluginChanged) h({ id: "com.test.provider", version: 10 });
    // 版本比对在排队前完成（无追平任务入队）：微任务冲刷后仍无任何取数与重挂
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(pluginList).not.toHaveBeenCalled();
    expect(mountPlugin).not.toHaveBeenCalled();
    expect(unmountPlugin).not.toHaveBeenCalled();
  });

  it("漏发一次广播：更高版本仍追平；迟到旧广播忽略", async () => {
    vi.mocked(pluginList).mockResolvedValue(rowsWithProviderDisabled(12));
    vi.mocked(getAssemblyVersion).mockResolvedValue(12);
    // 版本 11 的广播丢失，直接收到 12：按单调比对追平，不要求逐版对账
    for (const h of assemblyBroadcast.pluginChanged) h({ id: "com.test.provider", version: 12 });
    await vi.waitFor(() => {
      expect(mountPlugin).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ id: "builtin.search" }));
    });
    // 迟到的版本 11：游标已到 12，忽略（不产生第二轮取数）
    vi.mocked(pluginList).mockClear();
    for (const h of assemblyBroadcast.compositionChanged) h(11);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(pluginList).not.toHaveBeenCalled();
  });

  it("高版本广播：只重挂裁决变化的行，未受影响行不重启", async () => {
    vi.mocked(pluginList).mockResolvedValue(rowsWithProviderDisabled(11));
    vi.mocked(getAssemblyVersion).mockResolvedValue(11);
    for (const h of assemblyBroadcast.pluginChanged) h({ id: "com.test.provider", version: 11 });
    await vi.waitFor(() => {
      expect(mountPlugin).toHaveBeenCalledWith(expect.anything(), expect.objectContaining({ id: "builtin.search" }));
    });
    // builtin.search 由 provider 实现回退内置实现：先撤旧挂载再重挂内置实现
    expect(unmountPlugin).toHaveBeenCalledWith(expect.anything(), "builtin.search");
    // provider 停用后接管失效且自身行不装配：无任何磁盘包挂载
    expect(mountPluginFromPackage).not.toHaveBeenCalled();
    // 恰好一轮取数（before/after 版本一致，无重跑循环）
    expect(pluginList).toHaveBeenCalledTimes(1);
  });

  it("拉取期间装配版本又前移：重跑追平至取数与版本一致后记游标", async () => {
    vi.mocked(getAssemblyVersion)
      .mockResolvedValueOnce(12) // 第一轮 before
      .mockResolvedValueOnce(13) // 第一轮 after（拉取期间他窗又改）→ 重跑
      .mockResolvedValue(13); // 第二轮起稳定
    vi.mocked(pluginList).mockResolvedValue(rowsWithProviderDisabled(13));
    for (const h of assemblyBroadcast.pluginChanged) h({ id: "com.test.provider", version: 13 });
    await vi.waitFor(() => expect(pluginList).toHaveBeenCalledTimes(2));
    // 重跑不重复重挂：第一轮已把装配对齐，第二轮 diff 为空。
    // unmount 计 2 次均在第一轮：diff 拆除 1 次 + spawn 重挂前的自拆除 1 次（防重复注册）。
    expect(mountPlugin).toHaveBeenCalledTimes(1);
    expect(unmountPlugin).toHaveBeenCalledTimes(2);
  });

  it("尚无装配计划（composition 为 null）：追平走全量重载，不在串行队列上死锁", async () => {
    // boot 首次装载失败过的窗口：composition 为 null、游标停在旧值
    usePluginStore.setState({ composition: null, assemblyCursor: 0 });
    vi.mocked(pluginList).mockResolvedValue({
      rows: [builtinRow("builtin.search"), providerRow("com.test.provider", [{ target: "builtin.search" }])],
      assemblyVersion: 11,
    });
    vi.mocked(getAssemblyVersion).mockResolvedValue(11);
    for (const h of assemblyBroadcast.compositionChanged) h(11);
    // 死锁回归断言：若走 syncCompositionMounts 的 load() 兜底会在本队列上自等，waitFor 超时
    await vi.waitFor(() => expect(pluginList).toHaveBeenCalledTimes(1));
    expect(usePluginStore.getState().composition).not.toBeNull();
  });

  it("连续多轮拉取期间都有新变更：退回全量重载兜底", async () => {
    let version = 10;
    vi.mocked(getAssemblyVersion).mockImplementation(async () => ++version);
    vi.mocked(pluginList).mockResolvedValue({
      rows: [builtinRow("builtin.search"), providerRow("com.test.provider", [{ target: "builtin.search" }])],
      assemblyVersion: 99,
    });
    for (const h of assemblyBroadcast.pluginChanged) h({ id: "com.test.provider", version: 11 });
    // 追平循环上限 3 轮（每轮取数 1 次）后走全量重载（第 4 次取数）
    await vi.waitFor(() => expect(pluginList).toHaveBeenCalledTimes(4));
  });
});

describe("安装落位", () => {
  const installedRow = (id: string): PluginRow => ({
    id,
    name: id,
    version: "1.0.0",
    type: "panel",
    installDir: `/tmp/${id}`,
    sourceKind: "market",
    enabled: false,
    manifest: {
      name: id,
      version: "1.0.0",
      main: "main.js",
      atelyx: { name: id, type: "panel" },
    },
  });

  it("installLocal 透传本地路径给服务", async () => {
    vi.mocked(pluginInstallLocal).mockResolvedValueOnce(installedRow("com.test.localscope") as never);
    await usePluginStore.getState().installLocal("/tmp/src");
    expect(pluginInstallLocal).toHaveBeenCalledWith("/tmp/src");
  });

  it("installGit 透传归一化地址给服务", async () => {
    vi.mocked(pluginInstall).mockResolvedValueOnce(installedRow("com.test.git") as never);
    await usePluginStore.getState().installGit("owner/repo");
    expect(pluginInstall).toHaveBeenCalledWith("https://github.com/owner/repo.git");
  });

  it("pickLocalPluginDir 返回选择器结果，取消返回 null", async () => {
    vi.mocked(pickDirectory).mockResolvedValueOnce("/tmp/plugin-src");
    await expect(usePluginStore.getState().pickLocalPluginDir()).resolves.toBe("/tmp/plugin-src");
    vi.mocked(pickDirectory).mockResolvedValueOnce(null);
    await expect(usePluginStore.getState().pickLocalPluginDir()).resolves.toBeNull();
  });
});

describe("插件进程随运行时结束", () => {
  /** 在册一个该插件的进程（登记表以内核上下文为键，与 store 传的同一对象）。 */
  function track(id: string, pid: number): void {
    trackPluginProcess(fakeKernel.ctx, id, pid);
  }

  it("停用插件结束它启动的进程", async () => {
    usePluginStore.setState({
      plugins: { "com.test.proc": row({ id: "com.test.proc", enabled: true }) },
    });
    track("com.test.proc", 4242);

    await usePluginStore.getState().setEnabled("com.test.proc", false);

    expect(killProcessTree).toHaveBeenCalledWith(4242);
  });

  it("卸载插件结束它启动的进程", async () => {
    usePluginStore.setState({
      plugins: { "com.test.proc-uninstall": row({ id: "com.test.proc-uninstall", enabled: true }) },
    });
    track("com.test.proc-uninstall", 4243);

    await usePluginStore.getState().uninstall("com.test.proc-uninstall");

    expect(killProcessTree).toHaveBeenCalledWith(4243);
  });

  it("全量重载结束仍启用的插件在跑的进程（不跨重载保留）", async () => {
    // 插件行仍在列表且启用：重挂前的拆除（spawn → stopPlugin）必须结束上一轮进程，
    // 否则每次重载都会多出一个无人认领的重复服务。
    vi.mocked(pluginList).mockResolvedValueOnce({
      rows: [
        {
          id: "com.test.proc-mounted",
          name: "com.test.proc-mounted",
          version: "1.0.0",
          type: "panel",
          installDir: "/tmp/com.test.proc-mounted",
          sourceKind: "market",
          enabled: true,
          manifest: {
            name: "com.test.proc-mounted",
            version: "1.0.0",
            main: "main.js",
            atelyx: { name: "proc-mounted", type: "panel" },
          },
        } as unknown as PluginRow,
      ],
    });
    vi.mocked(mountedPluginIds).mockReturnValueOnce(["com.test.proc-mounted"]);
    track("com.test.proc-mounted", 4245);

    await usePluginStore.getState().load();

    expect(killProcessTree).toHaveBeenCalledWith(4245);
  });

  it("全量重载收尾结束未挂载插件的残留进程（含插件已不在列表）", async () => {
    // 插件行已从列表消失（跨窗口卸载/apply 抛错），但本窗口登记表里还有它的进程
    track("com.test.proc-reload", 4244);

    await usePluginStore.getState().load();

    expect(killProcessTree).toHaveBeenCalledWith(4244);
  });
});

describe("协作插件消息入站桥", () => {
  it("load 常驻接线：plugin-msg 入站帧按订阅注册表投递（不经事件广播）", async () => {
    await usePluginStore.getState().load();
    expect(emitPluginEvent).not.toHaveBeenCalled();

    const received: Array<[number, unknown]> = [];
    const off = registerPluginChannel("com.test.a:ch", (peerId, payload) =>
      received.push([peerId, payload]),
    );
    dispatchCollabChannel("plugin-msg", 7, "com.test.a:ch", { cmd: "start" });
    expect(received).toEqual([[7, { cmd: "start" }]]);
    // 未订阅频道不投递（无调用、无事件）
    dispatchCollabChannel("plugin-msg", 8, "com.test.b:ch", { cmd: "x" });
    expect(received).toEqual([[7, { cmd: "start" }]]);
    expect(emitPluginEvent).not.toHaveBeenCalled();
    off();
  });
});

describe("入口图顺序（视图候选按组合行位置排）", () => {
  it("注册到达顺序与组合行顺序不一致时，以组合行顺序为准", async () => {
    // builtin.canvas 在默认组合里排在 builtin.note 之前；这里先注册 note 的视图再注册 canvas 的，
    // 入口图仍须按行位置排出 canvas 在前（不受槽注册到达时间影响）。
    const offNote = registerViewSlot("probe-note", "builtin.note", { label: "N", component: () => null });
    const offCanvas = registerViewSlot("probe-canvas", "builtin.canvas", { label: "C", component: () => null });
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
        {
          id: "builtin.canvas",
          name: "画布",
          version: "1.0.0",
          type: "panel",
          installDir: "",
          sourceKind: "builtin",
          enabled: true,
          manifest: { name: "builtin.canvas", version: "1.0.0", main: "builtin", atelyx: { name: "画布", type: "panel" } },
        },
        {
          id: "builtin.note",
          name: "笔记",
          version: "1.0.0",
          type: "panel",
          installDir: "",
          sourceKind: "builtin",
          enabled: true,
          manifest: { name: "builtin.note", version: "1.0.0", main: "builtin", atelyx: { name: "笔记", type: "panel" } },
        },
      ],
    });
    await usePluginStore.getState().load();

    const kinds = usePluginStore.getState().pluginViewKinds().filter((k) => k.startsWith("probe-"));
    expect(kinds).toEqual(["probe-canvas", "probe-note"]);

    offCanvas();
    offNote();
  });
});

describe("组合接管实现可用性", () => {
  it("提供者清单无效：不可用作实现，目标行回退默认实现并给出可读原因", async () => {
    vi.mocked(pluginList).mockResolvedValue({
      rows: [
        {
          id: "builtin.search",
          name: "builtin.search",
          version: "1.0.0",
          type: "panel",
          installDir: "",
          sourceKind: "builtin",
          enabled: true,
          manifest: { name: "builtin.search", version: "1.0.0", main: "builtin", atelyx: { name: "s", type: "panel" } },
        },
        {
          id: "com.test.bad",
          name: "bad",
          version: "1.0.0",
          type: "background",
          installDir: "/tmp/bad",
          sourceKind: "market",
          enabled: true,
          // 缺 main（非 theme）→ 清单无效
          manifest: { name: "com.test.bad", version: "1.0.0", atelyx: { name: "b", type: "background" } },
        },
      ],
    });
    vi.mocked(readGlobalConfig).mockResolvedValue({
      config: { recentVaults: [], compositionPatches: { "builtin.search": "com.test.bad" } },
      corruptBackup: null,
    });
    await usePluginStore.getState().load();

    const binding = usePluginStore.getState().composition?.bindings["builtin.search"];
    expect(binding?.implId).toBe("builtin.search");
    expect(binding?.problem).toContain("com.test.bad");
    expect(mountPluginFromPackage).not.toHaveBeenCalled();
  });
});
