/**
 * 远端重命名跟随测试（协作 renamed 帧 → 目录前缀类设置迁移）。
 *
 * renamed 帧不区分文件/目录：文件夹改名须前缀整体迁移（含目录自身的键），单文件改名须精确命中
 * （提示词标记/Agent 引用/上次打开）。设置侧只迁内存不写真源（真源已由发起方写好，断言不产生
 * 团队 meta 写盘）；展开集合/上次打开走 uiState 同步。装配与 builtin.files 的 collabWiring 同口径。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

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
  setSpaceSessionExpiredHandler: () => undefined,
  createSpaceClient: (_serverUrl: string) => space.makeClient(_serverUrl),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

type SettingsStore = typeof import("./settingsStore");
type UiStateStore = typeof import("./uiStateStore");
type Factory = typeof import("@/services/content/factory");
type AppStore = typeof import("./appStore");
type CollabHost = typeof import("@/utils/collabHost");

let app: AppStore;
let factory: Factory;
let settings: SettingsStore;
let uiState: UiStateStore;
let collabHost: CollabHost;

const SPACE = { kind: "space" as const, serverUrl: "http://s1", spaceId: "sp1" };

/** 测试期注册的协作接线（beforeEach 重置模块后逐个撤销）。 */
const offs: Array<() => void> = [];

beforeEach(async () => {
  vi.resetModules();
  space.state.teamValues = {};
  space.state.myValues = {};
  space.state.patchSpaceCalls = [];
  await import("./noteSessionStore");
  await import("./pluginStore");
  app = await import("./appStore");
  factory = await import("@/services/content/factory");
  settings = await import("./settingsStore");
  uiState = await import("./uiStateStore");
  // 注册与投递必须来自同一 collabHost 实例（resetModules 后动态 import 与顶层静态 import 不同实例）
  collabHost = await import("@/utils/collabHost");
  factory.activateContentIdentity(SPACE);
  app.useAppStore.setState({ vaultIdentity: SPACE, vaultRoot: null });
  // 装配与 builtin.files 的 collabWiring 同口径
  const off = collabHost.registerCollabRenamed((oldPath, newPath) => {
    settings.useSettingsStore.getState().followRemotePathRename(oldPath, newPath);
    uiState.useUiStateStore.getState().renameByDir(oldPath, newPath);
  });
  offs.push(off);
});

describe("远端重命名跟随：目录前缀类设置", () => {
  it("远端文件夹改名：颜色键前缀迁移（含目录自身），仅内存不写团队 meta", async () => {
    space.state.teamValues["folder-colors"] = JSON.stringify({
      "目录": "#ff0000",
      "目录/子": "#00aa00",
      "其他": "#0000ff",
    });
    await settings.useSettingsStore.getState().loadVaultConfig();

    collabHost.dispatchCollabRenamed("目录", "新目录");

    expect(settings.useSettingsStore.getState().folderColors).toEqual({
      "新目录": "#ff0000",
      "新目录/子": "#00aa00",
      "其他": "#0000ff",
    });
    // 真源已由发起方写好：本地跟随不得产生团队 meta 写盘
    expect(space.state.patchSpaceCalls).toHaveLength(0);
  });

  it("远端文件夹改名：提示词标记前缀迁移 + 展开集合迁移（含目录自身）", async () => {
    space.state.teamValues["prompt-notes"] = JSON.stringify(["目录/提示词.md"]);
    await settings.useSettingsStore.getState().loadVaultConfig();
    uiState.useUiStateStore.setState({ fileExplorerExpanded: new Set(["目录", "目录/子", "其他"]) });

    collabHost.dispatchCollabRenamed("目录", "新目录");

    expect(settings.useSettingsStore.getState().promptNotes).toEqual(["新目录/提示词.md"]);
    expect([...uiState.useUiStateStore.getState().fileExplorerExpanded].sort()).toEqual([
      "其他",
      "新目录",
      "新目录/子",
    ]);
  });

  it("远端单文件改名：提示词标记与 Agent 引用精确命中迁移，颜色键不动", async () => {
    space.state.teamValues["folder-colors"] = JSON.stringify({ "目录": "#ff0000" });
    space.state.teamValues["prompt-notes"] = JSON.stringify(["目录/旧.md"]);
    space.state.teamValues["agents"] = JSON.stringify([
      { id: "a1", name: "A", systemPromptFile: "目录/旧.md" },
      { id: "a2", name: "B", systemPromptFile: "目录/他.md" },
    ]);
    await settings.useSettingsStore.getState().loadVaultConfig();

    collabHost.dispatchCollabRenamed("目录/旧.md", "目录/新.md");

    const s = settings.useSettingsStore.getState();
    expect(s.folderColors).toEqual({ "目录": "#ff0000" });
    expect(s.promptNotes).toEqual(["目录/新.md"]);
    // 预置 Agent 排在清单前部，按 id 定位自定义 Agent
    const byId = (id: string) => s.agents.find((a) => a.id === id);
    expect(byId("a1")?.systemPromptFile).toBe("目录/新.md");
    expect(byId("a2")?.systemPromptFile).toBe("目录/他.md");
    expect(space.state.patchSpaceCalls).toHaveLength(0);
  });

  it("远端单文件改名：上次打开记录精确命中迁移", () => {
    uiState.useUiStateStore.getState().recordOpenFile("note", "目录/笔记.md");

    collabHost.dispatchCollabRenamed("目录/笔记.md", "目录/改名.md");

    expect(uiState.useUiStateStore.getState().lastNoteFile).toBe("目录/改名.md");
  });

  it("发起者回放帧：键已迁移后重复跟随为 no-op", async () => {
    space.state.teamValues["folder-colors"] = JSON.stringify({ "目录": "#ff0000" });
    await settings.useSettingsStore.getState().loadVaultConfig();

    collabHost.dispatchCollabRenamed("目录", "新目录");
    collabHost.dispatchCollabRenamed("目录", "新目录");

    expect(settings.useSettingsStore.getState().folderColors).toEqual({ "新目录": "#ff0000" });
  });
});
