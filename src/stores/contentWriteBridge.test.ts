/**
 * 跨窗口写盘感知桥分发测试（stores/contentWriteBridge）。
 * 只覆盖分发语义：仓库身份守卫、按扩展分域、画布/表格的干净态重读与脏态保留、
 * 路径跟随（title 漂移/移动/目录改名）与笔记换路簿记、引用文件补读。
 * 域内收敛语义（笔记采纳/保留本地、画布重读回环）由各 store 的既有测试覆盖。
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

// 桥模块导入时注册 Tauri 事件监听；测试环境替换为 no-op。
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ label: "test" }) }));
// 路径跟随触发的簿记（adoptRemoteNoteMigration → loadFiles 读树）与打开会话的读盘走 invoke。
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async (cmd: string) => (cmd === "list_vault_tree" ? [] : "")),
}));
// 换路簿记替换为可观察的 mock：域内细节（缓存/撤销栈/协作文档）由 noteCollabStore 既有链路保证。
vi.mock("@/stores/noteCollabStore", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/stores/noteCollabStore")>();
  return { ...actual, adoptRemoteNoteRelocate: vi.fn() };
});

import { activateContentIdentity, deactivateContentVault } from "@/services/content/factory";
import { useAppStore } from "@/stores/appStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useNoteStore } from "@/stores/noteStore";
import { useTableStore } from "@/stores/tableStore";
import { noteSurfaceProvider, openNoteSessionFiles } from "@/stores/noteSessionStore";
import { adoptRemoteNoteRelocate } from "@/stores/noteCollabStore";
import {
  handleVaultContentChanged,
  type VaultContentChangedPayload,
} from "@/stores/contentWriteBridge";

const ROOT = "E:/仓库";
const { invoke } = vi.mocked(await import("@tauri-apps/api/core"));

function dispatch(changes: VaultContentChangedPayload["changes"], root = ROOT): void {
  handleVaultContentChanged({ root, origin: "其他窗口", changes });
}

/** 伪装画布动作可观察（reloadFromDisk 为稳定引用，spy 后续 setState 合并保留引用）。 */
function spyCanvasReload(): ReturnType<typeof vi.fn> {
  const spy = vi.fn().mockResolvedValue(undefined);
  useCanvasStore.setState({ reloadFromDisk: spy as unknown as never });
  return spy;
}

function spyTableReload(): ReturnType<typeof vi.fn> {
  const spy = vi.fn().mockResolvedValue(undefined);
  useTableStore.setState({ reloadFromDisk: spy as unknown as never });
  return spy;
}

beforeEach(() => {
  vi.clearAllMocks();
  invoke.mockResolvedValue([]);
  activateContentIdentity({ kind: "local", root: ROOT });
  for (const file of openNoteSessionFiles()) noteSurfaceProvider.close(file);
  useCanvasStore.setState({
    canvasFile: null,
    dirty: false,
    saving: false,
    nodes: [],
    reloadFromDisk: undefined as never,
  });
  useTableStore.setState({
    tableFile: null,
    dirty: false,
    saving: false,
    rows: [],
    reloadFromDisk: undefined as never,
  });
  useAppStore.setState({ currentCanvasFile: null, currentTableFile: null, currentNoteFile: null });
  useNoteStore.setState({ noteContents: {}, externalNoteEdits: {}, pendingNoteContent: {} });
});

describe("仓库身份守卫", () => {
  it("root 不符（切换在途的迟到广播）不分发", () => {
    const reload = spyCanvasReload();
    useCanvasStore.setState({ canvasFile: "a.atlx" });
    dispatch([{ kind: "write", file: "a.atlx" }], "E:/别的仓库");
    expect(reload).not.toHaveBeenCalled();
  });

  it("无激活身份不分发", () => {
    deactivateContentVault();
    const reload = spyCanvasReload();
    useCanvasStore.setState({ canvasFile: "a.atlx" });
    dispatch([{ kind: "write", file: "a.atlx" }]);
    expect(reload).not.toHaveBeenCalled();
  });

  it("root 分隔符/大小写书写差异不误拒（Rust PathBuf 与前端身份串）", () => {
    useCanvasStore.setState({ canvasFile: "a.atlx" });
    const reload = spyCanvasReload();
    dispatch([{ kind: "write", file: "a.atlx" }], "e:\\仓库");
    expect(reload).toHaveBeenCalledTimes(1);
  });
});

describe("画布写盘对账", () => {
  it("本窗口正打开该画布且干净：重读磁盘", () => {
    useCanvasStore.setState({ canvasFile: "a.atlx" });
    const reload = spyCanvasReload();
    dispatch([{ kind: "write", file: "a.atlx" }]);
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("有脏或写盘在途：保留本地，不重读（下次自动保存按本地落盘）", () => {
    useCanvasStore.setState({ canvasFile: "a.atlx" });
    const reload = spyCanvasReload();
    useCanvasStore.setState({ dirty: true });
    dispatch([{ kind: "write", file: "a.atlx" }]);
    expect(reload).not.toHaveBeenCalled();
    useCanvasStore.setState({ dirty: false, saving: true });
    dispatch([{ kind: "write", file: "a.atlx" }]);
    expect(reload).not.toHaveBeenCalled();
  });

  it("title 漂移写盘：路径先跟上，干净态再重读", () => {
    useCanvasStore.setState({ canvasFile: "旧.atlx" });
    useAppStore.setState({ currentCanvasFile: "旧.atlx" });
    const reload = spyCanvasReload();
    dispatch([{ kind: "write", file: "新.atlx", from: "旧.atlx" }]);
    expect(useCanvasStore.getState().canvasFile).toBe("新.atlx");
    expect(useAppStore.getState().currentCanvasFile).toBe("新.atlx");
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("title 漂移写盘且脏：路径跟上但保留本地内容", () => {
    useCanvasStore.setState({ canvasFile: "旧.atlx", dirty: true });
    const reload = spyCanvasReload();
    dispatch([{ kind: "write", file: "新.atlx", from: "旧.atlx" }]);
    expect(useCanvasStore.getState().canvasFile).toBe("新.atlx");
    expect(reload).not.toHaveBeenCalled();
  });

  it("未打开该画布不分发", () => {
    useCanvasStore.setState({ canvasFile: "b.atlx" });
    const reload = spyCanvasReload();
    dispatch([{ kind: "write", file: "a.atlx" }]);
    expect(reload).not.toHaveBeenCalled();
  });
});

describe("表格写盘对账", () => {
  it("本窗口正打开该表格且干净：重读磁盘", () => {
    useTableStore.setState({ tableFile: "t.atb" });
    const reload = spyTableReload();
    dispatch([{ kind: "write", file: "t.atb" }]);
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("有脏：保留本地不重读；漂移时路径仍跟上", () => {
    useTableStore.setState({ tableFile: "旧.atb", dirty: true });
    const reload = spyTableReload();
    dispatch([{ kind: "write", file: "新.atb", from: "旧.atb" }]);
    expect(useTableStore.getState().tableFile).toBe("新.atb");
    expect(reload).not.toHaveBeenCalled();
  });
});

describe("笔记写盘对账", () => {
  it(".md 写盘：bump 外部变更序号 + 作废内容缓存（编辑会话据此收敛）", () => {
    useNoteStore.setState({ noteContents: { "a.md": "旧缓存" } });
    dispatch([{ kind: "write", file: "a.md" }]);
    const s = useNoteStore.getState();
    expect(s.externalNoteEdits["a.md"]).toBe(1);
    expect(s.noteContents["a.md"]).toBeUndefined();
  });

  it(".md 写盘被打开会话订阅前的重复信号递增序号（幂等收敛由会话去重）", () => {
    dispatch([{ kind: "write", file: "a.md" }]);
    dispatch([{ kind: "write", file: "a.md" }]);
    expect(useNoteStore.getState().externalNoteEdits["a.md"]).toBe(2);
  });
});

describe("路径迁移", () => {
  it("打开中的笔记被改名：换路簿记按旧→新路径投递", () => {
    noteSurfaceProvider.open("旧.md");
    dispatch([{ kind: "rename", file: "新.md", from: "旧.md" }]);
    expect(adoptRemoteNoteRelocate).toHaveBeenCalledWith("旧.md", "新.md");
    noteSurfaceProvider.close("旧.md");
  });

  it("目录改名：打开中的笔记按前缀映射换路", () => {
    noteSurfaceProvider.open("目录/笔记.md");
    dispatch([{ kind: "rename", file: "新目录", from: "目录" }]);
    expect(adoptRemoteNoteRelocate).toHaveBeenCalledWith("目录/笔记.md", "新目录/笔记.md");
    noteSurfaceProvider.close("目录/笔记.md");
  });

  it("画布文件被移动且干净：路径跟上并重读", () => {
    useCanvasStore.setState({ canvasFile: "旧/a.atlx" });
    const reload = spyCanvasReload();
    dispatch([{ kind: "rename", file: "新/a.atlx", from: "旧/a.atlx" }]);
    expect(useCanvasStore.getState().canvasFile).toBe("新/a.atlx");
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("表格文件被移动且脏：路径跟上不重读", () => {
    useTableStore.setState({ tableFile: "旧/t.atb", dirty: true });
    const reload = spyTableReload();
    dispatch([{ kind: "rename", file: "新/t.atb", from: "旧/t.atb" }]);
    expect(useTableStore.getState().tableFile).toBe("新/t.atb");
    expect(reload).not.toHaveBeenCalled();
  });
});

describe("引用文件补读", () => {
  it(".md 写盘触发画布 text 节点补读（无引用时动作自身 no-op）", () => {
    useCanvasStore.setState({ canvasFile: "画布.atlx" });
    const refresh = vi.fn().mockResolvedValue(undefined);
    useCanvasStore.setState({ refreshTextContent: refresh as unknown as never });
    dispatch([{ kind: "write", file: "a.md" }]);
    expect(refresh).toHaveBeenCalledWith("a.md");
  });

  it("附件写盘触发画布 media 节点补读", () => {
    useCanvasStore.setState({ canvasFile: "画布.atlx" });
    const refresh = vi.fn().mockResolvedValue(undefined);
    useCanvasStore.setState({ refreshMediaContent: refresh as unknown as never });
    dispatch([{ kind: "write", file: "img/图.png" }]);
    expect(refresh).toHaveBeenCalledWith("img/图.png");
  });
});
