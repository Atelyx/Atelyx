// @vitest-environment jsdom
/**
 * 文件面板仓库行：右键菜单项（在文件管理器中打开 / 从列表移除）与派生态
 * （激活仓库不可移出列表）。appStore 以最小 zustand 替身注入。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { VaultRows } from "@/components/canvas/panels/file-explorer/VaultRows";

const { openInExplorerMock, removeRecentVaultMock } = vi.hoisted(() => ({
  openInExplorerMock: vi.fn(),
  removeRecentVaultMock: vi.fn(),
}));

vi.mock("@/stores/appStore", async () => {
  const { create } = await import("zustand");
  const useAppStore = create(() => ({
    openInExplorer: openInExplorerMock,
    removeRecentVault: removeRecentVaultMock,
  }));
  return { useAppStore };
});

const fileTree = {
  sortKey: "name" as const,
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
  canvasRowOf: () => undefined,
  startPotentialDrag: vi.fn(),
  onOpenMenu: vi.fn(),
} as never;

const VAULTS = [
  { root: "E:/当前", name: "当前仓库", lastOpenedAt: 2 },
  { root: "E:/另一个", name: "另一个仓库", lastOpenedAt: 1 },
];

/** 右键第 index 行并返回该行元素。 */
function openMenu(index: number) {
  const rows = screen.getAllByTitle(/E:\//);
  fireEvent.contextMenu(rows[index]);
  return rows[index];
}

beforeEach(() => {
  cleanup();
  openInExplorerMock.mockReset();
  removeRecentVaultMock.mockReset();
});

describe("仓库行右键菜单", () => {
  it("菜单只含在文件管理器中打开（仓库级设置入口已统一到标题栏设置）", () => {
    render(
      <VaultRows
        vaults={VAULTS}
        vaultRoot="E:/当前"
        switchingTo={null}
        onEnter={vi.fn()}
        collapsedVaults={new Set()}
        toggleVaultCollapsed={vi.fn()}
        tree={[]}
        fileTree={fileTree}
      />,
    );
    openMenu(1);
    expect(screen.queryByText("仓库设置")).toBeNull();
    fireEvent.click(screen.getByText("在文件管理器中打开"));
    expect(openInExplorerMock).toHaveBeenCalledWith("E:/另一个");
  });

  it("激活仓库不可移出列表，非激活仓库移除需确认", () => {
    render(
      <VaultRows
        vaults={VAULTS}
        vaultRoot="E:/当前"
        switchingTo={null}
        onEnter={vi.fn()}
        collapsedVaults={new Set()}
        toggleVaultCollapsed={vi.fn()}
        tree={[]}
        fileTree={fileTree}
      />,
    );
    openMenu(0);
    expect(screen.getByText("从列表移除").closest("button")?.disabled).toBe(true);

    openMenu(1);
    fireEvent.click(screen.getByText("从列表移除"));
    fireEvent.click(screen.getByText("移除"));
    expect(removeRecentVaultMock).toHaveBeenCalledWith("E:/另一个");
  });
});
