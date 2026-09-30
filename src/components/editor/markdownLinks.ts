/**
 * 链接域纯函数与链接回调契约（分块渲染的解析侧与点击侧共用，保证同一 href 的分类一致）。
 *
 * 分类只依据 href 与宿主注入的回调：能命中仓库笔记 = path，能创建 = create，
 * 可走系统浏览器 = external，其余 = plain（保持原文，不产出可点形态）。
 * `MarkdownEditorLinks` 由 `MarkdownEditor` 原样 re-export，消费方导入路径不变。
 */
import { decodeLinkHref, isOpenableUrl } from "@/utils/markdown";
import { isSafeVaultRelPath } from "@/utils/markdownCore";
import type { LinkForm } from "@/types/markdown";

/** 链接/定位回调（wiki/仓库路径/空链接新建/画布定位）。 */
export interface MarkdownEditorLinks {
  isVaultPathNote?: (href: string) => boolean;
  onOpenVaultPathNote?: (href: string) => void;
  /** 快捷新建同名笔记，返回新文件相对路径（失败 = null）。只创建不打开——
   *  打开归 onOpenCreatedNote（必须晚于回填，防先切走编辑面导致回填写进错误文档）。 */
  onCreateNote?: (name: string) => Promise<string | null>;
  /** 打开快捷新建的笔记（回填完成后由渲染层调用）。 */
  onOpenCreatedNote?: (file: string, name: string) => void;
  onOpenNote?: (name: string) => void;
  /** wiki 目标是否命中仓库笔记（未提供 = 渲染层不做缺失判定，行为同命中打开）。 */
  resolveWikiNote?: (value: string) => boolean;
  isLocatable?: (value: string) => boolean;
  onLocate?: (value: string) => void;
}

/** 无 .md 后缀的路径链接按「路径 + .md」解析命中（`[链接](链接)` → 链接.md）→ 内部链接打开。 */
export function isNotePathHit(links: MarkdownEditorLinks, url: string): boolean {
  if (url === "" || url.startsWith("<")) return false;
  return !!links.onOpenVaultPathNote && !!links.isVaultPathNote?.(url + ".md");
}

/** 链接路径是否可作为「未命中的笔记目标」来创建：干净仓库相对路径，且后缀要么没有、
 *  要么是 .md；锚点（`#片段`）与外部/附件后缀（.pdf 等）保持原文，不产出可新建形态。 */
export function isNoteCreatableHref(url: string): boolean {
  if (url === "" || url.startsWith("<") || url.startsWith("#")) return false;
  const hasExt = /\.[a-z0-9]+$/i.test(url);
  return isSafeVaultRelPath(url) && (!hasExt || /\.md$/i.test(url));
}

/** 单一分类入口：解析期（决定链接形态/样式）与点击期（决定行为）都用它，避免两处实现分叉。 */
export function resolveLinkForm(href: string, links: MarkdownEditorLinks): LinkForm {
  if (href === "") return "create";
  const url = decodeLinkHref(href);
  if (isOpenableUrl(url)) return "external";
  if (links.isVaultPathNote?.(href) || links.isVaultPathNote?.(url)) return "path";
  if (isNotePathHit(links, url)) return "path";
  if (isNoteCreatableHref(url) && links.onCreateNote) return "create";
  return "plain";
}

/** 供内核解析使用的链接判定器。 */
export function createLinkResolver(links: MarkdownEditorLinks | undefined) {
  return (href: string): LinkForm => resolveLinkForm(href, links ?? {});
}