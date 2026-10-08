// @vitest-environment jsdom
/**
 * 撕裂窗口仓库上下文应答测试（applyOpenFileContext 的仓库级配置加载判据）。
 *
 * 覆盖：仓库级配置的加载判据是**仓库身份**而非 vaultRoot——空间仓库 vaultRoot 恒为
 * null，readVaultSettings 已按身份分流（空间 = 服务端团队元数据），撕裂窗口应答只认
 * vaultRoot 会让空间仓库的撕裂窗口永远没有供应商/Agent 配置（宿主自带 AI 面板撕裂后
 * 同样命中）。本地仓库照旧；未激活仓库（身份 null）不加载。
 *
 * 依赖边界：panelStore 触点外的服务/仓库全部桩掉，settingsStore 用真实模块，
 * loadVaultConfig 以 spy 断言调用（其在空间身份下的取数语义由 settingsStore.space.test 覆盖）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const busState = vi.hoisted(() => ({
  openFileHandler: null as ((payload: unknown) => void) | null,
}));

const calls = vi.hoisted(() => ({ loadFiles: 0, pluginLoad: 0 }));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => {},
  emit: async () => {},
}));

vi.mock("@/services/windowBus", () => ({
  onPanelLayoutOp: async () => () => {},
  emitPanelLayoutOp: async () => {},
}));

vi.mock("@/services/hostContext", () => ({
  onOpenFileContextChanged: async (handler: (payload: never) => void) => {
    busState.openFileHandler = handler as (payload: unknown) => void;
    return () => {};
  },
  getOpenFileContext: async () => null,
  setOpenFileContext: async () => {},
}));

vi.mock("@/services/layout", () => ({
  layoutBootstrap: async () => null,
  layoutFlush: async () => null,
  layoutOp: async () => ({}),
  onLayoutBroadcast: async () => () => {},
  uiStatePatch: async () => null,
  dragEnd: async () => {},
  dragHit: async () => {},
  dragUpdate: async () => {},
  onDragSession: async () => () => {},
  panelWindowClosed: async () => {},
}));

vi.mock("@/services/window", () => ({
  ackExitFlushDone: async () => {},
  getCurrentOuterPosition: async () => ({ x: 0, y: 0 }),
  getCurrentWindowLabel: () => "panel-w1",
  isMouseLeftDown: async () => null,
  onCloseRequested: async () => () => {},
  onTrayExitRequested: async () => () => {},
  onWindowMoved: async () => () => {},
  setWindowTitle: async () => {},
}));

vi.mock("@/services/viewHandoff", () => ({
  cacheCanvasViewport: () => {},
  emitCanvasViewportHandoff: async () => {},
  getCachedCanvasViewport: () => null,
  onCanvasViewportHandoff: async () => () => {},
}));

vi.mock("@/services/cordis/slots", () => ({
  pluginViewLabel: (view: string) => view,
}));

vi.mock("@/stores/pluginStore", () => ({
  usePluginStore: {
    getState: () => ({
      load: async () => {
        calls.pluginLoad++;
      },
    }),
  },
}));

vi.mock("@/stores/vaultStore", () => ({
  useVaultStore: {
    getState: () => ({
      loadFiles: async () => {
        calls.loadFiles++;
      },
    }),
  },
}));

vi.mock("@/stores/collabStore", () => ({
  useCollabStore: {
    getState: () => ({ connected: false, pluginDemand: 0, init: () => {}, dispose: () => {} }),
    subscribe: () => () => {},
  },
}));

type PanelStore = typeof import("./panelStore");
type AppStore = typeof import("./appStore");
type SettingsStore = typeof import("./settingsStore");

let panel: PanelStore;
let app: AppStore;
let settings: SettingsStore;
let loadVaultConfigSpy: ReturnType<typeof vi.spyOn>;

const SPACE = { kind: "space" as const, serverUrl: "http://s1", spaceId: "sp1" };

const basePayload = {
  vaultName: "测试仓库",
  currentCanvasFile: null,
  currentNoteFile: null,
  currentTableFile: null,
  currentNoteTitle: "",
  currentTableTitle: "",
};

beforeEach(async () => {
  vi.resetModules();
  busState.openFileHandler = null;
  calls.loadFiles = 0;
  calls.pluginLoad = 0;
  // 面板 bootstrap 内有超时定时器（布局/上下文限时），用假时钟防挂起句柄泄漏
  vi.useFakeTimers();
  panel = await import("./panelStore");
  app = await import("./appStore");
  settings = await import("./settingsStore");
  loadVaultConfigSpy = vi.spyOn(settings.useSettingsStore.getState(), "loadVaultConfig");
  await panel.usePanelStore.getState().initPanel();
});

afterEach(() => {
  vi.useRealTimers();
});

function dispatchOpenFileChanged(payload: unknown): void {
  expect(busState.openFileHandler, "initPanel 应已注册仓库上下文应答监听").toBeTypeOf("function");
  (busState.openFileHandler as (payload: unknown) => void)(payload);
}

/** 排空 microtask 队列：上下文应答的加载链为顺序 await（配置 → 文件树 → 领域 → 插件），
 *  断言前须让整条链跑完（纯 microtask，无定时器参与，轮次覆盖链内 await 深度即可）。 */
const settle = async (): Promise<void> => {
  for (let i = 0; i < 50; i++) await Promise.resolve();
};

describe("撕裂窗口仓库上下文应答：仓库级配置加载判据", () => {
  it("空间仓库（vaultRoot 恒 null）：按身份加载仓库级配置", async () => {
    dispatchOpenFileChanged({ ...basePayload, vaultRoot: null, vaultIdentity: SPACE });
    await settle();

    expect(app.useAppStore.getState().vaultIdentity).toEqual(SPACE);
    expect(loadVaultConfigSpy).toHaveBeenCalledTimes(1);
    expect(calls.loadFiles).toBe(1);
    expect(calls.pluginLoad).toBe(1);
  });

  it("本地仓库（vaultRoot 非空）：照旧加载", async () => {
    dispatchOpenFileChanged({
      ...basePayload,
      vaultRoot: "E:/repo",
      vaultIdentity: { kind: "local", root: "E:/repo" },
    });
    await settle();

    expect(loadVaultConfigSpy).toHaveBeenCalledTimes(1);
    expect(calls.loadFiles).toBe(1);
  });

  it("未激活仓库（身份 null）：不加载配置与文件树", async () => {
    dispatchOpenFileChanged({ ...basePayload, vaultRoot: null, vaultIdentity: null });
    await settle();

    expect(loadVaultConfigSpy).not.toHaveBeenCalled();
    expect(calls.loadFiles).toBe(0);
  });
});
