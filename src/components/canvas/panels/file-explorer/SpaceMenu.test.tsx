// @vitest-environment jsdom
/**
 * 空间条目右键菜单：菜单项出现与动作派发（空间设置/重命名/成员管理/邀请码/断开/重新连接）。
 */
import { it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { SpaceMenu } from "@/components/canvas/panels/file-explorer/SpaceMenu";

function makeHandlers() {
  return {
    onClose: vi.fn(),
    onSettings: vi.fn(),
    onRename: vi.fn(),
    onMembers: vi.fn(),
    onInvite: vi.fn(),
    onDisconnect: vi.fn(),
    onReconnect: vi.fn(),
  };
}

function renderMenu(handlers: ReturnType<typeof makeHandlers>) {
  return render(
    <SpaceMenu
      serverUrl="http://s1"
      spaceId="sp1"
      name="项目空间"
      role="owner"
      x={10}
      y={10}
      onClose={handlers.onClose}
      onSettings={handlers.onSettings}
      onRename={handlers.onRename}
      onMembers={handlers.onMembers}
      onInvite={handlers.onInvite}
      onDisconnect={handlers.onDisconnect}
      onReconnect={handlers.onReconnect}
    />,
  );
}

beforeEach(() => {
  cleanup();
});

it("菜单项齐全：空间设置/重命名/成员管理/邀请码/断开连接/重新连接", () => {
  const handlers = makeHandlers();
  renderMenu(handlers);
  for (const label of ["空间设置", "重命名", "成员管理", "邀请码", "断开连接", "重新连接"]) {
    expect(screen.getByText(label)).toBeTruthy();
  }
});

it("点击动作派发并关闭菜单", () => {
  const handlers = makeHandlers();
  renderMenu(handlers);
  fireEvent.click(screen.getByText("空间设置"));
  expect(handlers.onSettings).toHaveBeenCalledTimes(1);
  expect(handlers.onClose).toHaveBeenCalledTimes(1);

  fireEvent.click(screen.getByText("重命名"));
  expect(handlers.onRename).toHaveBeenCalledTimes(1);
  expect(handlers.onClose).toHaveBeenCalledTimes(2);

  fireEvent.click(screen.getByText("成员管理"));
  expect(handlers.onMembers).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByText("邀请码"));
  expect(handlers.onInvite).toHaveBeenCalledTimes(1);
  fireEvent.click(screen.getByText("断开连接"));
  expect(handlers.onDisconnect).toHaveBeenCalledTimes(1);
});
