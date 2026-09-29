/**
 * 远端改名/移动跟随测试（stores/tableStore + canvasStore）。
 *
 * 协作空间内其他成员改名/移动文件时，服务端落地后广播 renamed 帧，本端把打开中的
 * 文件即时切到新路径：不同步会在树刷新后误判删除静默关表（挂起编辑丢弃），且在途
 * 保存打旧路径（404 整写兜底会复活旧文件）。改名不改内容——内存内容、脏标记与落盘
 * 基线必须原样保留，挂起编辑随下一轮保存落到新路径。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { Node } from "@xyflow/react";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async () => null,
}));

// 插件注册表会把全量领域 store 拉进模块图并在 ESM 初始化期互相取用未就绪的 store；
// 本测试只关心两个领域 store 自身，桩掉它斩断这条环（同 tableStore.vaultReset.test.ts）。
vi.mock("@/components/plugins/cordis/builtins", () => ({
  CORDIS_BUILTIN_BY_ID: {},
  CORDIS_BUILTIN_DEFS: [],
  DEFAULT_COMPOSITION: [],
  builtinManifest: {},
}));

import { useTableStore, followRemoteTableRename } from "./tableStore";
import { useCanvasStore, followRemoteCanvasRename } from "./canvasStore";
import { useAppStore } from "./appStore";
import { useUiStateStore } from "./uiStateStore";

beforeEach(() => {
  useTableStore.setState({
    tableFile: null,
    title: "",
    fields: [],
    rows: [],
    dirty: false,
    error: null,
  });
  useCanvasStore.setState({ canvasFile: null, nodes: [] });
  useAppStore.setState({
    currentTableFile: null,
    currentTableTitle: "",
    currentCanvasFile: null,
  });
});

describe("表格远端改名跟随", () => {
  beforeEach(() => {
    useTableStore.setState({
      tableFile: "目录/旧名.atb",
      title: "旧名",
      fields: [{ id: "f1", name: "名称", type: "text" }],
      rows: [{ id: "r1", values: { f1: "内容" } }],
      dirty: true,
    });
    useAppStore.setState({ currentTableFile: "目录/旧名.atb", currentTableTitle: "旧名" });
    useUiStateStore.getState().recordOpenFile("table", "目录/旧名.atb");
  });

  it("精确命中：路径与标题切到新值，内容/脏标记原样保留（挂起编辑不丢）", () => {
    followRemoteTableRename("目录/旧名.atb", "目录/新名.atb");
    const s = useTableStore.getState();
    expect(s.tableFile).toBe("目录/新名.atb");
    expect(s.title).toBe("新名");
    expect(s.fields).toEqual([{ id: "f1", name: "名称", type: "text" }]);
    expect(s.rows).toEqual([{ id: "r1", values: { f1: "内容" } }]);
    expect(s.dirty).toBe(true);
    expect(useAppStore.getState().currentTableFile).toBe("目录/新名.atb");
    expect(useAppStore.getState().currentTableTitle).toBe("新名");
    expect(useUiStateStore.getState().lastTableFile).toBe("目录/新名.atb");
  });

  it("前缀命中（远端文件夹改名/移动）：路径前缀整体迁移", () => {
    useTableStore.setState({ tableFile: "目录/子/表.atb" });
    useAppStore.setState({ currentTableFile: "目录/子/表.atb" });
    followRemoteTableRename("目录", "新目录");
    expect(useTableStore.getState().tableFile).toBe("新目录/子/表.atb");
    expect(useAppStore.getState().currentTableFile).toBe("新目录/子/表.atb");
  });

  it("无关路径不动", () => {
    followRemoteTableRename("别的.atb", "新.atb");
    expect(useTableStore.getState().tableFile).toBe("目录/旧名.atb");
    expect(useAppStore.getState().currentTableFile).toBe("目录/旧名.atb");
    expect(useUiStateStore.getState().lastTableFile).toBe("目录/旧名.atb");
  });
});

describe("画布远端改名跟随", () => {
  const tableNode = (id: string, file: string): Node => ({
    id,
    type: "table",
    position: { x: 0, y: 0 },
    data: { title: "t", file },
  });

  beforeEach(() => {
    useCanvasStore.setState({
      canvasFile: "画布/画布.atlx",
      nodes: [tableNode("n1", "目录/旧名.atb"), tableNode("n2", "无关.atb")],
    });
    useAppStore.setState({ currentCanvasFile: "画布/画布.atlx" });
  });

  it("打开画布路径切换 + 引用旧路径的节点 file 同步（无关节点不动）", () => {
    followRemoteCanvasRename("目录/旧名.atb", "目录/新名.atb");
    expect(useCanvasStore.getState().canvasFile).toBe("画布/画布.atlx");
    const files = useCanvasStore
      .getState()
      .nodes.map((n) => (n.data as { file: string }).file);
    expect(files).toEqual(["目录/新名.atb", "无关.atb"]);
  });

  it("画布自身被改名：打开路径切换（防下次保存回写旧路径），「上次打开」同步", () => {
    useUiStateStore.getState().recordOpenFile("canvas", "画布/画布.atlx");
    followRemoteCanvasRename("画布/画布.atlx", "画布/新画布.atlx");
    expect(useCanvasStore.getState().canvasFile).toBe("画布/新画布.atlx");
    expect(useAppStore.getState().currentCanvasFile).toBe("画布/新画布.atlx");
    expect(useUiStateStore.getState().lastCanvasFile).toBe("画布/新画布.atlx");
  });

  it("前缀命中（远端文件夹改名）：打开路径与节点引用前缀整体迁移", () => {
    useCanvasStore.setState({
      canvasFile: "目录/子/画布.atlx",
      nodes: [tableNode("n1", "目录/子/表.atb")],
    });
    useAppStore.setState({ currentCanvasFile: "目录/子/画布.atlx" });
    followRemoteCanvasRename("目录", "新目录");
    expect(useCanvasStore.getState().canvasFile).toBe("新目录/子/画布.atlx");
    expect(useAppStore.getState().currentCanvasFile).toBe("新目录/子/画布.atlx");
    expect((useCanvasStore.getState().nodes[0].data as { file: string }).file).toBe(
      "新目录/子/表.atb",
    );
  });
});
