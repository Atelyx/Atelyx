// @vitest-environment jsdom
/**
 * 市场安装确认测试（components/plugins/MarketplaceSection）。
 *
 * 覆盖：点安装弹出确认，继续安装把 repo 传给 store；已有同名 id 行时按钮/提示进入替换态。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import React from "react";
import { createRoot, type Root } from "react-dom/client";
import type { InstalledPlugin, PluginIndexEntry } from "@/types";
import { MarketplaceSection } from "./MarketplaceSection";

// React 18 的 act() 需显式声明测试环境（无全局 setup 文件，测试内声明）。
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** mock 数据面：测试用例直接改这些对象再重挂载（selector 每次渲染都读它们）。 */
const mock = vi.hoisted(() => {
  const market: {
    marketItems: PluginIndexEntry[];
    marketLoaded: boolean;
    marketLoading: boolean;
    marketError: string | null;
    loadMarket: (force?: boolean) => void;
    install: (repo: string) => Promise<{ id: string; replaced: boolean }>;
    plugins: Record<string, InstalledPlugin>;
  } = {
    marketItems: [],
    marketLoaded: true,
    marketLoading: false,
    marketError: null,
    loadMarket: vi.fn(),
    install: vi.fn(async () => ({ id: "com.test.market", replaced: false })),
    plugins: {},
  };
  return { market };
});

vi.mock("@/stores/pluginStore", () => {
  const hook = (selector: (s: unknown) => unknown) => selector(mock.market);
  return { usePluginStore: hook };
});

const ITEM: PluginIndexEntry = {
  id: "com.test.market",
  name: "市场插件",
  repo: "owner/market",
  defaultBranch: "main",
  stars: 10,
  updatedAt: "2026-01-01",
  topics: [],
  type: "tool",
};

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  vi.clearAllMocks();
  mock.market.marketItems = [ITEM];
  mock.market.plugins = {};
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

function mountSection(): HTMLDivElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root!.render(React.createElement(MarketplaceSection));
  });
  return container;
}

/** 列表行内安装按钮（含「安装（替换同名）」态；排除刷新等无文案按钮）。 */
function installButton(): HTMLButtonElement {
  const btn = Array.from(container!.querySelectorAll("button")).find(
    (b) => b.textContent?.trim().startsWith("安装") && !b.textContent?.includes("安装中"),
  );
  if (!btn) throw new Error("未找到安装按钮");
  return btn as HTMLButtonElement;
}

function dialogConfirmButton(): HTMLButtonElement {
  const btn = Array.from(container!.querySelectorAll("button")).find((b) => b.textContent?.trim() === "继续安装");
  if (!btn) throw new Error("未找到继续安装按钮");
  return btn as HTMLButtonElement;
}

describe("MarketplaceSection 安装确认", () => {
  it("点安装弹出确认，继续安装把 repo 传给 store", async () => {
    mountSection();
    act(() => {
      installButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(container!.textContent).toContain("社区插件未经官方审查");
    await act(async () => {
      dialogConfirmButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(mock.market.install).toHaveBeenCalledWith("owner/market");
  });

  it("已有同名 id 行时按钮进入替换态，安装沿用原行", async () => {
    mock.market.plugins = {
      "com.test.market": {
        id: "com.test.market",
        manifest: { id: "com.test.market", name: "市场插件", version: "1.0.0", type: "panel" },
        installDir: "",
        sourceKind: "builtin",
        enabled: false,
        phase: "pending",
      },
    };
    mountSection();
    expect(installButton().textContent).toContain("替换同名");
    expect(container!.textContent).toContain("同名 id 行已存在");
    act(() => {
      installButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    await act(async () => {
      dialogConfirmButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(mock.market.install).toHaveBeenCalledWith("owner/market");
  });
});
