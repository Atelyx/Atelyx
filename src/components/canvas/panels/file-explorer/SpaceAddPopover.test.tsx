// @vitest-environment jsdom
/**
 * 协作空间新增浮层：三模式的提交链路（创建 / 纳管服务器文件夹 / 输邀请码）、校验与失败
 * 可见、模式切换与重开的状态复位、无服务器引导、多服务器目标切换。
 * appStore 以最小 zustand 替身注入（selectSpace/openSettings 可断言）。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { SpaceAddPopover } from "@/components/canvas/panels/file-explorer/SpaceAddPopover";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";

const { selectSpaceMock, openSettingsMock } = vi.hoisted(() => ({
  selectSpaceMock: vi.fn(),
  openSettingsMock: vi.fn(),
}));

vi.mock("@/stores/appStore", async () => {
  const { create } = await import("zustand");
  const useAppStore = create(() => ({
    // 切换进行中的守卫值：null = 空闲（非 null 时工具条按钮禁用）
    switchingVaultRoot: null as string | null,
    selectSpace: selectSpaceMock,
    openSettings: openSettingsMock,
  }));
  return { useAppStore };
});

const S1 = "http://s1";
const S2 = "http://s2";
const server = (serverUrl: string) => ({ serverUrl, userId: "u1", username: "alice", displayName: "Alice" });

function renderPopover() {
  const onNotice = vi.fn();
  render(<SpaceAddPopover onNotice={onNotice} />);
  return { onNotice };
}

/** 点工具条按钮展开浮层。 */
function openLayer() {
  fireEvent.click(screen.getByTitle("新增协作空间"));
}

/** 当前浮层里的文本输入（服务器下拉为 combobox，与文本框不冲突）。 */
function input() {
  return screen.getByRole("textbox") as HTMLInputElement;
}

function typeDraft(text: string) {
  fireEvent.change(input(), { target: { value: text } });
}

beforeEach(() => {
  cleanup();
  selectSpaceMock.mockReset().mockResolvedValue("ok");
  openSettingsMock.mockReset();
  useSpaceAuthStore.setState({ servers: [server(S1)] });
  useSpaceDirectoryStore.setState({
    createSpace: vi.fn(),
    acceptInvite: vi.fn(),
  });
});

describe("SpaceAddPopover 创建空间", () => {
  it("提交后建空间并直接进入，浮层收起", async () => {
    const createSpace = vi.fn().mockResolvedValue({ spaceId: "sp-new", name: "新空间" });
    useSpaceDirectoryStore.setState({ createSpace });
    renderPopover();
    openLayer();
    typeDraft("新空间");
    fireEvent.click(screen.getByRole("button", { name: "创建并进入" }));
    await waitFor(() => {
      expect(createSpace).toHaveBeenCalledWith(S1, "新空间", undefined);
      expect(selectSpaceMock).toHaveBeenCalledWith({ serverUrl: S1, spaceId: "sp-new", name: "新空间" });
    });
    expect(screen.queryByText("新增协作空间")).toBeNull();
  });

  it("失败：错误在浮层内可见、不进入、输入保留", async () => {
    const createSpace = vi.fn().mockRejectedValue(new Error("名称已存在"));
    useSpaceDirectoryStore.setState({ createSpace });
    renderPopover();
    openLayer();
    typeDraft("新空间");
    fireEvent.click(screen.getByRole("button", { name: "创建并进入" }));
    await waitFor(() => {
      expect(screen.getByText("创建空间失败：名称已存在")).toBeTruthy();
    });
    expect(selectSpaceMock).not.toHaveBeenCalled();
    expect(screen.getByText("新增协作空间")).toBeTruthy();
    expect(input().value).toBe("新空间");
  });
});

describe("SpaceAddPopover 打开服务器文件夹", () => {
  it("路径末段作空间名、路径作内容根，成功后进入", async () => {
    const createSpace = vi.fn().mockResolvedValue({ spaceId: "sp-dir", name: "team-library" });
    useSpaceDirectoryStore.setState({ createSpace });
    renderPopover();
    openLayer();
    fireEvent.click(screen.getByRole("button", { name: "打开文件夹" }));
    typeDraft("/mnt/team-library");
    fireEvent.click(screen.getByRole("button", { name: "打开并进入" }));
    await waitFor(() => {
      expect(createSpace).toHaveBeenCalledWith(S1, "team-library", "/mnt/team-library");
      expect(selectSpaceMock).toHaveBeenCalledWith({ serverUrl: S1, spaceId: "sp-dir", name: "team-library" });
    });
  });

  it("非绝对路径：不发请求，错误内联可见", async () => {
    const createSpace = vi.fn();
    useSpaceDirectoryStore.setState({ createSpace });
    renderPopover();
    openLayer();
    fireEvent.click(screen.getByRole("button", { name: "打开文件夹" }));
    typeDraft("team-library");
    fireEvent.click(screen.getByRole("button", { name: "打开并进入" }));
    await waitFor(() => {
      expect(screen.getByText(/绝对路径/)).toBeTruthy();
    });
    expect(createSpace).not.toHaveBeenCalled();
    expect(selectSpaceMock).not.toHaveBeenCalled();
  });

  it("服务器拒绝纳管：错误在浮层内可见", async () => {
    useSpaceDirectoryStore.setState({
      createSpace: vi.fn().mockRejectedValue(new Error("内容根与数据目录互相嵌套")),
    });
    renderPopover();
    openLayer();
    fireEvent.click(screen.getByRole("button", { name: "打开文件夹" }));
    typeDraft("/mnt/team-library");
    fireEvent.click(screen.getByRole("button", { name: "打开并进入" }));
    await waitFor(() => {
      expect(screen.getByText(/打开文件夹失败/)).toBeTruthy();
    });
  });
});

describe("SpaceAddPopover 输入邀请码", () => {
  it("加入成功：只提示，不切换当前仓库", async () => {
    const acceptInvite = vi.fn().mockResolvedValue({ spaceId: "sp9", name: "别人的空间", role: "editor" });
    useSpaceDirectoryStore.setState({ acceptInvite });
    const { onNotice } = renderPopover();
    openLayer();
    fireEvent.click(screen.getByRole("button", { name: "输入邀请码" }));
    typeDraft("CODE-1");
    fireEvent.click(screen.getByRole("button", { name: "加入" }));
    await waitFor(() => {
      expect(acceptInvite).toHaveBeenCalledWith(S1, "CODE-1");
      expect(onNotice).toHaveBeenCalledWith("已加入协作空间");
    });
    expect(selectSpaceMock).not.toHaveBeenCalled();
  });

  it("加入失败：错误在浮层内可见", async () => {
    useSpaceDirectoryStore.setState({ acceptInvite: vi.fn().mockRejectedValue(new Error("邀请码无效")) });
    renderPopover();
    openLayer();
    fireEvent.click(screen.getByRole("button", { name: "输入邀请码" }));
    typeDraft("CODE-1");
    fireEvent.click(screen.getByRole("button", { name: "加入" }));
    await waitFor(() => {
      expect(screen.getByText("加入失败：邀请码无效")).toBeTruthy();
    });
  });
});

describe("SpaceAddPopover 浮层状态", () => {
  it("切换模式与重新打开都不沿用上一次的输入，重开回默认模式", () => {
    renderPopover();
    openLayer();
    typeDraft("草稿名");
    fireEvent.click(screen.getByRole("button", { name: "输入邀请码" }));
    expect(input().value).toBe("");

    typeDraft("CODE-1");
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByText("新增协作空间")).toBeNull();

    openLayer();
    expect(screen.getByRole("button", { name: "创建并进入" })).toBeTruthy();
    expect(input().value).toBe("");
  });
});

describe("SpaceAddPopover 服务器维度", () => {
  it("未连接服务器：浮层内引导去设置连接", () => {
    useSpaceAuthStore.setState({ servers: [] });
    renderPopover();
    openLayer();
    expect(screen.getByText("尚未连接协作服务器")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "连接服务器" }));
    expect(openSettingsMock).toHaveBeenCalledWith("collab");
  });

  it("多服务器：可切换目标服务器，请求落到选中的那个", async () => {
    useSpaceAuthStore.setState({ servers: [server(S1), server(S2)] });
    const createSpace = vi.fn().mockResolvedValue({ spaceId: "sp2", name: "空间" });
    useSpaceDirectoryStore.setState({ createSpace });
    renderPopover();
    openLayer();
    fireEvent.change(screen.getByRole("combobox"), { target: { value: S2 } });
    typeDraft("空间");
    fireEvent.click(screen.getByRole("button", { name: "创建并进入" }));
    await waitFor(() => {
      expect(createSpace).toHaveBeenCalledWith(S2, "空间", undefined);
    });
  });
});
