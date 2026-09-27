/**
 * `[[` 双链候选触发上下文（编辑器级纯函数）。
 *
 * 文本级判定（未闭合 `[[查询词` 片段）之上叠加语法树守卫：行内代码/围栏代码/缩进代码/
 * HTML 区间内链接语法不生效（装饰层 opaque 同语义），在这些区间不弹候选——
 * 防止在代码里插入链接语法产生永不渲染的正文污染。
 */
import { syntaxTree } from "@codemirror/language";
import type { EditorState } from "@codemirror/state";
import { wikiLinkContextAt } from "@/utils/markdown";

/** 链接语法不生效的 lezer 节点（与装饰层 opaque 覆盖的代码/HTML 区间一致）。 */
const SUPPRESS_NODE_NAMES = new Set([
  "InlineCode",
  "IndentedCode",
  "FencedCode",
  "CodeText",
  "HTMLTag",
  "HTMLBlock",
]);

/** 光标处的候选触发上下文；不触发（无片段 / 已闭合 / 代码与 HTML 区间内）返回 null。 */
export function wikiLinkTriggerContext(
  state: EditorState,
  pos: number,
): { from: number; query: string } | null {
  const line = state.doc.lineAt(pos);
  const ctx = wikiLinkContextAt(
    line.text.slice(0, pos - line.from),
    line.text.slice(pos - line.from),
  );
  if (!ctx) return null;
  let node = syntaxTree(state).resolveInner(pos, -1);
  while (true) {
    if (SUPPRESS_NODE_NAMES.has(node.type.name)) return null;
    const parent = node.parent;
    if (!parent) break;
    node = parent;
  }
  return { from: line.from + ctx.from, query: ctx.query };
}
