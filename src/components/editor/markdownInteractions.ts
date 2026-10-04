/**
 * 分块渲染的交互代理（只读面与编辑面共用）。
 *
 * 渲染产物由内核生成（只出 class 与 data- 标记），这里以事件委托统一绑定行为：
 * 链接打开/定位/快捷新建、wiki 链接、@引用胶囊、代码块复制、raw HTML 内链接，
 * 以及相对路径图片的异步加载。避免为每个片段单独挂监听，也让只读与编辑面行为一致。
 *
 * 安全：只读取/写入既有 data- 标记，不产生新的 HTML 注入点。
 */
import { decodeLinkHref, isOpenableUrl } from "@/utils/markdown";
import { noteTitleFromFile } from "@/utils/filename";
import { isNoteCreatableHref, isNotePathHit, type MarkdownEditorLinks } from "./markdownLinks";

export interface MarkdownInteractionOptions {
  links?: MarkdownEditorLinks;
  onOpenUrl: (url: string) => void;
  readImage: (src: string) => Promise<string | null>;
  onMentionClick?: (key: string, label: string) => void;
}

type OptionsGetter = () => MarkdownInteractionOptions;

const COPY_ICON_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>';
const CHECK_ICON_SVG =
  '<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>';

/** 点击在途的快捷新建去重（防创建窗口内重复点击建出第二篇笔记，先建的变无入链孤儿）。 */
const creatingElements = new WeakSet<Element>();

function beginCreate(el: Element | null): boolean {
  if (!el) return true;
  if (creatingElements.has(el)) return false;
  creatingElements.add(el);
  return true;
}
function endCreate(el: Element | null): void {
  if (el) creatingElements.delete(el);
}

/** 快捷新建并打开（只读面：无编辑面回填，创建后直接打开）。 */
function createAndOpen(el: Element | null, name: string, links: MarkdownEditorLinks): void {
  const createNote = links.onCreateNote;
  if (!createNote || !beginCreate(el)) return;
  void createNote(name)
    .then((file) => {
      if (file) links.onOpenCreatedNote?.(file, noteTitleFromFile(file));
    })
    .catch(() => {})
    .finally(() => endCreate(el));
}

/** 内部链接（仓库路径/wiki）点击：命中打开 → 可定位定位 → 未命中且允许则快捷新建。 */
function handleInternalLink(el: HTMLElement, href: string, links: MarkdownEditorLinks): void {
  const url = decodeLinkHref(href);
  if (links.isVaultPathNote?.(href) || links.isVaultPathNote?.(url)) {
    links.onOpenVaultPathNote?.(href);
    return;
  }
  if (isNotePathHit(links, url)) {
    links.onOpenVaultPathNote?.(url + ".md");
    return;
  }
  if (!isNoteCreatableHref(url)) return;
  // 无后缀路径补全 .md，否则链接永不解析
  createAndOpen(el, /\.md$/i.test(url) ? url : url + ".md", links);
}

/** wiki 链接点击：优先画布定位 → 命中笔记打开 → 未命中且允许则快捷新建同名笔记。 */
function handleWikiLink(el: HTMLElement, target: string, links: MarkdownEditorLinks): void {
  if (links.isLocatable?.(target)) {
    links.onLocate?.(target);
    return;
  }
  const resolved = links.resolveWikiNote ? links.resolveWikiNote(target) : true;
  if (resolved && links.onOpenNote) {
    links.onOpenNote(target);
    return;
  }
  if (!resolved && links.onCreateNote) {
    createAndOpen(el, target, links);
  }
}

function findLinkElement(target: EventTarget | null, container: HTMLElement): HTMLElement | null {
  let node = target instanceof Element ? target : null;
  // 揭示态的源标记（`](地址)`、`##` 等）是可编辑的源文本：点击落光标，不触发渲染件的动作
  if (node?.closest(".md-marker")) return null;
  while (node && node !== container) {
    if (
      node.hasAttribute("data-md-href") ||
      node.hasAttribute("data-md-wiki") ||
      node.hasAttribute("data-md-mention-key") ||
      node.hasAttribute("data-md-copy")
    ) {
      return node as HTMLElement;
    }
    node = node.parentElement;
  }
  return null;
}

/** 相对路径图片经宿主读取为 dataURL（外链/data/blob 直接可用）；失败降级为灰显原文。 */
export function hydrateMarkdownImages(container: HTMLElement, getOptions: OptionsGetter): void {
  for (const box of Array.from(container.querySelectorAll<HTMLElement>("span.md-editor-image[data-md-src]"))) {
    const img = box.querySelector("img");
    if (!img || img.getAttribute("src")) continue;
    const src = box.getAttribute("data-md-src") ?? "";
    if (src === "" || /^(https?:|data:|blob:)/i.test(src)) continue;
    void getOptions()
      .readImage(src)
      .then((dataUrl) => {
        if (!box.isConnected) return;
        if (dataUrl) {
          img.setAttribute("src", dataUrl);
          return;
        }
        box.classList.add("md-editor-image-missing");
        box.textContent = `![${box.getAttribute("data-md-alt") ?? ""}](${src})`;
      })
      .catch(() => {});
  }
}

/** 代码块复制按钮补图标（内核只出结构与标记，图标与状态切换归视图层）。 */
export function decorateMarkdownControls(container: HTMLElement): void {
  for (const btn of Array.from(container.querySelectorAll<HTMLElement>("[data-md-copy]"))) {
    if (!btn.innerHTML.trim()) btn.innerHTML = COPY_ICON_SVG;
  }
}

async function copyCode(btn: HTMLElement): Promise<void> {
  const code = btn.closest(".md-editor-code-block")?.querySelector("code")?.textContent ?? "";
  try {
    await navigator.clipboard.writeText(code);
    btn.innerHTML = CHECK_ICON_SVG;
    btn.classList.add("md-editor-code-copy-done");
    window.setTimeout(() => {
      btn.innerHTML = COPY_ICON_SVG;
      btn.classList.remove("md-editor-code-copy-done");
    }, 1500);
  } catch {
    // 剪贴板不可用：保留原按钮，不打断阅读
  }
}

/**
 * 绑定容器内交互（事件委托）。返回解绑函数。调用方在每次替换内容后应重新调用
 * {@link hydrateMarkdownImages} 与 {@link decorateMarkdownControls}。
 */
export function attachMarkdownInteractions(container: HTMLElement, getOptions: OptionsGetter): () => void {
  const onClick = (event: MouseEvent) => {
    if (event.button !== 0) return;
    const el = findLinkElement(event.target, container);
    if (!el) return;
    const opts = getOptions();
    const links = opts.links ?? {};

    if (el.hasAttribute("data-md-copy")) {
      event.preventDefault();
      event.stopPropagation();
      void copyCode(el);
      return;
    }

    const mentionKey = el.getAttribute("data-md-mention-key");
    if (mentionKey !== null) {
      event.preventDefault();
      event.stopPropagation();
      opts.onMentionClick?.(mentionKey, (el.textContent ?? "").replace(/^[@#]/, ""));
      return;
    }

    const wiki = el.getAttribute("data-md-wiki");
    if (wiki !== null) {
      event.preventDefault();
      event.stopPropagation();
      handleWikiLink(el, wiki, links);
      return;
    }

    const href = el.getAttribute("data-md-href");
    if (href !== null) {
      event.preventDefault();
      event.stopPropagation();
      if (el.classList.contains("md-editor-link")) {
        if (isOpenableUrl(decodeLinkHref(href))) opts.onOpenUrl(decodeLinkHref(href));
        return;
      }
      if (href === "") {
        // `[名]()` 空路径 = 快捷新建同名笔记（label 即目标名）
        const name = (el.textContent ?? "").trim();
        if (name) createAndOpen(el, name, links);
        return;
      }
      handleInternalLink(el, href, links);
      return;
    }
  };

  // raw HTML 块内的原生 <a>：外链走系统浏览器、仓库路径打开笔记（webview 不导航）
  const onHtmlLinkClick = (event: MouseEvent) => {
    const target = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (!target || !container.contains(target)) return;
    const href = target.getAttribute("href");
    if (!href) return;
    event.preventDefault();
    event.stopPropagation();
    const url = decodeLinkHref(href);
    const links = getOptions().links ?? {};
    if (isOpenableUrl(url)) getOptions().onOpenUrl(url);
    else if (links.isVaultPathNote?.(url) && links.onOpenVaultPathNote) links.onOpenVaultPathNote(url);
  };

  container.addEventListener("click", onClick, true);
  container.addEventListener("click", onHtmlLinkClick, true);
  return () => {
    container.removeEventListener("click", onClick, true);
    container.removeEventListener("click", onHtmlLinkClick, true);
  };
}