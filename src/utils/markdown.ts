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

/** 拦截走系统浏览器的外部链接协议（MarkdownEditor 链接装饰共用同一正则）。 */
export const EXTERNAL_LINK_RE = /^(https?:|mailto:|xmpp:)/i;
