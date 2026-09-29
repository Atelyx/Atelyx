/**
 * 改名/移动前统一 flush 测试（stores/vaultStore + appStore）。
 *
 * 改名/移动会迁移磁盘路径：防抖窗口内的未落盘编辑若不先落盘，保存会打到旧路径
 * （协作空间 404 后整写兜底会复活旧文件）。断言各改名/移动入口在发出落盘变更服务
 * 调用之前已 await flushAllPending（顺序守卫，非仅调用过）。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

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
vi.mock("@/utils/vaultEvents", () => ({
  emitVaultEvent: vi.fn(),
  emitVaultEventAsync: vi.fn(async () => {}),
}));

import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore } from "@/stores/vaultStore";
import { renameCanvasVault, moveCanvasVault, renameFolder, renameNote } from "@/services/vault";
import { moveTableVault, renameTableVault } from "@/services/table";

/** 顺序记录：flush 与落盘变更服务的先后即被测行为。 */
let order: string[];

function pushFlush(): void {
  order.push("flush");
}

beforeEach(() => {
  order = [];
  vi.clearAllMocks();
  // flushAllPending 替换为可观察桩（真实实现会触发各域 store 与内核生命周期，测试不可达）
  useAppStore.setState({
    flushAllPending: async () => {
      pushFlush();
      // 异步让出：若调用方不 await，后续服务调用会先于 flush 记录，顺序断言即失败
      await new Promise((r) => setTimeout(r, 0));
    },
    loadList: vi.fn(async () => {}),
  });
  // 设置项迁移依赖服务层（提示词标记/文件夹颜色），替换为无操作
  useSettingsStore.setState({
    remapPromptNote: vi.fn(async () => {}),
    remapAgentPromptNote: vi.fn(async () => {}),
    remapPromptNotesByDir: vi.fn(async () => {}),
    remapAgentPromptNotesByDir: vi.fn(async () => {}),
    remapFolderColorsByDir: vi.fn(async () => {}),
  });
  vi.mocked(renameTableVault).mockImplementation(async () => {
    order.push("renameTable");
  });
  vi.mocked(renameNote).mockImplementation(async () => {
    order.push("renameNote");
    return { rewritten: [] };
  });
  vi.mocked(renameFolder).mockImplementation(async () => {
    order.push("renameFolder");
    return { rewritten: [] };
  });
  vi.mocked(renameCanvasVault).mockImplementation(async () => {
    order.push("renameCanvas");
  });
  vi.mocked(moveCanvasVault).mockImplementation(async () => {
    order.push("moveCanvas");
  });
  vi.mocked(moveTableVault).mockImplementation(async () => {
    order.push("moveTable");
  });
});

describe("改名/移动前统一 flush", () => {
  it("renameTable：先 flush 再发改名服务调用", async () => {
    await useVaultStore.getState().renameTable("旧.atb", "新");
    expect(order).toEqual(["flush", "renameTable"]);
  });

  it("moveTable：先 flush 再发移动服务调用", async () => {
    await useVaultStore.getState().moveTable("旧.atb", "目录");
    expect(order).toEqual(["flush", "moveTable"]);
  });

  it("renameNote：先 flush 再发改名服务调用", async () => {
    await useVaultStore.getState().renameNote("旧.md", "新");
    expect(order).toEqual(["flush", "renameNote"]);
  });

  it("renameFolder：先 flush 再发目录改名服务调用", async () => {
    await useVaultStore.getState().renameFolder("目录", "新目录");
    expect(order).toEqual(["flush", "renameFolder"]);
  });

  it("appStore.renameCanvas：先 flush 再发改名服务调用", async () => {
    useAppStore.setState({
      canvases: [{ id: "c1", title: "旧名", file: "旧名.atlx", updatedAt: 1 }],
    });
    await useAppStore.getState().renameCanvas(
      { id: "c1", title: "旧名", file: "旧名.atlx", updatedAt: 1 },
      "新名",
    );
    expect(order).toEqual(["flush", "renameCanvas"]);
  });

  it("appStore.moveCanvas：先 flush 再发移动服务调用", async () => {
    useAppStore.setState({
      canvases: [{ id: "c1", title: "旧名", file: "旧名.atlx", updatedAt: 1 }],
    });
    await useAppStore.getState().moveCanvas(
      { id: "c1", title: "旧名", file: "旧名.atlx", updatedAt: 1 },
      "目标",
    );
    expect(order).toEqual(["flush", "moveCanvas"]);
  });

  // uiState 的「上次打开」随路径同步（改名后不残留旧路径），行为锁定防回归
  it("renameTable 后 uiState 的 lastTableFile 跟随新路径", async () => {
    useUiStateStore.getState().recordOpenFile("table", "旧.atb");
    await useVaultStore.getState().renameTable("旧.atb", "新");
    expect(useUiStateStore.getState().lastTableFile).toBe("新.atb");
  });
});
