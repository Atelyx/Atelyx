// @vitest-environment jsdom
/**
 * 编辑面结构回归：自绘光标/选区的绘制层必须与内容容器同级。
 *
 * 内容容器每次输入都会整篇替换 innerHTML；绘制层若在其中会被一并清掉，
 * 之后所有绘制都落在已脱离文档的节点上（表现为「编辑光标不显示」）。
 */
import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

// 编辑面经 store 访问宿主能力，其依赖链会拖入 esbuild-wasm（jsdom 不兼容）；
// 本测试只验证结构与绘制层归属，mock 掉插件 store 以免加载重型依赖链。
vi.mock("@/stores/pluginStore", () => ({
  usePluginStore: (selector: (s: unknown) => unknown) => selector({ slotRevisions: {}, uiRevision: 0 }),
}));

// 光标矩形固定在视口上方外侧：滚动跟随与劫持行为都需要一个可视区外的光标才能触发
vi.mock("./markdownSourceMap", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./markdownSourceMap")>();
  return {
    ...actual,
    caretRect: () =>
      ({ top: -50, bottom: -40, left: 0, right: 2, width: 2, height: 18, x: 0, y: -50, toJSON: () => ({}) }) as DOMRect,
  };
});

import { MarkdownEditor } from "./MarkdownEditor";

function Harness() {
  const [seq, setSeq] = useState(0);
  return (
    <>
      <button onClick={() => setSeq((s) => s + 1)}>bump</button>
      <MarkdownEditor body={"# 标题\n\n正文 **粗**"} syncSeq={seq} readOnly={false} />
    </>
  );
}

describe("MarkdownEditor 编辑面结构", () => {
  it("绘制层不在内容容器内，且已挂载", () => {
    const { container } = render(<Harness />);
    const layer = container.querySelector(".md-caret-layer");
    const content = container.querySelector(".md-edit-content");
    expect(layer).not.toBeNull();
    expect(content).not.toBeNull();
    expect(content?.contains(layer as Node)).toBe(false);
    expect((layer as HTMLElement).isConnected).toBe(true);
  });

  it("内容整篇重绘后绘制层仍是同一个节点且仍在文档中", () => {
    const { container } = render(<Harness />);
    const before = container.querySelector(".md-caret-layer");
    fireEvent.click(container.querySelector("button") as HTMLElement); // syncSeq 变化 → 内容整篇重绘
    const after = container.querySelector(".md-caret-layer");
    expect(after).not.toBeNull();
    expect(after).toBe(before);
    expect((after as HTMLElement).isConnected).toBe(true);
    expect(container.querySelector(".md-edit-content")?.contains(after as Node)).toBe(false);
  });

  it("隐藏输入面存在，点击编辑区不抛错", () => {
    const { container } = render(<Harness />);
    expect(container.querySelector("textarea")).not.toBeNull();
    const host = container.querySelector("[data-markdown-editor]") as HTMLElement;
    expect(() => fireEvent.mouseDown(host, { clientX: 5, clientY: 5 })).not.toThrow();
  });
});

describe("MarkdownEditor 滚动与选区行为", () => {
  /** 聚焦输入面并把光标移到指定偏移（经 document selectionchange 驱动 applySelection）。 */
  function moveCaret(ta: HTMLTextAreaElement, offset: number): void {
    ta.focus();
    ta.setSelectionRange(offset, offset);
    document.dispatchEvent(new Event("selectionchange"));
  }

  it("滚动重绘不反调 scrollTop（容器自由滚动，不被光标拉回）", () => {
    const { container } = render(<Harness />);
    const scroll = container.querySelector(".md-edit-scroll") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 2);
    scroll.scrollTop = 100;
    scroll.dispatchEvent(new Event("scroll"));
    expect(scroll.scrollTop).toBe(100);
  });

  it("选区落在块外（文末）不切换活动块：当前块保持源码态", () => {
    const { container } = render(
      <MarkdownEditor body={"# 标题\n\n正文段落\n"} syncSeq={0} readOnly={false} />,
    );
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    const sourceText = () =>
      (container.querySelector(".md-editor-source") as HTMLElement | null)?.textContent;
    moveCaret(ta, 2);
    expect(sourceText()).toBe("# 标题");
    // 文末偏移不落在任何块内（尾随换行在最后一块 to 之外）
    moveCaret(ta, 11);
    expect(sourceText()).toBe("# 标题");
  });
});

describe("外点判定与活动块重建的时序", () => {
  it("mousedown 冒泡途中同步移除命中节点：捕获阶段判内点、冒泡阶段误判外点", () => {
    render(
      <div data-testid="root">
        <button data-testid="block">块</button>
      </div>,
    );
    const root = screen.getByTestId("root");
    const block = screen.getByTestId("block");
    const hits: boolean[] = [];
    const record = (e: Event) => hits.push(root.contains(e.target as Node));
    document.addEventListener("mousedown", record, true);
    document.addEventListener("mousedown", record, false);
    // 模拟活动块切换：mousedown 处理器同步重建正文 DOM，命中节点脱离文档
    block.addEventListener("mousedown", () => block.remove());
    fireEvent.mouseDown(block);
    document.removeEventListener("mousedown", record, true);
    document.removeEventListener("mousedown", record, false);
    expect(hits).toEqual([true, false]);
  });
});