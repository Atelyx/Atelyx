/**
 * Markdown 内核渲染：文档规格 → 已清洗 HTML 字符串（框架无关，供 React/插件复用）。
 *
 * 安全：raw HTML 一律经 `sanitizeHtmlFragment`（唯一注入点）清洗；其余内容一律转义，
 * 只出 class + 文本。类名契约与既有样式表保持一致（勿改名）。
 * 代码块只出结构与 data- 标记，复制按钮的点击行为由视图层绑定。
 */
import katex from "katex";
import { sanitizeHtmlFragment } from "@/utils/htmlSanitize";
import type { InlineSpan, MarkdownBlock, RenderOptions } from "@/types/markdown";
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
  return doc.blocks.map((b) => renderBlock(b, options, doc.source)).join("");
}

/** 块以源码形态渲染（编辑面用：光标所在块与代码块回显 markdown 原文，便于直接改标记）。 */
function renderBlockSource(block: { from: number; to: number; kind: string }, source: string, options: RenderOptions): string {
  const raw = source.slice(block.from, block.to);
  const inner = options.offsets
    ? `<span data-md-from="${block.from}" data-md-to="${block.to}">${escapeHtml(raw)}</span>`
    : escapeHtml(raw);
  return `<div class="md-editor-source"${blockAttrs(block, options)}>${inner}</div>`;
}

/**
 * 编辑形态渲染：光标所在块与代码块显示源码、其余块渲染——与只读形态共用同一套块渲染器，
 * 因此「预览 ⇄ 编辑」只切换活动块的呈现，不产生两套视觉。
 */
export function renderMarkdownEditHtml(
  source: string,
  options: RenderOptions & { activeOffset?: number } = {},
): string {
  const doc = parseMarkdown(source, options);
  return doc.blocks
    .map((block) => {
      const active =
        options.activeOffset !== undefined && options.activeOffset >= block.from && options.activeOffset <= block.to;
      const code = block.kind === "fencedCode" || block.kind === "indentedCode";
      return active || code ? renderBlockSource(block, source, options) : renderBlock(block, options, doc.source);
    })
    .join("");
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

function renderBlock(block: MarkdownBlock, options: RenderOptions, source: string): string {
  const at = blockAttrs(block, options);
  switch (block.kind) {
    case "heading":
      return `<h${block.level}${at}>${renderSpans(block.inline, options, source)}</h${block.level}>`;
    case "paragraph":
      return `<p${at}>${renderSpans(block.inline, options, source)}</p>`;
    case "blockquote": {
      const inner = block.children.map((c) => renderBlock(c, options, source)).join("");
      if (block.callout) {
        const badge = `<span class="md-editor-callout-badge">${escapeHtml(block.callout)}</span>`;
        return `<blockquote class="md-callout callout-${escapeHtml(block.callout)}"${at}>${badge}${inner}</blockquote>`;
      }
      return `<blockquote class="md-quote"${at}>${inner}</blockquote>`;
    }
    case "list":
      return renderList(block, options, source, at);
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
 */
function renderList(
  block: Extract<MarkdownBlock, { kind: "list" }>,
  options: RenderOptions,
  source: string,
  containerAttrs: string,
): string {
  const tag = block.ordered ? "ol" : "ul";
  const items = block.items.map((item, index) => {
    const raw = source.slice(item.from, item.to);
    const m = /^[ \t]*([-+*]|\d+[.)])[ \t]+(\[[ xX]\][ \t]+)?/.exec(raw);
    const contentFrom = item.from + (m ? m[0].length : 0);
    // 首段内容止于第一个嵌套子块起点（无子块则到项尾），嵌套块自身递归渲染
    const contentTo = item.children[0]?.from ?? item.to;
    const content = renderSpans(
      parseInlineRange(source, contentFrom, Math.max(contentFrom, contentTo), options),
      options,
      source,
    );
    const inner = item.children.map((c) => renderBlock(c, options, source)).join("");
    const itemAttrs = offsetAttrs(options, item.from, item.to);
    if (item.task) {
      const checked = item.checked ? " checked" : "";
      return `<li${itemAttrs}><input type="checkbox" class="md-editor-checkbox"${checked} disabled>${content}${inner}</li>`;
    }
    const marker = block.ordered ? m?.[1] ?? `${index + 1}.` : "•";
    return `<li${itemAttrs}><span class="md-editor-list-marker">${escapeHtml(marker)}</span>${content}${inner}</li>`;
  });
  return `<${tag} class="md-editor-list"${containerAttrs}>${items.join("")}</${tag}>`;
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
  switch (span.form) {
    case "external":
      return `<span class="md-editor-link"${at} data-md-href="${href}" title="${href}">${label}</span>`;
    case "path":
      return `<span class="md-editor-internal-link"${at} data-md-href="${href}">${label}</span>`;
    case "create":
      return `<span class="md-editor-internal-link md-editor-internal-link-missing"${at} data-md-href="${href}">${label}</span>`;
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
    `title="${escapeHtml(span.alt || span.src)}"`,
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