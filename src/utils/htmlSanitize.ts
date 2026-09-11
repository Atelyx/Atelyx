/**
 * raw HTML 清洗（引擎渲染 HTML 的唯一注入点）。
 *
 * 安全红线：笔记/对话内容可能来自不可信来源（AI 生成、网络复制、他人分享），而应用
 * CSP 的 script-src 含 'unsafe-inline'——渲染出的内联脚本/事件在 webview 里真的会执行。
 * 因此 HTML 渲染前必须经 DOMPurify 严格白名单清洗，这是唯一防线，白名单从严：
 * - 白名单外标签/属性一律剔除（含 on* 事件、class、style、id、javascript: URL）；
 * - script/iframe/表单/媒体嵌入等高风险标签显式禁（FORBID_TAGS 双保险，虽然 ALLOWED_TAGS
 *   未列出已会剔除）；
 * - 结果只出受控 DOM，与编辑器装饰层「只出样式不注入脚本」一致。
 *
 * 白名单内容 = 常用排版标签 + kbd/details/dl 与媒体（video/audio/source）。
 * 清洗结果按输入缓存（击键重建装饰时未变化的片段直接命中，避免每次全量清洗）。
 */
import DOMPurify from "dompurify";

/** 白名单标签：排版 + kbd/details/dl + 媒体。 */
const ALLOWED_TAGS = [
  "kbd", "details", "summary", "dl", "dt", "dd",
  "div", "span", "p", "br", "hr", "strong", "b", "em", "i", "u", "s", "strike",
  "code", "pre", "blockquote", "h1", "h2", "h3", "h4", "h5", "h6",
  "ul", "ol", "li", "table", "thead", "tbody", "tfoot", "tr", "th", "td",
  "caption", "colgroup", "col", "a", "img", "sup", "sub", "abbr", "mark",
  "small", "figure", "figcaption", "section", "article", "header", "footer",
  "main", "nav", "aside", "var", "samp", "tt", "ins", "del", "q", "cite", "time",
  "video", "audio", "source",
];

/** 白名单属性：无 on*、无 style/class/id——防事件执行与伪造应用 UI。 */
const ALLOWED_ATTR = [
  "href", "title", "target", "src", "alt", "width", "height",
  "controls", "poster", "muted", "loop", "preload", "type", "datetime",
  "colspan", "rowspan",
];

/** 显式禁止标签（ALLOWED_TAGS 未列出也已剔除，此处双保险防未来误加）。 */
const FORBID_TAGS = [
  "script", "style", "iframe", "object", "embed", "form", "input", "button",
  "textarea", "select", "link", "meta", "base", "svg", "math", "canvas",
  "frame", "frameset", "noscript",
];

const CACHE_LIMIT = 200;
const sanitizeCache = new Map<string, string>();

export function sanitizeHtmlFragment(html: string): string {
  const hit = sanitizeCache.get(html);
  if (hit !== undefined) return hit;
  // ALLOW_DATA_ATTR 必须显式关：DOMPurify 默认 true，且该判定独立于 ALLOWED_ATTR——
  // 否则 raw HTML 可注入 data-note-file 等属性，被 closest() 取到后劫持笔记的撤销/右键路由。
  const out = DOMPurify.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    FORBID_TAGS,
    ALLOW_DATA_ATTR: false,
  });
  if (sanitizeCache.size >= CACHE_LIMIT) sanitizeCache.clear();
  sanitizeCache.set(html, out);
  return out;
}
