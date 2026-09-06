/**
 * 统一 Markdown 渲染引擎的 widget 类（装饰层产物，纯视觉 + 受控交互）。
 *
 * 安全：所有 widget 只出 class + textContent / 已清洗 HTML（HtmlWidget 经
 * sanitizeHtmlFragment），从不注入未清洗 HTML；链接点击经 shell 打开系统程序，webview 不导航。
 *
 * 供 markdownDecorations.ts 构建装饰、MarkdownEditor/MarkdownView 装配视图使用。
 */
import { WidgetType } from "@codemirror/view";
import katex from "katex";
import { EXTERNAL_LINK_RE, decodeLinkHref } from "@/utils/markdown";
import { sanitizeHtmlFragment } from "@/utils/htmlSanitize";

// ===== 共享类型 =====

export interface RangeInfo {
  from: number;
  to: number;
}

/** 装饰构建选项：widget 回调与渲染模式（MarkdownEditor/MarkdownView 注入）。 */
export interface DecorationOptions {
  vaultRoot: string | null;
  /** 只读：停用「光标行/光标内显示原文」规则（无编辑意图，widget 恒渲染）。 */
  readOnly: boolean;
  /** 任务勾选框是否可点（笔记可点写回；画布/对话只读面禁用态展示）。 */
  interactiveCheckbox: boolean;
  onOpenUrl: (url: string) => void;
  onOpenPath: (path: string) => void;
  readImage: (src: string) => Promise<string | null>;
  isVaultPathNote?: (href: string) => boolean;
  onOpenVaultPathNote?: (href: string) => void;
  onCreateNote?: (name: string) => void;
  /** wiki 链接打开笔记（画布不可定位时 / 笔记编辑器无定位能力时）。 */
  onOpenNote?: (name: string) => void;
  isLocatable?: (value: string) => boolean;
  onLocate?: (value: string) => void;
  /** @引用 胶囊（用户消息 displayContent 内的 `@label` → 胶囊，点击定位/打开）。 */
  mentions?: { key: string; label: string }[];
  onMentionClick?: (key: string, label: string) => void;
  /** 可编辑态点击块级 widget 撕源码后聚焦视图（使光标可见）。 */
  focusEditor?: () => void;
}

// ===== 共享检测工具 =====

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

/** `[text](url)` / `![alt](url)`（含可选 title）解析；不匹配返回 null（保持原文）。 */
export function parseBracketLink(text: string): { label: string; url: string } | null {
  const m =
    /^\[([^\]]*)\]\(([^)\s]+)(?:\s+["'`][^"'`]*["'`])?\)$/.exec(text) ||
    /^\[([^\]]*)\]\(([^)]+)\)$/.exec(text);
  if (!m) return null;
  return { label: m[1] ?? "", url: m[2]?.trim() ?? "" };
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

// ===== KaTeX 渲染（缓存 + 错误回显源码）=====

const katexCache = new Map<string, string>();

function renderKatex(text: string, display: boolean): string {
  const key = `${display ? "d" : "i"}:${text}`;
  const hit = katexCache.get(key);
  if (hit !== undefined) return hit;
  let html: string;
  try {
    html = katex.renderToString(text, { displayMode: display, throwOnError: false, strict: false });
  } catch {
    html = "";
  }
  if (katexCache.size >= 300) katexCache.clear();
  katexCache.set(key, html);
  return html;
}

// ===== 图片异步加载取消器（widget 销毁时中断，防卸载后脏更新）=====

const imageLoadCancels = new WeakMap<HTMLElement, () => void>();

/** widget 左键点击统一拦截：仅左键、可选跳过谓词（HtmlWidget 交互元素区放行），
 * preventDefault+stopPropagation 后执行回调（阻止 CM 内容层把点击当选区移动）。 */
function onLeftClick(el: HTMLElement, run: () => void, skip?: (e: MouseEvent) => boolean) {
  el.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    if (skip?.(e)) return;
    e.preventDefault();
    e.stopPropagation();
    run();
  });
}

// ===== Widgets =====

export type LinkKind = "external" | "wiki" | "path" | "create";

/** 链接 widget：样式化可点击；光标进入链接范围后装饰失效、显示原文。 */
export class LinkWidget extends WidgetType {
  constructor(
    private readonly text: string,
    private readonly url: string,
    private readonly kind: LinkKind,
    private readonly onClick: () => void,
  ) {
    super();
  }

  eq(other: LinkWidget) {
    return other.text === this.text && other.url === this.url && other.kind === this.kind;
  }

  toDOM() {
    const span = document.createElement("span");
    span.textContent = this.text;
    span.title = this.url;
    if (this.kind === "external") {
      span.className = "md-editor-link";
    } else {
      span.className =
        this.kind === "create"
          ? "md-editor-internal-link md-editor-internal-link-missing"
          : "md-editor-internal-link";
    }
    // 仅左键打开：右键由正文右键菜单接管，不能同时拉起系统浏览器
    onLeftClick(span, () => this.onClick());
    return span;
  }
}

/** 图片 widget：相对路径经 Rust 读 dataURL 异步加载；点击用系统默认程序打开原文件。 */
export class ImageWidget extends WidgetType {
  constructor(
    private readonly src: string,
    private readonly alt: string,
    private readonly width: string | null,
    private readonly height: string | null,
    private readonly readImage: (src: string) => Promise<string | null>,
    private readonly openPath: (path: string) => void,
    private readonly vaultRoot: string | null,
  ) {
    super();
  }

  eq(other: ImageWidget) {
    return (
      other.src === this.src &&
      other.alt === this.alt &&
      other.width === this.width &&
      other.height === this.height
    );
  }

  toDOM() {
    const box = document.createElement("span");
    box.className = "md-editor-image";
    box.title = this.src;
    const img = document.createElement("img");
    img.alt = this.alt || this.src;
    img.loading = "lazy";
    img.draggable = false;
    if (this.width) img.style.width = `${this.width}px`;
    if (this.height) img.style.height = `${this.height}px`;
    // 点击打开原文件（系统默认程序）；绝对路径 = 仓库根 + 相对路径（Windows 兼容混合分隔符）。
    // 仅干净的仓库相对路径可点击：`..`/绝对路径/盘符或协议前缀一律不挂点击，防 shell 打开仓库外文件
    if (this.vaultRoot && isSafeVaultRelPath(this.src)) {
      const absolute = this.vaultRoot.replace(/[\\/]+$/, "") + "/" + this.src;
      onLeftClick(box, () => this.openPath(absolute));
    }
    let cancelled = false;
    imageLoadCancels.set(box, () => {
      cancelled = true;
    });
    if (/^https?:/i.test(this.src)) {
      img.src = this.src;
    } else {
      void this.readImage(this.src).then((dataUrl) => {
        if (cancelled) return;
        if (!dataUrl) {
          // 加载失败：降级回显原文（灰显），不显示破图
          box.classList.add("md-editor-image-missing");
          box.textContent = `![${this.alt}](${this.src})`;
          return;
        }
        img.src = dataUrl;
      });
    }
    box.appendChild(img);
    return box;
  }

  destroy(box: HTMLElement) {
    imageLoadCancels.get(box)?.();
    imageLoadCancels.delete(box);
  }
}

/** 任务列表复选框 widget：点击在 `[ ]`/`[x]` 之间切换（纯文本替换，零改写风险）；
 *  toggle 为空 = 禁用态展示（画布/对话只读面）。 */
export class CheckboxWidget extends WidgetType {
  constructor(
    private readonly checked: boolean,
    private readonly toggle: (() => void) | null,
  ) {
    super();
  }

  eq(other: CheckboxWidget) {
    return other.checked === this.checked && !!other.toggle === !!this.toggle;
  }

  toDOM() {
    const input = document.createElement("input");
    input.type = "checkbox";
    input.className = "md-editor-checkbox";
    input.checked = this.checked;
    if (!this.toggle) {
      input.disabled = true;
    } else {
      input.addEventListener("mousedown", (e) => e.stopPropagation());
      input.addEventListener("change", () => this.toggle?.());
    }
    return input;
  }
}

/** 内联标签 `#tag` widget：胶囊样式（与属性徽章同一视觉），纯展示不可点。 */
export class TagWidget extends WidgetType {
  constructor(private readonly tag: string) {
    super();
  }

  eq(other: TagWidget) {
    return other.tag === this.tag;
  }

  toDOM() {
    const span = document.createElement("span");
    span.className = "md-editor-tag";
    span.textContent = `#${this.tag}`;
    return span;
  }
}

/** 列表标记 widget：无序列表小圆点 / 有序列表序号（替代被隐藏的 `- `、`1. ` 标记）。 */
export class ListMarkerWidget extends WidgetType {
  constructor(private readonly text: string) {
    super();
  }

  eq(other: ListMarkerWidget) {
    return other.text === this.text;
  }

  toDOM() {
    const span = document.createElement("span");
    span.className = "md-editor-list-marker";
    span.textContent = this.text;
    return span;
  }
}

/** 横隔条 block widget：`---` 整行替换为水平线（光标所在行保持原文）。 */
export class DividerWidget extends WidgetType {
  eq() {
    return true;
  }

  toDOM() {
    const div = document.createElement("div");
    div.className = "md-editor-divider";
    return div;
  }
}

/** 数学 widget：行内/块级 KaTeX 渲染；解析失败回显源码（灰显）；
 *  点击（onEdit 非空时）把光标送入源码范围 → 可编辑态撕掉 widget 露源码。 */
export class MathWidget extends WidgetType {
  constructor(
    private readonly text: string,
    private readonly display: boolean,
    private readonly onEdit: (() => void) | null,
  ) {
    super();
  }

  eq(other: MathWidget) {
    return (
      other.text === this.text &&
      other.display === this.display &&
      !!other.onEdit === !!this.onEdit
    );
  }

  toDOM() {
    // 显式 HTMLElement（非 div|span 联合）：让 addEventListener 解析到具体事件过载（MouseEvent）
    const el: HTMLElement = document.createElement(this.display ? "div" : "span");
    el.className = this.display ? "md-editor-math md-editor-math-block" : "md-editor-math";
    const html = renderKatex(this.text, this.display);
    if (!html) {
      el.classList.add("md-editor-math-error");
      el.textContent = this.display ? `$$\n${this.text}\n$$` : `$${this.text}$`;
      return el;
    }
    el.innerHTML = html;
    if (this.onEdit) {
      onLeftClick(el, () => this.onEdit?.());
    }
    return el;
  }
}

/** callout 类型徽章 widget：替换 `> [!type]` 标记（块级着色由行装饰 md-callout 提供）。 */
export class CalloutBadgeWidget extends WidgetType {
  constructor(private readonly type: string) {
    super();
  }

  eq(other: CalloutBadgeWidget) {
    return other.type === this.type;
  }

  toDOM() {
    const span = document.createElement("span");
    span.className = "md-editor-callout-badge";
    span.textContent = this.type;
    return span;
  }
}

/** 表格 block widget：源码整表替换为 `<table>`；单元格内容取原文（含 markdown 标记，不渲染行内格式）；
 * 点击（onEdit）把光标送入表格源码起点 → 可编辑态撕掉 widget 露源码。
 * eq 必须覆盖 onEdit 有无：CM 按 eq 决定是否复用旧 DOM，漏比会在只读↔编辑翻转后留下无监听器的旧表。 */
export class TableWidget extends WidgetType {
  constructor(
    private readonly source: string,
    private readonly onEdit: (() => void) | null,
  ) {
    super();
  }

  eq(other: TableWidget) {
    return other.source === this.source && !!other.onEdit === !!this.onEdit;
  }

  toDOM() {
    const table = document.createElement("table");
    table.className = "md-editor-table";
    const lines = this.source.split("\n").filter((l) => l.trim() !== "");
    if (lines.length < 2) {
      table.textContent = this.source;
      return table;
    }
    const splitRow = (l: string) => l.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((c) => c.trim());
    const header = splitRow(lines[0] ?? "");
    const aligns = splitRow(lines[1] ?? "").map((c) =>
      c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : "",
    );
    const colCount = Math.max(header.length, aligns.length);
    const thead = table.createTHead();
    const headRow = document.createElement("tr");
    for (let i = 0; i < colCount; i++) {
      const th = document.createElement("th");
      th.textContent = header[i] ?? "";
      if (aligns[i]) th.style.textAlign = aligns[i];
      headRow.appendChild(th);
    }
    thead.appendChild(headRow);
    const tbody = document.createElement("tbody");
    for (const line of lines.slice(2)) {
      const cells = splitRow(line);
      const tr = document.createElement("tr");
      for (let i = 0; i < colCount; i++) {
        const td = document.createElement("td");
        td.textContent = cells[i] ?? "";
        if (aligns[i]) td.style.textAlign = aligns[i];
        tr.appendChild(td);
      }
      tbody.appendChild(tr);
    }
    table.appendChild(tbody);
    if (this.onEdit) {
      onLeftClick(table, () => this.onEdit?.());
    }
    return table;
  }
}

/** raw HTML widget：内容 = DOMPurify 白名单清洗结果（唯一 HTML 注入点）；
 *  链接点击拦截 + 相对路径图片经 Rust 读 dataURL；点击（onEdit）送光标进源码 → 可编辑态撕 widget。
 *  eq 须覆盖 onEdit 有无（同 TableWidget：防翻转后复用无监听器旧 DOM）。 */
export class HtmlWidget extends WidgetType {
  constructor(
    private readonly html: string,
    private readonly block: boolean,
    private readonly opts: DecorationOptions,
    private readonly onEdit: (() => void) | null,
  ) {
    super();
  }

  eq(other: HtmlWidget) {
    return (
      other.html === this.html &&
      other.block === this.block &&
      !!other.onEdit === !!this.onEdit
    );
  }

  toDOM() {
    // 显式 HTMLElement（非 div|span 联合）：让 addEventListener 解析到具体事件过载（MouseEvent）
    const el: HTMLElement = document.createElement(this.block ? "div" : "span");
    el.className = "md-editor-html";
    el.innerHTML = sanitizeHtmlFragment(this.html);
    // 链接点击拦截：外链走系统浏览器、仓库路径打开笔记，webview 不导航
    el.addEventListener("click", (e) => {
      const target = e.target as Element | null;
      const a = target?.closest?.("a[href]");
      if (!a) return;
      const href = a.getAttribute("href");
      if (!href) return;
      e.preventDefault();
      e.stopPropagation();
      const url = decodeLinkHref(href);
      if (EXTERNAL_LINK_RE.test(url)) this.opts.onOpenUrl(url);
      else if (this.opts.isVaultPathNote?.(url) && this.opts.onOpenVaultPathNote)
        this.opts.onOpenVaultPathNote(url);
    });
    // 相对路径图片 → Rust 读 dataURL（外链 https/data/blob 直出）
    for (const img of Array.from(el.querySelectorAll("img[src]"))) {
      const src = img.getAttribute("src") ?? "";
      if (/^(https?:|data:|blob:)/i.test(src)) continue;
      if (!isSafeVaultRelPath(src)) {
        img.removeAttribute("src");
        continue;
      }
      void this.opts.readImage(src).then((dataUrl) => {
        if (dataUrl) img.setAttribute("src", dataUrl);
      });
    }
    if (this.onEdit) {
      // 块内链接/交互元素不拦截（链接点击经 click 处理）；仅空白/其余区域送光标
      onLeftClick(
        el,
        () => this.onEdit?.(),
        (e) => !!(e.target as Element | null)?.closest?.("a, button, input, summary"),
      );
    }
    return el;
  }
}

/** 脚注引用 widget：`[^label]` → 上标标签（不做跳转导航）。 */
export class FootnoteRefWidget extends WidgetType {
  constructor(private readonly label: string) {
    super();
  }

  eq(other: FootnoteRefWidget) {
    return other.label === this.label;
  }

  toDOM() {
    const sup = document.createElement("sup");
    sup.className = "md-editor-footnote-ref";
    sup.textContent = this.label;
    return sup;
  }
}

/** 脚注定义 block widget：`[^label]: text` 整行替换为定义块（内容为原文，不渲染行内 markdown）；
 *  点击（onEdit）送光标进源码 → 可编辑态撕 widget。eq 须覆盖 onEdit 有无（同 TableWidget）。 */
export class FootnoteDefWidget extends WidgetType {
  constructor(
    private readonly label: string,
    private readonly text: string,
    private readonly onEdit: (() => void) | null,
  ) {
    super();
  }

  eq(other: FootnoteDefWidget) {
    return (
      other.label === this.label &&
      other.text === this.text &&
      !!other.onEdit === !!this.onEdit
    );
  }

  toDOM() {
    const div = document.createElement("div");
    div.className = "md-editor-footnote-def";
    const sup = document.createElement("sup");
    sup.textContent = this.label;
    div.appendChild(sup);
    const span = document.createElement("span");
    span.textContent = this.text;
    div.appendChild(span);
    if (this.onEdit) {
      onLeftClick(div, () => this.onEdit?.());
    }
    return div;
  }
}

/** 围栏代码块 widget（只读面）：整块替换为盒装 `<pre><code>` + 右上角复制按钮，``` 标记不显示。 */
export class CodeBlockWidget extends WidgetType {
  constructor(private readonly code: string) {
    super();
  }

  eq(other: CodeBlockWidget) {
    return other.code === this.code;
  }

  toDOM() {
    const box = document.createElement("div");
    box.className = "md-editor-code-block";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "md-editor-code-copy";
    btn.textContent = "复制";
    btn.title = "复制代码";
    let timer: number | undefined;
    btn.addEventListener("mousedown", (e) => e.stopPropagation());
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      void navigator.clipboard
        .writeText(this.code)
        .then(() => {
          btn.textContent = "已复制";
          window.clearTimeout(timer);
          timer = window.setTimeout(() => {
            btn.textContent = "复制";
          }, 1500);
        })
        .catch(() => {});
    });
    const pre = document.createElement("pre");
    const code = document.createElement("code");
    code.textContent = this.code;
    pre.appendChild(code);
    box.appendChild(btn);
    box.appendChild(pre);
    return box;
  }
}

/** @引用 胶囊 widget（用户消息内 `@label` → 胶囊，点击定位节点/打开笔记）。 */
export class MentionWidget extends WidgetType {
  constructor(
    private readonly key: string,
    private readonly label: string,
    private readonly onClick: (key: string, label: string) => void,
  ) {
    super();
  }

  eq(other: MentionWidget) {
    return other.key === this.key && other.label === this.label;
  }

  toDOM() {
    const span = document.createElement("span");
    span.className = "mention-capsule";
    span.textContent = `@${this.label}`;
    onLeftClick(span, () => this.onClick(this.key, this.label));
    return span;
  }
}
