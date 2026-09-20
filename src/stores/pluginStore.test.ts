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
  pluginList: vi.fn(async () => ({ rows: [] })),
  pluginSeedDefault: vi.fn(),
  pluginSetEnabled: vi.fn(async () => {}),
  pluginUninstall: vi.fn(async () => {}),
  pluginUpdate: vi.fn(),
  pluginRollback: vi.fn(),
  pluginApproveDir: vi.fn(async () => {}),
  pluginRevokeDir: vi.fn(async () => {}),
  onPluginChanged: vi.fn(async () => () => {}),
}));

vi.mock("@/services/app", () => ({ getAppVersion: vi.fn(async () => "0.0.0") }));
vi.mock("@/services/dialog", () => ({ pickDirectory: vi.fn() }));
// 事件发射以替身替代：本文件只验证接线「plugin-msg 入站 → collab:message 发射」，投递本身在 kernel/events 测试覆盖
vi.mock("@/services/cordis/events", () => ({ emitPluginEvent: vi.fn() }));

// 挂载链路以替身替代：本文件只验证 store 侧的编排（入口选择、阶段归类），内核不参与。
// 内核对象必须**稳定**（同一 ctx 引用）：插件进程登记表以内核上下文为键，每次返回新对象会让
// 登记与结束落在不同表上，测不出真实行为。`vi.hoisted` 让 mock 工厂能引用到它。
const fakeKernel = vi.hoisted(() => ({ ctx: {} }));
vi.mock("@/services/cordis/kernel", () => ({ getKernel: () => fakeKernel }));
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

import { pluginApproveDir, pluginInstall, pluginInstallLocal, pluginList, pluginRevokeDir, pluginRollback, pluginUninstall, pluginUpdate } from "@/services/plugins";
import type { PluginRow } from "@/services/plugins";
import { getAppVersion } from "@/services/app";
import { pickDirectory } from "@/services/dialog";
import { mountPluginFromPackage } from "@/services/cordis/packageMount";
import { killProcessTree } from "@/services/shell";
import { mountedPluginIds } from "@/services/cordis/loader";
import { trackPluginProcess } from "@/services/cordis/pluginProcesses";
import { emitPluginEvent } from "@/services/cordis/events";
import { dispatchCollabChannel } from "@/utils/collabHost";
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

beforeEach(() => {
  vi.clearAllMocks();
  usePluginStore.setState({ plugins: {}, initialized: false, stateError: null });
  vi.mocked(pluginList).mockResolvedValue({ rows: [] });
  vi.mocked(mountedPluginIds).mockReturnValue([]);
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
    expect(pluginUninstall).toHaveBeenCalledWith("com.test.constrained");
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
    expect(pluginUninstall).toHaveBeenCalledWith("com.test.noversion");
  });
});

describe("磁盘包入口选择（宿主产物优先）", () => {
  const diskRow = (id: string, over: Partial<InstalledPlugin>) =>
    row({
      id,
      installDir: "/tmp/plugin",
      sourceKind: "market",
      manifest: {
        id,
        name: id,
        version: "1.0.0",
        type: "panel",
        main: "src/index.ts",
        dependencies: { nanoid: "^5.0.0" },
      },
      ...over,
    });

  it("有打包产物时用产物入口（不再回落清单 main）", async () => {
    usePluginStore.setState({
      plugins: { "com.test.dep": diskRow("com.test.dep", { entry: ".atelyx-dist/entry.js" }) },
    });
    await usePluginStore.getState().setEnabled("com.test.dep", true);
    expect(mountPluginFromPackage).toHaveBeenCalledWith(expect.anything(), "com.test.dep", ".atelyx-dist/entry.js");
    expect(usePluginStore.getState().plugins["com.test.dep"].phase).toBe("active");
  });

  it("无产物时回落清单 main（未声明依赖的插件行为不变）", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.plain": diskRow("com.test.plain", {
          manifest: { id: "com.test.plain", name: "com.test.plain", version: "1.0.0", type: "panel", main: "src/index.ts" },
        }),
      },
    });
    await usePluginStore.getState().setEnabled("com.test.plain", true);
    expect(mountPluginFromPackage).toHaveBeenCalledWith(expect.anything(), "com.test.plain", "src/index.ts");
  });

  it("产物与 main 都缺 → 声明式插件，直接置 active 且不挂载", async () => {
    usePluginStore.setState({
      plugins: {
        "com.test.theme": diskRow("com.test.theme", {
          manifest: { id: "com.test.theme", name: "com.test.theme", version: "1.0.0", type: "theme" },
        }),
      },
    });
    await usePluginStore.getState().setEnabled("com.test.theme", true);
    expect(mountPluginFromPackage).not.toHaveBeenCalled();
    expect(usePluginStore.getState().plugins["com.test.theme"].phase).toBe("active");
  });
});

describe("仓库外目录授权动作", () => {
  const fsManifest = (): PluginPackageJson => ({
    name: "com.test.fs",
    version: "1.0.0",
    main: "index.js",
    atelyx: { type: "background", declaredDirs: ["~/Projects/foo"] },
  });
  const fsRow = (over: Partial<{ approvedDirs: string[] }> = {}): PluginRow => ({
    id: "com.test.fs",
    name: "FS",
    version: "1.0.0",
    type: "background",
    installDir: "/tmp/fs",
    sourceKind: "git",
    enabled: false,
    manifest: fsManifest(),
    ...(over.approvedDirs ? { approvedDirs: over.approvedDirs } : {}),
  });

  it("approveDir 调用服务，重载后行携带批准目录", async () => {
    // 首次列表无批准；批准后 Rust 落盘返回带 approvedDirs 的行（重载由 runVersionOp 触发）
    vi.mocked(pluginList)
      .mockResolvedValueOnce({ rows: [fsRow()] })
      .mockResolvedValueOnce({ rows: [fsRow({ approvedDirs: ["~/Projects/foo"] })] });
    await usePluginStore.getState().load();
    await usePluginStore.getState().approveDir("com.test.fs", "~/Projects/foo");
    expect(pluginApproveDir).toHaveBeenCalledWith("com.test.fs", "~/Projects/foo");
    expect(usePluginStore.getState().plugins["com.test.fs"]?.approvedDirs).toEqual(["~/Projects/foo"]);
  });

  it("revokeDir 调用服务，重载后行移除该目录", async () => {
    // 撤销前列表带 approvedDirs；撤销后 Rust 落盘返回不带该目录的行（重载由 runVersionOp 触发）
    vi.mocked(pluginList)
      .mockResolvedValueOnce({ rows: [fsRow({ approvedDirs: ["~/Projects/foo"] })] })
      .mockResolvedValueOnce({ rows: [fsRow()] });
    await usePluginStore.getState().load();
    expect(usePluginStore.getState().plugins["com.test.fs"]?.approvedDirs).toEqual(["~/Projects/foo"]);
    await usePluginStore.getState().revokeDir("com.test.fs", "~/Projects/foo");
    expect(pluginRevokeDir).toHaveBeenCalledWith("com.test.fs", "~/Projects/foo");
    expect(usePluginStore.getState().plugins["com.test.fs"]?.approvedDirs).toBeUndefined();
  });

  it("行不存在时批准/撤销为 no-op（不调服务不报错）", async () => {
    await usePluginStore.getState().approveDir("com.example.gone", "~/x");
    await usePluginStore.getState().revokeDir("com.example.gone", "~/x");
    expect(pluginApproveDir).not.toHaveBeenCalled();
    expect(pluginRevokeDir).not.toHaveBeenCalled();
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

describe("协作消息入站事件桥", () => {
  it("load 常驻接线：plugin-msg 通道入站帧触发 collab:message 事件发射", async () => {
    await usePluginStore.getState().load();
    expect(emitPluginEvent).not.toHaveBeenCalled();

    dispatchCollabChannel("plugin-msg", 7, "comfyui.remote", { cmd: "start" });

    expect(emitPluginEvent).toHaveBeenCalledWith("collab:message", {
      peerId: 7,
      channel: "comfyui.remote",
      payload: { cmd: "start" },
    });
  });
});
