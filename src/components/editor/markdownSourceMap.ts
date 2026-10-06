/**
 * 源文本偏移 ↔ 渲染 DOM 的映射（编辑面专用）：光标/选区绘制与点击落点换算基于本模块，
 * 只依赖 DOM 结构、不依赖布局测量。内核 `offsets` 模式在块容器与行内元素上打
 * `data-md-from/to`，片段分线性（逐字符对应）与原子（整体取边界）两类，定义见 buildSourceIndex。
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

/** 标记宿主：直接含源标记字符的元素（块或行内），揭示时给它切 `md-reveal`。 */
export interface SourceOwner {
  readonly from: number;
  readonly to: number;
  readonly el: Element;
}

export interface SourceIndex {
  readonly runs: SourceRun[];
  readonly atomics: AtomicSpan[];
  readonly blocks: SourceBlock[];
  readonly owners: SourceOwner[];
}

function num(el: Element, name: string): number | null {
  const raw = el.getAttribute(name);
  if (raw === null) return null;
  const value = Number(raw);
  return Number.isFinite(value) ? value : null;
}

/**
 * 文本节点是否只含空白：列表项内容与首个嵌套子块之间的缩进空白标记。
 * 它渲染在上一行的行尾，源偏移却落在下一行的缩进上——命中它定落点会把光标带到下一行行首，
 * 故点击命中与几何兜底都把它排除在落点之外（零宽锚含 U+200B，不算空白，仍可作落点）。
 */
export function isBlankTextNode(node: Text): boolean {
  return !/\S/.test(node.data);
}

/** 扫描渲染容器，建立偏移索引（内容每次替换后重建）。
 *  线性片段（runs）= 只含一个文本节点且文本长度等于源区间（纯文本 span、源标记 span、代码正文），每字符一一对应；
 *  原子片段（atomics）= 源区间与可见文本长度不等的替换形态（数学/图片/标签/胶囊/空标签回退成地址的链接）
 *  与零宽锚（空行行元素、空项内容锚、代码末尾空行），按整体取边界。 */
export function buildSourceIndex(root: HTMLElement): SourceIndex {
  const runs: SourceRun[] = [];
  const atomics: AtomicSpan[] = [];
  const blocks: SourceBlock[] = [];
  const owners: SourceOwner[] = [];

  for (const el of Array.from(root.querySelectorAll<HTMLElement>("[data-md-own]"))) {
    const from = num(el, "data-md-from");
    const to = num(el, "data-md-to");
    if (from === null || to === null) continue;
    owners.push({ from, to, el });
  }

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
  return { runs, atomics, blocks, owners };
}

/** 源偏移 → DOM 文本位置。落在原子片段内时贴到其起点（终点处贴终点）。 */
export function offsetToPoint(index: SourceIndex, offset: number): { node: Node; offset: number } | null {
  // 优先取区间内的片段（左闭右开）：偏移落在两段边界上时取后一段——后一段的起点与前一段
  // 的终点同处（相邻），而前一段可能是默认隐藏的源标记（display:none 量不出光标矩形）
  for (const run of index.runs) {
    if (offset >= run.from && offset < run.to) {
      return { node: run.node, offset: offset - run.from };
    }
  }
  for (const run of index.runs) {
    if (offset >= run.from && offset <= run.to) {
      return { node: run.node, offset: offset - run.from };
    }
  }
  for (const atomic of index.atomics) {
    // 零宽锚片段（空行行元素 / 空项内容锚）：点区间无真文本，光标驻留在其零宽字符处
    if (atomic.from === atomic.to) {
      if (offset === atomic.from && atomic.el.firstChild) return { node: atomic.el.firstChild, offset: 0 };
      continue;
    }
    if (offset > atomic.from && offset <= atomic.to) {
      // 原子片段内部没有可落点：起点贴起点，终点落兜底（终点不会再有更近的文本边界）
      if (offset === atomic.to) continue;
      const point = offsetToPoint(index, atomic.from);
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
  // 命中渲染件内部（图片 / 公式 / 徽标等自身不带文本的节点）：落到其宿主区间的起点——
  // 否则点击会一路冒泡到块起点，落点偏离被点的那一处
  if (node.nodeType === Node.ELEMENT_NODE || node.parentElement) {
    const host = (node.nodeType === Node.ELEMENT_NODE ? (node as Element) : node.parentElement!)?.closest(
      "[data-md-from]",
    );
    const from = host ? num(host, "data-md-from") : null;
    if (from !== null) return from;
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

/** 点击坐标最近的可见片段（含矩形）：点击兜底用。 */
export interface NearestSpan {
  readonly from: number;
  readonly to: number;
  readonly rect: DOMRect;
}

/**
 * 点击坐标最近的可见片段：先贴近所在行（垂直距离），同行再贴近水平位置。
 * 命中的不是文本节点时（缩进悬挂位、行间留白、块内边距、浮动标记、绘制层）用它来定落点——
 * 只按 DOM 祖先取块起点会让「点这一行」跳到别的行（如点嵌套列表项跳到下一行行首）。
 * 明显落在内容下方空白区（超过一行高）时返回 null，由调用方按「文末」处理；
 * 无布局测量能力的环境（如 jsdom）返回 null。
 */
export function nearestSpanAt(index: SourceIndex, x: number, y: number): NearestSpan | null {
  const candidates: NearestSpan[] = [];
  for (const run of index.runs) {
    // 只含空白的片段（列表项缩进空白标记）渲染在上一行行尾，不作落点
    if (isBlankTextNode(run.node)) continue;
    const el = run.node.parentElement;
    if (el) candidates.push({ from: run.from, to: run.to, rect: el.getBoundingClientRect() });
  }
  for (const atomic of index.atomics) {
    candidates.push({ from: atomic.from, to: atomic.to, rect: atomic.el.getBoundingClientRect() });
  }
  let best: NearestSpan | null = null;
  let bestDy = Infinity;
  let bestDx = Infinity;
  let bottom = -Infinity;
  for (const candidate of candidates) {
    const { rect } = candidate;
    // 默认隐藏的源标记量不出矩形，不能作落点
    if (rect.height === 0 && rect.width === 0) continue;
    if (rect.bottom > bottom) bottom = rect.bottom;
    const dy = y < rect.top ? rect.top - y : y > rect.bottom ? y - rect.bottom : 0;
    const dx = x < rect.left ? rect.left - x : x > rect.right ? x - rect.right : 0;
    if (best === null || dy < bestDy - 0.5 || (dy < bestDy + 0.5 && dx < bestDx)) {
      best = candidate;
      bestDy = dy;
      bestDx = dx;
    }
  }
  if (!best || y > bottom + 8) return null;
  return best;
}