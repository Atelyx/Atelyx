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

  it("光标所在块与所在行内元素揭示，移出即收起；光标移动不重建 DOM", () => {
    const { container } = render(
      <MarkdownEditor body={"# 标题 前后\n\n正文 **粗** 段\n"} syncSeq={0} readOnly={false} />,
    );
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    const revealed = () => Array.from(container.querySelectorAll(".md-reveal")).map((el) => el.tagName);
    const nodes = () => Array.from(container.querySelector(".md-edit-content")!.childNodes);

    moveCaret(ta, 2); // 标题内
    expect(revealed()).toContain("H1");
    const headingBlocks = nodes();

    moveCaret(ta, 8); // 块间空行：不揭示任何处
    expect(revealed()).toEqual([]);
    // 移光标只切 class，内容节点一个都没换
    expect(nodes()).toEqual(headingBlocks);

    moveCaret(ta, 10); // 正文里，但在 `**粗**` 之外：只揭示所在段落
    expect(revealed()).toEqual(["P"]);
    moveCaret(ta, 14); // 进入 `**粗**`：行内元素与所在段落一并揭示
    expect(revealed()).toEqual(["P", "STRONG"]);
    expect(nodes()).toEqual(headingBlocks);
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

describe("按分片增量替换", () => {
  /** 聚焦输入面并把光标移到指定偏移（经 document selectionchange 驱动 applySelection）。 */
  function moveCaret(ta: HTMLTextAreaElement, offset: number): void {
    ta.focus();
    ta.setSelectionRange(offset, offset);
    document.dispatchEvent(new Event("selectionchange"));
  }

  /** 模拟输入面上的一次文本变更（value 即正文真相）。 */
  function typeText(ta: HTMLTextAreaElement, value: string, caret: number): void {
    ta.value = value;
    ta.setSelectionRange(caret, caret);
    fireEvent.input(ta);
  }

  const body = "# 标题\n\n正文\n\n尾段";

  it("局部编辑只替换被编辑的分片，其余分片 DOM 节点原样保留", () => {
    const { container } = render(<MarkdownEditor body={body} syncSeq={0} readOnly={false} />);
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 8); // 光标进入「正文」块（该块只揭示标记，不重建 DOM）
    const before = Array.from(content.childNodes);
    expect(before).toHaveLength(5);

    typeText(ta, "# 标题\n\n正文x\n\n尾段", 9);

    const after = Array.from(content.childNodes);
    expect(after).toHaveLength(5);
    expect(after[0]).toBe(before[0]); // 标题块未变
    expect(after[1]).toBe(before[1]);
    expect(after[2]).not.toBe(before[2]); // 被编辑的块重建
    expect(after[3]).toBe(before[3]);
    expect(after[4]).toBe(before[4]);
  });

  it("保留分片的源偏移随前文增删平移（映射不错位）", () => {
    const { container } = render(<MarkdownEditor body={body} syncSeq={0} readOnly={false} />);
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 8);
    typeText(ta, "# 标题\n\n正文x\n\n尾段", 9);
    // 插入一个字符后，后续空行与段落的偏移都应 +1
    expect(content.children[3]?.getAttribute("data-md-from")).toBe("10");
    expect(content.children[4]?.getAttribute("data-md-from")).toBe("11");
    expect(
      content.querySelectorAll("[data-md-from]")[content.querySelectorAll("[data-md-from]").length - 1]
        ?.getAttribute("data-md-to"),
    ).toBe("13");
  });

  it("分片数量增加（Enter 开出空行）时原位分片保留、只追加新分片", () => {
    const { container } = render(<MarkdownEditor body={"甲"} syncSeq={0} readOnly={false} />);
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 1);
    const before = Array.from(content.childNodes);
    expect(before).toHaveLength(1);

    typeText(ta, "甲\n\n", 3);

    const after = Array.from(content.childNodes);
    expect(after).toHaveLength(2);
    expect(after[0]).toBe(before[0]); // 原段落未被重建
    expect(content.children[1]?.className).toBe("md-editor-gap");
  });

  it("在空行后继续输入：新段落立即进入正文并上报", () => {
    const onChange = vi.fn();
    const { container } = render(<MarkdownEditor body={""} syncSeq={0} readOnly={false} onBodyChange={onChange} />);
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 0);

    typeText(ta, "hello", 5);
    expect(onChange.mock.calls.at(-1)?.[0]).toBe("hello");

    typeText(ta, "hello\n", 6); // 第一次回车
    typeText(ta, "hello\n\n", 7); // 第二次回车：光标落在块间空行
    typeText(ta, "hello\n\nworld", 12);

    expect(onChange.mock.calls.at(-1)?.[0]).toBe("hello\n\nworld");
    expect(content.textContent).toContain("world");
    expect(content.children[content.children.length - 1]?.textContent).toBe("world");
  });

  it("分片数量减少（删掉空行）时移除多余分片、保留原位分片", () => {
    const { container } = render(<MarkdownEditor body={"甲\n\n"} syncSeq={0} readOnly={false} />);
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 1);
    const before = Array.from(content.childNodes);
    expect(before).toHaveLength(2);

    typeText(ta, "甲", 1);

    const after = Array.from(content.childNodes);
    expect(after).toHaveLength(1);
    expect(after[0]).toBe(before[0]);
  });

  it("输入之后移动光标仍不重建任何分片（整块源码态只认富装饰块）", () => {
    const { container } = render(<MarkdownEditor body={"甲\n\n乙\n"} syncSeq={0} readOnly={false} />);
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 1);
    typeText(ta, "甲甲\n\n乙\n", 2);
    const after = Array.from(content.childNodes);

    moveCaret(ta, 1); // 同段内移动
    expect(Array.from(content.childNodes)).toEqual(after);
    moveCaret(ta, 4); // 跨段移动
    expect(Array.from(content.childNodes)).toEqual(after);
  });

  it("代码块：光标在代码正文里保持渲染态，落到围栏行才显围栏（点标题栏的落点）", () => {
    const { container } = render(<MarkdownEditor body={"```js\nconst x = 1;\n```"} syncSeq={0} readOnly={false} />);
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    const revealed = () => Array.from(container.querySelectorAll(".md-marker.md-reveal")).map((el) => el.textContent);
    const tags = () => Array.from(container.querySelector(".md-edit-content")!.childNodes);

    moveCaret(ta, 10); // 代码正文里：围栏不显形，块保持渲染态可继续编辑
    expect(revealed()).toEqual([]);
    const codeBlock = tags();

    moveCaret(ta, 1); // 开围栏行内（点标题栏即落到块起点）
    expect(revealed()).toEqual(["```js"]);
    moveCaret(ta, 18); // 正文末字符（末行行尾）：仍属正文，不切源码态
    expect(revealed()).toEqual([]);
    moveCaret(ta, 20); // 闭围栏行内
    expect(revealed()).toEqual(["```"]);
    // 揭示全程只切 class，分片节点没有重建
    expect(tags()).toEqual(codeBlock);
  });

  it("跨块移动光标不重建任何分片（揭示只是切 class）", () => {
    const { container } = render(<MarkdownEditor body={body} syncSeq={0} readOnly={false} />);
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 2);
    const first = Array.from(content.childNodes);
    expect(first).toHaveLength(5);

    moveCaret(ta, 8);
    expect(Array.from(content.childNodes)).toEqual(first);
  });
});

describe("输入法组合", () => {
  function moveCaret(ta: HTMLTextAreaElement, offset: number): void {
    ta.focus();
    ta.setSelectionRange(offset, offset);
    document.dispatchEvent(new Event("selectionchange"));
  }

  /** jsdom 未实现 CompositionEvent：用普通事件携带 data 字段。 */
  function emitComposition(el: HTMLTextAreaElement, type: string, data: string): void {
    const event = new Event(type);
    Object.defineProperty(event, "data", { value: data });
    el.dispatchEvent(event);
  }

  it("未上屏文本就地渲染在光标处，组合结束后消失且正文落盘", () => {
    const onChange = vi.fn();
    const { container } = render(
      <MarkdownEditor body={"甲乙"} syncSeq={0} readOnly={false} onBodyChange={onChange} />,
    );
    const content = container.querySelector(".md-edit-content") as HTMLElement;
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 1);

    emitComposition(ta, "compositionstart", "");
    ta.value = "甲ni乙";
    emitComposition(ta, "compositionupdate", "ni");
    expect(content.querySelector(".md-composition")?.textContent).toBe("ni");
    // 组合串两侧仍是原正文区间，映射不随未上屏文本漂移
    const spans = Array.from(content.querySelectorAll("span[data-md-from]")).map((el) =>
      `${el.getAttribute("data-md-from")}-${el.getAttribute("data-md-to")}`,
    );
    expect(spans).toEqual(["0-1", "1-2"]);

    emitComposition(ta, "compositionend", "ni");
    expect(content.querySelector(".md-composition")).toBeNull();
    expect(onChange.mock.calls.at(-1)?.[0]).toBe("甲ni乙");
  });
});

describe("列表行 Enter 延续", () => {
  /** 聚焦输入面并把光标移到指定偏移（经 document selectionchange 驱动 applySelection）。 */
  function moveCaret(ta: HTMLTextAreaElement, offset: number): void {
    ta.focus();
    ta.setSelectionRange(offset, offset);
    document.dispatchEvent(new Event("selectionchange"));
  }

  it("输入面上的 Enter 不被原生控件守卫排除：列表行拆分并延续标记", () => {
    const onChange = vi.fn();
    const { container } = render(
      <MarkdownEditor body={"- 甲项\n- 乙项"} syncSeq={0} readOnly={false} onBodyChange={onChange} />,
    );
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 3); // "- 甲|项"
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onChange).toHaveBeenCalled();
    const latest = onChange.mock.calls.at(-1)?.[0] as string;
    expect(latest).toBe("- 甲\n- 项\n- 乙项");
  });

  it("空项标记之后退格：一次删掉整段标记，不残留标记（回车 + 退格可回到原状）", () => {
    const onChange = vi.fn();
    const { container } = render(
      <MarkdownEditor body={"- 甲项\n- "} syncSeq={0} readOnly={false} onBodyChange={onChange} />,
    );
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 7); // 空项标记之后
    fireEvent.keyDown(ta, { key: "Backspace" });
    expect(onChange.mock.calls.at(-1)?.[0]).toBe("- 甲项\n");
  });

  it("有内容的列表项上退格不被接管（逐字符删除交浏览器）", () => {
    const onChange = vi.fn();
    const { container } = render(
      <MarkdownEditor body={"- 甲项"} syncSeq={0} readOnly={false} onBodyChange={onChange} />,
    );
    const ta = container.querySelector("textarea") as HTMLTextAreaElement;
    moveCaret(ta, 4);
    fireEvent.keyDown(ta, { key: "Backspace" });
    expect(onChange).not.toHaveBeenCalled();
  });
});

describe("任务勾选框点击写回", () => {
  it("大写 [X] 的已勾选任务项点击后写回未勾选", () => {
    const onChange = vi.fn();
    const { container } = render(
      <MarkdownEditor body={"- [X] 甲"} syncSeq={0} readOnly={false} onBodyChange={onChange} />,
    );
    const checkbox = container.querySelector("input.md-editor-checkbox") as HTMLInputElement;
    expect(checkbox).not.toBeNull();
    expect(checkbox.checked).toBe(true); // 解析侧按大小写不敏感判定，大写同样渲染为已勾选
    fireEvent.mouseDown(checkbox, { button: 0 });
    expect(onChange.mock.calls.at(-1)?.[0]).toBe("- [ ] 甲");
  });

  it("未勾选任务项点击后写回小写 x", () => {
    const onChange = vi.fn();
    const { container } = render(
      <MarkdownEditor body={"- [ ] 甲"} syncSeq={0} readOnly={false} onBodyChange={onChange} />,
    );
    const checkbox = container.querySelector("input.md-editor-checkbox") as HTMLInputElement;
    expect(checkbox.checked).toBe(false);
    fireEvent.mouseDown(checkbox, { button: 0 });
    expect(onChange.mock.calls.at(-1)?.[0]).toBe("- [x] 甲");
  });
});