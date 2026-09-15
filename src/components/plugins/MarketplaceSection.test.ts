// @vitest-environment jsdom
/**
 * 市场安装作用域确认测试（components/plugins/MarketplaceSection）。
 *
 * 覆盖：点安装弹出确认（默认本机）→ 选「随仓库共享」后继续安装把作用域传给 store；
 * 同 id 行被随应用分发实现占用（installDir 为空）时确认弹窗锁定「本机」并说明（vault 必被 Rust 拒绝）。
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
    install: (repo: string, scope: string) => Promise<{ id: string; replaced: boolean }>;
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

function scopeRadios(): HTMLButtonElement[] {
  return Array.from(container!.querySelectorAll('button[role="radio"]')) as HTMLButtonElement[];
}

describe("MarketplaceSection 安装作用域确认", () => {
  it("点安装弹出确认（默认本机），继续安装把作用域传给 store", async () => {
    mountSection();
    act(() => {
      installButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const radios = scopeRadios();
    expect(radios.length).toBe(2);
    expect(radios[0].getAttribute("aria-checked")).toBe("true");
    await act(async () => {
      dialogConfirmButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(mock.market.install).toHaveBeenCalledWith("owner/market", "app");
  });

  it("确认弹窗内选「随仓库共享」后按 vault 安装", async () => {
    mountSection();
    act(() => {
      installButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const radios = scopeRadios();
    expect(radios[1].disabled).toBe(false);
    act(() => {
      radios[1].dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(scopeRadios()[1].getAttribute("aria-checked")).toBe("true");
    await act(async () => {
      dialogConfirmButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(mock.market.install).toHaveBeenCalledWith("owner/market", "vault");
  });

  it("同 id 行由随应用分发实现占用时锁定「本机」并说明", async () => {
    mock.market.plugins = {
      "com.test.market": {
        id: "com.test.market",
        manifest: { id: "com.test.market", name: "市场插件", version: "1.0.0", type: "panel" },
        scope: "app",
        installDir: "",
        sourceKind: "builtin",
        enabled: false,
        phase: "pending",
      },
    };
    mountSection();
    act(() => {
      installButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    const radios = scopeRadios();
    expect(radios[1].disabled).toBe(true);
    expect(container!.textContent).toContain("只能安装到本机");
    await act(async () => {
      dialogConfirmButton().dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(mock.market.install).toHaveBeenCalledWith("owner/market", "app");
  });
});
