// @vitest-environment jsdom
/**
 * 源偏移 ↔ DOM 映射单测：编辑面的点击落点、光标与选区绘制都依赖它。
 */
import { describe, expect, it } from "vitest";
import { renderMarkdownToHtml } from "@/utils/markdownCore";
import { blockAtOffset, buildSourceIndex, nearestSpanAt, offsetToPoint, pointToOffset } from "./markdownSourceMap";

function mount(source: string): { root: HTMLElement; index: ReturnType<typeof buildSourceIndex> } {
  const root = document.createElement("div");
  root.innerHTML = renderMarkdownToHtml(source, { offsets: true });
  return { root, index: buildSourceIndex(root) };
}

describe("buildSourceIndex", () => {
  it("线性文本片段与块都被收录，容器元素不重复计入", () => {
    const { index } = mount("para **bold** end");
    expect(index.blocks).toHaveLength(1);
    // 「para 」「**」「bold」「**」「 end」（strong 为容器，不自成片段；标记字符也是片段）
    expect(index.runs.map((r) => r.node.length)).toEqual([5, 2, 4, 2, 4]);
    expect(index.atomics).toHaveLength(0);
  });

  it("行内代码：定界反引号与代码正文各自成片段（可逐字符定位）", () => {
    const { index } = mount("a `code` b");
    expect(index.atomics).toHaveLength(0);
    expect(index.runs.map((r) => r.node.data)).toEqual(["a ", "`", "code", "`", " b"]);
  });

  it("标记宿主被收录（段落可带行前缀标记，故段落也是宿主）", () => {
    const { index } = mount("para **bold** end");
    expect(index.owners.map((o) => o.el.tagName)).toEqual(["P", "STRONG"]);
  });
});

describe("偏移 ↔ DOM 映射", () => {
  it("围栏末尾空行：光标落在空行锚上而非代码文本末端（几何可测）", () => {
    // 源码 "```\nx\n\n```"：正文 x（4..5）、末尾换行（5）、空行起点（6）
    const { index } = mount("```\nx\n\n```");
    const blank = offsetToPoint(index, 6);
    expect(blank).not.toBeNull();
    expect((blank!.node.parentElement as HTMLElement).className).toBe("md-editor-code-blank");
    // 末行行尾（5）仍落在代码文本上，两处是彼此独立的落点
    const codeEnd = offsetToPoint(index, 5);
    expect(codeEnd!.node.textContent).toBe("x");
    expect((codeEnd!.node.parentElement as Element).closest("code")).not.toBeNull();
    expect(codeEnd!.node).not.toBe(blank!.node);
  });

  const source = "para **bold** end";
  const { index } = mount(source);

  it("偏移落到对应文本节点与局部下标", () => {
    const point = offsetToPoint(index, 10); // 「bold」内的 b 之后
    expect(point?.node.textContent).toBe("bold");
    expect(point?.offset).toBe(3);
  });

  it("往返一致", () => {
    for (const offset of [0, 5, 7, 11, 13, source.length]) {
      const point = offsetToPoint(index, offset);
      expect(point, `offset ${offset}`).not.toBeNull();
      expect(pointToOffset(index, point!.node, point!.offset)).toBe(offset);
    }
  });

  it("定界反引号可直接落点（标记字符进 DOM，不再贴到片段边界）", () => {
    const { index: codeIndex } = mount("a `code` b");
    for (const offset of [2, 3, 7, 8]) {
      const point = offsetToPoint(codeIndex, offset);
      expect(point, `offset ${offset}`).not.toBeNull();
      expect(pointToOffset(codeIndex, point!.node, point!.offset), `offset ${offset}`).toBe(offset);
    }
  });

  it("空行行元素各自锚到其零宽字符（光标驻留与点击直接落在对应空行上）", () => {
    const { index } = mount("甲\n\n\n乙");
    const gaps = index.atomics.filter((a) => a.from === a.to);
    expect(gaps).toHaveLength(2);
    // 每个空行的偏移各自锚到自己的行元素（无几何换算）
    for (const gap of gaps) {
      const point = offsetToPoint(index, gap.from);
      expect(point?.node, `offset ${gap.from}`).toBe(gap.el.firstChild);
      expect(pointToOffset(index, point!.node, point!.offset)).toBe(gap.from);
    }
  });

  it("blockAtOffset 命中所在块", () => {
    const { index: multi } = mount("para\n\nsecond");
    const first = blockAtOffset(multi, 1);
    const second = blockAtOffset(multi, multi.blocks[1]!.from + 1);
    expect(first?.from).toBe(multi.blocks[0]?.from);
    expect(second?.from).toBe(multi.blocks[1]?.from);
  });
});

describe("nearestSpanAt 点击兜底", () => {
  /** 无布局环境给元素挂上矩形；只测几何规则本身。 */
  function stubRect(el: Element, left: number, top: number, right: number, bottom: number): void {
    el.getBoundingClientRect = () =>
      ({
        left,
        top,
        right,
        bottom,
        width: right - left,
        height: bottom - top,
        x: left,
        y: top,
        toJSON: () => ({}),
      }) as DOMRect;
  }

  it("只含空白的片段不作落点：它渲染在上一行行尾，源偏移却属于下一行的缩进", () => {
    // 源码 "- 项目二\n  - 嵌套项目"：项内容 2..5，缩进空白 6..8，子项标记 8..10
    const { index } = mount("- 项目二\n  - 嵌套项目");
    const content = index.runs.find((run) => run.from === 2)!;
    const indent = index.runs.find((run) => run.from === 6 && run.to === 8)!;
    stubRect(content.node.parentElement!, 40, 0, 94, 20);
    // 缩进空白标记渲染在项内容行尾（点击它右侧一带时几何上最近）
    stubRect(indent.node.parentElement!, 94, 0, 104, 20);
    expect(nearestSpanAt(index, 100, 10)).toMatchObject({ from: 2, to: 5 });
  });
});