/**
 * 文件夹重命名/移动后目录前缀类设置跟随测试（stores/vaultStore + settingsStore，空间身份全链路）。
 *
 * 文件夹图标颜色 / 提示词标记按目录前缀记账（键 = 相对仓库根路径），重命名后键须整体迁移，
 * 且真源（空间 = 团队 meta，个人 = `.atelyx/` 文件）与内存同步。本文件用空间客户端替身跑
 * 真实 settingsStore 读写链（readFolderColors/writeFolderColors 按身份取团队 meta），
 * 锁定「重命名 → remap → 真源写盘」全链；个人仓库由同一 `applyFolderFileChange` 承担。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

// ===== 空间客户端替身（同 settingsStore.space.test.ts：team meta 内存态 + 调用记录） =====

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

// ===== 本地命令替身（空间身份下不应触达；仅满足模块加载） =====

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

// ===== 仓库内容服务替身（renameFolder 只需返回改写清单；树为空） =====

vi.mock("@/services/vault", () => ({
  listVaultTree: vi.fn(async () => []),
  listCanvasesVault: vi.fn(async () => []),
  createCanvasVault: vi.fn(),
  deleteCanvasVault: vi.fn(),
  readCanvasVault: vi.fn(),
  writeCanvasVault: vi.fn(async () => 1),
  openVault: vi.fn(),
  convertWhiteboardToAtlx: vi.fn(),
  createFolder: vi.fn(),
  copyVaultFile: vi.fn(),
  copyVaultFolder: vi.fn(),
  deleteAttachment: vi.fn(),
  deleteFolder: vi.fn(),
  deleteNote: vi.fn(),
  readAttachmentDataUrl: vi.fn(),
  remapSideloads: vi.fn(async () => {}),
  remapSideloadsByDir: vi.fn(async () => {}),
  renameAttachment: vi.fn(),
  renameCanvasVault: vi.fn(async () => undefined),
  moveCanvasVault: vi.fn(async () => undefined),
  renameFolder: vi.fn(async () => ({ rewritten: [] })),
  renameNote: vi.fn(async () => ({ rewritten: [] })),
  scanWikiBacklinks: vi.fn(async () => []),
  scanVaultTags: vi.fn(async () => []),
  rebuildInternalLinks: vi.fn(),
  writeNote: vi.fn(async () => {}),
  fileExists: vi.fn(async () => false),
  readNote: vi.fn(async () => ""),
}));
vi.mock("@/services/history", () => ({
  migrateHistoryFile: vi.fn(async () => {}),
  setHistoryAuthor: vi.fn(),
}));
vi.mock("@/services/table", () => ({
  createTableVault: vi.fn(),
  deleteTableVault: vi.fn(),
  moveTableVault: vi.fn(async () => undefined),
  readTableVault: vi.fn(),
  renameTableVault: vi.fn(async () => undefined),
  writeTableVault: vi.fn(async () => undefined),
}));

type AppStore = typeof import("./appStore");
type Factory = typeof import("@/services/content/factory");
type SettingsStore = typeof import("./settingsStore");
type VaultStore = typeof import("./vaultStore");

let app: AppStore;
let factory: Factory;
let settings: SettingsStore;
let vault: VaultStore;

const SPACE = { kind: "space" as const, serverUrl: "http://s1", spaceId: "sp1" };

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
  vault = await import("./vaultStore");
  // 重命名链路的运行时依赖替换为无操作（flush/画布列表刷新与本测试无关）
  app.useAppStore.setState({
    flushAllPending: async () => {},
    loadList: vi.fn(async () => {}),
  });
  factory.activateContentIdentity(SPACE);
  app.useAppStore.setState({ vaultIdentity: SPACE, vaultRoot: null });
});

describe("文件夹重命名后目录前缀类设置跟随（空间）", () => {
  it("重命名：颜色键随前缀迁移，内存与团队 meta 真源一致", async () => {
    space.state.teamValues["folder-colors"] = JSON.stringify({
      "目录": "#ff0000",
      "目录/子": "#00aa00",
      "其他": "#0000ff",
    });
    await settings.useSettingsStore.getState().loadVaultConfig();
    expect(settings.useSettingsStore.getState().folderColors).toEqual({
      "目录": "#ff0000",
      "目录/子": "#00aa00",
      "其他": "#0000ff",
    });

    await vault.useVaultStore.getState().renameFolder("目录", "新目录");

    const colors = settings.useSettingsStore.getState().folderColors;
    expect(colors).toEqual({
      "新目录": "#ff0000",
      "新目录/子": "#00aa00",
      "其他": "#0000ff",
    });
    // 真源（团队 meta）同步迁移：内存与落点不得分叉
    expect(JSON.parse(space.state.teamValues["folder-colors"])).toEqual(colors);
  });

  it("移动：颜色键随目标前缀迁移", async () => {
    space.state.teamValues["folder-colors"] = JSON.stringify({ "目录": "#ff0000" });
    await settings.useSettingsStore.getState().loadVaultConfig();

    await vault.useVaultStore.getState().moveFolder("目录", "目标");

    expect(settings.useSettingsStore.getState().folderColors).toEqual({
      "目标/目录": "#ff0000",
    });
    expect(JSON.parse(space.state.teamValues["folder-colors"])).toEqual({
      "目标/目录": "#ff0000",
    });
  });

  it("无颜色配置时重命名不产生 folder-colors 写盘（无改动不写）", async () => {
    await settings.useSettingsStore.getState().loadVaultConfig();

    await vault.useVaultStore.getState().renameFolder("目录", "新目录");

    expect(
      space.state.patchSpaceCalls.filter((p) => "folder-colors" in p.values),
    ).toHaveLength(0);
  });

  it("重命名：提示词标记键随前缀迁移，内存与团队 meta 真源一致", async () => {
    space.state.teamValues["prompt-notes"] = JSON.stringify(["目录/提示词.md", "其他.md"]);
    await settings.useSettingsStore.getState().loadVaultConfig();

    await vault.useVaultStore.getState().renameFolder("目录", "新目录");

    expect(settings.useSettingsStore.getState().promptNotes).toEqual([
      "新目录/提示词.md",
      "其他.md",
    ]);
    expect(JSON.parse(space.state.teamValues["prompt-notes"])).toEqual([
      "新目录/提示词.md",
      "其他.md",
    ]);
  });
});
