/**
 * Markdown 链接工具（统一渲染引擎与仓库链接处理的公共解析）。
 *
 * 渲染管线说明：全部渲染面（笔记只读/实时预览编辑、画布文本节点、对话气泡、AI 面板）
 * 统一 CodeMirror 单引擎（`MarkdownEditor` 编辑 / `MarkdownView` 只读，widget 与装饰构建见
 * `components/editor/` 下 `markdownWidgets.tsx` + `markdownDecorations.ts`）。
 * 本文件仅保留与渲染无关的纯解析工具：外部链接协议判定、wiki/仓库路径链接解析、
 * 链接 href 解码。安全清洗（raw HTML 白名单）见 `utils/htmlSanitize.ts`。
 */
import { baseName, sanitizeFilename } from "@/utils/filename";

/** `[[笔记名]]` → 候选文件名（无目录前缀：笔记任意文件夹存放，按文件名匹配全仓库同名笔记）。
 * 返回原样 + 净化后两种候选（文件名 = title 净化）。 */
export function wikiNoteFileCandidates(value: string): string[] {
  const trimmed = value.trim();
  const candidates = [`${trimmed}.md`];
  const sanitized = sanitizeFilename(trimmed);
  if (sanitized && sanitized !== trimmed) candidates.push(`${sanitized}.md`);
  return candidates;
}

/** `[[笔记名]]` → 按文件名匹配全仓库同名笔记，返回 `{file, title}`（title = 文件名去 .md）。 */
export function wikiNoteFileOf(
  value: string,
  noteList: { name: string; file: string }[],
): { file: string; title: string } | null {
  for (const candidate of wikiNoteFileCandidates(value)) {
    const hit = noteList.find((n) => n.name === candidate);
    if (hit) return { file: hit.file, title: hit.name.replace(/\.md$/i, "") };
  }
  return null;
}

/** 链接 href 解码（用于匹配与展示）；非法百分号编码原样返回（不抛错、不拦截）。 */
export function decodeLinkHref(href: string): string {
  try {
    return decodeURIComponent(href);
  } catch {
    return href;
  }
}

/** `[label](基于仓库的路径)` → 按仓库相对路径（或文件名兜底）匹配笔记，返回 `{file, title}`。
 * percent 解码 + 反斜杠→`/` + 去 `./`/前导 `/`；含 `..` 段或未命中返回 null（防越出仓库、不拦截）。
 * 大小写不敏感兜底（Windows 文件系统不区分大小写：`方案.MD` 与 `方案.md` 是同一文件）。 */
export function vaultPathNoteOf(
  href: string,
  noteList: { name: string; file: string }[],
): { file: string; title: string } | null {
  let path = decodeLinkHref(href);
  path = path.replace(/\\/g, "/");
  while (path.startsWith("./")) path = path.slice(2);
  while (path.startsWith("/")) path = path.slice(1);
  if (!path) return null;
  if (path.split("/").includes("..")) return null;
  const basename = baseName(path);
  const ci = (s: string) => s.toLowerCase();
  const hit = noteList.find(
    (n) =>
      n.file === path ||
      ci(n.file) === ci(path) ||
      n.file === basename ||
      ci(n.file) === ci(basename),
  );
  if (!hit) return null;
  return { file: hit.file, title: hit.name.replace(/\.md$/i, "") };
}

/** 允许走系统浏览器的外部链接协议。 */
const EXTERNAL_LINK_RE = /^(https?:|mailto:|xmpp:)/i;

/** `[label](href)` 插入用 href 编码：仅转义破坏行内链接解析的 ASCII 字符——
 *  `%` 先行转义保证解码往返无歧义，空格会截断 URL 组、括号会提前闭合链接；
 *  中文等合法字符保留原样。与 Rust 反链扫描和前端 `vaultPathNoteOf` 的 percent
 *  解码契约一致（两侧均先解码再匹配路径）。 */
export function encodeMarkdownLinkHref(path: string): string {
  return path
    .replace(/%/g, "%25")
    .replace(/ /g, "%20")
    .replace(/\(/g, "%28")
    .replace(/\)/g, "%29");
}

/**
 * 光标前 wiki 链接候选上下文：`before` = 光标所在行光标前文本，`after` = 光标后文本
 * （均相对本行）。光标处于未闭合的 `[[查询词` / `【【查询词` 片段内时返回
 * `{ from, query }`（from = 触发符在 before 内的下标，query = 触发符到光标之间的查询词）。
 * 不触发的情形：触发符后已闭合（after 以 `]]` 或 `】】` 起始）；片段含闭合括号
 * （`[` `]` `【` `】`）、别名分隔 `|` 或换行。 */
export function wikiLinkContextAt(before: string, after: string): { from: number; query: string } | null {
  const m = /(?:\[\[|【【)([^[\]【】\n|]*)$/.exec(before);
  if (!m) return null;
  if (/^(?:\]\]|】】)/.test(after)) return null;
  const query = m[1] ?? "";
  return { from: before.length - query.length - 2, query };
}

/**
 * 行内可新建链接（未闭合 wiki `[[目标|别名]]` 或空路径链接 `[label]()`）定位：
 * 返回包含 rel（点击落点的行内偏移，落点对齐区间末端也算命中）的区间与显示名。
 * 供快捷新建回填按点击落点重扫链接当前区间（构建期区间已随文档变更过期时的兜底）；
 * 与装饰层的 wiki / 空链接判定同语义（wiki 目标非空、空链接 label 非空）。 */
export function creatableLinkRangeInLine(
  lineText: string,
  rel: number,
): { start: number; end: number; label: string } | null {
  const hits: { start: number; end: number; label: string }[] = [];
  const wikiRe = /\[\[([^\]|]*?)(?:\|([^\]]*?))?\]\]/g;
  let m: RegExpExecArray | null;
  while ((m = wikiRe.exec(lineText))) {
    const target = (m[1] ?? "").trim();
    if (!target) continue;
    hits.push({ start: m.index, end: m.index + m[0].length, label: (m[2] ?? "").trim() || target });
  }
  const emptyRe = /\[([^\]\n]*)\]\(\s*\)/g;
  while ((m = emptyRe.exec(lineText))) {
    if (!(m[1] ?? "").trim()) continue;
    hits.push({ start: m.index, end: m.index + m[0].length, label: m[1] ?? "" });
  }
  return hits.find((r) => rel >= r.start && rel <= r.end) ?? null;
}

/**
 * 可否用系统默认程序打开该 URL：仅放行 `EXTERNAL_LINK_RE` 覆盖的协议，其余（`file:`/`javascript:`/
 * 非法 URL）返回 false。先经 `URL` 解析出协议再判定——前缀正则直接匹配原始串会把
 * `javascript:alert(1)` 误判为可放行。点击、渲染入口在调用 `openUrl` 前都应先过它。
 */
export function isOpenableUrl(raw: string): boolean {
  try {
    return EXTERNAL_LINK_RE.test(new URL(raw).protocol);
  } catch {
    return false;
  }
}
