// @vitest-environment jsdom
/**
 * 文件树行点击：打开动作统一经 useFileNavigation 落到 appStore。
 * appStore 以最小 zustand 替身注入（openCanvas/openNote/openTable 可断言）。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { FileTree, type FileTreeProps } from "@/components/canvas/panels/file-explorer/FileTree";
import type { FileTreeNode } from "@/types";

const { openCanvasMock, openNoteMock, openTableMock } = vi.hoisted(() => ({
  openCanvasMock: vi.fn(),
  openNoteMock: vi.fn(),
  openTableMock: vi.fn(),
}));

vi.mock("@/stores/appStore", async () => {
  const { create } = await import("zustand");
  const useAppStore = create(() => ({
    openCanvas: openCanvasMock,
    openNote: openNoteMock,
    openTable: openTableMock,
  }));
  return { useAppStore };
});

/** 文件行节点（非目录）。 */
function file(name: string): FileTreeNode {
  return { name, path: name, isDir: false, updatedAt: 0, children: [] };
}

const CANVAS_ROW = { id: "c1", title: "画布", file: "图表.atlx", updatedAt: 0 };

function renderTree(nodes: FileTreeNode[]) {
  const props = {
    nodes,
    depth: 0,
    parentDir: "",
    sortKey: "name-asc" as const,
    expanded: new Set<string>(),
    toggleExpanded: vi.fn(),
    editing: null,
    onEditingChange: vi.fn(),
    onCommitEditing: vi.fn(),
    dropDir: null,
    folderColors: undefined,
    currentCanvasFile: null,
    openedNoteFile: null,
    openedTableFile: null,
    canvasRowOf: (path: string) => (path === "图表.atlx" ? CANVAS_ROW : undefined),
    startPotentialDrag: vi.fn(),
    onOpenMenu: vi.fn(),
  } satisfies FileTreeProps;
  render(<FileTree {...props} />);
}

beforeEach(() => {
  cleanup();
  openCanvasMock.mockReset();
  openNoteMock.mockReset();
  openTableMock.mockReset();
});

describe("文件树行点击打开", () => {
  it("笔记行：openNote(file, 去扩展名标题)", () => {
    renderTree([file("笔记.md")]);
    fireEvent.click(screen.getByText("笔记.md"));
    expect(openNoteMock).toHaveBeenCalledWith("笔记.md", "笔记");
  });

  it("表格行：openTable(file, 去扩展名标题)", () => {
    renderTree([file("数据.atb")]);
    fireEvent.click(screen.getByText("数据.atb"));
    expect(openTableMock).toHaveBeenCalledWith("数据.atb", "数据");
  });

  it("画布行：openCanvas(列表命中的 row)", () => {
    renderTree([file("图表.atlx")]);
    fireEvent.click(screen.getByText("图表.atlx"));
    expect(openCanvasMock).toHaveBeenCalledWith(CANVAS_ROW);
  });

  it("外部白板行：openCanvas(合成只读行，id = 路径)", () => {
    renderTree([file("白板.canvas")]);
    fireEvent.click(screen.getByText("白板.canvas"));
    expect(openCanvasMock).toHaveBeenCalledWith({
      id: "白板.canvas",
      title: "白板",
      file: "白板.canvas",
      updatedAt: 0,
    });
  });
});
