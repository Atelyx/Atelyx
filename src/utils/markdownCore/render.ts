/**
 * Markdown 内核渲染：文档规格 → 已清洗 HTML 字符串（框架无关，供 React/插件复用）。
 *
 * 安全：raw HTML 一律经 `sanitizeHtmlFragment`（唯一注入点）清洗；其余内容一律转义，
 * 只出 class + 文本。类名契约与既有样式表保持一致（勿改名）。
 * 代码块只出结构与 data- 标记，复制按钮的点击行为由视图层绑定。
 */
import katex from "katex";
import { sanitizeHtmlFragment } from "@/utils/htmlSanitize";
import type { InlineSpan, MarkdownBlock, MarkdownListItem, RenderOptions } from "@/types/markdown";
import { parseMarkdown, parseInlineRange } from "./parse";

/** KaTeX 渲染结果按输入缓存（上限 300，与既有做法一致）；空串 = 解析失败。 */
const katexCache = new Map<string, string>();
function renderKatexHtml(tex: string, display: boolean): string {
  const key = `${display ? "d" : "i"}:${tex}`;
  const hit = katexCache.get(key);
  if (hit !== undefined) return hit;
  let html: string;
  try {
    html = katex.renderToString(tex, { displayMode: display, throwOnError: false, strict: false });
  } catch {
    html = "";
  }
  if (katexCache.size >= 300) katexCache.clear();
  katexCache.set(key, html);
  return html;
}

export function renderMarkdownToHtml(source: string, options: RenderOptions = {}): string {
  const doc = parseMarkdown(source, options);
  return renderDocument(doc.blocks, doc.source, options, (b) => renderBlock(b, options, doc.source));
}

/**
 * 文档级渲染：块间空行之外，文首/文末的源码空行同样渲染为行元素——
 * 文末按 Enter 开出的空行有真实行高与光标锚（只在块间渲染时文末新行无处显示）。
 */
function renderDocument(
  blocks: readonly MarkdownBlock[],
  source: string,
  options: RenderOptions,
  renderOne: (block: MarkdownBlock) => string,
): string {
  if (blocks.length === 0) return renderBlankLines(source, 0, source.length, options);
  let html = renderBlankLines(source, 0, blocks[0]!.from, options);
  html += renderBlocks(blocks, source, options, renderOne);
  html += renderBlankLines(source, blocks[blocks.length - 1]!.to, source.length, options);
  return html;
}

/**
 * 块序列渲染：相邻块之间的源码空行渲染为真实空行行元素（所见即所得——源码怎么空行，
 * 渲染就逐行怎么间隔；块级垂直外边距不参与，垂直间距 = 空行数 × 行高）。
 * 顶层、引用内、列表项段落间共用。
 */
function renderBlocks(
  blocks: readonly MarkdownBlock[],
  source: string,
  options: RenderOptions,
  renderOne: (block: MarkdownBlock) => string,
): string {
  let html = "";
  let prev: MarkdownBlock | null = null;
  for (const block of blocks) {
    if (prev) html += renderBlankLines(source, prev.to, block.from, options);
    html += renderOne(block);
    prev = block;
  }
  return html;
}

/** 空行行元素：每个源码空行一个一行高的真实行。编辑态带该空行的源偏移（点区间）
 *  与零宽字符锚——光标驻留、点击命中都直接落在空行上，无需几何换算。
 *  首段 = 前块行尾残余、末段 = 后块首行前缀；纯空白行与引用的空延续行（`>`）都算空行。 */
function renderBlankLines(source: string, from: number, to: number, options: RenderOptions): string {
  const lines = source.slice(from, to).split("\n");
  let html = "";
  let offset = from;
  for (let i = 1; i < lines.length - 1; i++) {
    offset += lines[i - 1]!.length + 1;
    if (!/^[\s>]*$/.test(lines[i]!)) continue;
    const at = options.offsets ? ` data-md-from="${offset}" data-md-to="${offset}"` : "";
    const anchor = options.offsets ? "\u200B" : "";
    html += `<div class="md-editor-gap"${at}>${anchor}</div>`;
  }
  return html;
}

/** 块以源码形态渲染（编辑面用：光标所在块回显 markdown 原文，便于直接改标记）。 */
function renderBlockSource(block: { from: number; to: number; kind: string }, source: string, options: RenderOptions): string {
  const raw = source.slice(block.from, block.to);
  const inner = options.offsets
    ? `<span data-md-from="${block.from}" data-md-to="${block.to}">${escapeHtml(raw)}</span>`
    : escapeHtml(raw);
  return `<div class="md-editor-source"${blockAttrs(block, options)}>${inner}</div>`;
}

/**
 * 编辑形态渲染：光标所在块显示源码、其余块渲染——与只读形态共用同一套块渲染器，
 * 因此「预览 ⇄ 编辑」只切换活动块的呈现，不产生两套视觉。
 */
export function renderMarkdownEditHtml(
  source: string,
  options: RenderOptions & { activeOffset?: number } = {},
): string {
  const doc = parseMarkdown(source, options);
  return renderDocument(doc.blocks, doc.source, options, (block) => {
    const activeOffset = options.activeOffset;
    const active = activeOffset !== undefined && activeOffset >= block.from && activeOffset <= block.to;
    // 列表项级编辑：活动判定细到项（嵌套下探到最小项；空项恒渲染态显示 marker），只有所在项回显源码
    if (active && block.kind === "list") {
      return renderList(block, options, doc.source, blockAttrs(block, options), activeOffset);
    }
    return active ? renderBlockSource(block, source, options) : renderBlock(block, options, doc.source);
  });
}

/** 编辑面定位属性：offsets 关闭时为空串，插件侧拿到的 HTML 与只读渲染完全一致。 */
function offsetAttrs(options: RenderOptions, from: number, to: number): string {
  return options.offsets ? ` data-md-from="${from}" data-md-to="${to}"` : "";
}

/** 块容器定位属性（含块类型：编辑面据此判定代码块等「不回显渲染」的块）。 */
function blockAttrs(block: { from: number; to: number; kind: string }, options: RenderOptions): string {
  return options.offsets
    ? ` data-md-block data-md-kind="${block.kind}" data-md-from="${block.from}" data-md-to="${block.to}"`
    : "";
}

function renderBlock(block: MarkdownBlock, options: RenderOptions, source: string, activeOffset?: number): string {
  const at = blockAttrs(block, options);
  switch (block.kind) {
    case "heading":
      return `<h${block.level}${at}>${renderSpans(block.inline, options, source)}</h${block.level}>`;
    case "paragraph":
      return `<p${at}>${renderSpans(block.inline, options, source)}</p>`;
    case "blockquote": {
      // 引用维持整块源码态（嵌套在列表项内时由项级渲染递归传入活动偏移）
      if (activeOffset !== undefined && activeOffset >= block.from && activeOffset <= block.to) {
        return renderBlockSource(block, source, options);
      }
      const inner = renderBlocks(block.children, source, options, (c) => renderBlock(c, options, source, activeOffset));
      if (block.callout) {
        const badge = `<span class="md-editor-callout-badge">${escapeHtml(block.callout)}</span>`;
        return `<blockquote class="md-callout callout-${escapeHtml(block.callout)}"${at}>${badge}${inner}</blockquote>`;
      }
      return `<blockquote class="md-quote"${at}>${inner}</blockquote>`;
    }
    case "list":
      return renderList(block, options, source, at, activeOffset);
    case "fencedCode":
      return renderCodeBlock(block, options, at);
    case "indentedCode":
      return `<div class="md-editor-code-block"${at}><pre><code>${escapeHtml(block.code)}</code></pre></div>`;
    case "hr":
      return `<div class="md-editor-divider"${at}></div>`;
    case "table":
      return renderTable(block, at);
    case "mathBlock":
      return renderMath(block.tex, true, options, at);
    case "htmlBlock":
      // raw HTML 块整体为原子块：编辑面点击落到块起点（内部 DOM 来自清洗后的外部 HTML，不含偏移）
      return `<div class="md-editor-html"${at}>${sanitizeHtmlFragment(block.html)}</div>`;
    case "footnoteDef":
      return `<div class="md-editor-footnote-def"${at}><sup>${escapeHtml(block.label)}</sup>${renderSpans(block.inline, options, source)}</div>`;
    default:
      return "";
  }
}

/**
 * 列表渲染：圆点/序号由自绘 `md-editor-list-marker` 承载（有序项保留源序号），容器打上
 * `md-editor-list` 类供样式层关闭原生 marker（防双重显示；raw HTML 中的列表不受影响）。
 * 项内容 = 剥标记后的首段行内片段 + 递归渲染的嵌套子块。
 * 项间源码空行折算为真实空行行元素（与块间同语义）；编辑态传 activeOffset 时逐项判定活动，
 * 光标落在嵌套子列表的项内时让位给那一项，只有最小项回显源码，其余项保持渲染；
 * 空项（只有标记无内容）恒渲染态，自绘 marker 即输入 "- "/"1. " 的即时反馈。
 */
function renderList(
  block: Extract<MarkdownBlock, { kind: "list" }>,
  options: RenderOptions,
  source: string,
  containerAttrs: string,
  activeOffset?: number,
): string {
  const tag = block.ordered ? "ol" : "ul";
  const parts: string[] = [];
  block.items.forEach((item, index) => {
    if (index > 0) {
      const prev = block.items[index - 1]!;
      parts.push(renderBlankLines(source, prev.to, item.from, options));
    }
    const isActive =
      activeOffset !== undefined &&
      activeOffset >= item.from &&
      activeOffset <= item.to &&
      !activeInNestedItem(activeOffset, item) &&
      !itemIsEmpty(source, item);
    parts.push(
      isActive
        ? `<li${blockAttrs({ ...item, kind: "list" }, options)}>${renderBlockSource({ ...item, kind: "list" }, source, options)}</li>`
        : renderListItem(item, block, index, options, source, activeOffset),
    );
  });
  return `<${tag} class="md-editor-list"${containerAttrs}>${parts.join("")}</${tag}>`;
}

/** 列表项标记（含任务框）：项内容剥取与空项判定共用。 */
const ITEM_MARKER_RE = /^[ \t]*([-+*]|\d+[.)])[ \t]+(\[[ xX]\][ \t]+)?/;

/** 项是否只有标记无内容：空项保持渲染态（源码态没有可编辑的内容）。 */
function itemIsEmpty(source: string, item: MarkdownListItem): boolean {
  const m = ITEM_MARKER_RE.exec(source.slice(item.from, item.to));
  if (!m) return false;
  const contentEnd = item.children[0]?.from ?? item.to;
  return source.slice(item.from + m[0].length, contentEnd).trim() === "";
}

/** 活动偏移是否落在项内嵌套列表的某个子项中（引用内的列表同样下探）。 */
function activeInNestedItem(offset: number, item: MarkdownListItem): boolean {
  return item.children.some((child) => nestedItemContains(offset, child));
}

function nestedItemContains(offset: number, block: MarkdownBlock): boolean {
  if (block.kind === "list") {
    return block.items.some(
      (it) => (offset >= it.from && offset <= it.to) || activeInNestedItem(offset, it),
    );
  }
  if (block.kind === "blockquote") {
    return block.children.some((child) => nestedItemContains(offset, child));
  }
  return false;
}

/** 单个列表项渲染：li 带项级块标记（编辑面的活动判定与偏移映射细到项）。 */
function renderListItem(
  item: MarkdownListItem,
  block: Extract<MarkdownBlock, { kind: "list" }>,
  index: number,
  options: RenderOptions,
  source: string,
  activeOffset?: number,
): string {
  const raw = source.slice(item.from, item.to);
  const m = ITEM_MARKER_RE.exec(raw);
  const contentFrom = item.from + (m ? m[0].length : 0);
  // 首段内容止于第一个嵌套子块起点（无子块则到项尾），嵌套块自身递归渲染
  const contentTo = item.children[0]?.from ?? item.to;
  // 空项编辑态放零宽锚：渲染态下光标可驻留项内容位，输入即落于此
  const content =
    options.offsets && itemIsEmpty(source, item)
      ? `<span data-md-from="${contentFrom}" data-md-to="${contentFrom}">\u200B</span>`
      : renderSpans(
          parseInlineRange(source, contentFrom, Math.max(contentFrom, contentTo), options),
          options,
          source,
        );
  const inner = renderBlocks(item.children, source, options, (c) => renderBlock(c, options, source, activeOffset));
  const itemAttrs = blockAttrs({ ...item, kind: "list" }, options);
  if (item.task) {
    const checked = item.checked ? " checked" : "";
    return `<li${itemAttrs}><input type="checkbox" class="md-editor-checkbox"${checked} disabled>${content}${inner}</li>`;
  }
  const marker = block.ordered ? m?.[1] ?? `${index + 1}.` : "•";
  return `<li${itemAttrs}><span class="md-editor-list-marker">${escapeHtml(marker)}</span>${content}${inner}</li>`;
}

function renderCodeBlock(block: Extract<MarkdownBlock, { kind: "fencedCode" }>, options: RenderOptions, attrs: string): string {
  const head =
    `<div class="md-editor-code-head">` +
    `<span class="md-editor-code-lang">${escapeHtml(block.lang || "代码")}</span>` +
    `<button type="button" class="md-editor-code-copy" data-md-copy title="复制代码" aria-label="复制代码"></button>` +
    `</div>`;
  // 代码正文区间供编辑面把源码偏移映射进 <code>（内容节点单一，映射为线性）
  const codeAttrs = options.offsets ? ` data-md-from="${block.contentFrom}" data-md-to="${block.contentTo}"` : "";
  return `<div class="md-editor-code-block"${attrs}>${head}<pre><code${codeAttrs}>${escapeHtml(block.code)}</code></pre></div>`;
}

function renderTable(block: Extract<MarkdownBlock, { kind: "table" }>, attrs: string): string {
  const colCount = Math.max(
    block.header.length,
    block.aligns.length,
    ...block.rows.map((r) => r.length),
    1,
  );
  const cell = (tag: "th" | "td", value: string, i: number) => {
    const align = block.aligns[i];
    const style = align ? ` style="text-align:${align}"` : "";
    return `<${tag}${style}>${escapeHtml(value)}</${tag}>`;
  };
  const head = `<thead><tr>${Array.from({ length: colCount }, (_, i) => cell("th", block.header[i] ?? "", i)).join("")}</tr></thead>`;
  const body = `<tbody>${block.rows
    .map((row) => `<tr>${Array.from({ length: colCount }, (_, i) => cell("td", row[i] ?? "", i)).join("")}</tr>`)
    .join("")}</tbody>`;
  return `<div class="md-editor-table-wrap"${attrs}><table class="md-editor-table">${head}${body}</table></div>`;
}

function renderMath(tex: string, display: boolean, options: RenderOptions, attrs = ""): string {
  const tag = display ? "div" : "span";
  const cls = display ? "md-editor-math md-editor-math-block" : "md-editor-math";
  const html = options.katex === false ? "" : renderKatexHtml(tex, display);
  if (!html) {
    const src = display ? `$$\n${tex}\n$$` : `$${tex}$`;
    return `<${tag} class="${cls} md-editor-math-error"${attrs}>${escapeHtml(src)}</${tag}>`;
  }
  return `<${tag} class="${cls}"${attrs}>${html}</${tag}>`;
}

function renderSpans(spans: InlineSpan[], options: RenderOptions, source: string): string {
  return spans.map((s) => renderSpan(s, options, source)).join("");
}

function renderSpan(span: InlineSpan, options: RenderOptions, source: string): string {
  const at = offsetAttrs(options, span.from, span.to);
  switch (span.kind) {
    case "text":
      // 编辑面把纯文本包一层带去偏移的 span，DOM 文本节点的每个字符才能定位回源码
      return options.offsets
        ? `<span data-md-from="${span.from}" data-md-to="${span.to}">${escapeHtml(span.text)}</span>`
        : escapeHtml(span.text);
    case "strong":
      return `<strong${at}>${renderSpans(span.children, options, source)}</strong>`;
    case "em":
      return `<em${at}>${renderSpans(span.children, options, source)}</em>`;
    case "strike":
      return `<del${at}>${renderSpans(span.children, options, source)}</del>`;
    case "highlight":
      return `<mark class="md-highlight"${at}>${renderSpans(span.children, options, source)}</mark>`;
    case "code":
      return `<code${at}>${escapeHtml(span.text)}</code>`;
    case "mathInline":
      return renderMath(span.tex, false, options, at);
    case "comment":
      return "";
    case "link":
      return renderLink(span, source, options);
    case "autolink":
      return `<span class="md-editor-link"${at} data-md-href="${escapeHtml(span.href)}" title="${escapeHtml(span.href)}">${escapeHtml(span.label)}</span>`;
    case "wiki":
      return `<span class="md-editor-internal-link"${at} data-md-wiki="${escapeHtml(span.target)}">${escapeHtml(span.label)}</span>`;
    case "image":
      return renderImage(span, options);
    case "tag":
      return `<span class="md-editor-tag"${at}>#${escapeHtml(span.tag)}</span>`;
    case "footnoteRef":
      return `<sup class="md-editor-footnote-ref"${at}>${escapeHtml(span.label)}</sup>`;
    case "mention":
      return `<span class="mention-capsule"${at} data-md-mention-key="${escapeHtml(span.key)}">@${escapeHtml(span.label)}</span>`;
    case "html":
      return `<span class="md-editor-html"${at}>${sanitizeHtmlFragment(span.html)}</span>`;
    case "hardBreak":
      return `<br${at}>`;
    default:
      return "";
  }
}

function renderLink(span: Extract<InlineSpan, { kind: "link" }>, source: string, options: RenderOptions): string {
  const at = offsetAttrs(options, span.from, span.to);
  const href = escapeHtml(span.href);
  const label = escapeHtml(span.label);
  const tip = escapeHtml(span.title ?? span.href);
  switch (span.form) {
    case "external":
      return `<span class="md-editor-link"${at} data-md-href="${href}" title="${tip}">${label}</span>`;
    case "path":
      return `<span class="md-editor-internal-link"${at} data-md-href="${href}" title="${tip}">${label}</span>`;
    case "create":
      return `<span class="md-editor-internal-link md-editor-internal-link-missing"${at} data-md-href="${href}" title="${tip}">${label}</span>`;
    case "wiki":
      return `<span class="md-editor-internal-link"${at} data-md-wiki="${href}">${label}</span>`;
    default:
      // 未识别形态：保持原文
      return options.offsets
        ? `<span${at}>${escapeHtml(source.slice(span.from, span.to))}</span>`
        : escapeHtml(source.slice(span.from, span.to));
  }
}

function renderImage(span: Extract<InlineSpan, { kind: "image" }>, options: RenderOptions): string {
  const attrs = [
    `data-md-src="${escapeHtml(span.src)}"`,
    `data-md-alt="${escapeHtml(span.alt)}"`,
    `title="${escapeHtml(span.title || span.alt || span.src)}"`,
  ];
  if (options.offsets) {
    attrs.unshift(`data-md-from="${span.from}"`, `data-md-to="${span.to}"`);
  }
  const style: string[] = [];
  if (span.width) {
    attrs.push(`data-md-width="${escapeHtml(span.width)}"`);
    style.push(`width:${span.width}px`);
  }
  if (span.height) {
    attrs.push(`data-md-height="${escapeHtml(span.height)}"`);
    style.push(`height:${span.height}px`);
  }
  const styleAttr = style.length > 0 ? ` style="${style.join(";")}"` : "";
  const external = /^(https?:|data:|blob:)/i.test(span.src);
  const srcAttr = external ? ` src="${escapeHtml(span.src)}"` : "";
  return (
    `<span class="md-editor-image" ${attrs.join(" ")}${styleAttr}>` +
    `<img alt="${escapeHtml(span.alt || span.src)}" loading="lazy" draggable="false"${srcAttr}></span>`
  );
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}