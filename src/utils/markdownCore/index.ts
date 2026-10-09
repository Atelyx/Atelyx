/**
 * Markdown 内核汇总导出：文本 → 文档规格 → 已清洗 HTML（框架无关纯函数）。
 * React 视图与编辑面统一从这里消费，勿绕过内核直连第三方解析器。
 */
export { parseMarkdown, isSafeVaultRelPath } from "./parse";
export {
  renderMarkdownToHtml,
  renderMarkdownChunks,
  renderMarkdownEditChunks,
  RAW_SOURCE_KINDS,
} from "./render";
export type { EditRenderOptions, EditRenderResult, RenderChunk } from "./render";

export type {
  InlineSpan,
  LinkForm,
  MarkdownBlock,
  MarkdownDocument,
  MarkdownListItem,
  ParseOptions,
  RangeInfo,
  RenderOptions,
} from "@/types/markdown";
