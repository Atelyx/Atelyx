/**
 * 统一 Markdown 渲染引擎的装饰构建（纯函数：文档 + 选区 → DecorationSet）。
 *
 * 覆盖语法（lezer 树 + 正则补充，GFM 已启用）：
 * - 结构：标题（字号/行装饰）、引用块与 callout（行装饰 + 徽章）、列表（缩进 + 标记）、
 *   围栏代码（保持源码 + 高亮）、横隔条、任务勾选框、表格（块 widget）、脚注（引用/定义）。
 * - 行内：粗斜删/行内代码（标记隐藏）、链接（外链/wiki/仓库路径/空链接新建）、图片、
 *   `[[wiki]]`、`#标签`、`==高亮==`、`%%注释%%`、行内/块级数学、raw HTML（块/行内）。
 *
 * 渲染模式：可编辑态沿用「光标所在行/行内元素显示原文」（写作时看源码）；只读态
 * （opts.readOnly）停用该规则，widget 恒渲染。
 *
 * 安全：widget 只出 class + textContent / 已清洗 HTML（HtmlWidget），详见 markdownWidgets.tsx。
 */
import { RangeSetBuilder, type EditorState, type Line, type StateEffectType, type Text, type Transaction, type TransactionSpec } from "@codemirror/state";
import { Decoration, type DecorationSet, type EditorView } from "@codemirror/view";
import { syntaxTree } from "@codemirror/language";
import {
  type DecorationOptions,
  type RangeInfo,
  type BlockEditHandler,
  LinkWidget,
  ImageWidget,
  CheckboxWidget,
  TagWidget,
  ListMarkerWidget,
  DividerWidget,
  MathWidget,
  CalloutBadgeWidget,
  TableWidget,
  HtmlWidget,
  FootnoteRefWidget,
  FootnoteDefWidget,
  MentionWidget,
  CodeBlockWidget,
  inlineTagRanges,
  inlineMathRanges,
  blockMathRanges,
  parseBracketLink,
} from "./markdownWidgets";
import { EXTERNAL_LINK_RE } from "@/utils/markdown";

// ===== 语法节点名（lezer-markdown，GFM 已启用）=====

/** 行内容器节点（Emphasis 等标记容器 + Link/Image 整体替换）。 */
const INLINE_CONTAINER_NAMES = new Set([
  "Emphasis",
  "StrongEmphasis",
  "InlineCode",
  "Strikethrough",
  "Link",
  "Image",
  "Autolink",
]);
/** 语法标记叶节点（容器边界 mark + 块级标记；围栏代码的 CodeMark 不在此列，保持原文）。 */
const MARK_NAMES = new Set([
  "HeaderMark",
  "QuoteMark",
  "ListMark",
  "EmphasisMark",
  "CodeMark",
  "LinkMark",
  "StrikethroughMark",
]);
/** 块级标记：按「光标所在行不隐藏」规则处理（其余 mark 由容器逻辑决定）。 */
const BLOCK_MARKS = new Set(["HeaderMark", "QuoteMark", "ListMark"]);

interface MarkInfo extends RangeInfo {
  name: string;
}

interface TaskMarkerInfo extends RangeInfo {
  marker: string;
}

interface ContainerInfo extends RangeInfo {
  name: string;
  marks: RangeInfo[];
}

const rangeKey = (r: RangeInfo) => `${r.from}:${r.to}`;

/** 围栏代码块源码 → 代码内容（去首行 ```lang 与末行 ``` 围栏）。 */
function fencedCodeContent(src: string): string {
  const lines = src.split("\n");
  if (lines.length >= 2) lines.shift();
  if (lines.length > 0 && /^```/.test(lines[lines.length - 1] ?? "")) lines.pop();
  return lines.join("\n");
}

/** 点击纵向比例 → 块源码对应行起点（纯函数，可单测）：
 *  frac 0..1 映射到块首行..末行（末行含行尾时按行数钳制），供块级 widget 点击定位。 */
export function sourceLineAtFraction(state: EditorState, from: number, to: number, frac: number): number {
  const doc = state.doc;
  const firstLine = doc.lineAt(from).number;
  const lastLine = doc.lineAt(Math.max(to - 1, from)).number;
  const lineCount = lastLine - firstLine + 1;
  const lineIndex = Math.min(lineCount - 1, Math.max(0, Math.floor(frac * lineCount)));
  return doc.line(firstLine + lineIndex).from;
}

/** 块级点击目标行起点（纯函数，可单测）：以「块当前区间边缘」反推首行。
 *  块 widget 经 posAtCoords 返回其当前区间 from（点击上半）/ to（点击下半）——
 *  高度图实时，因此落点始终基于当前文档，不随构建期闭包过期。
 *  lineCount 在构建期捕获（eq 复用 = 源码未变 → 行数稳定；不能从过期坐标重算，会随上方插入偏移）。 */
export function blockLineAtEdge(doc: Text, frac: number, edgePos: number, lineCount: number): number {
  const firstLine =
    frac < 0.5
      ? doc.lineAt(edgePos).number
      : doc.lineAt(Math.max(edgePos - 1, 0)).number - lineCount + 1;
  const lineIndex = Math.min(lineCount - 1, Math.max(0, Math.floor(frac * lineCount)));
  return doc.line(firstLine + lineIndex).from;
}

/** 行内任务标记区间 `- [ ]` / `1. [x]`（纯函数，可单测）：返回标记整体（含标记符与勾选态）。
 *  供勾选框 toggle 以「勾选框 DOM 当前行」重扫定位——不依赖构建期坐标。 */
export function taskMarkerRange(lineText: string, lineFrom: number): RangeInfo | null {
  const m = /([-+*]|\d+[.)])\s*\[( |x)\]/.exec(lineText);
  if (!m) return null;
  const from = lineFrom + m.index;
  return { from, to: from + m[0].length };
}

export function buildDecorations(
  state: EditorState,
  opts: DecorationOptions,
  dispatch: (spec: TransactionSpec) => void,
): DecorationSet {
  const doc = state.doc;
  const sel = state.selection.main;
  /** 可编辑态才应用「光标行/光标内显示原文」；只读态 widget 恒渲染。 */
  const editing = !opts.readOnly;
  const cursorLine = editing ? doc.lineAt(sel.head).number : -1;
  const tree = syntaxTree(state);
  const builder = new RangeSetBuilder<Decoration>();

  const marks: MarkInfo[] = [];
  const containers: ContainerInfo[] = [];
  const taskMarkers: TaskMarkerInfo[] = [];
  const dividerLines: RangeInfo[] = [];
  /** 正则检测（标签/wiki/高亮/注释/数学/脚注）跳过区：代码/链接/图片/HTML。 */
  const opaque: RangeInfo[] = [];
  const htmlBlocks: RangeInfo[] = [];
  const tableRanges: RangeInfo[] = [];
  const fencedCodes: RangeInfo[] = [];
  const headingLines = new Map<number, number>();
  const blockquotes: RangeInfo[] = [];
  const callouts: { from: number; to: number; type: string; markerFrom: number; markerTo: number }[] = [];
  const listItems: { from: number; to: number; depth: number }[] = [];

  let listDepth = 0;
  /** 段落 → 段内 HTMLTag 聚合区间（仅段落直系 HTMLTag）。 */
  const htmlTagPar = new Map<number, RangeInfo>();

  tree.iterate({
    enter: (node) => {
      const name = node.type.name;
      if (MARK_NAMES.has(name)) {
        marks.push({ from: node.from, to: node.to, name });
      } else if (INLINE_CONTAINER_NAMES.has(name)) {
        // 行内代码/链接/图片：正则检测在其中保持原文
        if (name === "InlineCode" || name === "Link" || name === "Autolink" || name === "Image") {
          opaque.push({ from: node.from, to: node.to });
        }
        const childMarks: RangeInfo[] = [];
        for (let child = node.node?.firstChild; child; child = child.nextSibling) {
          if (MARK_NAMES.has(child.type.name)) {
            childMarks.push({ from: child.from, to: child.to });
          }
        }
        containers.push({ from: node.from, to: node.to, name, marks: childMarks });
      } else if (name === "FencedCode") {
        opaque.push({ from: node.from, to: node.to });
        fencedCodes.push({ from: node.from, to: node.to });
      } else if (name === "IndentedCode") {
        opaque.push({ from: node.from, to: node.to });
      } else if (name === "TaskMarker") {
        let listItem = node.node?.parent;
        while (listItem && listItem.type.name !== "ListItem") listItem = listItem.parent;
        const listMark = listItem?.getChild("ListMark");
        if (listMark) {
          taskMarkers.push({
            from: listMark.from,
            to: node.to,
            marker: doc.sliceString(listMark.from, listMark.to),
          });
        }
      } else if (name === "HorizontalRule") {
        const line = doc.lineAt(node.from);
        dividerLines.push({ from: line.from, to: line.to });
      } else if (name === "HTMLBlock") {
        htmlBlocks.push({ from: node.from, to: node.to });
        opaque.push({ from: node.from, to: node.to });
      } else if (name === "HTMLTag") {
        // 仅段落直系 HTMLTag 参与行内 HTML 区间（嵌套在链接/强调/行内代码内保持源码）
        const parent = node.node?.parent;
        if (parent?.type.name === "Paragraph") {
          const cur = htmlTagPar.get(parent.from);
          htmlTagPar.set(
            parent.from,
            cur
              ? { from: Math.min(cur.from, node.from), to: Math.max(cur.to, node.to) }
              : { from: node.from, to: node.to },
          );
        }
      } else if (name === "Table") {
        tableRanges.push({ from: node.from, to: node.to });
      } else if (/^ATXHeading[1-6]$/.test(name)) {
        headingLines.set(doc.lineAt(node.from).number, Number(name.slice(-1)));
      } else if (/^SetextHeading[12]$/.test(name)) {
        headingLines.set(doc.lineAt(node.from).number, Number(name.slice(-1)));
      } else if (name === "Blockquote") {
        blockquotes.push({ from: node.from, to: node.to });
        // callout 检测：引用块首行 `> [!type]`
        const line = doc.lineAt(node.from);
        const m = /^[ \t]*>[ \t]*\[!([a-z][a-z0-9-]*)\]([+-])?\s*/i.exec(line.text);
        if (m) {
          const lb = line.text.indexOf("[");
          const rb = line.text.indexOf("]", lb);
          if (lb >= 0 && rb >= 0) {
            callouts.push({
              from: node.from,
              to: node.to,
              type: (m[1] ?? "").toLowerCase(),
              markerFrom: line.from + lb,
              markerTo: line.from + rb + 1,
            });
          }
        }
      } else if (name === "ListItem") {
        listDepth++;
        listItems.push({ from: node.from, to: node.to, depth: listDepth });
      }
    },
    leave: (node) => {
      if (node.type.name === "ListItem") listDepth--;
    },
  });

  // 行内 HTML 区间：段落内 HTMLTag 聚合范围；与任何容器（强调/链接/行内代码等）重叠则保持源码
  const htmlInline: RangeInfo[] = [];
  for (const range of htmlTagPar.values()) {
    if (containers.some((c) => range.from < c.to && range.to > c.from)) continue;
    htmlInline.push(range);
    opaque.push(range);
  }

  const widgetEntries: { from: number; to: number; dec: Decoration }[] = [];
  const widgetBounds: RangeInfo[] = [];
  const marksToHide = new Set<string>();
  const marksToShow = new Set<string>();

  /** 已覆盖区间（widget）检测：重叠即跳过（防双渲染/装饰冲突）。 */
  const overlapsWidgets = (from: number, to: number) =>
    widgetBounds.some((w) => from < w.to && to > w.from);
  /** 完全落在 opaque（代码/链接/图片/HTML）内 → 正则检测跳过。 */
  const coveredByOpaque = (from: number, to: number) =>
    opaque.some((o) => from >= o.from && to <= o.to);
  const isCursorLine = (n: number) => editing && n === cursorLine;
  /** 行内 widget（单行：行内 HTML/数学/脚注定义）点击 → 光标入源码起点 + 聚焦；只读态 null。
   *  单行无高度差，无需点击比例映射（光标行规则随即撕开 widget 显示原文）。
   *  落点 = posAtCoords 实时位置（构建期捕获坐标会随文档变更过期，同块级处理思路）。 */
  const editInlineAt = (from: number): BlockEditHandler =>
    editing
      ? (e, _el, view) => {
          const pos = view.posAtCoords({ x: e.clientX, y: e.clientY }, false);
          dispatch({ selection: { anchor: pos ?? from }, scrollIntoView: true });
          opts.focusEditor?.();
        }
      : null;
  /** 可编辑态点击块级 widget → 光标落到点击位置对应的源码行（撕 widget 露源码）+ 聚焦；
   *  只读态 null。整块渲染高度按比例映射源码行：点击中间行落中间行、点末行落末行。
   *  构建期捕获的 from/to 会随文档变更过期（CM 按 eq 复用旧 widget 实例、旧闭包），
   *  落点由实时 posAtCoords 边缘 + 构建期捕获的行数（源码未变则稳定）推导，基于当前文档。 */
  const editBlockAt = (from: number, to: number): BlockEditHandler => {
    const lineCount = doc.lineAt(Math.max(to - 1, from)).number - doc.lineAt(from).number + 1;
    return editing
      ? (e, el, view) => {
          const rect = el.getBoundingClientRect();
          const frac = rect.height > 0 ? (e.clientY - rect.top) / rect.height : 0;
          const clickPos = view.posAtCoords({ x: e.clientX, y: e.clientY }, false);
          const target =
            clickPos === null
              ? sourceLineAtFraction(view.state, from, to, frac) // 兜底：posAtCoords 不可得（罕见）
              : blockLineAtEdge(view.state.doc, frac, clickPos, lineCount);
          dispatch({ selection: { anchor: target }, scrollIntoView: true });
          opts.focusEditor?.();
        }
      : null;
  };
  /** 光标与区间相交（可编辑态才有效；只读恒 false = 恒渲染）——行内容器用，严格区间。 */
  const cursorInside = (from: number, to: number) => editing && sel.from < to && sel.to > from;
  /** block 装饰区间对齐整行边界。 */
  const lineSpan = (from: number, to: number): RangeInfo => ({
    from: doc.lineAt(from).from,
    to: doc.lineAt(Math.max(to - 1, from)).to,
  });
  /** 块级 widget 的「光标在整行区间内」判定：整行区间 + 末端闭合（光标在块末行行尾也不回包）。
   *  光标落在块整行区间内（含末行行尾）即保持源码展开，移出块（下一行起）才回包。 */
  const cursorInsideBlock = (from: number, to: number) => {
    if (!editing) return false;
    if (sel.from !== sel.to) return sel.from < to && sel.to > from;
    const span = lineSpan(from, to);
    return sel.head >= span.from && sel.head <= span.to;
  };
  /** widget 或 opaque 任一覆盖即跳过（行内正则检测统一守卫）。 */
  const coveredAll = (from: number, to: number) =>
    coveredByOpaque(from, to) || overlapsWidgets(from, to);
  /** 逐行扫描样板：跳过光标行（可编辑态光标行显示原文）。 */
  const forEachLine = (fn: (line: Line, lineNum: number) => void) => {
    for (let lineNum = 1; lineNum <= doc.lines; lineNum++) {
      if (isCursorLine(lineNum)) continue;
      fn(doc.line(lineNum), lineNum);
    }
  };
  /** widget 成对登记（entries + bounds）。 */
  const pushWidget = (from: number, to: number, dec: Decoration) => {
    widgetEntries.push({ from, to, dec });
    widgetBounds.push({ from, to });
  };

  // ===== 块级 widget（先行，供行内正则跳过检测）=====

  // 横隔条（`---` → 水平线 block widget；光标所在行保持原文）
  for (const d of dividerLines) {
    if (isCursorLine(doc.lineAt(d.from).number)) continue;
    pushWidget(d.from, d.to, Decoration.replace({ widget: new DividerWidget(), block: true }));
  }

  // 任务复选框（整段替换 `- [ ]` → 复选框；光标所在行保持原文；只读展示面禁用态）
  for (const t of taskMarkers) {
    if (isCursorLine(doc.lineAt(t.from).number)) continue;
    const checked = doc.sliceString(t.from, t.to).includes("[x]");
    const toggle = (view: EditorView, el: HTMLElement) => {
      // 构建期捕获的标记区间会随文档变更过期（CM 按 eq 复用旧 widget）——以勾选框 DOM
      // 当前所在行为锚重扫任务标记，勾选落点与勾选态都基于当前文档
      const line = view.state.doc.lineAt(view.posAtDOM(el));
      const range = taskMarkerRange(line.text, line.from);
      if (!range) return;
      const marker = view.state.sliceDoc(range.from, range.to);
      const nowChecked = marker.includes("[x]");
      dispatch({
        changes: {
          from: range.from,
          to: range.to,
          insert: marker.replace(/\[( |x)\]/, `[${nowChecked ? " " : "x"}]`),
        },
      });
    };
    pushWidget(
      t.from,
      t.to,
      Decoration.replace({
        widget: new CheckboxWidget(checked, opts.interactiveCheckbox ? toggle : null),
      }),
    );
  }

  // raw HTML 块（block widget；光标在整行区间内保持源码；点击 → 光标落到点击位置对应源码行）
  for (const h of htmlBlocks) {
    if (cursorInsideBlock(h.from, h.to)) continue;
    const span = lineSpan(h.from, h.to);
    pushWidget(
      span.from,
      span.to,
      Decoration.replace({
        widget: new HtmlWidget(doc.sliceString(h.from, h.to), true, opts, editBlockAt(h.from, h.to)),
        block: true,
      }),
    );
  }

  // 表格（block widget；光标在整行区间内保持源码；点击 → 光标落到点击位置对应源码行）
  for (const t of tableRanges) {
    if (cursorInsideBlock(t.from, t.to)) continue;
    const span = lineSpan(t.from, t.to);
    pushWidget(
      span.from,
      span.to,
      Decoration.replace({
        widget: new TableWidget(doc.sliceString(t.from, t.to), editBlockAt(t.from, t.to)),
        block: true,
      }),
    );
  }

  // 块级数学 `$$...$$`（block widget；光标在整行区间内保持源码；点击 → 光标落到点击位置对应源码行）
  const docText = doc.toString();
  for (const b of blockMathRanges(docText)) {
    if (cursorInsideBlock(b.from, b.to)) continue;
    if (coveredAll(b.from, b.to)) continue;
    const content = doc.sliceString(b.from, b.to);
    const inner = content.replace(/^\$\$/, "").replace(/\$\$$/, "").trim();
    if (!inner) continue;
    const span = lineSpan(b.from, b.to);
    pushWidget(span.from, span.to, Decoration.replace({ widget: new MathWidget(inner, true, editBlockAt(b.from, b.to)), block: true }));
  }

  // 脚注定义 `[^label]: text`（block widget 整行替换；光标所在行保持源码；点击 → 光标入源码起点）
  forEachLine((line) => {
    const m = /^\[\^([^\]]+)\]:\s*(.*)$/.exec(line.text);
    if (!m || !m[1]) return;
    if (overlapsWidgets(line.from, line.to)) return;
    pushWidget(
      line.from,
      line.to,
      Decoration.replace({
        widget: new FootnoteDefWidget(m[1] ?? "", m[2] ?? "", editInlineAt(line.from)),
        block: true,
      }),
    );
  });

  // 围栏代码块（只读面）：整块替换为盒装 `<pre><code>` + 右上角复制按钮，``` 标记不显示；
  // 可编辑态保持 CM 行渲染（文本即真相，围栏可见可改）
  if (opts.readOnly) {
    for (const f of fencedCodes) {
      if (overlapsWidgets(f.from, f.to)) continue;
      const span = lineSpan(f.from, f.to);
      pushWidget(
        span.from,
        span.to,
        Decoration.replace({
          widget: new CodeBlockWidget(fencedCodeContent(doc.sliceString(f.from, f.to))),
          block: true,
        }),
      );
    }
  }

  // callout 徽章（替换 `[!type]`；块级着色由行装饰 md-callout 提供）
  for (const c of callouts) {
    if (isCursorLine(doc.lineAt(c.markerFrom).number)) continue;
    pushWidget(c.markerFrom, c.markerTo, Decoration.replace({ widget: new CalloutBadgeWidget(c.type) }));
  }

  // ===== 行内 widget（容器 + 正则补充）=====

  // 行内容器：由内向外处理（Link/Image 整体替换为 widget；标记容器隐藏其边界 mark）
  containers.sort((a, b) => a.to - a.from - (b.to - b.from) || a.from - b.from);

  for (const c of containers) {
    if (cursorInside(c.from, c.to)) {
      for (const m of c.marks) marksToShow.add(rangeKey(m));
      continue;
    }
    if (c.name === "Link" || c.name === "Autolink") {
      const text = doc.sliceString(c.from, c.to);
      // `<https://x>` 角括号 Autolink：剥括号，url/label 用括号内内容
      const inner =
        text.length >= 2 && text[0] === "<" && text[text.length - 1] === ">"
          ? text.slice(1, -1)
          : text;
      const parsed = c.name === "Autolink" ? { label: inner, url: inner } : parseBracketLink(text);
      if (!parsed) {
        for (const m of c.marks) marksToShow.add(rangeKey(m));
        continue;
      }
      if (EXTERNAL_LINK_RE.test(parsed.url)) {
        // 外链 → 系统浏览器
        pushWidget(
          c.from,
          c.to,
          Decoration.replace({
            widget: new LinkWidget(parsed.label || parsed.url, parsed.url, "external", () =>
              opts.onOpenUrl(parsed.url),
            ),
          }),
        );
      } else if (parsed.url === "" && parsed.label && opts.onCreateNote) {
        // `[名]()` 空路径 = 快捷新建同名笔记（label 为空时不产出 widget，保持原文）
        pushWidget(
          c.from,
          c.to,
          Decoration.replace({
            widget: new LinkWidget(parsed.label, "", "create", () => opts.onCreateNote?.(parsed.label)),
          }),
        );
      } else if (opts.isVaultPathNote?.(parsed.url) && opts.onOpenVaultPathNote) {
        // 仓库相对路径命中笔记 → 内部链接打开
        pushWidget(
          c.from,
          c.to,
          Decoration.replace({
            widget: new LinkWidget(parsed.label || parsed.url, parsed.url, "path", () =>
              opts.onOpenVaultPathNote?.(parsed.url),
            ),
          }),
        );
      } else {
        // 非外链/非内部链接：标记保持原文，不隐藏
        for (const m of c.marks) marksToShow.add(rangeKey(m));
      }
    } else if (c.name === "Image") {
      // lezer Image 节点含前导 `!`（`![alt](url)`），parseBracketLink 以 `[` 锚定，解析前剥除
      const text = doc.sliceString(c.from, c.to);
      const parsed = parseBracketLink(text[0] === "!" ? text.slice(1) : text);
      if (!parsed || !parsed.url) {
        for (const m of c.marks) marksToShow.add(rangeKey(m));
        continue;
      }
      let alt = parsed.label;
      let width: string | null = null;
      let height: string | null = null;
      // `![alt|100x200]` 图片尺寸语法
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
      const dec = Decoration.replace({
        widget: new ImageWidget(parsed.url, alt, width, height, opts.readImage, opts.onOpenPath, opts.vaultRoot),
      });
      pushWidget(c.from, c.to, dec);
    } else {
      // Emphasis/StrongEmphasis/InlineCode/Strikethrough：隐藏其边界 mark
      for (const m of c.marks) marksToHide.add(rangeKey(m));
    }
  }

  // 行内 raw HTML（段内 HTMLTag 连续区间 → 清洗后 widget；光标在内保持源码）
  for (const h of htmlInline) {
    if (cursorInsideBlock(h.from, h.to)) continue;
    if (overlapsWidgets(h.from, h.to)) continue;
    pushWidget(h.from, h.to, Decoration.replace({
      widget: new HtmlWidget(doc.sliceString(h.from, h.to), false, opts, editInlineAt(h.from)),
    }));
  }

  // 行内数学 `$...$`（光标行显示原文；代码/链接/HTML 内保持源码；点击送光标 → 撕 widget）
  forEachLine((line) => {
    for (const r of inlineMathRanges(line.text, line.from)) {
      if (coveredAll(r.from, r.to)) continue;
      const content = doc.sliceString(r.from + 1, r.to - 1);
      pushWidget(r.from, r.to, Decoration.replace({ widget: new MathWidget(content, false, editInlineAt(r.from)) }));
    }
  });

  // `==高亮==`（隐藏定界符 + mark 内文）/ `%%注释%%`（整段隐藏；光标行显示原文）
  forEachLine((line) => {
    const eqRe = /==([^=\n]+)==/g;
    let m: RegExpExecArray | null;
    while ((m = eqRe.exec(line.text))) {
      const from = line.from + m.index;
      const to = from + m[0].length;
      if (coveredAll(from, to)) continue;
      widgetEntries.push({ from, to: from + 2, dec: Decoration.replace({}) });
      widgetEntries.push({ from: from + 2, to: to - 2, dec: Decoration.mark({ attributes: { class: "md-highlight" } }) });
      widgetEntries.push({ from: to - 2, to, dec: Decoration.replace({}) });
      widgetBounds.push({ from, to });
    }
    const pctRe = /%%([^%\n]+)%%/g;
    while ((m = pctRe.exec(line.text))) {
      const from = line.from + m.index;
      const to = from + m[0].length;
      if (coveredAll(from, to)) continue;
      pushWidget(from, to, Decoration.replace({}));
    }
  });

  // wiki 链接 `[[标题|别名]]`（lezer 不识别的语法，按行正则匹配；光标行显示原文；
  //  画布可定位 → 定位；否则打开笔记；无打开能力 → 纯展示）
  forEachLine((line) => {
    const re = /\[\[([^\]|]*?)(?:\|([^\]]*?))?\]\]/g;
    let match: RegExpExecArray | null;
    while ((match = re.exec(line.text))) {
      const target = (match[1] ?? "").trim();
      if (!target) continue;
      const from = line.from + match.index;
      const to = from + match[0].length;
      if (coveredAll(from, to)) continue;
      const label = (match[2] ?? target).trim() || target;
      if (opts.isLocatable && opts.isLocatable(target)) {
        pushWidget(from, to, Decoration.replace({
          widget: new LinkWidget(label, target, "wiki", () => opts.onLocate?.(target)),
        }));
      } else if (opts.onOpenNote) {
        pushWidget(from, to, Decoration.replace({
          widget: new LinkWidget(label, target, "wiki", () => opts.onOpenNote?.(target)),
        }));
      } else {
        pushWidget(from, to, Decoration.replace({
          widget: new LinkWidget(label, target, "wiki", () => {}),
        }));
      }
    }
  });

  // 内联标签 `#tag` → 胶囊 widget（光标行显示原文；代码/链接/HTML 范围保持原文——与预览/Rust 提取同语义）
  forEachLine((line) => {
    for (const r of inlineTagRanges(line.text, line.from)) {
      if (coveredAll(r.from, r.to)) continue;
      const tag = doc.sliceString(r.from + 1, r.to);
      pushWidget(r.from, r.to, Decoration.replace({ widget: new TagWidget(tag) }));
    }
  });

  // 脚注引用 `[^label]` → 上标（代码/链接/定义行内跳过）
  forEachLine((line) => {
    const re = /\[\^([^\]]+)\]/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(line.text))) {
      const from = line.from + m.index;
      const to = from + m[0].length;
      if (coveredAll(from, to)) continue;
      pushWidget(from, to, Decoration.replace({ widget: new FootnoteRefWidget(m[1] ?? "") }));
    }
  });

  // @引用 胶囊（用户消息 `@label` → 胶囊，最长匹配优先；画布定位/面板打开由 onMentionClick 决定）
  const mentions = opts.mentions;
  if (mentions && mentions.length > 0 && opts.onMentionClick) {
    forEachLine((line) => {
      let i = 0;
      while (i < line.text.length) {
        const at = line.text.indexOf("@", i);
        if (at === -1) break;
        // 边界：`@` 前非字母数字（行首或分隔符），label 后非字母数字
        const prev = at > 0 ? line.text[at - 1] ?? "" : "";
        if (prev !== "" && /[\p{L}\p{N}]/u.test(prev)) {
          i = at + 1;
          continue;
        }
        let best: { key: string; label: string } | null = null;
        for (const mn of mentions) {
          if (!mn.label) continue;
          if (line.text.startsWith(mn.label, at + 1)) {
            const after = line.text[at + 1 + mn.label.length];
            if (after === undefined || !/[\p{L}\p{N}]/u.test(after)) {
              if (!best || mn.label.length > best.label.length) best = mn;
            }
          }
        }
        if (!best) {
          i = at + 1;
          continue;
        }
        const from = line.from + at;
        const to = from + 1 + best.label.length;
        if (coveredAll(from, to)) {
          i = at + 1;
          continue;
        }
        pushWidget(from, to, Decoration.replace({
          widget: new MentionWidget(best.key, best.label, (key, label) => opts.onMentionClick?.(key, label)),
        }));
        i = to - line.from;
      }
    });
  }

  // 其余 mark：块级标记（标题/引用/列表）按行隐藏；行内标记已由容器逻辑决定
  for (const m of marks) {
    if (marksToShow.has(rangeKey(m))) continue;
    if (overlapsWidgets(m.from, m.to)) continue;
    if (marksToHide.has(rangeKey(m))) {
      pushWidget(m.from, m.to, Decoration.replace({}));
      continue;
    }
    if (BLOCK_MARKS.has(m.name) && !isCursorLine(doc.lineAt(m.from).number)) {
      const marker = m.name === "ListMark" ? doc.sliceString(m.from, m.to) : "";
      const dec = /^[-*+]$/.test(marker.trim())
        ? Decoration.replace({ widget: new ListMarkerWidget("•") })
        : marker
          ? Decoration.replace({ widget: new ListMarkerWidget(marker) })
          : Decoration.replace({});
      pushWidget(m.from, m.to, dec);
    }
  }

  // ===== 行装饰（标题字号 / 引用与 callout / 列表缩进）——逐行合并防同线重复添加 =====

  const lineDecor = new Map<number, { style: string[]; classes: string[] }>();
  const addLineStyle = (n: number, css: string) => {
    const d = lineDecor.get(n) ?? { style: [], classes: [] };
    d.style.push(css);
    lineDecor.set(n, d);
  };
  const addLineClass = (n: number, cls: string) => {
    const d = lineDecor.get(n) ?? { style: [], classes: [] };
    if (!d.classes.includes(cls)) d.classes.push(cls);
    lineDecor.set(n, d);
  };

  for (const [lineNum, level] of headingLines) {
    const size = [1.5, 1.35, 1.2, 1.1, 1, 0.95][level - 1] ?? 1;
    addLineStyle(lineNum, `font-size:${size}em;font-weight:600;line-height:1.3;`);
  }
  for (const bq of blockquotes) {
    const callout = callouts.find((c) => c.from === bq.from && c.to === bq.to);
    const cls = callout ? `md-callout callout-${callout.type}` : "md-quote";
    const start = doc.lineAt(bq.from).number;
    const end = doc.lineAt(Math.max(bq.to - 1, bq.from)).number;
    for (let n = start; n <= end; n++) addLineClass(n, cls);
  }
  // 列表缩进：按 listItems 区间逐行记最大深度（镜像 blockquotes 的按区间模式，避免 O(行×列表项)）
  const listDepthByLine = new Map<number, number>();
  for (const item of listItems) {
    const start = doc.lineAt(item.from).number;
    const end = doc.lineAt(Math.max(item.to - 1, item.from)).number;
    for (let n = start; n <= end; n++) {
      const cur = listDepthByLine.get(n) ?? 0;
      if (item.depth > cur) listDepthByLine.set(n, item.depth);
    }
  }
  for (const [lineNum, depth] of listDepthByLine) {
    addLineStyle(lineNum, `padding-left:${depth * 1.5 + 0.2}em;`);
  }

  for (const [lineNum, d] of lineDecor) {
    const line = doc.line(lineNum);
    const attrs: Record<string, string> = {};
    if (d.style.length > 0) attrs.style = d.style.join("");
    if (d.classes.length > 0) attrs.class = d.classes.join(" ");
    widgetEntries.push({ from: line.from, to: line.from, dec: Decoration.line({ attributes: attrs }) });
  }

  // ===== 组装：RangeSetBuilder 要求按 (from, startSide) 升序添加——
  // line 装饰与 widget 同 from 时靠 startSide 定序，统一排序后再 add =====

  widgetEntries.sort(
    (a, b) =>
      a.from - b.from ||
      a.dec.startSide - b.dec.startSide ||
      a.dec.endSide - b.dec.endSide ||
      a.to - b.to,
  );
  for (const w of widgetEntries) builder.add(w.from, w.to, w.dec);
  return builder.finish();
}

/** 实时预览装饰是否需要重建。
 *
 *  判定必须含**语法树推进**：CodeMirror 的后台解析补完只派发 `Language.setState` 效果事务
 *  （docChanged 为 false、无 selection），仅看前三条会漏掉它，装饰便只反映 `LanguageState.init`
 *  同步解析的前 ~3000 字符——文档尾部的标题/链接/表格/围栏码等因此保持源码可见。
 *  语法树引用比较是 O(1)，代价只是每次后台解析补完多重建一次。 */
export function livePreviewNeedsRebuild(
  tr: Transaction,
  readOnlyEffect: StateEffectType<boolean>,
): boolean {
  return (
    tr.docChanged ||
    tr.selection !== undefined ||
    tr.effects.some((e) => e.is(readOnlyEffect)) ||
    syntaxTree(tr.state) !== syntaxTree(tr.startState)
  );
}
