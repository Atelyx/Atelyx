/**
 * Markdown 内核类型（框架无关）：文本 → 文档规格 → 已清洗 HTML。
 *
 * 与渲染框架解耦：行内片段与块的 from/to 均为**源文本绝对偏移**，
 * 供后续编辑面做 offset ↔ DOM 映射；类型契约供 React 视图与插件 API 共同依赖，须保持稳定。
 */

/** 通用区间（源文本绝对偏移）。 */
export interface RangeInfo {
  from: number;
  to: number;
}

/** 链接形态：外链 / wiki / 仓库路径 / 可新建 / 未识别（保持原文）。 */
export type LinkForm = "external" | "wiki" | "path" | "create" | "plain";

/** 行内片段。from/to 为**源文本绝对偏移**（后续编辑面靠它做 offset↔DOM 映射）。 */
export type InlineSpan =
  | { kind: "text"; from: number; to: number; text: string }
  | { kind: "strong"; from: number; to: number; children: InlineSpan[] }
  | { kind: "em"; from: number; to: number; children: InlineSpan[] }
  | { kind: "strike"; from: number; to: number; children: InlineSpan[] }
  | { kind: "highlight"; from: number; to: number; children: InlineSpan[] }
  | { kind: "code"; from: number; to: number; text: string }
  | { kind: "mathInline"; from: number; to: number; tex: string }
  | { kind: "comment"; from: number; to: number }
  | { kind: "link"; from: number; to: number; href: string; label: string; form: LinkForm; title: string | null }
  | { kind: "autolink"; from: number; to: number; href: string; label: string }
  | { kind: "wiki"; from: number; to: number; target: string; label: string }
  | { kind: "image"; from: number; to: number; src: string; alt: string; width: string | null; height: string | null; title: string | null }
  | { kind: "tag"; from: number; to: number; tag: string }
  | { kind: "footnoteRef"; from: number; to: number; label: string }
  | { kind: "mention"; from: number; to: number; key: string; label: string; char: "@" | "#" }
  | { kind: "html"; from: number; to: number; html: string }
  | { kind: "hardBreak"; from: number; to: number };

/** 列表项：from/to 为项的源区间；children 为项内嵌套块（嵌套列表、松散项的后续段落等）。 */
export interface MarkdownListItem {
  from: number;
  to: number;
  /** 任务项（源文本带 `[ ]`/`[x]` 勾选框标记）。 */
  task: boolean;
  checked: boolean;
  children: MarkdownBlock[];
}

/** 表格单元格：内容以源区间承载（单元格行内渲染按区间重解析，与正文同一套行内语义）。 */
export interface MarkdownTableCell {
  from: number;
  to: number;
}

/** 块级规格（顶层块；引用块与列表项递归持有内层块）。 */
export type MarkdownBlock =
  | { kind: "heading"; level: number; from: number; to: number; inline: InlineSpan[] }
  | { kind: "paragraph"; from: number; to: number; inline: InlineSpan[] }
  | { kind: "blockquote"; from: number; to: number; callout: string | null; children: MarkdownBlock[] }
  | { kind: "list"; ordered: boolean; from: number; to: number; items: MarkdownListItem[] }
  | { kind: "fencedCode"; from: number; to: number; lang: string; code: string; contentFrom: number; contentTo: number }
  | { kind: "indentedCode"; from: number; to: number; code: string }
  | { kind: "hr"; from: number; to: number }
  | { kind: "table"; from: number; to: number; header: MarkdownTableCell[]; aligns: ("" | "left" | "center" | "right")[]; rows: MarkdownTableCell[][] }
  | { kind: "mathBlock"; from: number; to: number; tex: string }
  | { kind: "htmlBlock"; from: number; to: number; html: string }
  | { kind: "footnoteDef"; from: number; to: number; label: string; inline: InlineSpan[] };

/** 解析产物：原文 + 顶层块序列。 */
export interface MarkdownDocument {
  source: string;
  blocks: MarkdownBlock[];
}

/** 宿主注入：判定链接形态（外链 / 仓库路径 / 可新建）；缺省全部判为 "plain"。 */
type MarkdownLinkResolver = (href: string) => LinkForm;

/** 解析选项。 */
export interface ParseOptions {
  /** 链接形态判定（缺省全部判为 "plain"）。 */
  resolveLink?: MarkdownLinkResolver;
  /** mention 胶囊候选（宿主提供的用户/节点列表；源文本以 `@` 或 `#` 触发）。 */
  mentions?: { key: string; label: string }[];
}

/** 渲染选项：解析选项 + 是否启用 KaTeX（缺省启用；关闭时回显源码）。
 *  `offsets` 供编辑面使用：给块与行内元素打上源偏移，使 DOM 能与源码互相定位。
 *  `softBreakOff` = 宽松换行关闭：只读面（无 offsets）把段内软换行渲染为空格，
 *  硬换行（独立 span）不受影响；编辑面恒保留换行（文本即真相）。 */
export type RenderOptions = ParseOptions & {
  katex?: boolean;
  offsets?: boolean;
  softBreakOff?: boolean;
};