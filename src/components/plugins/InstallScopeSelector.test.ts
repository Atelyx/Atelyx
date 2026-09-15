// @vitest-environment jsdom
/**
 * 安装作用域选择卡片测试（components/plugins/InstallScopeSelector）。
 *
 * 纯展示组件：两选项渲染 + 选中回调 + 锁定作用域时另一项置灰并显示说明。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import React from "react";
import { createRoot, type Root } from "react-dom/client";
import type { PluginScope } from "@/types";
import { InstallScopeSelector } from "./InstallScopeSelector";

// React 18 的 act() 需显式声明测试环境（无全局 setup 文件，测试内声明）。
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  vi.clearAllMocks();
});

afterEach(async () => {
  if (root) {
    await act(async () => {
      root!.unmount();
    });
    root = null;
  }
  if (container) {
    container.remove();
    container = null;
  }
});

function mountSelector(props: {
  value: PluginScope;
  onChange: (scope: PluginScope) => void;
  lockedScope?: PluginScope;
  lockedNote?: string;
}): HTMLDivElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root!.render(React.createElement(InstallScopeSelector, props));
  });
  return container;
}

describe("InstallScopeSelector", () => {
  it("渲染本机/随仓库共享两项，选中项标记 aria-checked", () => {
    mountSelector({ value: "app", onChange: () => {} });
    const radios = container!.querySelectorAll('button[role="radio"]');
    expect(radios.length).toBe(2);
    expect(container!.textContent).toContain("本机");
    expect(container!.textContent).toContain("随仓库共享");
    expect(radios[0].getAttribute("aria-checked")).toBe("true");
    expect(radios[1].getAttribute("aria-checked")).toBe("false");
  });

  it("点击另一项回调 onChange 携带该作用域", () => {
    const onChange = vi.fn();
    mountSelector({ value: "app", onChange });
    const radios = container!.querySelectorAll('button[role="radio"]');
    act(() => {
      radios[1].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(onChange).toHaveBeenCalledWith("vault");
  });

  it("锁定某作用域时另一项置灰并显示说明文案", () => {
    mountSelector({ value: "app", onChange: () => {}, lockedScope: "app", lockedNote: "只能安装到本机" });
    const radios = container!.querySelectorAll('button[role="radio"]');
    expect((radios[1] as HTMLButtonElement).disabled).toBe(true);
    expect(container!.textContent).toContain("只能安装到本机");
  });
});
