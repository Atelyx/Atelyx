/**
 * 统一 Markdown 渲染/编辑引擎（分块 DOM 视图层）。
 *
 * 文档模型 = 纯文本正文（与文件正文逐字节一致）。渲染走框架无关内核
 * （`utils/markdownCore`：文本 → 块/行内规格 → DOM），编辑面不引入 HTML 往返。
 *
 * 编辑形态：光标所在块与代码块显示源码，其余块照常渲染；整篇源码由一个隐藏 textarea
 * 承载，因此输入法组合、方向键、Home/End、跨块选区、剪贴板复制（得到源码）全部沿用
 * 浏览器原生行为；光标与选区由本组件按源偏移在渲染结果上测量后自绘。
 *
 * 只读形态（`readOnly`）：同一渲染器全量出渲染结果，不挂输入焦点——因此「预览 ⇄ 编辑」
 * 共用同一容器与同一内核，切换时只换活动块的呈现，滚动位置保留。
 *
 * 安全：渲染产物只出 class + textContent / 已清洗 HTML（raw HTML 经 `utils/htmlSanitize`）。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { collapseSoftLineBreaks } from "@/utils/softLineBreak";
import { renderMarkdownEditHtml, renderMarkdownToHtml } from "@/utils/markdownCore";
import { encodeMarkdownLinkHref, wikiLinkContextAt } from "@/utils/markdown";
import { WikiLinkPicker } from "@/components/editor/WikiLinkPicker";
import type { PopupAnchor } from "@/components/common/PopupLayer";
import { createLinkResolver, type MarkdownEditorLinks } from "./markdownLinks";
import { attachMarkdownInteractions, decorateMarkdownControls, hydrateMarkdownImages } from "./markdownInteractions";
import { blockAtOffset, buildSourceIndex, caretRect, pointToOffset, rangeRects, type SourceIndex } from "./markdownSourceMap";
import { CaretOverlay, MarkdownEditSink, type RemoteCursor } from "./markdownInput";
import type { Transaction } from "yjs";
import type { NoteEditorBinding } from "@/types/noteSurface";

export type { MarkdownEditorLinks } from "./markdownLinks";

/** 编辑器句柄：宿主做划词剪切/粘贴/定位等命令式操作时使用。 */
export interface MarkdownEditorHandle {
  /** 当前正文（LF）。 */
  getText(): string;
  /** 当前选区（源偏移）。 */
  getSelection(): { from: number; to: number };
  /** 设置选区并聚焦。 */
  setSelection(from: number, to: number): void;
  /** 替换区间（走用户编辑链：onBodyChange 上报）。 */
  replaceRange(from: number, to: number, text: string): void;
  /** 视口坐标 → 源偏移（未命中返回 null）。 */
  posAtCoords(x: number, y: number): number | null;
  focus(): void;
}

interface WikiPickerState {
  from: number;
  query: string;
  anchor: PopupAnchor;
}

interface Props {
  /** 当前正文（挂载时初始注入；外部同步时以 syncSeq 触发回灌）。 */
  body: string;
  /** 非用户编辑的内容更新序号（外部修改/加载完成时递增），变化即同步编辑器。 */
  syncSeq: number;
  /** 用户编辑回调：输出编辑器当前全文 markdown 正文。 */
  onBodyChange?: (markdown: string) => void;
  /** 协作绑定（Y.Text 全文 + awareness）：本地编辑按最小差量写回 ytext，远端更新直接回灌。 */
  collab?: NoteEditorBinding;
  /** 编辑器句柄外抛（编辑面划词右键操作按选区 dispatch 用）。 */
  editorViewRef?: { current: MarkdownEditorHandle | null };
  /** 协作挂载时 ytext 与 body 分歧的处置。 */
  onCollabDivergence?: (ytextText: string) => void;
  /** 只读展示面：全量渲染、不参与编辑。 */
  readOnly?: boolean;
  /** 本地原生撤销（无文件编辑面，如画布内文本节点草稿）。 */
  localHistory?: boolean;
  /** 任务勾选框可点（笔记可点写回；只读展示面禁用态）。 */
  interactiveCheckbox?: boolean;
  links?: MarkdownEditorLinks;
  mentions?: { key: string; label: string }[];
  onMentionClick?: (key: string, label: string) => void;
  className?: string;
}

/** 单块最小差量（公共前后缀）：本地编辑落到 Y.Text 时只改变化区间，避免整篇重写造成并发重复。 */
function singleHunk(base: string, next: string): { at: number; remove: number; insert: string } | null {
  if (base === next) return null;
  let head = 0;
  while (head < base.length && head < next.length && base[head] === next[head]) head++;
  let baseEnd = base.length;
  let nextEnd = next.length;
  while (baseEnd > head && nextEnd > head && base[baseEnd - 1] === next[nextEnd - 1]) {
    baseEnd--;
    nextEnd--;
  }
  return { at: head, remove: baseEnd - head, insert: next.slice(head, nextEnd) };
}

/** 差量偏移映射：`at` 之前的偏移不动，之后的按插删长度平移，落在删区内的贴到插入末端。 */
function mapOffsetByHunk(hunk: { at: number; remove: number; insert: string } | null, offset: number, length: number): number {
  if (!hunk) return Math.min(offset, length);
  if (offset <= hunk.at) return offset;
  if (offset >= hunk.at + hunk.remove) return offset + (hunk.insert.length - hunk.remove);
  return hunk.at + hunk.insert.length;
}

/** `caretPositionFromPoint` 双分支（WebKit 只实现 `caretRangeFromPoint`）。 */
function caretPointAt(x: number, y: number): { node: Node; offset: number } | null {
  const doc = document as Document & {
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
    caretRangeFromPoint?: (x: number, y: number) => Range | null;
  };
  if (typeof doc.caretPositionFromPoint === "function") {
    const hit = doc.caretPositionFromPoint(x, y);
    if (hit) return { node: hit.offsetNode, offset: hit.offset };
  }
  const range = doc.caretRangeFromPoint?.(x, y);
  return range ? { node: range.startContainer, offset: range.startOffset } : null;
}

/** 原生表单控件：点击语义归浏览器（聚焦/勾选），不落光标、不接管拖选。 */
const NATIVE_CONTROL_SELECTOR = "input, textarea, select, button";

export function MarkdownEditor({
  body,
  syncSeq,
  onBodyChange,
  collab,
  editorViewRef,
  onCollabDivergence,
  readOnly = false,
  localHistory = false,
  interactiveCheckbox = true,
  links,
  mentions,
  onMentionClick,
  className = "h-full",
}: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const sinkRef = useRef<MarkdownEditSink | null>(null);
  const overlayRef = useRef<CaretOverlay | null>(null);
  const indexRef = useRef<SourceIndex | null>(null);
  const textRef = useRef(body.replace(/\r\n/g, "\n"));
  const selRef = useRef({ from: 0, to: 0 });
  const activeBlockRef = useRef<number | null>(null);
  const dragRef = useRef<{ anchor: number } | null>(null);
  const composingRef = useRef(false);
  const syncSeqRef = useRef(syncSeq);
  const collabRef = useRef(collab);
  collabRef.current = collab;
  const onCollabDivergenceRef = useRef(onCollabDivergence);
  onCollabDivergenceRef.current = onCollabDivergence;

  /** 回调与模式经 ref 转发：视图只挂载一次，需取到最新值且不因它们重挂。 */
  const linksRef = useRef(links);
  linksRef.current = links;
  const mentionsRef = useRef(mentions);
  mentionsRef.current = mentions;
  const onBodyChangeRef = useRef(onBodyChange);
  onBodyChangeRef.current = onBodyChange;
  const onMentionClickRef = useRef(onMentionClick);
  onMentionClickRef.current = onMentionClick;
  const readOnlyRef = useRef(readOnly);
  readOnlyRef.current = readOnly;
  const interactiveRef = useRef(interactiveCheckbox);
  interactiveRef.current = interactiveCheckbox;

  const [wikiPicker, setWikiPicker] = useState<WikiPickerState | null>(null);
  const wikiPickerRef = useRef<WikiPickerState | null>(null);
  wikiPickerRef.current = wikiPicker;
  const wikiDismissedFromRef = useRef<number | null>(null);

  const getOptions = useCallback(
    () => ({
      links: linksRef.current,
      onOpenUrl: (url: string) => void useAppStore.getState().openUrl(url),
      readImage: async (src: string): Promise<string | null> => {
        try {
          return await useVaultStore.getState().readAttachmentDataUrl(src);
        } catch {
          return null;
        }
      },
      onMentionClick: onMentionClickRef.current,
    }),
    [],
  );

  /** 内容重绘（结构变化：文本变更 / 活动块切换 / 只读翻转）。 */
  const renderContent = useCallback(
    (text: string, activeOffset: number | undefined): void => {
      const content = contentRef.current;
      if (!content) return;
      const scrollTop = scrollRef.current?.scrollTop ?? 0;
      const base = { resolveLink: createLinkResolver(linksRef.current), mentions: mentionsRef.current };
      content.innerHTML = readOnlyRef.current
        ? renderMarkdownToHtml(text, base)
        : renderMarkdownEditHtml(text, { ...base, offsets: true, activeOffset });
      const index = buildSourceIndex(content);
      indexRef.current = index;
      activeBlockRef.current = activeOffset === undefined ? null : blockAtOffset(index, activeOffset)?.from ?? null;
      // 任务勾选框：可编辑面解除禁用（点击切换源码），只读面保持禁用展示
      if (!readOnlyRef.current && interactiveRef.current) {
        for (const box of Array.from(content.querySelectorAll("input.md-editor-checkbox"))) {
          box.removeAttribute("disabled");
        }
      }
      if (scrollRef.current) scrollRef.current.scrollTop = scrollTop;
      hydrateMarkdownImages(content, getOptions);
      decorateMarkdownControls(content);
    },
    [getOptions],
  );

  /**
   * 视觉重绘（仅光标/选区：不重建内容 DOM）。followCaret = 光标移出可视区时滚动跟随，
   * 只用于本端光标移动（键盘导航/输入）；scroll 重绘等被动路径绝不反调 scrollTop，
   * 否则容器的自由滚动会被拉回光标处。
   */
  const drawOverlay = useCallback((from: number, to: number, focus: boolean, followCaret = false): void => {
    const index = indexRef.current;
    const overlay = overlayRef.current;
    if (!index || !overlay) return;
    // 绘制层是内容容器的兄弟节点（不随内容替换销毁），坐标以它自身为原点
    const origin = overlay.el.getBoundingClientRect();
    overlay.setSelectionRects(rangeRects(index, from, to), origin);
    const caret = focus && !readOnlyRef.current ? caretRect(index, from) : null;
    overlay.setCaretRect(caret, origin);
    // 隐藏输入面按宿主坐标定位，让输入法候选框跟随真实光标
    sinkRef.current?.moveTo(
      caret ? { left: caret.left - origin.left, top: caret.top - origin.top, height: caret.height } : null,
    );
    if (followCaret && caret) {
      // 光标移出可视区时把内容滚回来（隐藏输入面不会带动滚动）
      const scroll = scrollRef.current;
      if (scroll) {
        const box = scroll.getBoundingClientRect();
        if (caret.top < box.top) scroll.scrollTop -= box.top - caret.top + 8;
        else if (caret.bottom > box.bottom) scroll.scrollTop += caret.bottom - box.bottom + 8;
      }
    }
  }, []);

  /** 绘制协作者光标（awareness 的 selection/cursor 字段；本端 clientID 跳过）。 */
  const drawRemoteCursors = useCallback((): void => {
    const awareness = collabRef.current?.awareness;
    const index = indexRef.current;
    const overlay = overlayRef.current;
    if (!awareness || !index || !overlay) return;
    const origin = overlay.el.getBoundingClientRect();
    const cursors: RemoteCursor[] = [];
    for (const [clientId, state] of awareness.getStates()) {
      if (clientId === awareness.clientID) continue;
      const raw = (state as { selection?: { anchor: number; head?: number }; cursor?: { anchor: number } }).selection
        ?? (state as { cursor?: { anchor: number } }).cursor;
      if (!raw || typeof raw.anchor !== "number") continue;
      const user = (state as { user?: { name?: string; color?: string } }).user;
      cursors.push({ rect: caretRect(index, raw.anchor), label: user?.name ?? "", color: user?.color ?? "var(--accent)" });
    }
    overlayRef.current?.setRemoteCursors(cursors, origin);
  }, []);

  /** 光标是否落在不回显候选的语法区间（代码 / raw HTML）：此时不弹双链候选。 */
  const inOpaqueRegion = useCallback((offset: number): boolean => {
    const index = indexRef.current;
    if (!index) return false;
    const block = blockAtOffset(index, offset);
    const kind = block?.el.getAttribute("data-md-kind");
    if (kind === "fencedCode" || kind === "indentedCode" || kind === "htmlBlock") return true;
    return index.atomics.some((atomic) => atomic.from <= offset && offset <= atomic.to && atomic.el.tagName === "CODE");
  }, []);

  const applySelection = useCallback(
    (from: number, to: number, focus: boolean): void => {
      selRef.current = { from, to };
      const index = indexRef.current;
      const hitBlock = index ? blockAtOffset(index, from)?.from ?? null : null;
      // 落点在块外（文末/块间空隙）不切换活动块：保持当前块的源码态，避免整篇翻回渲染
      const nextBlock = hitBlock ?? activeBlockRef.current;
      // 活动块变化才重建内容（该块要在源码与渲染之间切换）
      if (!readOnlyRef.current && nextBlock !== activeBlockRef.current) renderContent(textRef.current, nextBlock ?? undefined);
      drawOverlay(from, to, focus, true);
      // 广播本端光标/选区（字段与既有协作端约定一致；节流由协作传输层负责）
      collabRef.current?.awareness.setLocalStateField("selection", { anchor: from, head: to });
      if (readOnlyRef.current) {
        setWikiPicker((prev) => (prev ? null : prev));
        return;
      }
      const text = textRef.current;
      if (to !== from || composingRef.current || inOpaqueRegion(from)) {
        wikiDismissedFromRef.current = null;
        setWikiPicker((prev) => (prev ? null : prev));
        return;
      }
      const lineStart = text.lastIndexOf("\n", Math.max(0, from - 1)) + 1;
      const lineEnd = text.indexOf("\n", from);
      const ctx = wikiLinkContextAt(text.slice(lineStart, from), text.slice(from, lineEnd === -1 ? text.length : lineEnd));
      if (!ctx) {
        wikiDismissedFromRef.current = null;
        setWikiPicker((prev) => (prev ? null : prev));
        return;
      }
      const triggerFrom = lineStart + ctx.from;
      if (wikiDismissedFromRef.current === triggerFrom) {
        setWikiPicker((prev) => (prev ? null : prev));
        return;
      }
      const caret = index ? caretRect(index, from) : null;
      if (!caret) return;
      const anchor: PopupAnchor = { x: caret.left, y: caret.bottom + 4, flipY: caret.top - 8 };
      setWikiPicker((prev) =>
        prev && prev.from === triggerFrom && prev.query === ctx.query && prev.anchor.x === anchor.x && prev.anchor.y === anchor.y
          ? prev
          : { from: triggerFrom, query: ctx.query, anchor },
      );
    },
    [drawOverlay, inOpaqueRegion, renderContent],
  );

  /**
   * 本地编辑提交：先更新本地正文真相，再按最小差量写回 ytext（协作态）。
   * 顺序不可反——ytext 写回会同步触发 observe，若本地真相滞后会被误判为远端更新而全量重同步、回拨光标。
   */
  const commitLocalEdit = useCallback((next: string): void => {
    const base = textRef.current;
    if (next === base) return;
    textRef.current = next;
    const binding = collabRef.current;
    if (!binding) return;
    const hunk = singleHunk(base, next);
    if (!hunk) return;
    const ytext = binding.ytext;
    const apply = () => {
      if (hunk.remove > 0) ytext.delete(hunk.at, hunk.remove);
      if (hunk.insert) ytext.insert(hunk.at, hunk.insert);
    };
    if (ytext.doc) ytext.doc.transact(apply);
    else apply();
  }, []);

  /** 用新正文替换全文（程序化编辑：句柄、勾选框、候选插入共用）。 */
  const applyText = useCallback(
    (next: string, caretAt: number): void => {
      commitLocalEdit(next);
      sinkRef.current?.setText(next);
      renderContent(next, caretAt);
      sinkRef.current?.setSelection(caretAt, caretAt, false);
      drawOverlay(caretAt, caretAt, true, true);
      onBodyChangeRef.current?.(next);
    },
    [commitLocalEdit, drawOverlay, renderContent],
  );

  // ===== 输入面挂载（一次）=====
  useEffect(() => {
    const host = hostRef.current;
    const content = contentRef.current;
    if (!host || !content) return;
    const detachInteractions = attachMarkdownInteractions(content, getOptions);
    // 绘制层挂到宿主（与内容容器同级）：内容每次整篇重绘都不会把它一起清掉
    const overlay = new CaretOverlay(host);
    overlayRef.current = overlay;
    const sink = new MarkdownEditSink(
      host,
      {
        onTextChange: (text) => {
          // 文本未变化（如部分浏览器在 compositionend 后补发的 input）为幂等事件，跳过
          if (text === textRef.current) return;
          commitLocalEdit(text);
          const selection = sink.selection;
          selRef.current = selection;
          renderContent(text, selection.from);
          drawOverlay(selection.from, selection.to, true, true);
          onBodyChangeRef.current?.(text);
        },
        onSelectionChange: (from, to) => {
          if (readOnlyRef.current) return;
          applySelection(from, to, document.hasFocus());
        },
        onCompositionChange: (composing) => {
          composingRef.current = composing;
        },
      },
      { interceptHistory: !localHistory },
    );
    sinkRef.current = sink;
    // 挂载即把当前正文注入输入面：输入面 value 就是文档模型，等外部 syncSeq 首递增兜底
    // 会让 syncSeq 恒定的编辑面（画布内文本节点）在首次击键时按「空 → 全文」差量清掉正文
    sink.setText(textRef.current);
    renderContent(textRef.current, undefined);

    /** 焦点/滚动变化后重绘（隐藏输入面不带动滚动，滚动时需按当前选区重算位置）。 */
    const refreshOverlay = (): void => {
      const selection = selRef.current;
      // 与选区回调的焦点判定一致：窗口失焦时自绘光标同样隐藏
      drawOverlay(selection.from, selection.to, document.hasFocus() && document.activeElement === sink.el);
    };
    const scrollEl = scrollRef.current;
    scrollEl?.addEventListener("scroll", refreshOverlay, { passive: true });
    sink.el.addEventListener("focus", refreshOverlay);
    sink.el.addEventListener("blur", refreshOverlay);

    // 拖选释放兜底挂 window：松开在编辑面之外（拖出窗口/落在不冒泡的兄弟节点）时
    // 宿主的 onMouseUp 收不到，拖选锚点不清除会导致悬停持续改写选区
    const clearDrag = (): void => {
      dragRef.current = null;
    };
    window.addEventListener("mouseup", clearDrag);

    return () => {
      window.removeEventListener("mouseup", clearDrag);
      scrollEl?.removeEventListener("scroll", refreshOverlay);
      sink.el.removeEventListener("focus", refreshOverlay);
      sink.el.removeEventListener("blur", refreshOverlay);
      detachInteractions();
      overlay.destroy();
      sink.destroy();
      overlayRef.current = null;
      sinkRef.current = null;
      indexRef.current = null;
      activeBlockRef.current = null;
    };
    // localHistory 为挂载期配置，只在挂载时生效
  }, [applySelection, commitLocalEdit, drawOverlay, getOptions, localHistory, renderContent]);

  // 只读翻转：内容形态变化，重建一次（容器不重建，滚动保留）
  useEffect(() => {
    renderContent(textRef.current, readOnly ? undefined : selRef.current.from);
    drawOverlay(selRef.current.from, selRef.current.to, false);
  }, [readOnly, renderContent, drawOverlay]);

  // 外部同步（加载完成 / 撤销回放 / 协作收敛）：写入权威文本，选区按差量映射避免落在错位内容处
  useEffect(() => {
    if (syncSeq === 0 || syncSeq === syncSeqRef.current) return;
    syncSeqRef.current = syncSeq;
    const normalized = body.replace(/\r\n/g, "\n");
    const hunk = singleHunk(textRef.current, normalized);
    const next = {
      from: mapOffsetByHunk(hunk, selRef.current.from, normalized.length),
      to: mapOffsetByHunk(hunk, selRef.current.to, normalized.length),
    };
    textRef.current = normalized;
    sinkRef.current?.setText(normalized);
    selRef.current = next;
    sinkRef.current?.setSelection(next.from, next.to, false);
    renderContent(normalized, readOnly ? undefined : next.from);
    drawOverlay(next.from, next.to, false);
  }, [syncSeq, body, readOnly, renderContent, drawOverlay]);

  // 只读面的正文直接随 props 变化重绘（无 syncSeq 通道）
  useEffect(() => {
    if (!readOnly) return;
    const normalized = body.replace(/\r\n/g, "\n");
    if (normalized === textRef.current) return;
    textRef.current = normalized;
    renderContent(normalized, undefined);
  }, [body, readOnly, renderContent]);

  // ===== 协作绑定：Y.Text ↔ 本地正文 + awareness 远端光标 =====
  useEffect(() => {
    if (!collab) return;
    const { ytext, awareness } = collab;
    // 挂载分歧：ytext 与正文相悖 → 交父级处置（干净则收敛到 ytext，有未落盘输入则本地写回）
    if (ytext.toString() !== textRef.current) {
      onCollabDivergenceRef.current?.(ytext.toString());
    }
    /** 远端合入：整篇对齐，并把本地选区按最小差量映射，避免光标跳走。
     *  本地事务（本端编辑写回 ytext）不走此路径：本地正文在提交时已对齐。 */
    const applyRemote = (_ytext: unknown, transaction: Transaction): void => {
      if (transaction.local) return;
      const remote = ytext.toString();
      if (remote === textRef.current) return;
      const base = textRef.current;
      const hunk = singleHunk(base, remote);
      const selection = selRef.current;
      const next = {
        from: mapOffsetByHunk(hunk, selection.from, remote.length),
        to: mapOffsetByHunk(hunk, selection.to, remote.length),
      };
      textRef.current = remote;
      selRef.current = next;
      sinkRef.current?.setText(remote);
      sinkRef.current?.setSelection(next.from, next.to, false);
      renderContent(remote, next.from);
      drawOverlay(next.from, next.to, document.hasFocus());
      onBodyChangeRef.current?.(remote);
    };
    ytext.observe(applyRemote);
    awareness.on("change", drawRemoteCursors);
    drawRemoteCursors();
    return () => {
      ytext.unobserve(applyRemote);
      awareness.off("change", drawRemoteCursors);
    };
  }, [collab, drawOverlay, drawRemoteCursors, renderContent]);

  // ===== 落点换算 =====
  const offsetAtPoint = useCallback((x: number, y: number): number | null => {
    const content = contentRef.current;
    const index = indexRef.current;
    if (!content || !index) return null;
    const hit = caretPointAt(x, y);
    if (!hit || !content.contains(hit.node)) return null;
    const direct = pointToOffset(index, hit.node, hit.offset);
    if (direct !== null) return direct;
    const el = hit.node.nodeType === Node.ELEMENT_NODE ? (hit.node as Element) : hit.node.parentElement;
    const block = el?.closest("[data-md-block]");
    const from = block ? Number(block.getAttribute("data-md-from")) : NaN;
    return Number.isFinite(from) ? from : null;
  }, []);

  // ===== 编辑命令：任务勾选框切换 =====
  const onHostMouseDown = useCallback(
    (event: React.MouseEvent) => {
      if (event.button !== 0) return;
      const target = event.target as Element | null;
      const checkbox = target?.closest("input.md-editor-checkbox");
      if (checkbox) {
        event.preventDefault();
        event.stopPropagation();
        const item = checkbox.closest("[data-md-from]");
        const from = item ? Number(item.getAttribute("data-md-from")) : NaN;
        if (!Number.isFinite(from)) return;
        const raw = textRef.current.slice(from, from + 200);
        const m = /([-+*]|\d+[.)])[ \t]+\[( |x)\]/.exec(raw);
        if (!m) return;
        const marker = from + m.index + m[0].length - 2;
        const next = textRef.current.slice(0, marker) + (m[2] === "x" ? " " : "x") + textRef.current.slice(marker + 1);
        applyText(next, marker);
        return;
      }
      // 原生表单控件交给浏览器；其余（含链接/图片/胶囊）统一接管：preventDefault 掉原生
      // 拖选后由自绘选区负责（否则控件区域原生选区与自绘选区双轨并存），点击激活仍走 click 委托
      if (target?.closest(NATIVE_CONTROL_SELECTOR)) return;
      // 落点换算不到（如内容下方的空白区）时落到正文末尾，与编辑器惯例一致
      const offset = offsetAtPoint(event.clientX, event.clientY) ?? textRef.current.length;
      event.preventDefault();
      dragRef.current = { anchor: offset };
      sinkRef.current?.setSelection(offset, offset);
    },
    [applyText, offsetAtPoint],
  );

  const onHostMouseMove = useCallback(
    (event: React.MouseEvent) => {
      if (!dragRef.current) return;
      // 主键已释放（如拖选在编辑面外松开）则终止拖选，防止悬停持续改写选区
      if (!(event.buttons & 1)) {
        dragRef.current = null;
        return;
      }
      const offset = offsetAtPoint(event.clientX, event.clientY);
      if (offset === null) return;
      const anchor = dragRef.current.anchor;
      sinkRef.current?.setSelection(Math.min(anchor, offset), Math.max(anchor, offset), false);
    },
    [offsetAtPoint],
  );

  const onHostMouseUp = useCallback(() => {
    dragRef.current = null;
  }, []);

  // ===== 编辑器句柄 =====
  useEffect(() => {
    if (!editorViewRef) return;
    editorViewRef.current = {
      getText: () => textRef.current,
      getSelection: () => ({ ...selRef.current }),
      setSelection: (from, to) => sinkRef.current?.setSelection(from, to),
      replaceRange: (from, to, text) => {
        const next = textRef.current.slice(0, from) + text + textRef.current.slice(to);
        applyText(next, from + text.length);
      },
      posAtCoords: (x, y) => offsetAtPoint(x, y),
      focus: () => sinkRef.current?.focus(),
    };
    return () => {
      editorViewRef.current = null;
    };
  }, [editorViewRef, applyText, offsetAtPoint]);

  // ===== 双链候选 ====
  const closeWikiPicker = useCallback(() => {
    const picker = wikiPickerRef.current;
    if (picker) wikiDismissedFromRef.current = picker.from;
    setWikiPicker(null);
  }, []);

  const pickWikiTarget = useCallback(
    (file: string, name: string) => {
      const picker = wikiPickerRef.current;
      if (!picker) return;
      const label = name.replace(/\.md$/i, "").replace(/[[\]]/g, "");
      const insert = `[${label}](${encodeMarkdownLinkHref(file)})`;
      const head = selRef.current.from;
      applyText(textRef.current.slice(0, picker.from) + insert + textRef.current.slice(head), picker.from + insert.length);
      setWikiPicker(null);
    },
    [applyText],
  );

  const editable = !readOnly;
  return (
    <>
      <div
        ref={hostRef}
        className={className}
        data-markdown-editor
        data-editable={editable || undefined}
        onMouseDown={editable ? onHostMouseDown : undefined}
        onMouseMove={editable ? onHostMouseMove : undefined}
        onMouseUp={editable ? onHostMouseUp : undefined}
      >
        <div ref={scrollRef} className="md-edit-scroll">
          <div ref={contentRef} className="md-edit-content markdown-body" />
        </div>
      </div>
      {wikiPicker && (
        <WikiLinkPicker
          query={wikiPicker.query}
          anchor={wikiPicker.anchor}
          onPick={pickWikiTarget}
          onCreate={links?.onCreateNote}
          onClose={closeWikiPicker}
        />
      )}
    </>
  );
}

// ===== 只读展示面封装（画布文本节点 / 对话气泡 / AI 面板）=====

interface MarkdownViewProps {
  /** 静态正文（随 props 变化重绘）。 */
  text: string;
  links?: MarkdownEditorLinks;
  mentions?: { key: string; label: string }[];
  onMentionClick?: (key: string, label: string) => void;
  className?: string;
}

/** 只读 Markdown 渲染（与编辑面同一引擎与内核，渲染完全一致）。
 * 宽松换行关闭（softLineBreak=false）时折叠段内单换行为空格（见 utils/softLineBreak）。 */
export function MarkdownView({ text, links, mentions, onMentionClick, className }: MarkdownViewProps) {
  const softLineBreak = useSettingsStore((s) => s.softLineBreak);
  const displayText = softLineBreak ? text : collapseSoftLineBreaks(text);
  return (
    <MarkdownEditor
      body={displayText}
      syncSeq={0}
      readOnly
      links={links}
      mentions={mentions}
      onMentionClick={onMentionClick}
      className={className ?? ""}
    />
  );
}