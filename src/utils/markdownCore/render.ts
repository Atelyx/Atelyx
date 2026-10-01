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

/**
 * 顶层渲染分片：一个分片对应一个顶层元素（块 / 空行行元素）。
 * 编辑面按分片做增量替换——文本变更后只重建真正变化的分片，不整篇替换 DOM。
 */
export interface RenderChunk {
  /** 分片自身的源区间（空行分片为点区间）。 */
  from: number;
  to: number;
  /** 单个顶层元素的 HTML。 */
  html: string;
}

/** 编辑面渲染选项：只读渲染选项 + 富装饰块判定偏移 + 输入法组合串。 */
export interface EditRenderOptions extends RenderOptions {
  /** 光标偏移：落在富装饰块内时该块整块回显源码（其余块的揭示由视图层切 class）。 */
  activeOffset?: number;
  /** 沿用偏移：`activeOffset` 不落在任何块内（如停在块间空行）时的回显目标。 */
  fallbackOffset?: number;
  /** 输入法组合中的未上屏文本：`remove` 为被它替换掉的正文长度。 */
  composition?: { at: number; text: string; remove?: number };
}

/** 编辑形态渲染结果：分片 + 本次整块回显源码的块起点。 */
export interface EditRenderResult {
  chunks: RenderChunk[];
  /** 本次整块回显源码的富装饰块起点（没有则为 null），视图层据此判断是否需要重绘。 */
  activeBlock: number | null;
}

/** 仍整块回显源码的块类型：富装饰件（表格 / raw HTML / 缩进代码）不做标记层，光标进入即回显源码。 */
export const RAW_SOURCE_KINDS: ReadonlySet<string> = new Set(["table", "indentedCode", "htmlBlock"]);

/** 组合串占位元素的 class 标记（渲染后据此判断是否已就地插入）。 */
const COMPOSITION_CLASS = 'class="md-composition"';

/**
 * 源标记片段：源码里的标记字符（`**`、`- `、`##` 等）必须原样进 DOM，位置与纯文本一致；
 * 默认由样式层 `display:none` 收掉，光标进入其宿主时给宿主切 `md-reveal` 才显形——
 * 因此揭示只切 class、不重建 DOM，编辑态与只读态视觉一致、无横移。
 * 只有编辑面（`offsets`）才产出标记：只读面与插件面拿到的是纯渲染结果。
 */
function markSpan(source: string, from: number, to: number, options: EditRenderOptions, selfOwned = false): string {
  if (!options.offsets || to <= from) return "";
  // selfOwned：标记自身即揭示宿主（如围栏行）——只在自己那段源码区间内才显形，
  // 与「宿主元素整块揭示」互不影响，故两者可并存
  const own = selfOwned ? " data-md-own" : "";
  const segment = (a: number, b: number): string =>
    b > a
      ? `<span class="md-marker"${own} data-md-from="${a}" data-md-to="${b}">${escapeHtml(source.slice(a, b))}</span>`
      : "";
  // 组合串落在标记区间内时就地插入（左闭右开，与相邻片段/标记的归属互不重叠）：
  // 光标可以停在标记里（行首 Home、方向键），未上屏文本若无处插入会被挂到文档末尾
  const composition = options.composition;
  if (!composition?.text) return segment(from, to);
  const { at } = composition;
  const end = at + (composition.remove ?? 0);
  if (at < from) return end > from ? segment(Math.max(from, end), to) : segment(from, to);
  if (at >= to) return segment(from, to);
  return segment(from, at) + insertComposition(options) + segment(Math.min(to, end), to);
}

/** 带标记宿主的元素：直接含标记字符的元素，揭示时只给它切 class（编辑器按 data-md-own 收集）。 */
function ownAttrs(options: EditRenderOptions): string {
  return options.offsets ? " data-md-own" : "";
}

/** 揭示时让位的渲染件：它是源文本的「替换形态」（图片、公式、脚注角标、代码块标题栏等），
 *  源标记显形后与它同现就是重复，故揭示态下隐藏。只编辑面产出，只读面与插件面产物不变。 */
const YIELD_CLASS = "md-yield";

/** 让位类（挂在渲染件自身）：`class="xxx${yieldClass(options)}"`。 */
function yieldClass(options: EditRenderOptions): string {
  return options.offsets ? ` ${YIELD_CLASS}` : "";
}

/** 让位壳（包住替换形态）：编辑面外包一层让位元素，只读面原样输出。 */
function yielded(html: string, options: EditRenderOptions): string {
  return options.offsets ? `<span class="${YIELD_CLASS}">${html}</span>` : html;
}

/** 输入法组合串的就地占位元素：不带源偏移（不是正文的一部分），由映射层跳过。 */
function compositionHtml(text: string): string {
  return `<span ${COMPOSITION_CLASS}>${escapeHtml(text)}</span>`;
}

/**
 * 组合串插入记账：一次渲染里同一个偏移只能插一份。多个片段都会回看同一偏移——`**b**` 的
 * 末端既是加粗元素末端也是所在内容区末端、代码正文末端同时是闭定界符起点、围栏代码的闭围栏
 * 起点紧邻末尾空行——所以记账按「整次渲染」共享，由 {@link withCompositionClaim} 在渲染入口
 * 建立、退出时恢复（渲染是同步的，嵌套调用不会交叉）。
 */
let compositionClaim: { placed: boolean } | null = null;

/** 渲染入口挂记账：本次渲染要插入的未上屏文本来自 `options.composition`。 */
function withCompositionClaim<T>(options: EditRenderOptions, run: () => T): T {
  const previous = compositionClaim;
  compositionClaim = options.composition?.text ? { placed: false } : null;
  try {
    return run();
  } finally {
    compositionClaim = previous;
  }
}

/** 未上屏文本的占位元素（已插过返回空串）：所有插入点都经此函数，保证一个偏移只插一份。 */
function insertComposition(options: EditRenderOptions): string {
  const composition = options.composition;
  if (!composition?.text || compositionClaim?.placed) return "";
  if (compositionClaim) compositionClaim.placed = true;
  return compositionHtml(composition.text);
}

/** 带源区间的纯文本片段（编辑面逐字符定位的最小单位）；空片段不产出元素。 */
function textSpanHtml(from: number, to: number, text: string, options: EditRenderOptions): string {
  if (to <= from) return "";
  return options.offsets ? `<span data-md-from="${from}" data-md-to="${to}">${escapeHtml(text)}</span>` : escapeHtml(text);
}

/** 源文本片段的 HTML：把输入法组合串按源偏移就地插进去（已由别处插入时不重复插）。 */
function spliceComposition(from: number, to: number, text: string, options: EditRenderOptions): string {
  const composition = options.composition;
  // 文本与区间不等长（解析层对标签做过 trim、空标签回退成地址等）：区间不可拆分，整段作原子渲染
  if (!composition?.text || text.length !== to - from) return textSpanHtml(from, to, text, options);
  const at = composition.at;
  // 组合串替换掉的正文（有选区起输入法时）不再出渲染，否则未上屏文本会与旧文字叠显
  const end = at + (composition.remove ?? 0);
  if (end > at && at <= from && end >= to) return "";
  if (at < from || at > to) {
    if (end > from && end <= to) return textSpanHtml(end, to, text.slice(end - from), options);
    return textSpanHtml(from, to, text, options);
  }
  const head = at > from ? textSpanHtml(from, at, text.slice(0, at - from), options) : "";
  const tailFrom = end > at && end <= to ? end : at;
  const tail = tailFrom < to ? textSpanHtml(tailFrom, to, text.slice(tailFrom - from), options) : "";
  return head + insertComposition(options) + tail;
}

export function renderMarkdownToHtml(source: string, options: RenderOptions = {}): string {
  return joinChunks(renderMarkdownChunks(source, options));
}

/** 只读渲染分片（与 {@link renderMarkdownToHtml} 同源，供编辑面增量替换）。 */
export function renderMarkdownChunks(source: string, options: RenderOptions = {}): RenderChunk[] {
  return withCompositionClaim(options, () => {
    const doc = parseMarkdown(source, options);
    return documentChunks(doc.blocks, doc.source, options, (b) => renderBlock(b, options, doc.source));
  });
}

function joinChunks(chunks: readonly RenderChunk[]): string {
  return chunks.map((chunk) => chunk.html).join("");
}

/**
 * 文档级分片：块间空行之外，文首/文末的源码空行同样渲染为行元素——
 * 文末按 Enter 开出的空行有真实行高与光标锚（只在块间渲染时文末新行无处显示）。
 */
function documentChunks(
  blocks: readonly MarkdownBlock[],
  source: string,
  options: EditRenderOptions,
  renderOne: (block: MarkdownBlock) => string,
): RenderChunk[] {
  if (blocks.length === 0) return blankLineChunks(source, 0, source.length, options);
  return [
    ...blankLineChunks(source, 0, blocks[0]!.from, options),
    ...blockChunks(blocks, source, options, renderOne),
    ...blankLineChunks(source, blocks[blocks.length - 1]!.to, source.length, options),
  ];
}

/**
 * 块序列分片：相邻块之间的源码空行渲染为真实空行行元素（所见即所得——源码怎么空行，
 * 渲染就逐行怎么间隔；块级垂直外边距不参与，垂直间距 = 空行数 × 行高）。
 * 顶层直接产出分片；引用内 / 列表项内由 {@link renderBlocks} 拼回单段 HTML。
 */
function blockChunks(
  blocks: readonly MarkdownBlock[],
  source: string,
  options: EditRenderOptions,
  renderOne: (block: MarkdownBlock) => string,
): RenderChunk[] {
  const out: RenderChunk[] = [];
  let prev: MarkdownBlock | null = null;
  for (const block of blocks) {
    if (prev) out.push(...blankLineChunks(source, prev.to, block.from, options));
    const html = renderOne(block);
    if (html) out.push({ from: block.from, to: block.to, html });
    prev = block;
  }
  return out;
}

/** 嵌套块序列（引用内 / 列表项内）：与顶层分片同一套渲染，拼回单段 HTML 嵌入父元素。 */
function renderBlocks(
  blocks: readonly MarkdownBlock[],
  source: string,
  options: EditRenderOptions,
  renderOne: (block: MarkdownBlock) => string,
): string {
  return joinChunks(blockChunks(blocks, source, options, renderOne));
}

/** 空行行元素：每个源码空行一个一行高的真实行。编辑态带该空行的源偏移（点区间）
 *  与零宽字符锚——光标驻留、点击命中都直接落在空行上，无需几何换算。
 *  首段 = 前块行尾残余、末段 = 后块首行前缀；纯空白行与引用的空延续行（`>`）都算空行。 */
function blankLineChunks(source: string, from: number, to: number, options: EditRenderOptions): RenderChunk[] {
  const lines = source.slice(from, to).split("\n");
  const out: RenderChunk[] = [];
  let offset = from;
  for (let i = 1; i < lines.length - 1; i++) {
    offset += lines[i - 1]!.length + 1;
    if (!/^[\s>]*$/.test(lines[i]!)) continue;
    const at = options.offsets ? ` data-md-from="${offset}" data-md-to="${offset}"` : "";
    const anchor = options.offsets ? "\u200B" : "";
    // 光标停在该空行上组合时，未上屏文本就地显示在这一行
    const composition =
      options.offsets && options.composition?.at === offset ? insertComposition(options) : "";
    out.push({ from: offset, to: offset, html: `<div class="md-editor-gap"${at}>${anchor}${composition}</div>` });
  }
  return out;
}

/** 块以源码形态渲染（编辑面用：光标所在块回显 markdown 原文，便于直接改标记）。 */
function renderBlockSource(block: { from: number; to: number; kind: string }, source: string, options: EditRenderOptions): string {
  const raw = source.slice(block.from, block.to);
  return `<div class="md-editor-source"${blockAttrs(block, options)}>${spliceComposition(block.from, block.to, raw, options)}</div>`;
}

/**
 * 编辑形态渲染：与只读形态共用同一套块渲染器，另加一层默认隐藏的源码标记
 * （只有富装饰块整块回显源码），因此「预览 ⇄ 编辑」视觉一致、不产生两套样式。
 */
export function renderMarkdownEditHtml(source: string, options: EditRenderOptions = {}): string {
  return joinChunks(renderMarkdownEditChunks(source, options).chunks);
}

/**
 * 覆盖该偏移、且属于「整块回显源码」类型的块（引用内与列表项内的嵌套块一并下探）。
 * 返回值与编辑面 DOM 上带 `data-md-kind` 的 `[data-md-block]` 一一对应：
 * 视图层据此比对「是否正落在该块上」，避免光标微动就重绘。
 */
function findRawSourceBlock(blocks: readonly MarkdownBlock[], offset: number | undefined): MarkdownBlock | null {
  if (offset === undefined) return null;
  let best: MarkdownBlock | null = null;
  let bestSpan = Infinity;
  const consider = (block: MarkdownBlock): void => {
    if (!RAW_SOURCE_KINDS.has(block.kind)) return;
    if (offset < block.from || offset > block.to) return;
    const span = block.to - block.from;
    if (span < bestSpan) {
      bestSpan = span;
      best = block;
    }
  };
  const walk = (list: readonly MarkdownBlock[]): void => {
    for (const block of list) {
      consider(block);
      if (block.kind === "blockquote") walk(block.children);
      else if (block.kind === "list") {
        for (const item of block.items) walk(item.children);
      }
    }
  };
  walk(blocks);
  return best;
}

/** 编辑形态分片（与 {@link renderMarkdownEditHtml} 同源，供编辑面增量替换）。 */
export function renderMarkdownEditChunks(source: string, options: EditRenderOptions = {}): EditRenderResult {
  return withCompositionClaim(options, () => {
    const doc = parseMarkdown(source, options);
    // 只有富装饰块（表格 / raw HTML / 缩进代码）仍整块回显源码；其余块一律「渲染 + 标记层」，
    // 标记默认隐藏、揭示由视图层切 class，因此光标移动不再触发内容重绘
    const rawBlock =
      findRawSourceBlock(doc.blocks, options.activeOffset) ?? findRawSourceBlock(doc.blocks, options.fallbackOffset);
    const chunks = documentChunks(doc.blocks, doc.source, options, (block) =>
      renderBlock(block, options, doc.source, rawBlock),
    );
    // 未上屏文本没有落在任何分片里（空文档、文末换行之后等没有块也没有空行的位置）：
    // 单独成一片挂在末尾，保证组合串与光标在正文尚无内容时也可见
    if (options.composition?.text && !chunks.some((chunk) => chunk.html.includes(COMPOSITION_CLASS))) {
      chunks.push({
        from: options.composition.at,
        to: options.composition.at,
        html: compositionHtml(options.composition.text),
      });
    }
    return { chunks, activeBlock: rawBlock?.from ?? null };
  });
}

/** 编辑面定位属性：offsets 关闭时为空串，插件侧拿到的 HTML 与只读渲染完全一致。 */
function offsetAttrs(options: EditRenderOptions, from: number, to: number): string {
  return options.offsets ? ` data-md-from="${from}" data-md-to="${to}"` : "";
}

/** 块容器定位属性（含块类型：编辑面据此判定代码块等「不回显渲染」的块）。 */
function blockAttrs(block: { from: number; to: number; kind: string }, options: EditRenderOptions): string {
  return options.offsets
    ? ` data-md-block data-md-kind="${block.kind}" data-md-from="${block.from}" data-md-to="${block.to}"`
    : "";
}

/**
 * 块内容起点：其前的源码若只是引用标记与空白（`> ` / `> > `），它属于**该源码行**的前缀，
 * 而不是某个嵌套容器的一部分——返回行首，让这份前缀随本行内容一起进标记层。
 * 引用嵌套时每行各自携带自己那几层，与源文本逐字对应；行首是缩进（列表嵌套）则不动。
 */
function contentStart(from: number, source: string): number {
  const lineStart = source.lastIndexOf("\n", from - 1) + 1;
  return /^[ \t]*>[ \t>]*$/.test(source.slice(lineStart, from)) ? lineStart : from;
}

/** 块级行内内容：行前缀（引用的 `>`）与片段之间、两端的源码一并进标记层。 */
function blockContent(
  block: { from: number; to: number; inline: InlineSpan[] },
  options: EditRenderOptions,
  source: string,
): string {
  return renderSpans(block.inline, options, source, { from: contentStart(block.from, source), to: block.to });
}

function renderBlock(
  block: MarkdownBlock,
  options: EditRenderOptions,
  source: string,
  rawBlock: MarkdownBlock | null = null,
): string {
  // 富装饰块整体回显源码（引用内与列表项内的嵌套块由 rawBlock 逐层比对）
  if (block === rawBlock) return renderBlockSource(block, source, options);
  const at = blockAttrs(block, options);
  const own = ownAttrs(options);
  switch (block.kind) {
    case "heading":
      return `<h${block.level}${at}${own}>${blockContent(block, options, source)}</h${block.level}>`;
    case "paragraph":
      return `<p${at}${own}>${blockContent(block, options, source)}</p>`;
    case "blockquote": {
      // 引用标记 `>` 是**行**前缀（由各行内容自己携带，见 contentStart），容器不再另出标记
      const inner = renderBlocks(block.children, source, options, (c) => renderBlock(c, options, source, rawBlock));
      if (block.callout) {
        // 徽标是 `[!类型]` 的替换形态：光标进入该块显形源标记时让位
        const badge = `<span class="md-editor-callout-badge${yieldClass(options)}">${escapeHtml(block.callout)}</span>`;
        return `<blockquote class="md-callout callout-${escapeHtml(block.callout)}"${at}${own}>${badge}${inner}</blockquote>`;
      }
      return `<blockquote class="md-quote"${at}>${inner}</blockquote>`;
    }
    case "list":
      return renderList(block, options, source, at, rawBlock);
    case "fencedCode":
      return renderCodeBlock(block, options, source, at);
    case "indentedCode":
      return `<div class="md-editor-code-block"${at}><pre><code>${escapeHtml(block.code)}</code></pre></div>`;
    case "hr":
      // `---` 整行替换为分隔线：源标记进标记层（揭示时显形，隐藏时只剩线）。另需一个零宽锚——
      // 分隔线本身是块、内部源标记默认隐藏，没有可量出矩形的位置，点击落点会落到相邻行上，
      // 分隔线也就永远进不了源码态；零宽锚随分隔线一起揭示，锚点落在块起点
      return (
        `<div class="md-editor-divider"${at}${own}>` +
        (options.offsets ? `<span data-md-from="${block.from}" data-md-to="${block.from}">\u200B</span>` : "") +
        markSpan(source, block.from, block.to, options) +
        `</div>`
      );
    case "table":
      return renderTable(block, at);
    case "mathBlock":
      return renderMathBlock(block, options, source, at);
    case "htmlBlock":
      // raw HTML 块整体为原子块：编辑面点击落到块起点（内部 DOM 来自清洗后的外部 HTML，不含偏移）
      return `<div class="md-editor-html"${at}>${sanitizeHtmlFragment(block.html)}</div>`;
    case "footnoteDef": {
      const first = block.inline[0];
      const marker = markSpan(source, block.from, first?.from ?? block.to, options);
      return `<div class="md-editor-footnote-def"${at}${own}>${marker}${yielded(`<sup>${escapeHtml(block.label)}</sup>`, options)}${renderSpans(block.inline, options, source)}</div>`;
    }
    default:
      return "";
  }
}

/**
 * 列表渲染：圆点/序号由自绘 `md-editor-list-marker` 承载（有序项保留源序号），容器打上
 * `md-editor-list` 类供样式层关闭原生 marker（防双重显示；raw HTML 中的列表不受影响）。
 * 项内容 = 剥标记后的首段行内片段 + 递归渲染的嵌套子块，子块间源码空行折算为真实空行行元素。
 * 源标记与自绘圆点同时产出、由样式层按揭示态取舍；空项（只有标记无内容）恒为渲染态，
 * 自绘 marker 即输入标记的即时反馈。
 */
function renderList(
  block: Extract<MarkdownBlock, { kind: "list" }>,
  options: EditRenderOptions,
  source: string,
  containerAttrs: string,
  rawBlock: MarkdownBlock | null,
): string {
  const tag = block.ordered ? "ol" : "ul";
  const parts: string[] = [];
  block.items.forEach((item, index) => {
    if (index > 0) {
      const prev = block.items[index - 1]!;
      parts.push(joinChunks(blankLineChunks(source, prev.to, item.from, options)));
    }
    parts.push(renderListItem(item, block, index, options, source, rawBlock));
  });
  return `<${tag} class="md-editor-list"${containerAttrs}>${parts.join("")}</${tag}>`;
}

/** 列表项标记（含任务框）：项内容剥取与空项判定共用。
 *  标记后可以没有空格（`-` 独占一行也是合法空项），故空格按零个起算——
 *  否则整行会被当成项内容，渲染出「圆点 + 字面量 `-`」、揭示态又无标记可显。 */
const ITEM_MARKER_RE = /^[ \t]*([-+*]|\d+[.)])[ \t]*(\[[ xX]\][ \t]*)?/;

/**
 * 项是否要独占一行空内容：行内内容为空、且首个子块不是可与标记同行的块。
 * 首子块为嵌套列表时，那一行属于子项（圆点也属子项），父项须自己占一行——
 * 否则两者会挤进同一行；标题/引用/代码等块则与父项标记同行，不另起一行。
 */
function itemNeedsOwnLine(source: string, item: MarkdownListItem): boolean {
  const m = ITEM_MARKER_RE.exec(source.slice(item.from, item.to));
  if (!m) return false;
  const contentEnd = item.children[0]?.from ?? item.to;
  if (source.slice(item.from + m[0].length, contentEnd).trim() !== "") return false;
  return item.children.length === 0 || item.children[0]!.kind === "list";
}

/**
 * 单个列表项渲染：li 带项级块标记（揭示与偏移映射细到项）。
 * 源标记与自绘圆点同处悬挂位（揭示时圆点让位、源标记显形），两者都不占内容流（浮动悬挂）。
 */
function renderListItem(
  item: MarkdownListItem,
  block: Extract<MarkdownBlock, { kind: "list" }>,
  index: number,
  options: EditRenderOptions,
  source: string,
  rawBlock: MarkdownBlock | null,
): string {
  const raw = source.slice(item.from, item.to);
  const m = ITEM_MARKER_RE.exec(raw);
  const contentFrom = item.from + (m ? m[0].length : 0);
  // 首段内容止于第一个嵌套子块起点（无子块则到项尾）：子块前那段换行与续行缩进由子块承担，
  // 留在文本里会被 pre-wrap 渲染成多余空行；段内换行是真实换行，须保留
  let contentTo = item.children[0]?.from ?? item.to;
  while (contentTo > contentFrom && /\s/.test(source[contentTo - 1]!)) contentTo--;
  // 空项必带一个流内占位（零宽字符）：圆点浮动悬挂、不产生行盒，无流内内容时嵌套子列表
  // 会顶到首行、与圆点挤在同一行。编辑态它还是光标驻留位，输入即落于此；
  // 组合串须一并就地插入（该路径不经过行内片段渲染，漏掉会让它退化成文末独立分片）
  const content = itemNeedsOwnLine(source, item)
    ? options.offsets
      ? `<span data-md-from="${contentFrom}" data-md-to="${contentFrom}">\u200B</span>` +
        spliceComposition(contentFrom, contentTo, "", options)
      : "\u200B"
    : renderSpans(
        parseInlineRange(source, contentFrom, Math.max(contentFrom, contentTo), options),
        options,
        source,
      );
  const inner = renderBlocks(item.children, source, options, (c) => renderBlock(c, options, source, rawBlock));
  const itemAttrs = blockAttrs({ ...item, kind: "list" }, options) + ownAttrs(options);
  // 内容与首个子块之间：源码里的空行按所见即所得渲染成真实空行行元素（与项之间、块之间同一套），
  // 子块前的缩进空白则并进内容行的标记层——这两段都没有可见内容，但都要有光标落点。
  // 缩进空白不能排在空行元素之后：inline 空白夹在两个块级元素之间会自成一个行盒，
  // 揭示态就会平白多出一行（项内源码只有一处空行，渲染也只得一行）
  const childFrom = item.children[0]?.from;
  const indented = childFrom !== undefined && childFrom > contentTo;
  const indentMark = indented
    ? markSpan(source, source.lastIndexOf("\n", childFrom! - 1) + 1, childFrom!, options)
    : "";
  const gapLines = indented ? joinChunks(blankLineChunks(source, contentTo, childFrom!, options)) : "";
  // 源标记（默认隐藏、揭示时显形）必居首；自绘圆点/序号/任务框是渲染件，揭示时让位
  const mark = markSpan(source, contentStart(item.from, source), contentFrom, options);
  if (item.task) {
    const checked = item.checked ? " checked" : "";
    return `<li${itemAttrs}>${mark}<input type="checkbox" class="md-editor-checkbox"${checked} disabled>${content}${indentMark}${gapLines}${inner}</li>`;
  }
  // 无序项的小圆点不写字形：各平台回退字体对 "•" 的字宽与墨迹位置不一致（全角字形的墨迹居中，
  // 会偏出揭示态源标记 "- " 的左缘），由样式画出圆点才能与源标记落在同一处
  const marker = block.ordered ? m?.[1] ?? `${index + 1}.` : "";
  const bullet = block.ordered ? "" : " md-editor-list-bullet";
  return `<li${itemAttrs}>${mark}<span class="md-editor-list-marker${bullet}">${escapeHtml(marker)}</span>${content}${indentMark}${gapLines}${inner}</li>`;
}

function renderCodeBlock(
  block: Extract<MarkdownBlock, { kind: "fencedCode" }>,
  options: EditRenderOptions,
  source: string,
  attrs: string,
): string {
  const head =
    `<div class="md-editor-code-head">` +
    `<span class="md-editor-code-lang">${escapeHtml(block.lang || "代码")}</span>` +
    `<button type="button" class="md-editor-code-copy" data-md-copy title="复制代码" aria-label="复制代码"></button>` +
    `</div>`;
  // 末尾换行不进 <code>：<pre> 会吞掉末尾那个换行，且那里没有可落点的文本节点
  // （光标矩形成零、输入法候选框跑到文档末尾）。末尾空行改由带零宽锚的独立块承载，
  // 与块间空行同一套做法——光标直接驻留在锚上，几何可测。正文区间随之止于换行前
  const trailing = /(?:\n)+$/.exec(block.code)?.[0].length ?? 0;
  const coreTo = block.contentTo - trailing;
  // 围栏行各自作揭示宿主：光标落在代码正文里时保持渲染态（可在 <code> 内就地编辑），
  // 只有光标落到围栏行（点标题栏即落到开围栏起点）才显形围栏、让盒装外框塌陷为源码。
  // 两端的换行不算围栏行——否则光标停在正文首/末字符上就会被判成落在围栏里
  const fenceTo =
    block.contentFrom > block.from && source[block.contentFrom - 1] === "\n" ? block.contentFrom - 1 : block.contentFrom;
  const closeFrom =
    block.contentTo < block.to && source[block.contentTo] === "\n" ? block.contentTo + 1 : block.contentTo;
  const fence = markSpan(source, block.from, fenceTo, options, true);
  const close = markSpan(source, closeFrom, block.to, options, true);
  let blanks = "";
  for (let i = 1; i <= trailing; i++) {
    const at = coreTo + i;
    const at2 = options.offsets ? ` data-md-from="${at}" data-md-to="${at}"` : "";
    // 组合串落在这一空行时就地插入：它是本行的落点，输入法候选框才跟得上
    const composing = options.composition?.at === at ? insertComposition(options) : "";
    blanks += `<div class="md-editor-code-blank"${at2}>${options.offsets ? `\u200B${composing}` : ""}</div>`;
  }
  // 正文区间止于末尾换行前；正文与行内同一套渲染（组合串按源偏移就地插入，否则没有落点会被挂到文末）
  const body = trailing > 0 ? block.code.slice(0, block.code.length - trailing) : block.code;
  // 空正文（无内容或整段都是换行）给一个零宽锚：否则 <code> 内没有文本子节点，
  // 光标与输入法候选框都无处可测——只读面不需要落点，产物保持不变
  const code = body
    ? spliceComposition(block.contentFrom, coreTo, body, options)
    : options.offsets
      ? `<span data-md-from="${block.contentFrom}" data-md-to="${block.contentFrom}">\u200B</span>` +
        spliceComposition(block.contentFrom, coreTo, "", options)
      : "";
  return (
    `<div class="md-editor-code-block"${attrs}>${fence}${head}<pre>` +
    `<code>${code}</code>` +
    `${blanks}</pre>${close}</div>`
  );
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

/** KaTeX 渲染件：不是正文源文本（源 `$…$` 由标记层承载）；关闭 KaTeX 或渲染失败返回空串。 */
function mathHtml(tex: string, display: boolean, options: EditRenderOptions): string {
  return options.katex === false ? "" : renderKatexHtml(tex, display);
}

/** 公式块：源 `$$…$$` 进标记层，KaTeX 结果是渲染件；渲染失败时退回可见源文本（不丢内容）。 */
function renderMathBlock(block: Extract<MarkdownBlock, { kind: "mathBlock" }>, options: EditRenderOptions, source: string, attrs: string): string {
  const html = mathHtml(block.tex, true, options);
  if (!html) {
    const src = `$$\n${block.tex}\n$$`;
    return `<div class="md-editor-math md-editor-math-block md-editor-math-error"${attrs}>${escapeHtml(src)}</div>`;
  }
  return `<div class="md-editor-math md-editor-math-block"${attrs}${ownAttrs(options)}>${markSpan(source, block.from, block.to, options)}${html}</div>`;
}

/**
 * 行内片段序列：输入法组合串按源偏移就地插入——片段间是连续源文本，故取首个
 * 「区间覆盖该偏移」的片段插入（起点前插、终点后插、区间内则拆成前后两段），
 * 同一偏移只插一次（见 {@link insertComposition}）；偏移落在此序列之外时交给上层处理。
 */
function renderSpans(
  spans: InlineSpan[],
  options: EditRenderOptions,
  source: string,
  host?: { from: number; to: number },
): string {
  const composition = options.composition;
  const marks = host !== undefined && options.offsets === true;
  if (!composition && !marks) return spans.map((s) => renderSpan(s, options, source)).join("");
  if (spans.length === 0) {
    if (!marks || !host) return "";
    const html = markSpan(source, host.from, host.to, options);
    // 宿主区间末端（左闭右开覆盖不到）：未上屏文本接在末尾
    return html + (composition && composition.at === host.to ? insertComposition(options) : "");
  }
  const cut = composition?.remove ?? 0;
  const cutEnd = composition ? composition.at + cut : 0;
  let out = "";
  let cursor = host?.from ?? 0;
  for (const span of spans) {
    if (marks) {
      out += markSpan(source, cursor, span.from, options);
      cursor = span.to;
    }
    // 非文本片段起点即落点：插在元素之前（文本片段交给下面统一处理，避免边界重复插入）
    if (composition && composition.at === span.from && span.kind !== "text") {
      out += insertComposition(options);
    }
    // 片段被组合串的替换区间整体覆盖：与文本片段同一语义，不再出渲染（只覆盖一部分时照常渲染）
    if (composition && cut > 0 && composition.at <= span.from && cutEnd >= span.to) continue;
    // 文本片段与组合串（或被它替换掉的正文）相交：统一走 spliceComposition
    if (
      composition &&
      span.kind === "text" &&
      ((composition.at >= span.from && composition.at <= span.to) ||
        (cut > 0 && composition.at < span.from && cutEnd > span.from))
    ) {
      out += spliceComposition(span.from, span.to, span.text, options);
      continue;
    }
    out += renderSpan(span, options, source);
    if (composition && composition.at === span.to) out += insertComposition(options);
  }
  if (!marks || !host) return out;
  out += markSpan(source, cursor, host.to, options);
  // 宿主区间末端（左闭右开覆盖不到）：未上屏文本接在末尾
  if (composition && composition.at === host.to) out += insertComposition(options);
  return out;
}

function renderSpan(span: InlineSpan, options: EditRenderOptions, source: string): string {
  const at = offsetAttrs(options, span.from, span.to);
  switch (span.kind) {
    case "text":
      // 编辑面把纯文本包一层带去偏移的 span，DOM 文本节点的每个字符才能定位回源码
      return textSpanHtml(span.from, span.to, span.text, options);
    case "strong":
      return `<strong${at}${ownAttrs(options)}>${renderSpans(span.children, options, source, span)}</strong>`;
    case "em":
      return `<em${at}${ownAttrs(options)}>${renderSpans(span.children, options, source, span)}</em>`;
    case "strike":
      return `<del${at}${ownAttrs(options)}>${renderSpans(span.children, options, source, span)}</del>`;
    case "highlight":
      return `<mark class="md-highlight"${at}${ownAttrs(options)}>${renderSpans(span.children, options, source, span)}</mark>`;
    case "code": {
      // 定界反引号进标记层，代码文本单独成片段（映射与揭示都按源区间走）
      const ticks = /^`+/.exec(source.slice(span.from, span.to))?.[0].length ?? 0;
      const contentFrom = span.from + ticks;
      const contentTo = Math.max(contentFrom, span.to - ticks);
      return (
        `<code${at}${ownAttrs(options)}>` +
        markSpan(source, span.from, contentFrom, options) +
        spliceComposition(contentFrom, contentTo, span.text, options) +
        markSpan(source, contentTo, span.to, options) +
        `</code>`
      );
    }
    case "mathInline": {
      const html = mathHtml(span.tex, false, options);
      if (!html) {
        return `<span class="md-editor-math md-editor-math-error"${at}>${textSpanHtml(span.from, span.to, source.slice(span.from, span.to), options)}</span>`;
      }
      return `<span class="md-editor-math"${at}${ownAttrs(options)}>${markSpan(source, span.from, span.to, options)}${html}</span>`;
    }
    case "comment":
      // 承载内容但不渲染：编辑面把源文本放进标记层（带宿主，光标进入时才显形），只读面不输出
      return options.offsets
        ? `<span${at}${ownAttrs(options)}>${markSpan(source, span.from, span.to, options)}</span>`
        : "";
    case "link":
      return renderLink(span, source, options);
    case "autolink": {
      const link = `class="md-editor-link"${at}`;
      const meta = `data-md-href="${escapeHtml(span.href)}" title="${escapeHtml(span.href)}"`;
      // 裸 URL 整段就是可见文本；`<http://x>` 只有尖括号是标记
      if (source[span.from] !== "<") {
        return `<span ${link} ${meta}>${textSpanHtml(span.from, span.to, span.label, options)}</span>`;
      }
      return (
        `<span ${link}${ownAttrs(options)} ${meta}>` +
        markSpan(source, span.from, span.from + 1, options) +
        spliceComposition(span.from + 1, span.to - 1, span.label, options) +
        markSpan(source, span.to - 1, span.to, options) +
        `</span>`
      );
    }
    case "wiki":
      return renderWiki(span, source, options);
    case "image":
      return renderImage(span, source, options);
    case "tag":
      return `<span class="md-editor-tag"${at}>#${escapeHtml(span.tag)}</span>`;
    case "footnoteRef":
      return (
        `<sup class="md-editor-footnote-ref"${at}${ownAttrs(options)}>` +
        markSpan(source, span.from, span.to, options) +
        yielded(escapeHtml(span.label), options) +
        `</sup>`
      );
    case "mention":
      return `<span class="mention-capsule"${at} data-md-mention-key="${escapeHtml(span.key)}">@${escapeHtml(span.label)}</span>`;
    case "html":
      return (
        `<span class="md-editor-html"${at}${ownAttrs(options)}>` +
        markSpan(source, span.from, span.to, options) +
        yielded(sanitizeHtmlFragment(span.html), options) +
        `</span>`
      );
    case "hardBreak":
      // 源里的两个空格 + 换行（或反斜杠）进标记层；换行本身由 <br> 承担
      return options.offsets
        ? `<span${at}${ownAttrs(options)}>${markSpan(source, span.from, span.to, options)}<br></span>`
        : "<br>";
    default:
      return "";
  }
}

/** 可见标签区间 + 两侧源码标记：`[`+标签+`](地址)`（wiki 为 `[[`+标签+`]]`）。
 *  标签区间按源文本取（解析层 trim 过标签、空标签会回退成地址，标称长度不可靠）。 */
function labelWithMarks(
  span: { from: number; to: number },
  labelFrom: number,
  labelTo: number,
  label: string,
  options: EditRenderOptions,
  source: string,
): string {
  return (
    markSpan(source, span.from, labelFrom, options) +
    spliceComposition(labelFrom, labelTo, label, options) +
    markSpan(source, labelTo, span.to, options)
  );
}

function renderLink(span: Extract<InlineSpan, { kind: "link" }>, source: string, options: EditRenderOptions): string {
  const at = offsetAttrs(options, span.from, span.to);
  const href = escapeHtml(span.href);
  const tip = escapeHtml(span.title ?? span.href);
  const labelFrom = span.from + 1;
  const inner = labelWithMarks(span, labelFrom, labelFrom + span.label.length, span.label, options, source);
  switch (span.form) {
    case "external":
      return `<span class="md-editor-link"${at}${ownAttrs(options)} data-md-href="${href}" title="${tip}">${inner}</span>`;
    case "path":
      return `<span class="md-editor-internal-link"${at}${ownAttrs(options)} data-md-href="${href}" title="${tip}">${inner}</span>`;
    case "create":
      return `<span class="md-editor-internal-link md-editor-internal-link-missing"${at}${ownAttrs(options)} data-md-href="${href}" title="${tip}">${inner}</span>`;
    case "wiki":
      return `<span class="md-editor-internal-link"${at}${ownAttrs(options)} data-md-wiki="${href}">${inner}</span>`;
    default:
      // 未识别形态：保持原文（编辑面整段作可见文本，只读面原样输出）
      return options.offsets
        ? `<span${at}>${textSpanHtml(span.from, span.to, source.slice(span.from, span.to), options)}</span>`
        : escapeHtml(source.slice(span.from, span.to));
  }
}

/** wiki 链接：`[[目标|标签]]` —— 标签之外的源码（`[[`、`目标|`、`]]`）都进标记层。 */
function renderWiki(span: Extract<InlineSpan, { kind: "wiki" }>, source: string, options: EditRenderOptions): string {
  const at = offsetAttrs(options, span.from, span.to);
  // 标签区间按源文本反推（解析层对目标/标签做过 trim，标称长度可能短于源码实长）；
  // 可见文本取源切片，区间长度与文本长度严格相等，映射不会错位
  const labelTo = Math.max(span.from + 2, span.to - 2);
  const bar = source.slice(span.from + 2, labelTo).lastIndexOf("|");
  const labelFrom = Math.max(span.from + 2, bar >= 0 ? span.from + 3 + bar : span.from + 2);
  const label = source.slice(labelFrom, labelTo);
  return (
    `<span class="md-editor-internal-link"${at}${ownAttrs(options)} data-md-wiki="${escapeHtml(span.target)}">` +
    labelWithMarks(span, labelFrom, labelTo, label, options, source) +
    `</span>`
  );
}

function renderImage(span: Extract<InlineSpan, { kind: "image" }>, source: string, options: EditRenderOptions): string {
  const attrs = [
    `data-md-src="${escapeHtml(span.src)}"`,
    `data-md-alt="${escapeHtml(span.alt)}"`,
    `title="${escapeHtml(span.title || span.alt || span.src)}"`,
  ];
  if (options.offsets) {
    attrs.unshift(`data-md-from="${span.from}"`, `data-md-to="${span.to}"`, "data-md-own");
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
    // `![替代](地址)` 整段进标记层（图片是渲染件，源文本只在揭示时显形）
    markSpan(source, span.from, span.to, options) +
    yielded(
      `<img alt="${escapeHtml(span.alt || span.src)}" loading="lazy" draggable="false"${srcAttr}>`,
      options,
    ) +
    `</span>`
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