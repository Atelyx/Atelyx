// @vitest-environment jsdom
/**
 * 文件面板空间区：点击三分支（ok / need-login / error）、创建空间与加入邀请码调用链、
 * 无登录引导。appStore 以最小 zustand 替身注入（selectSpace/openSettings 可断言）。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { SpaceRows, type SpaceEntry } from "@/components/canvas/panels/file-explorer/SpaceRows";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";

const { selectSpaceMock, openSettingsMock } = vi.hoisted(() => ({
  selectSpaceMock: vi.fn(),
  openSettingsMock: vi.fn(),
}));

vi.mock("@/stores/appStore", async () => {
  const { create } = await import("zustand");
  const useAppStore = create(() => ({
    vaultIdentity: null,
    recentSpaces: [] as unknown[],
    selectSpace: selectSpaceMock,
    openSettings: openSettingsMock,
  }));
  return { useAppStore };
});

const SERVER = "http://s1";
const ENTRY: SpaceEntry = { serverUrl: SERVER, spaceId: "sp1", name: "项目空间", role: "owner" };

const baseFileTree = {
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
  onOpenNoteForEdit: vi.fn(),
  onOpenMenu: vi.fn(),
} as never;

function renderRows(entries: SpaceEntry[] = [ENTRY]) {
  const onNotice = vi.fn();
  render(
    <ul>
      <SpaceRows
        entries={entries}
        hasServers
        loading={false}
        listError={null}
        identity={null}
        switchingTo={null}
        tree={[]}
        fileTree={baseFileTree}
        renamingKey={null}
        onRenameCommit={vi.fn()}
        onRenameCancel={vi.fn()}
        onNotice={onNotice}
        onOpenMenu={vi.fn()}
      />
    </ul>,
  );
  return { onNotice };
}

beforeEach(() => {
  cleanup();
  selectSpaceMock.mockReset();
  openSettingsMock.mockReset();
  useSpaceAuthStore.setState({
    servers: [{ serverUrl: SERVER, userId: "u1", username: "alice", displayName: "Alice" }],
    login: vi.fn(),
    register: vi.fn(),
    logout: vi.fn(),
    listDevices: vi.fn().mockResolvedValue([]),
    revokeDevice: vi.fn().mockResolvedValue(undefined),
  });
  useSpaceDirectoryStore.setState({
    createSpace: vi.fn(),
    acceptInvite: vi.fn(),
    requestLogin: vi.fn(),
    loginPrompt: null,
  });
});

describe("SpaceRows 点击进入", () => {
  it("ok：进入成功，不触发登录引导", async () => {
    selectSpaceMock.mockResolvedValue("ok");
    renderRows();
    fireEvent.click(screen.getByText("项目空间"));
    await waitFor(() => {
      expect(selectSpaceMock).toHaveBeenCalledWith({
        serverUrl: SERVER,
        spaceId: "sp1",
        name: "项目空间",
      });
    });
    expect(useSpaceDirectoryStore.getState().requestLogin).not.toHaveBeenCalled();
    expect(openSettingsMock).not.toHaveBeenCalled();
  });

  it("need-login：置登录引导（含重试条目）并打开设置「多人协作」", async () => {
    selectSpaceMock.mockResolvedValue("need-login");
    renderRows();
    fireEvent.click(screen.getByText("项目空间"));
    await waitFor(() => {
      expect(useSpaceDirectoryStore.getState().requestLogin).toHaveBeenCalledWith({
        serverUrl: SERVER,
        retry: { serverUrl: SERVER, spaceId: "sp1", name: "项目空间" },
      });
      expect(openSettingsMock).toHaveBeenCalledWith("collab");
    });
  });

  it("error：服务端不可达已由 store 通知，不再弹引导", async () => {
    selectSpaceMock.mockResolvedValue("error");
    renderRows();
    fireEvent.click(screen.getByText("项目空间"));
    await waitFor(() => {
      expect(selectSpaceMock).toHaveBeenCalled();
    });
    expect(useSpaceDirectoryStore.getState().requestLogin).not.toHaveBeenCalled();
    expect(openSettingsMock).not.toHaveBeenCalled();
  });
});

describe("SpaceRows 创建与加入", () => {
  it("创建空间：输入名称 Enter → createSpace → 携新空间调用 selectSpace 直接进入", async () => {
    const createSpace = vi.fn().mockResolvedValue({ spaceId: "sp-new", name: "新空间", role: "owner" });
    useSpaceDirectoryStore.setState({ createSpace });
    selectSpaceMock.mockResolvedValue("ok");
    renderRows();
    fireEvent.click(screen.getByText("创建空间"));
    const input = screen.getByPlaceholderText("空间名称");
    fireEvent.change(input, { target: { value: "新空间" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => {
      expect(createSpace).toHaveBeenCalledWith(SERVER, "新空间");
      expect(selectSpaceMock).toHaveBeenCalledWith({ serverUrl: SERVER, spaceId: "sp-new", name: "新空间" });
    });
  });

  it("加入邀请码：输入 code Enter → acceptInvite，失败时提示可见", async () => {
    const acceptInvite = vi.fn().mockRejectedValue(new Error("邀请码无效"));
    useSpaceDirectoryStore.setState({ acceptInvite });
    const { onNotice } = renderRows();
    fireEvent.click(screen.getByText("输入邀请码"));
    const input = screen.getByPlaceholderText("邀请码");
    fireEvent.change(input, { target: { value: "CODE-1" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await waitFor(() => {
      expect(acceptInvite).toHaveBeenCalledWith(SERVER, "CODE-1");
      expect(onNotice).toHaveBeenCalledWith(expect.stringContaining("加入失败"));
    });
  });
});

describe("SpaceRows 无登录引导", () => {
  it("无已登录服务器：显示「连接服务器后可见」，按钮打开设置协作 tab", () => {
    useSpaceAuthStore.setState({ servers: [] });
    render(
      <ul>
        <SpaceRows
          entries={[]}
          hasServers={false}
          loading={false}
          listError={null}
          identity={null}
          switchingTo={null}
          tree={[]}
          fileTree={baseFileTree}
          renamingKey={null}
          onRenameCommit={vi.fn()}
          onRenameCancel={vi.fn()}
          onNotice={vi.fn()}
          onOpenMenu={vi.fn()}
        />
      </ul>,
    );
    expect(screen.getByText("连接服务器后可见")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "连接服务器" }));
    expect(openSettingsMock).toHaveBeenCalledWith("collab");
  });
});
