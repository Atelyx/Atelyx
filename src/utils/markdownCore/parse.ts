/**
 * Markdown 内核解析：纯文本 → 框架无关文档规格（MarkdownDocument）。
 *
 * 独立用 @lezer/markdown 解析；lezer 不识别的语法（`[[wiki]]`、`#标签`、`==高亮==`、
 * `%%注释%%`、行内/块级数学、脚注、`@mention`）按行正则补充，正则命中若落在代码/链接/
 * 图片/HTML 等不透明区间内则跳过。所有 from/to 均为源文本绝对偏移。
 */
import { parser, GFM } from "@lezer/markdown";
import type { SyntaxNode, Tree } from "@lezer/common";
import { isOpenableUrl } from "@/utils/markdown";
import type {
  InlineSpan,
  LinkForm,
  MarkdownBlock,
  MarkdownDocument,
  MarkdownListItem,
  ParseOptions,
  RangeInfo,
} from "@/types/markdown";

// ===== 私有预扫纯函数（链接/标签/数学等 lezer 不覆盖的语法的检测原语）=====

/** 干净的仓库相对路径校验（无 `..`/`.`/空段、非绝对路径、非盘符或协议前缀），防 shell 打开逃逸仓库根。 */
export function isSafeVaultRelPath(src: string): boolean {
  if (!src) return false;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(src)) return false;
  const parts = src.split(/[\\/]+/);
  return parts.every((p) => p !== "" && p !== "." && p !== "..");
}

/** 行内标签正则（与预览/Rust 提取同语义）：`#` 前非字母/数字/`#`/`_`/`/`（排除标题、日期、`foo#bar`、URL 片段），
 * 标签含字母、字符集字母数字 `_ - /`。 */
const INLINE_TAG_RE = /(^|[^\p{L}\p{N}_#/])#([\p{L}\p{N}_\-/]+)/gu;

/** 行内 `#标签` 匹配区间（行文本 + 行起点偏移 → 绝对区间；`from` = `#` 位置，`to` 含标签尾）。 */
export function inlineTagRanges(lineText: string, lineFrom: number): RangeInfo[] {
  const out: RangeInfo[] = [];
  INLINE_TAG_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = INLINE_TAG_RE.exec(lineText))) {
    const tag = m[2] ?? "";
    if (!/[\p{L}]/u.test(tag)) continue;
    const start = lineFrom + m.index + (m[1]?.length ?? 0);
    out.push({ from: start, to: start + 1 + tag.length });
  }
  return out;
}

/** `[text](url)` / `![alt](url)`（含可选 title）解析；不匹配返回 null（保持原文）。
 *  URL 可空：`[名]()` 是「快捷新建同名笔记」语法，故两个正则的 URL 组都允许零字符。 */
export function parseBracketLink(text: string): { label: string; url: string; title: string | null } | null {
  const m =
    /^\[([^\]]*)\]\(([^)\s]*)(?:\s+(["'`][^"'`]*["'`]))?\)$/.exec(text) ||
    /^\[([^\]]*)\]\(([^)]*)\)$/.exec(text);
  if (!m) return null;
  const quoted = m[3];
  return { label: m[1] ?? "", url: m[2]?.trim() ?? "", title: quoted ? quoted.slice(1, -1) : null };
}

/** 行内数学 `$...$` 匹配区间（单 `$` 定界、内容不跨行、非 `$$`、内容不以空格起止）；
 *  手动扫描避免 lookbehind（WebKitGTK 兼容）。 */
export function inlineMathRanges(lineText: string, lineFrom: number): RangeInfo[] {
  const out: RangeInfo[] = [];
  let i = 0;
  while (i < lineText.length) {
    const idx = lineText.indexOf("$", i);
    if (idx === -1) break;
    if (lineText[idx - 1] === "\\" || lineText[idx + 1] === "$") {
      i = idx + 1;
      continue;
    }
    const close = lineText.indexOf("$", idx + 1);
    if (close === -1) break;
    if (lineText[close + 1] === "$") {
      i = close + 1;
      continue;
    }
    const content = lineText.slice(idx + 1, close);
    if (content.trim() !== "" && !content.startsWith(" ") && !content.endsWith(" ")) {
      out.push({ from: lineFrom + idx, to: lineFrom + close + 1 });
    }
    i = close + 1;
  }
  return out;
}

/** 块级数学 `$$...$$` 区间（可跨行；同行闭合或 `$$` 开段到下一 `$$` 闭段）。 */
export function blockMathRanges(docText: string): RangeInfo[] {
  const out: RangeInfo[] = [];
  const lines = docText.split("\n");
  let i = 0;
  while (i < lines.length) {
    const text = lines[i] ?? "";
    const openIdx = /^[ \t]*\$\$/.test(text) ? text.indexOf("$$") : -1;
    if (openIdx === -1) {
      i++;
      continue;
    }
    const rest = text.slice(openIdx + 2);
    // 同行闭合（`$$...$$`）
    const closeInLine = rest.indexOf("$$");
    if (closeInLine >= 0) {
      const from = offsetOf(lines, i) + openIdx;
      const to = offsetOf(lines, i) + openIdx + 2 + closeInLine + 2;
      out.push({ from, to });
      i++;
      continue;
    }
    // 跨行：从 `$$` 起始行到首个以 `$$` 结尾的行
    let j = i + 1;
    let closed = false;
    while (j < lines.length) {
      const lj = lines[j] ?? "";
      const closeIdx = lj.indexOf("$$");
      if (closeIdx >= 0) {
        const from = offsetOf(lines, i) + openIdx;
        const to = offsetOf(lines, j) + closeIdx + 2;
        out.push({ from, to });
        i = j + 1;
        closed = true;
        break;
      }
      j++;
    }
    if (!closed) break;
  }
  return out;
}

/** 按行数组 + 行号计算行起始偏移。 */
function offsetOf(lines: string[], lineIndex: number): number {
  let off = 0;
  for (let k = 0; k < lineIndex; k++) off += (lines[k]?.length ?? 0) + 1;
  return off;
}

// ===== lezer 解析器 =====

const mdParser = parser.configure(GFM);

/** 解析器实例按源文本缓存：文本→规格为纯函数，重复解析同一文本直接命中。 */
let treeCacheSource: string | null = null;
let treeCache: Tree | null = null;
function treeOf(source: string): Tree {
  if (treeCacheSource === source && treeCache) return treeCache;
  treeCache = mdParser.parse(source);
  treeCacheSource = source;
  return treeCache;
}

/** 行内结构节点名（容器可递归；叶节点为不透明区）。 */
const CONTAINER_TYPES = new Set(["Emphasis", "StrongEmphasis", "Strikethrough"]);
const LEAF_TYPES = new Set(["InlineCode", "Link", "Autolink", "Image"]);
const ATOM_TYPES = new Set([
  ...CONTAINER_TYPES,
  ...LEAF_TYPES,
  "HTMLTag",
  "Comment",
  "HardBreak",
  "Entity",
  "Escape",
]);

interface Ctx {
  source: string;
  options: ParseOptions;
  tree: Tree;
}

interface Candidate {
  from: number;
  to: number;
  span: InlineSpan;
}

/** 正则候选项：span 延迟构建（高亮内文需递归解析，构建前必须先经区间归属过滤，避免自递归）。 */
interface RawMatch {
  from: number;
  to: number;
  build: () => InlineSpan;
}

// ===== 顶层块切分 =====

/** 数学扫描的盲区：这些块内的 `$$` 是代码/HTML 文本，不是数学定界符。 */
function opaqueBlockRanges(tree: Tree): RangeInfo[] {
  const out: RangeInfo[] = [];
  for (let c = tree.topNode.firstChild; c; c = c.nextSibling) {
    if (c.type.name === "FencedCode" || c.type.name === "CodeBlock" || c.type.name === "HTMLBlock" || c.type.name === "CommentBlock") {
      out.push({ from: c.from, to: c.to });
    }
  }
  return out;
}

/** 数学区间 → mathBlock；空数学体降级为原文段落（源文本不从渲染面消失）。 */
function mathBlockOf(ctx: Ctx, r: RangeInfo): MarkdownBlock {
  const inner = ctx.source.slice(r.from, r.to).replace(/^\$\$/, "").replace(/\$\$$/, "").trim();
  if (inner) return { kind: "mathBlock", from: r.from, to: r.to, tex: inner };
  return { kind: "paragraph", from: r.from, to: r.to, inline: parseInlineInTree(ctx, r.from, r.to) };
}

function paragraphInRange(ctx: Ctx, from: number, to: number): MarkdownBlock {
  return { kind: "paragraph", from, to, inline: parseInlineInTree(ctx, from, to) };
}

/**
 * 段落按交叠的数学区间切分：未消费的区间依次消费为 mathBlock，区间之间与之外的残余
 * 保持为段落；已被先前段落消费的跨段区间只跳过它在本段的覆盖部分，其后的残余文本照常
 * 输出（区间可能只盖住本段开头，整段丢弃会吃掉区间外的正文）。调用方只在存在交叠区间时
 * 调用，故本函数必定产出「覆盖之外的全部残余」，不丢文本。mathRanges 按源文本顺序，
 * 遇起点越过段落末尾即可停止。
 */
function paragraphSegments(
  ctx: Ctx,
  from: number,
  to: number,
  mathRanges: RangeInfo[],
  usedMath: Set<number>,
): MarkdownBlock[] {
  const out: MarkdownBlock[] = [];
  let cursor = from;
  for (let i = 0; i < mathRanges.length; i++) {
    const r = mathRanges[i]!;
    if (r.to <= cursor) continue;
    if (r.from >= to) break;
    if (usedMath.has(i)) {
      cursor = Math.max(cursor, r.to);
      if (cursor >= to) break;
      continue;
    }
    if (r.from > cursor) out.push(paragraphInRange(ctx, cursor, r.from));
    usedMath.add(i);
    out.push(mathBlockOf(ctx, r));
    cursor = Math.max(cursor, r.to);
    if (cursor >= to) break;
  }
  if (cursor < to) out.push(paragraphInRange(ctx, cursor, to));
  return out;
}

export function parseMarkdown(source: string, options: ParseOptions = {}): MarkdownDocument {
  const tree = treeOf(source);
  const ctx: Ctx = { source, options, tree };
  const blocks: MarkdownBlock[] = [];
  const opaque = opaqueBlockRanges(tree);
  const mathRanges = blockMathRanges(source).filter((r) => !opaque.some((o) => r.from < o.to && r.to > o.from));
  const usedMath = new Set<number>();

  let node = tree.topNode.firstChild;
  while (node) {
    // 段落首行为脚注定义：切出定义块，行后残余保持为段落
    if (node.type.name === "Paragraph") {
      const fn = footnoteDefOf(node, ctx);
      if (fn) {
        blocks.push(fn.block);
        if (node.to > fn.next) blocks.push(paragraphInRange(ctx, fn.next, node.to));
        node = node.nextSibling;
        continue;
      }
    }
    // 段落与数学区间交叠：按区间切成 段落/数学块/段落（多个区间依次消费），区间外文本不丢弃
    if (node.type.name === "Paragraph") {
      const overlapped = mathRanges.some((r) => node!.from < r.to && node!.to > r.from);
      if (overlapped) {
        // 已被先前段落消费的区间只跳过覆盖部分，本段其余残余（含跨段区间之后的正文）照常输出
        blocks.push(...paragraphSegments(ctx, node.from, node.to, mathRanges, usedMath));
        node = node.nextSibling;
        continue;
      }
    }
    const block = blockOf(node, ctx);
    if (block) blocks.push(block);
    node = node.nextSibling;
  }
  return { source, blocks };
}

function blockOf(node: SyntaxNode, ctx: Ctx): MarkdownBlock | null {
  const name = node.type.name;
  const atx = /^ATXHeading([1-6])$/.exec(name);
  if (atx) return headingBlock(node, Number(atx[1]), ctx);
  const setext = /^SetextHeading([12])$/.exec(name);
  if (setext) return headingBlock(node, Number(setext[1]), ctx);

  switch (name) {
    case "Paragraph":
      return paragraphBlock(node, ctx);
    case "LinkReference": {
      // 脚注定义 `[^label]: text`；其余引用定义无渲染语义 → 以原文段落保留（不丢文本）
      return footnoteDefOf(node, ctx)?.block ?? paragraphBlock(node, ctx);
    }
    case "Blockquote":
      return blockquoteBlock(node, ctx);
    case "BulletList":
    case "OrderedList":
      return listBlock(node, ctx);
    case "FencedCode":
      return fencedCodeBlock(node, ctx.source);
    case "CodeBlock":
      return indentedCodeBlock(node, ctx.source);
    case "HorizontalRule":
      return { kind: "hr", from: node.from, to: node.to };
    case "HTMLBlock":
    case "CommentBlock":
      return { kind: "htmlBlock", from: node.from, to: node.to, html: ctx.source.slice(node.from, node.to) };
    case "Table":
      return tableBlock(node, ctx.source);
    default:
      return null;
  }
}

function headingBlock(node: SyntaxNode, level: number, ctx: Ctx): MarkdownBlock {
  const content = headingContentRange(node, ctx.source);
  return {
    kind: "heading",
    level,
    from: node.from,
    to: node.to,
    inline: parseInlineInTree(ctx, content.from, content.to),
  };
}

/** 标题内容区间：剥 ATX 开头 `#`（与可选结尾 `#`）或 Setext 下划线行。 */
function headingContentRange(node: SyntaxNode, source: string): RangeInfo {
  let from = node.from;
  let to = node.to;
  const marks: SyntaxNode[] = [];
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.type.name === "HeaderMark") marks.push(c);
  }
  const first = marks[0];
  if (first && first.from === node.from) from = first.to;
  const last = marks[marks.length - 1];
  if (last && last.from > from) to = last.from;
  while (from < to && /\s/.test(source[from] ?? "")) from++;
  while (to > from && /\s/.test(source[to - 1] ?? "")) to--;
  return { from, to };
}

/** 行内容器内容区间：剥去首尾定界 mark（`**`/`_`/`~~`），避免其进入子片段文本。 */
function containerContentRange(node: SyntaxNode): RangeInfo {
  const marks: SyntaxNode[] = [];
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.type.name === "EmphasisMark" || c.type.name === "StrikethroughMark") marks.push(c);
  }
  let from = node.from;
  let to = node.to;
  const first = marks[0];
  if (first && first.from === node.from) from = first.to;
  const last = marks[marks.length - 1];
  if (last && last.to === node.to) to = last.from;
  return { from, to };
}

function paragraphBlock(node: SyntaxNode, ctx: Ctx): MarkdownBlock {
  return {
    kind: "paragraph",
    from: node.from,
    to: node.to,
    inline: parseInlineInTree(ctx, node.from, node.to),
  };
}

/**
 * 脚注定义 `[^label]: text`（覆盖定义行）。
 * 行尾 \r（CRLF 源文本）不属于定义内容，须剥掉再做行匹配（`.*` 不匹配 \r）。
 * 定义块只含定义行；定义行之后的段落残余由调用方保留（next = 定义行换行符之后的起点）。
 */
function footnoteDefOf(node: SyntaxNode, ctx: Ctx): { block: MarkdownBlock; next: number } | null {
  const lineEnd = lineEndAt(ctx.source, node.from);
  const line = ctx.source.slice(node.from, lineEnd);
  const hadCr = line.endsWith("\r");
  const stripped = hadCr ? line.slice(0, -1) : line;
  const m = /^\[\^([^\]]+)\]:[ \t]*(.*)$/.exec(stripped);
  if (!m || !m[1]) return null;
  const text = m[2] ?? "";
  const textFrom = node.from + stripped.length - text.length;
  return {
    block: {
      kind: "footnoteDef",
      from: node.from,
      to: node.from + stripped.length,
      label: m[1],
      inline: parseInlineInTree(ctx, textFrom, node.from + stripped.length),
    },
    next: lineEnd + 1,
  };
}

/**
 * 引用块内每一行的 `>` 前缀范围（含前置缩进与标记后空格）。
 * 块级节点只按首行定位，续行的 `>` 会落进子块区间而被当成正文，须按范围剔除。
 */
function quoteLinePrefixes(source: string, from: number, to: number): RangeInfo[] {
  const out: RangeInfo[] = [];
  let lineStart = from;
  while (lineStart < to) {
    const lineEnd = Math.min(lineEndAt(source, lineStart), to);
    const m = /^[ \t]*>[ \t>]*/.exec(source.slice(lineStart, lineEnd));
    if (m) out.push({ from: lineStart, to: lineStart + m[0].length });
    if (lineEnd >= to) break;
    lineStart = lineEnd + 1;
  }
  return out;
}

/** 剔除落在子块区间内的引用行前缀：只影响带行内片段的子块，容器自身的首行前缀不在其区间内。 */
function stripQuotePrefixes(children: MarkdownBlock[], prefixes: readonly RangeInfo[], source: string): void {
  if (prefixes.length === 0) return;
  for (let i = 0; i < children.length; i++) {
    const child = children[i]!;
    if (!("inline" in child)) continue;
    const inner = prefixes.filter((p) => p.from >= child.from && p.to <= child.to);
    if (inner.length > 0) children[i] = { ...child, inline: stripRanges(child.inline, inner, source) };
  }
}

function blockquoteBlock(node: SyntaxNode, ctx: Ctx): MarkdownBlock {
  const children: MarkdownBlock[] = [];
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.type.name === "QuoteMark") continue;
    const b = blockOf(c, ctx);
    if (b) children.push(b);
  }
  stripQuotePrefixes(children, quoteLinePrefixes(ctx.source, node.from, node.to), ctx.source);
  const lineEnd = lineEndAt(ctx.source, node.from);
  const line = ctx.source.slice(node.from, lineEnd);
  const m = /^[ \t]*>[ \t]*\[!([a-z][a-z0-9-]*)\]([+-])?\s*/i.exec(line);
  if (!m) return { kind: "blockquote", from: node.from, to: node.to, callout: null, children };
  const callout = (m[1] ?? "").toLowerCase();
  const lb = line.indexOf("[");
  const rb = line.indexOf("]", lb);
  const firstBlock = children[0];
  if (lb >= 0 && rb >= 0 && firstBlock && firstBlock.kind === "paragraph") {
    children[0] = {
      ...firstBlock,
      inline: stripRanges(firstBlock.inline, [{ from: node.from + lb, to: node.from + rb + 1 }], ctx.source),
    };
  }
  return { kind: "blockquote", from: node.from, to: node.to, callout, children };
}

function listBlock(node: SyntaxNode, ctx: Ctx): MarkdownBlock {
  const ordered = node.type.name === "OrderedList";
  const items: MarkdownListItem[] = [];
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.type.name !== "ListItem") continue;
    const taskMarker = c.getChild("Task")?.getChild("TaskMarker");
    const checked = taskMarker ? /x/i.test(ctx.source.slice(taskMarker.from, taskMarker.to)) : false;
    items.push({
      from: c.from,
      to: c.to,
      task: !!taskMarker,
      checked,
      children: listItemChildBlocks(c, ctx),
    });
  }
  return { kind: "list", ordered, from: node.from, to: node.to, items };
}

/**
 * 列表项的子块：跳过 ListMark 与首个承载内容的 Task/Paragraph（内容由渲染层剥标记后
 * 行内渲染），其余（嵌套列表、松散项的后续段落等）递归成块，保证嵌套列表不退化为正文。
 */
function listItemChildBlocks(item: SyntaxNode, ctx: Ctx): MarkdownBlock[] {
  const children: MarkdownBlock[] = [];
  let first = true;
  for (let c = item.firstChild; c; c = c.nextSibling) {
    if (c.type.name === "ListMark") continue;
    if (first) {
      first = false;
      if (c.type.name === "Task" || c.type.name === "Paragraph") continue;
    }
    const b = blockOf(c, ctx);
    if (b) children.push(b);
  }
  return children;
}

function fencedCodeBlock(node: SyntaxNode, source: string): MarkdownBlock {
  let lang = "";
  const content: string[] = [];
  let contentFrom = -1;
  let contentTo = -1;
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.type.name === "CodeInfo" && !lang) lang = source.slice(c.from, c.to).trim();
    if (c.type.name === "CodeText") {
      if (content.length === 0) contentFrom = c.from;
      contentTo = c.to;
      content.push(source.slice(c.from, c.to));
    }
  }
  // 空正文没有 CodeText 子节点：内容区间取开围栏行之后的空区间——否则区间会落到块尾，
  // 渲染层据此算出的开围栏标记会吞掉整块、闭围栏标记消失（无正文时也应有独立的围栏标记）
  if (contentFrom < 0) {
    const lineEnd = source.indexOf("\n", node.from);
    contentFrom = lineEnd === -1 ? node.to : lineEnd + 1;
    contentTo = contentFrom;
  }
  return {
    kind: "fencedCode",
    from: node.from,
    to: node.to,
    lang,
    code: content.join(""),
    contentFrom,
    contentTo,
  };
}

function indentedCodeBlock(node: SyntaxNode, source: string): MarkdownBlock {
  const content: string[] = [];
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.type.name === "CodeText") content.push(source.slice(c.from, c.to));
  }
  return { kind: "indentedCode", from: node.from, to: node.to, code: content.join("") };
}

function tableBlock(node: SyntaxNode, source: string): MarkdownBlock {
  const raw = source.slice(node.from, node.to);
  const lines = raw.split("\n").filter((l) => l.trim() !== "");
  // `\|` 是字面竖线：先占位再切列（否则行尾转义管道会被当定界剥掉），切完后还原
  const splitRow = (l: string) =>
    l
      .trim()
      .replace(/\\\|/g, "\u0000")
      .replace(/^\|/, "")
      .replace(/\|$/, "")
      .split("|")
      .map((c) => c.trim().split("\u0000").join("|"));
  const header = lines[0] ? splitRow(lines[0]) : [];
  const aligns = (lines[1] ? splitRow(lines[1]) : []).map(
    (c): "" | "left" | "center" | "right" =>
      c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : "",
  );
  const rows = lines.slice(2).map(splitRow);
  return { kind: "table", from: node.from, to: node.to, header, aligns, rows };
}

// ===== 行内解析 =====

/** 公开入口：解析任意源区间的行内片段（供视图层按需重解析，如列表项内容）。 */
export function parseInlineRange(
  source: string,
  from: number,
  to: number,
  options: ParseOptions = {},
): InlineSpan[] {
  return parseInlineInTree({ source, options, tree: treeOf(source) }, from, to);
}

function parseInlineInTree(
  ctx: Ctx,
  from: number,
  to: number,
  excludes: RangeInfo[] = [],
  start?: SyntaxNode,
): InlineSpan[] {
  if (to <= from) return [];
  const atoms = collectAtoms(ctx, from, to, start);
  atoms.sort((a, b) => a.from - b.from || a.to - b.to);

  const candidates: RawMatch[] = [];
  let lineStart = ctx.source.lastIndexOf("\n", from - 1) + 1;
  while (lineStart < to) {
    const nl = ctx.source.indexOf("\n", lineStart);
    const lineEnd = nl === -1 ? ctx.source.length : nl;
    candidates.push(...scanLine(ctx, ctx.source.slice(lineStart, lineEnd), lineStart));
    lineStart = lineEnd + 1;
  }
  candidates.sort((a, b) => a.from - b.from || b.to - a.to);

  const accepted: RawMatch[] = [];
  for (const c of candidates) {
    if (c.from < from || c.to > to) continue;
    if (accepted.some((a) => c.from < a.to && c.to > a.from)) continue;
    if (atoms.some((a) => c.from < a.to && c.to > a.from)) continue;
    accepted.push(c);
  }

  const entries: Candidate[] = [...atoms, ...accepted.map((c) => ({ from: c.from, to: c.to, span: c.build() }))];
  entries.sort((a, b) => a.from - b.from || a.to - b.to);
  const spans: InlineSpan[] = [];
  let pos = from;
  for (const e of entries) {
    if (e.from > pos) pushText(spans, ctx.source, pos, e.from, excludes);
    spans.push(e.span);
    if (e.to > pos) pos = e.to;
  }
  if (pos < to) pushText(spans, ctx.source, pos, to, excludes);
  return spans;
}

/** 收集区间内的行内结构节点，过滤出极大者（外层节点优先，嵌套交给递归处理）。
 *  start 为本次扫描起点（容器递归时传容器自身，避免把容器再当原子造成无限递归）。 */
function collectAtoms(ctx: Ctx, from: number, to: number, start?: SyntaxNode): Candidate[] {
  const found: SyntaxNode[] = [];
  const walk = (node: SyntaxNode) => {
    for (let c = node.firstChild; c; c = c.nextSibling) {
      if (c.to <= from) continue;
      if (c.from >= to) break;
      if (ATOM_TYPES.has(c.type.name) || (c.type.name === "URL" && c.parent?.type.name === "Paragraph")) {
        found.push(c);
      }
      walk(c);
    }
  };
  walk(start ?? ctx.tree.topNode);
  found.sort((a, b) => a.from - b.from || b.to - a.to);
  const maximal: SyntaxNode[] = [];
  for (const n of found) {
    const contained = maximal.some(
      (m) => m.from <= n.from && m.to >= n.to && !(m.from === n.from && m.to === n.to),
    );
    if (!contained) maximal.push(n);
  }

  // 配对的 HTMLTag 合并为一个 html 片段（开标签起、配对深度归零止），整体作为不透明区；
  // 越界合并（跨过配对之外的普通文本）会把 #标签、$数学$ 等语法一起吞进不透明区造成语义丢失，
  // 故只在配对深度归零时成组。
  const atoms: Candidate[] = [];
  let htmlGroup: SyntaxNode[] = [];
  let depth = 0;
  const flushHtml = () => {
    const first = htmlGroup[0];
    const last = htmlGroup[htmlGroup.length - 1];
    if (first && last) {
      const html = ctx.source.slice(first.from, last.to);
      atoms.push({ from: first.from, to: last.to, span: { kind: "html", from: first.from, to: last.to, html } });
    }
    htmlGroup = [];
    depth = 0;
  };
  for (const n of maximal) {
    if (n.type.name === "HTMLTag") {
      const raw = ctx.source.slice(n.from, n.to);
      if (raw.startsWith("</")) {
        if (depth === 0) {
          // 游离闭标签：自成一组（清洗后为空，不吞并其他内容）
          flushHtml();
          htmlGroup.push(n);
          flushHtml();
        } else {
          htmlGroup.push(n);
          depth--;
          if (depth === 0) flushHtml();
        }
        continue;
      }
      htmlGroup.push(n);
      if (raw.endsWith("/>")) {
        if (depth === 0) flushHtml();
      } else {
        depth++;
      }
      continue;
    }
    // 配对内部的 html 原生原子（实体）随组吸收；其余原子切断合并，避免越界吞并
    if (depth > 0 && n.type.name === "Entity") continue;
    flushHtml();
    const a = atomOf(n, ctx);
    if (a) atoms.push(a);
  }
  flushHtml();
  return atoms;
}

function atomOf(node: SyntaxNode, ctx: Ctx): Candidate | null {
  const from = node.from;
  const to = node.to;
  switch (node.type.name) {
    case "StrongEmphasis":
    case "Emphasis":
    case "Strikethrough": {
      const kind = node.type.name === "StrongEmphasis" ? "strong" : node.type.name === "Emphasis" ? "em" : "strike";
      const content = containerContentRange(node);
      return { from, to, span: { kind, from, to, children: parseInlineInTree(ctx, content.from, content.to, [], node) } };
    }
    case "InlineCode":
      return { from, to, span: { kind: "code", from, to, text: inlineCodeText(ctx.source.slice(from, to)) } };
    case "Link":
    case "Autolink": {
      const span = linkSpan(node, ctx);
      return span ? { from, to, span } : null;
    }
    case "Image": {
      const span = imageSpan(node, ctx.source);
      return span ? { from, to, span } : null;
    }
    case "URL": {
      const text = ctx.source.slice(from, to);
      return { from, to, span: { kind: "autolink", from, to, href: text, label: text } };
    }
    case "Comment":
      // 行内 HTML 注释与 `%%注释%%` 同语义：承载内容但不渲染
      return { from, to, span: { kind: "comment", from, to } };
    case "HardBreak":
      return { from, to, span: { kind: "hardBreak", from, to } };
    case "Entity":
      // 实体已是合法 HTML 片段，直接作为受控 html 渲染（避免二次转义）
      return { from, to, span: { kind: "html", from, to, html: ctx.source.slice(from, to) } };
    case "Escape": {
      const raw = ctx.source.slice(from, to);
      return { from, to, span: { kind: "text", from, to, text: raw.length > 1 ? raw.slice(1) : raw } };
    }
    default:
      return null;
  }
}

function linkSpan(node: SyntaxNode, ctx: Ctx): InlineSpan | null {
  const text = ctx.source.slice(node.from, node.to);
  if (node.type.name === "Autolink") {
    const inner = text.length >= 2 && text.startsWith("<") && text.endsWith(">") ? text.slice(1, -1) : text;
    return { kind: "autolink", from: node.from, to: node.to, href: inner, label: inner };
  }
  const parsed = parseBracketLink(text);
  if (!parsed) return null;
  return {
    kind: "link",
    from: node.from,
    to: node.to,
    href: parsed.url,
    label: parsed.label || parsed.url,
    form: classifyLink(parsed.label, parsed.url, ctx.options),
    title: parsed.title,
  };
}

function classifyLink(label: string, url: string, options: ParseOptions): LinkForm {
  if (url === "" && label !== "") return "create";
  if (isOpenableUrl(url)) return "external";
  return options.resolveLink ? options.resolveLink(url) : "plain";
}

function imageSpan(node: SyntaxNode, source: string): InlineSpan | null {
  const text = source.slice(node.from, node.to);
  const parsed = parseBracketLink(text.startsWith("!") ? text.slice(1) : text);
  if (!parsed || !parsed.url) return null;
  let alt = parsed.label;
  let width: string | null = null;
  let height: string | null = null;
  const sep = alt.lastIndexOf("|");
  if (sep > 0) {
    const size = alt.slice(sep + 1).trim();
    if (/^\d+(x\d+)?$/.test(size)) {
      alt = alt.slice(0, sep).trim();
      const parts = size.split("x");
      width = parts[0] ?? null;
      height = parts[1] ?? null;
    }
  }
  return { kind: "image", from: node.from, to: node.to, src: parsed.url, alt, width, height, title: parsed.title };
}

/** 围栏行内代码去定界反引号（`` ` `` 数量可不等）。 */
function inlineCodeText(raw: string): string {
  const m = /^(`+)([\s\S]*?)\1$/.exec(raw);
  return m ? (m[2] ?? "") : raw;
}

// ===== 正则补充扫描（按行）=====

function scanLine(ctx: Ctx, lineText: string, lineFrom: number): RawMatch[] {
  const out: RawMatch[] = [];
  const push = (from: number, to: number, build: () => InlineSpan) => out.push({ from, to, build });

  for (const m of matchAll(lineText, /%%([^%\n]+)%%/g)) {
    const from = lineFrom + m.index;
    const to = from + m[0].length;
    push(from, to, () => ({ kind: "comment", from, to }));
  }
  for (const m of matchAll(lineText, /==([^=\n]+)==/g)) {
    const from = lineFrom + m.index;
    const to = from + m[0].length;
    push(from, to, () => ({
      kind: "highlight",
      from,
      to,
      children: parseInlineInTree(ctx, from + 2, to - 2),
    }));
  }
  for (const m of matchAll(lineText, /\[\[([^\]|]*?)(?:\|([^\]]*?))?\]\]/g)) {
    const target = (m[1] ?? "").trim();
    if (!target) continue;
    const from = lineFrom + m.index;
    const to = from + m[0].length;
    const label = (m[2] ?? target).trim() || target;
    push(from, to, () => ({ kind: "wiki", from, to, target, label }));
  }
  for (const m of matchAll(lineText, /\[\^([^\]]+)\]/g)) {
    if (!m[1]) continue;
    const from = lineFrom + m.index;
    const to = from + m[0].length;
    const label = m[1];
    push(from, to, () => ({ kind: "footnoteRef", from, to, label }));
  }
  for (const r of inlineMathRanges(lineText, lineFrom)) {
    push(r.from, r.to, () => ({ kind: "mathInline", from: r.from, to: r.to, tex: ctx.source.slice(r.from + 1, r.to - 1) }));
  }
  for (const r of inlineTagRanges(lineText, lineFrom)) {
    push(r.from, r.to, () => ({ kind: "tag", from: r.from, to: r.to, tag: ctx.source.slice(r.from + 1, r.to) }));
  }
  const mentions = ctx.options.mentions;
  if (mentions && mentions.length > 0) {
    let i = 0;
    while (i < lineText.length) {
      const at = lineText.indexOf("@", i);
      if (at === -1) break;
      const prev = at > 0 ? lineText[at - 1] ?? "" : "";
      if (prev !== "" && /[\p{L}\p{N}]/u.test(prev)) {
        i = at + 1;
        continue;
      }
      let best: { key: string; label: string } | null = null;
      for (const mn of mentions) {
        if (!mn.label) continue;
        if (!lineText.startsWith(mn.label, at + 1)) continue;
        const after = lineText[at + 1 + mn.label.length];
        if (after === undefined || !/[\p{L}\p{N}]/u.test(after)) {
          if (!best || mn.label.length > best.label.length) best = mn;
        }
      }
      if (!best) {
        i = at + 1;
        continue;
      }
      const from = lineFrom + at;
      const to = from + 1 + best.label.length;
      const hit = best;
      push(from, to, () => ({ kind: "mention", from, to, key: hit.key, label: hit.label }));
      i = to - lineFrom;
    }
  }
  return out;
}

function matchAll(text: string, re: RegExp): RegExpExecArray[] {
  const out: RegExpExecArray[] = [];
  re.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    out.push(m);
    if (m[0].length === 0) re.lastIndex++;
  }
  return out;
}

// ===== 文本片段与区间裁剪 =====

function pushText(out: InlineSpan[], source: string, from: number, to: number, excludes: RangeInfo[]): void {
  if (excludes.length === 0) {
    out.push({ kind: "text", from, to, text: source.slice(from, to) });
    return;
  }
  for (const chunk of subtract(from, to, excludes)) {
    out.push({ kind: "text", from: chunk.from, to: chunk.to, text: source.slice(chunk.from, chunk.to) });
  }
}

/** 从既有片段集中抠掉指定区间（只影响文本片段；用于 callout 标记剔除）。 */
function stripRanges(spans: InlineSpan[], excludes: RangeInfo[], source: string): InlineSpan[] {
  if (excludes.length === 0) return spans;
  const out: InlineSpan[] = [];
  for (const s of spans) {
    if (s.kind !== "text") {
      out.push(s);
      continue;
    }
    for (const chunk of subtract(s.from, s.to, excludes)) {
      out.push({ kind: "text", from: chunk.from, to: chunk.to, text: source.slice(chunk.from, chunk.to) });
    }
  }
  return out;
}

function subtract(from: number, to: number, excludes: RangeInfo[]): RangeInfo[] {
  const cuts = excludes.filter((e) => e.to > from && e.from < to).sort((a, b) => a.from - b.from);
  const out: RangeInfo[] = [];
  let cursor = from;
  for (const c of cuts) {
    const stop = Math.min(c.from, to);
    if (stop > cursor) out.push({ from: cursor, to: stop });
    cursor = Math.max(cursor, Math.min(c.to, to));
  }
  if (cursor < to) out.push({ from: cursor, to });
  return out;
}

function lineEndAt(source: string, pos: number): number {
  const nl = source.indexOf("\n", pos);
  return nl === -1 ? source.length : nl;
}