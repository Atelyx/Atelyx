// @vitest-environment jsdom
/**
 * 槽位装饰器渲染校验测试（components/plugins/SlotHost 的 SlotDecoratedContent）。
 *
 * 覆盖：装饰器正常包裹 children 时内容在其内渲染；装饰器吞掉 children（不渲染 children）时
 * 被校验剔除并回退内容——「拒绝吞掉宿主 UI」的运行时语义（纯注册表侧测试见 slotsApi.test.ts，
 * 本测试验证 DOM 侧 marker 检测与回退）。
 * 注：不渲染 SlotListMount（避免拖入 pluginStore 的重型依赖链），SlotDecoratedContent 即装饰机制本体。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import React from "react";
import { createRoot, type Root } from "react-dom/client";
import { SlotDecoratedContent } from "./SlotHost";
import {
  listDecorators,
  registerSlotDecoratorFor,
  unregisterSlotDecorator,
} from "@/services/cordis/slots";

// React 18 的 act() 需显式声明测试环境（无全局 setup 文件，测试内声明）。
(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// SlotHost 的其余宿主组件依赖 pluginStore，而其依赖链会拖入 esbuild-wasm（jsdom 不兼容）。
// 本测试只验证装饰机制本体（SlotDecoratedContent），不依赖 store，mock 掉以免加载重型依赖链。
vi.mock("@/stores/pluginStore", () => ({
  usePluginStore: (selector: (s: unknown) => unknown) => selector({ slotRevisions: {}, uiRevision: 0 }),
}));

let root: Root | null = null;
let container: HTMLDivElement | null = null;

/** 挂载被装饰内容（jsdom 容器内），返回可查询容器。 */
function mountDecorated(): HTMLDivElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  return container;
}

afterEach(async () => {
  for (const d of listDecorators("toolbar/note/right")) unregisterSlotDecorator(d.id);
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

describe("SlotDecoratedContent 装饰器校验", () => {
  it("装饰器正常包裹 children：内容在装饰器内渲染", async () => {
    registerSlotDecoratorFor("toolbar/note/right", "com.test.deco", ({ children }) =>
      React.createElement("div", { id: "wrapper" }, children),
    );
    const host = mountDecorated();
    await act(async () => {
      root!.render(
        React.createElement(
          SlotDecoratedContent,
          { slot: "toolbar/note/right" },
          React.createElement("div", { id: "content" }, "内容"),
        ),
      );
    });
    const wrapper = host.querySelector("#wrapper");
    expect(wrapper).not.toBeNull();
    expect(wrapper?.querySelector("#content")).not.toBeNull();
  });

  it("装饰器吞掉 children：被校验剔除，内容回退渲染（不消失）", async () => {
    registerSlotDecoratorFor("toolbar/note/right", "com.test.swallow", () => null);
    const host = mountDecorated();
    await act(async () => {
      root!.render(
        React.createElement(
          SlotDecoratedContent,
          { slot: "toolbar/note/right" },
          React.createElement("div", { id: "content" }, "内容"),
        ),
      );
    });
    // 提交后 marker 未挂载 → 剔除装饰器 → 内容回退（无装饰器包裹）。
    expect(host.querySelector("#content")).not.toBeNull();
    expect(host.querySelector("#wrapper")).toBeNull();
  });

  it("无装饰器时原样渲染内容", async () => {
    const host = mountDecorated();
    await act(async () => {
      root!.render(
        React.createElement(
          SlotDecoratedContent,
          { slot: "toolbar/note/right" },
          React.createElement("div", { id: "content" }, "内容"),
        ),
      );
    });
    expect(host.querySelector("#content")).not.toBeNull();
  });

  it("装饰器渲染抛错：崩溃守卫剔除，内容回退渲染", async () => {
    registerSlotDecoratorFor("toolbar/note/right", "com.test.crash", () => {
      throw new Error("boom");
    });
    const host = mountDecorated();
    await act(async () => {
      root!.render(
        React.createElement(
          SlotDecoratedContent,
          { slot: "toolbar/note/right" },
          React.createElement("div", { id: "content" }, "内容"),
        ),
      );
    });
    // 崩溃的装饰器被 SlotDecoratorGuard 捕获并剔除 → 内容回退（不被崩溃拖垮）。
    expect(host.querySelector("#content")).not.toBeNull();
  });

  it("多层装饰器：外层吞掉 children → 只剔除外层，内层与内容保留", async () => {
    registerSlotDecoratorFor("toolbar/note/right", "com.test.outer", () => null, { priority: 5 });
    registerSlotDecoratorFor("toolbar/note/right", "com.test.inner", ({ children }) =>
      React.createElement("div", { id: "inner" }, children),
    );
    const host = mountDecorated();
    await act(async () => {
      root!.render(
        React.createElement(
          SlotDecoratedContent,
          { slot: "toolbar/note/right" },
          React.createElement("div", { id: "content" }, "内容"),
        ),
      );
    });
    // 外层吞 children → marker[0] 缺失 → 剔除外层；内层正常包裹，内容在内层内。
    expect(host.querySelector("#inner")).not.toBeNull();
    expect(host.querySelector("#inner")?.querySelector("#content")).not.toBeNull();
  });

  it("剔除后同 id 重注册（插件重载）：纪元号变化复位剔除，修复后的装饰器重新生效", async () => {
    const off = registerSlotDecoratorFor("toolbar/note/right", "com.test.fixed", () => null);
    const host = mountDecorated();
    await act(async () => {
      root!.render(
        React.createElement(
          SlotDecoratedContent,
          { slot: "toolbar/note/right" },
          React.createElement("div", { id: "content" }, "内容"),
        ),
      );
    });
    // 吞 children → 被剔除 → 内容回退（无包裹）。
    expect(host.querySelector("#content")).not.toBeNull();
    expect(host.querySelector("#wrapper")).toBeNull();

    // 模拟插件重载：撤销旧装饰器 → 同 id 重注册（修复为正常包裹）→ 宿主重渲染。
    off();
    registerSlotDecoratorFor("toolbar/note/right", "com.test.fixed", ({ children }) =>
      React.createElement("div", { id: "wrapper" }, children),
    );
    await act(async () => {
      root!.render(
        React.createElement(
          SlotDecoratedContent,
          { slot: "toolbar/note/right" },
          React.createElement("div", { id: "content" }, "内容"),
        ),
      );
    });
    // 纪元号变化 → 剔除记录复位 → 修复后的装饰器重新包裹内容。
    expect(host.querySelector("#wrapper")).not.toBeNull();
    expect(host.querySelector("#wrapper")?.querySelector("#content")).not.toBeNull();
  });
});
