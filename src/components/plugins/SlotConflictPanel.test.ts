// @vitest-environment jsdom
/**
 * 槽位冲突裁决面板测试（components/plugins/SlotConflictPanel）。
 *
 * 纯 UI 逻辑：有冲突行才渲染、切换胜者写 uiStateStore.setSlotWinner、「跟随优先级」清钉住、
 * 失效钉住显式标注。数据源（pluginStore/uiStateStore）整体 mock——面板只消费两 store 的
 * 方法与状态，不直连 services。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import React from "react";
import { createRoot, type Root } from "react-dom/client";
import { SlotConflictPanel } from "./SlotConflictPanel";

// React 18 的 act() 需显式声明测试环境（无全局 setup 文件，测试内声明）。
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

/** mock 数据面：测试用例直接改这两个对象再重挂载（useXxx 的 selector 每次渲染都读它们）。 */
const mock = vi.hoisted(() => {
  const pluginState: { uiRevision: number; slotConflictRows: () => ConflictRow[] } = {
    uiRevision: 0,
    slotConflictRows: () => [],
  };
  const uiState: {
    slotWinnerOverrides: Record<string, string>;
    setSlotWinner: (slot: string, id: string | null) => void;
  } = { slotWinnerOverrides: {}, setSlotWinner: vi.fn() };
  return { pluginState, uiState };
});

/** 冲突行形状（与 types/plugin.ts SlotConflictRow 对齐；hoisted 闭包里引用类型会 TDZ，故内联声明）。 */
type ConflictRow = {
  slot: string;
  declarer: string;
  contributors: { id: string; pluginId: string; priority: number; label?: string }[];
  pinnedId: string | null;
  winnerId: string | null;
};

vi.mock("@/stores/pluginStore", () => {
  const hook = (selector: (s: unknown) => unknown) => selector(mock.pluginState);
  // Zustand hook 自带 getState：组件经 usePluginStore.getState() 读方法（与真实 store 同形状）。
  (hook as unknown as { getState: () => unknown }).getState = () => mock.pluginState;
  return { usePluginStore: hook };
});

vi.mock("@/stores/uiStateStore", () => ({
  useUiStateStore: (selector: (s: unknown) => unknown) => selector(mock.uiState),
}));

let root: Root | null = null;
let container: HTMLDivElement | null = null;

beforeEach(() => {
  vi.clearAllMocks();
  mock.pluginState.uiRevision = 0;
  mock.pluginState.slotConflictRows = () => [];
  mock.uiState.slotWinnerOverrides = {};
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

function mountPanel(): HTMLDivElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => {
    root!.render(React.createElement(SlotConflictPanel));
  });
  return container;
}

const CONFLICT_ROW: ConflictRow = {
  slot: "empty/canvas",
  declarer: "宿主",
  contributors: [
    { id: "com.a:empty/canvas", pluginId: "com.a", priority: 1, label: "A 空态" },
    { id: "com.b:empty/canvas", pluginId: "com.b", priority: 10 },
  ],
  pinnedId: null,
  winnerId: "com.b:empty/canvas",
};

describe("SlotConflictPanel", () => {
  it("无冲突行 = 不渲染（治理面板只在需要裁决时出现）", () => {
    const el = mountPanel();
    expect(el.querySelector("select")).toBeNull();
  });

  it("渲染冲突行：槽名/声明方/贡献者选项/当前胜出提示", () => {
    mock.pluginState.slotConflictRows = () => [CONFLICT_ROW];
    const el = mountPanel();
    expect(el.textContent).toContain("empty/canvas");
    expect(el.textContent).toContain("声明方：宿主");
    const select = el.querySelector("select")!;
    expect(select.value).toBe(""); // 跟随优先级
    expect(el.textContent).toContain("当前胜出：com.b");
    const options = [...select.querySelectorAll("option")].map((o) => o.textContent ?? "");
    expect(options.some((t) => t.includes("com.a"))).toBe(true);
    expect(options.some((t) => t.includes("com.b"))).toBe(true);
  });

  it("切换胜者 → setSlotWinner(槽, 贡献 id)；「跟随优先级」→ setSlotWinner(槽, null)", () => {
    mock.pluginState.slotConflictRows = () => [CONFLICT_ROW];
    const el = mountPanel();
    const select = el.querySelector("select")!;

    act(() => {
      select.value = "com.a:empty/canvas";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(mock.uiState.setSlotWinner).toHaveBeenCalledWith("empty/canvas", "com.a:empty/canvas");

    act(() => {
      select.value = "";
      select.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(mock.uiState.setSlotWinner).toHaveBeenCalledWith("empty/canvas", null);
  });

  it("有效钉住：select 显示被钉贡献为选中，无「当前胜出/已失效」提示", () => {
    mock.pluginState.slotConflictRows = () => [
      { ...CONFLICT_ROW, pinnedId: "com.a:empty/canvas", winnerId: "com.a:empty/canvas" },
    ];
    const el = mountPanel();
    const select = el.querySelector("select")!;
    expect(select.value).toBe("com.a:empty/canvas");
    expect(el.textContent).not.toContain("当前胜出");
    expect(el.textContent).not.toContain("已失效");
  });

  it("失效钉住：选项标注已失效且显示当前实际胜出者（用户可清掉）", () => {
    mock.pluginState.slotConflictRows = () => [
      { ...CONFLICT_ROW, pinnedId: "com.gone:empty/canvas", winnerId: "com.b:empty/canvas" },
    ];
    const el = mountPanel();
    const select = el.querySelector("select")!;
    expect(select.value).toBe("com.gone:empty/canvas");
    expect(el.textContent).toContain("已失效的钉住（贡献已卸载）");
    expect(el.textContent).toContain("当前按 priority 胜出：com.b");
  });
});
