/**
 * 源文本偏移 ↔ 渲染 DOM 的映射（编辑面专用）。
 *
 * 内核在 `offsets` 模式下给块容器与行内元素打了 `data-md-from/to`：
 * - 「线性片段」：元素只含一个文本节点且文本长度等于源区间长度（纯文本 span、代码正文），
 *   区间内每个字符都能一一对应；
 * - 「原子片段」：行内代码 / 数学 / 图片 / 标签 / 链接等标记被隐藏的替换形态，源区间与可见
 *   文本长度不等，按整体取边界（光标落在其起点或终点）。
 *
 * 光标与选区的绘制、点击落点换算都基于本模块；映射只依赖 DOM 结构，不依赖布局测量。
 */

export interface SourceRun {
  readonly from: number;
  readonly to: number;
  readonly node: Text;
}

export interface AtomicSpan {
  readonly from: number;
  readonly to: number;
  readonly el: Element;
}

export interface SourceBlock {
  readonly from: number;
  readonly to: number;
  readonly el: Element;
}

export interface SourceIndex {
  readonly runs: SourceRun[];
  readonly atomics: AtomicSpan[];
  readonly blocks: SourceBlock[];
}

function num(el: Element, name: string): number | null {
  const raw = el.getAttribute(name);
  if (raw === null) return null;
  const value = Number(raw);
  return Number.isFinite(value) ? value : null;
}

/** 扫描渲染容器，建立偏移索引（内容每次替换后重建）。 */
export function buildSourceIndex(root: HTMLElement): SourceIndex {
  const runs: SourceRun[] = [];
  const atomics: AtomicSpan[] = [];
  const blocks: SourceBlock[] = [];

  for (const el of Array.from(root.querySelectorAll<HTMLElement>("[data-md-from]"))) {
    const from = num(el, "data-md-from");
    const to = num(el, "data-md-to");
    if (from === null || to === null || to < from) continue;
    if (el.hasAttribute("data-md-block")) {
      blocks.push({ from, to, el });
      continue;
    }
    // 容器元素（子元素自带偏移）不参与映射，由更内层的元素承担
    if (el.querySelector("[data-md-from]")) continue;
    const only = el.childNodes.length === 1 ? el.firstChild : null;
    if (only?.nodeType === Node.TEXT_NODE && (only as Text).length === to - from) {
      runs.push({ from, to, node: only as Text });
      continue;
    }
    atomics.push({ from, to, el });
  }

  runs.sort((a, b) => a.from - b.from);
  atomics.sort((a, b) => a.from - b.from);
  blocks.sort((a, b) => a.from - b.from);
  return { runs, atomics, blocks };
}

/** 源偏移 → DOM 文本位置。落在原子片段内时贴到其起点（终点处贴终点）。 */
export function offsetToPoint(index: SourceIndex, offset: number): { node: Node; offset: number } | null {
  for (const run of index.runs) {
    if (offset >= run.from && offset <= run.to) {
      return { node: run.node, offset: offset - run.from };
    }
  }
  for (const atomic of index.atomics) {
    if (offset > atomic.from && offset <= atomic.to) {
      // 原子片段内部没有可落点，贴到最近边界
      const atEnd = offset === atomic.to;
      const point = offsetToPoint(index, atEnd ? atomic.to : atomic.from);
      if (point) return point;
    }
  }
  // 兜底：贴到最近的线性片段边界（块级替换：表格/HTML/围栏代码等）
  let best: SourceRun | null = null;
  for (const run of index.runs) {
    if (run.to <= offset && (!best || run.to > best.to)) best = run;
  }
  if (best) return { node: best.node, offset: best.node.length };
  const first = index.runs[0];
  return first ? { node: first.node, offset: 0 } : null;
}

/** DOM 文本位置 → 源偏移（点击/选区换算用）。不在任何片段内返回 null。 */
export function pointToOffset(index: SourceIndex, node: Node, localOffset: number): number | null {
  for (const run of index.runs) {
    if (run.node === node) {
      return run.from + Math.max(0, Math.min(localOffset, run.to - run.from));
    }
  }
  for (const atomic of index.atomics) {
    if (atomic.el === node || atomic.el.contains(node)) {
      return atomic.from;
    }
  }
  return null;
}

/** 源偏移处的光标矩形（自绘光标用）；无布局环境返回 null。 */
export function caretRect(index: SourceIndex, offset: number): DOMRect | null {
  const point = offsetToPoint(index, offset);
  if (!point) return null;
  // 无布局测量能力的环境（如 jsdom）没有光标可画：降级为不显示，而不是抛错
  if (typeof Range.prototype.getBoundingClientRect !== "function") return null;
  const range = document.createRange();
  range.setStart(point.node, point.offset);
  range.collapse(true);
  const rect = range.getBoundingClientRect();
  if (rect.height === 0 && rect.width === 0 && rect.top === 0) return null;
  return rect;
}

/** 源区间在视口内的若干矩形（自绘选区用；跨块会得到多段）。 */
export function rangeRects(index: SourceIndex, from: number, to: number): DOMRect[] {
  if (to <= from) return [];
  if (typeof Range.prototype.getClientRects !== "function") return [];
  const rects: DOMRect[] = [];
  for (const run of index.runs) {
    const start = Math.max(from, run.from);
    const end = Math.min(to, run.to);
    if (end <= start) continue;
    const range = document.createRange();
    range.setStart(run.node, start - run.from);
    range.setEnd(run.node, end - run.from);
    for (const rect of Array.from(range.getClientRects())) {
      if (rect.width > 0 || rect.height > 0) rects.push(rect);
    }
  }
  // 原子片段整体选中时给一个盒子，避免选区在链接/代码处断开
  for (const atomic of index.atomics) {
    if (from <= atomic.from && to >= atomic.to) {
      const rect = atomic.el.getBoundingClientRect();
      if (rect.width > 0 || rect.height > 0) rects.push(rect);
    }
  }
  return rects;
}

/** 源偏移所在的块（编辑面据此决定「活动块回显源码」）。 */
export function blockAtOffset(index: SourceIndex, offset: number): SourceBlock | null {
  let best: SourceBlock | null = null;
  for (const block of index.blocks) {
    if (offset >= block.from && offset <= block.to) {
      // 嵌套块（引用内）取最小的那个
      if (!best || block.to - block.from < best.to - best.from) best = block;
    }
  }
  return best;
}