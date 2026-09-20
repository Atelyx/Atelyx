// @vitest-environment jsdom
/**
 * 设置区协作空间账号段交互：按钮弹出登录/注册窗、弹窗内表单流转（校验/成功/失败）、
 * need-login 自动弹窗与预填、登录成功后清引导并自动重试进入空间。
 * appStore 以最小替身注入（本组件只消费 getState().selectSpace）。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
import { SpaceAccountSection } from "@/components/settings/SpaceAccountSection";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";

const { selectSpaceMock } = vi.hoisted(() => ({
  selectSpaceMock: vi.fn(),
}));

vi.mock("@/stores/appStore", () => ({
  useAppStore: {
    getState: () => ({ selectSpace: selectSpaceMock }),
    setState: vi.fn(),
  },
}));

/** 重置两个 store 为测试基线（动作可被用例覆写）。 */
function resetStores() {
  useSpaceAuthStore.setState({
    servers: [],
    restored: true,
    busy: false,
    login: vi.fn(),
    register: vi.fn(),
    logout: vi.fn(),
    listDevices: vi.fn().mockResolvedValue([]),
    revokeDevice: vi.fn().mockResolvedValue(undefined),
  });
  useSpaceDirectoryStore.setState({
    loginPrompt: null,
    clearLoginPrompt: vi.fn(),
    requestLogin: vi.fn(),
  });
}

beforeEach(() => {
  cleanup();
  selectSpaceMock.mockReset();
  resetStores();
});

/** 点击设置区入口按钮打开弹窗（打开前区内按钮唯一）。 */
function openDialog(name: "登录" | "注册") {
  fireEvent.click(screen.getByRole("button", { name }));
  return within(screen.getByRole("dialog", { name: name === "登录" ? "登录协作服务器" : "注册协作空间账号" }));
}

function fillForm(scope: ReturnType<typeof within>, serverUrl: string, username: string, password: string) {
  fireEvent.change(scope.getByLabelText("服务器地址"), { target: { value: serverUrl } });
  fireEvent.change(scope.getByLabelText("用户名"), { target: { value: username } });
  fireEvent.change(scope.getByLabelText("密码"), { target: { value: password } });
}

describe("SpaceAccountSection 登录/注册弹窗", () => {
  it("点击登录弹出表单；必填为空提交不发请求并显示校验错误", async () => {
    render(<SpaceAccountSection />);
    const dialog = openDialog("登录");
    fireEvent.click(dialog.getByRole("button", { name: "登录" }));
    await waitFor(() => {
      expect(dialog.getByText("服务器地址、用户名与密码不能为空")).toBeTruthy();
    });
    expect(useSpaceAuthStore.getState().login).not.toHaveBeenCalled();
  });

  it("登录成功：携带表单值调用 login 并关闭弹窗", async () => {
    const login = vi.fn().mockResolvedValue({
      serverUrl: "http://s1",
      userId: "u1",
      username: "alice",
      displayName: "Alice",
    });
    useSpaceAuthStore.setState({ login });
    render(<SpaceAccountSection />);
    const dialog = openDialog("登录");
    fillForm(dialog, "http://s1", "alice", "pw");
    fireEvent.click(dialog.getByRole("button", { name: "登录" }));
    await waitFor(() => {
      expect(login).toHaveBeenCalledWith("http://s1", "alice", "pw", undefined, undefined);
      expect(screen.queryByRole("dialog")).toBeNull();
    });
  });

  it("登录失败：错误消息可见且弹窗保持打开", async () => {
    useSpaceAuthStore.setState({
      login: vi.fn().mockRejectedValue(new Error("密码错误")),
    });
    render(<SpaceAccountSection />);
    const dialog = openDialog("登录");
    fillForm(dialog, "http://s1", "alice", "bad");
    fireEvent.click(dialog.getByRole("button", { name: "登录" }));
    expect(await screen.findByText("密码错误")).toBeTruthy();
    expect(screen.getByRole("dialog")).toBeTruthy();
  });

  it("注册弹窗：密码确认不一致拒绝提交，成功时含昵称调用 register", async () => {
    const register = vi.fn().mockResolvedValue({
      serverUrl: "http://s1",
      userId: "u2",
      username: "bob",
      displayName: "Bob",
    });
    useSpaceAuthStore.setState({ register });
    render(<SpaceAccountSection />);
    const dialog = openDialog("注册");
    fillForm(dialog, "http://s1", "bob", "pw");
    fireEvent.change(dialog.getByLabelText("确认密码"), { target: { value: "pw2" } });
    fireEvent.change(dialog.getByLabelText("昵称"), { target: { value: "Bob" } });
    fireEvent.click(dialog.getByRole("button", { name: "注册" }));
    expect(await dialog.findByText("两次输入的密码不一致")).toBeTruthy();
    expect(register).not.toHaveBeenCalled();

    fireEvent.change(dialog.getByLabelText("确认密码"), { target: { value: "pw" } });
    fireEvent.click(dialog.getByRole("button", { name: "注册" }));
    await waitFor(() => {
      expect(register).toHaveBeenCalledWith("http://s1", "bob", "pw", undefined, "Bob");
    });
  });

  it("need-login：自动弹出登录窗并预填地址，登录成功后清引导并自动重试进入空间", async () => {
    const clearLoginPrompt = vi.fn();
    useSpaceDirectoryStore.setState({
      loginPrompt: {
        serverUrl: "http://s9",
        retry: { serverUrl: "http://s9", spaceId: "sp1", name: "项目空间" },
      },
      clearLoginPrompt,
    });
    useSpaceAuthStore.setState({ login: vi.fn().mockResolvedValue({ serverUrl: "http://s9" }) });
    render(<SpaceAccountSection />);
    // 引导出现即自动弹窗：无需点击入口按钮
    const dialog = within(screen.getByRole("dialog", { name: "登录协作服务器" }));
    expect((dialog.getByLabelText("服务器地址") as HTMLInputElement).value).toBe("http://s9");
    expect(dialog.getByText(/登录后将继续打开协作空间/)).toBeTruthy();
    // 设置区引导条与弹窗内提示各一份，均可见
    expect(screen.getAllByText(/登录后将继续打开协作空间/).length).toBe(2);

    fireEvent.change(dialog.getByLabelText("用户名"), { target: { value: "bob" } });
    fireEvent.change(dialog.getByLabelText("密码"), { target: { value: "pw" } });
    fireEvent.click(dialog.getByRole("button", { name: "登录" }));
    await waitFor(() => {
      expect(clearLoginPrompt).toHaveBeenCalled();
      expect(selectSpaceMock).toHaveBeenCalledWith({
        serverUrl: "http://s9",
        spaceId: "sp1",
        name: "项目空间",
      });
      expect(screen.queryByRole("dialog")).toBeNull();
    });
  });

  it("need-login 引导条可取消：清除引导", async () => {
    const clearLoginPrompt = vi.fn();
    useSpaceDirectoryStore.setState({
      loginPrompt: { serverUrl: "http://s9" },
      clearLoginPrompt,
    });
    render(<SpaceAccountSection />);
    // 无重试条目时弹窗内无引导提示，设置区引导条提供取消入口
    fireEvent.click(screen.getByTitle("取消引导"));
    expect(clearLoginPrompt).toHaveBeenCalled();
  });
});
