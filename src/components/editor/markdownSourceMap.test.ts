// @vitest-environment jsdom
/**
 * 源偏移 ↔ DOM 映射单测：编辑面的点击落点、光标与选区绘制都依赖它。
 */
import { describe, expect, it } from "vitest";
import { renderMarkdownToHtml } from "@/utils/markdownCore";
import { blockAtOffset, buildSourceIndex, offsetToPoint, pointToOffset } from "./markdownSourceMap";

function mount(source: string): { root: HTMLElement; index: ReturnType<typeof buildSourceIndex> } {
  const root = document.createElement("div");
  root.innerHTML = renderMarkdownToHtml(source, { offsets: true });
  return { root, index: buildSourceIndex(root) };
}

describe("buildSourceIndex", () => {
  it("线性文本片段与块都被收录，容器元素不重复计入", () => {
    const { index } = mount("para **bold** end");
    expect(index.blocks).toHaveLength(1);
    // 「para 」「bold」「 end」三段线性文本（strong 为容器，不自成片段）
    expect(index.runs.map((r) => r.node.length)).toEqual([5, 4, 4]);
    expect(index.atomics).toHaveLength(0);
  });

  it("隐藏标记的替换形态按原子片段处理（行内代码）", () => {
    const { index } = mount("a `code` b");
    expect(index.atomics).toHaveLength(1);
    expect(index.atomics[0]?.el.tagName).toBe("CODE");
  });
});

describe("偏移 ↔ DOM 映射", () => {
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

  it("落在原子片段内贴到边界", () => {
    const { index: codeIndex } = mount("a `code` b");
    const atomic = codeIndex.atomics[0]!;
    const point = offsetToPoint(codeIndex, atomic.from + 2);
    expect(point).not.toBeNull();
    // 贴到原子片段起点（源码下一个字符位于 `code` 之前）
    expect(pointToOffset(codeIndex, point!.node, point!.offset)).toBe(atomic.from);
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