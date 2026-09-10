/**
 * `.md` 笔记编辑器（未打开画布时单击笔记打开）。
 *
 * 占据主编辑区（画布位置）：顶部文件操作条，正文 = 统一 CodeMirror 引擎
 * （默认只读实时视图，双击/铅笔进入实时预览编辑；「···」菜单切源码模式 textarea）。
 * 正文内容、保存、协作与撤销归 `stores/noteSessionStore` 的编辑会话（与画布文本节点共用同一会话），
 * 本组件只做面板 chrome 与交互编排。
 */
import { Check, ClipboardPaste, Copy, MoreHorizontal, Pencil, Scissors, Wand2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { EditorView } from "@codemirror/view";
import { useNoteStore } from "@/stores/noteStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useAppStore } from "@/stores/appStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { useCollabStore } from "@/stores/collabStore";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import type { BacklinkRow, CollabPeer } from "@/types";
import { parseFrontmatter, stringifyFrontmatter } from "@/utils/frontmatter";
import { noteTitleFromFile } from "@/utils/filename";
import { NotePropertiesView } from "@/components/editor/NotePropertiesView";
import { type MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";
import { NoteBodyEditor } from "@/components/editor/NoteBodyEditor";
import { HistoryModal } from "@/components/history/HistoryModal";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useVaultLinkHandlers } from "@/hooks/useVaultLinkHandlers";
import { useNoteBodySession } from "@/hooks/useNoteBodySession";
import { useNoteUndoRouting } from "@/hooks/useNoteUndoRouting";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { useVaultTagCandidates } from "@/hooks/useVaultTagCandidates";
import { PopupLayer } from "@/components/common/PopupLayer";

/** 模块级空数组：notePeers 缺省引用（避免每次渲染新数组导致无限重渲染）。 */
const EMPTY_PEERS: CollabPeer[] = [];

/** 预览右键进编辑后，等待光标/选区落位稳定（挂载、StrictMode 重挂载、selectionchange 收敛）再弹菜单的时延。 */
const PENDING_MENU_DELAY_MS = 60;

/** 在源码中定位预览选区原文（预览渲染文本与源码可能带标记差异，找不到返回 null）；
 *  多处出现取离参照位置最近的一处——预览选区就在右键点附近。 */
function locateSelectionInDoc(
  doc: string,
  text: string,
  refPos: number,
): { from: number; to: number } | null {
  if (!text) return null;
  let from = doc.indexOf(text);
  if (from === -1) return null;
  let best = { from, to: from + text.length };
  let bestDist = Math.abs(from - refPos);
  for (from = doc.indexOf(text, from + 1); from !== -1; from = doc.indexOf(text, from + 1)) {
    const dist = Math.abs(from - refPos);
    if (dist < bestDist) {
      best = { from, to: from + text.length };
      bestDist = dist;
    }
  }
  return best;
}

export function NoteEditor({ file }: { file: string }) {
  /** 正文编辑会话：全文、保存、协作绑定与撤销都在会话里（画布文本节点共用同一篇的会话）。 */
  const { session, view } = useNoteBodySession(file);
  const content = view?.content ?? "";
  // 保存状态存 noteStore（面板 header 展示；会话写入，本组件只读）
  const noteSaveStatus = useNoteStore((s) => s.noteSaveStates[file]);
  const loadError = noteSaveStatus?.loadError ?? false;
  const collabEnabled = useSettingsStore((s) => s.collabEnabled);
  const collabConnected = useCollabStore((s) => s.connected);
  const isCollab = collabEnabled && collabConnected;
  /** 撤销/重做按焦点所在编辑面归属（面板与画布节点共用一套路由）。 */
  useNoteUndoRouting();
  const [preview, setPreview] = useState(true);
  /** 源码模式：编辑区显示完整 Markdown 源码 textarea；不勾选 = 实时预览编辑（CodeMirror）。 */
  const [sourceMode, setSourceMode] = useState(false);
  /** 右上角「···」更多选项弹层（统一 usePopupAnchor + PopupLayer）。 */
  const menuTriggerRef = useRef<HTMLButtonElement>(null);
  const menu = usePopupAnchor(menuTriggerRef);
  /** 正文右键菜单（右键时的视口坐标 + 选区原文，可为空 = 空白处右键）；
   *  selectionLive = 选区在编辑面是否仍可操作（预览切编辑重建编辑器会丢选区，源码模式同元素保留）；null = 关闭。 */
  const [contentMenu, setContentMenu] = useState<{
    x: number;
    y: number;
    text: string;
    selectionLive: boolean;
  } | null>(null);
  /** 划词改写菜单第二级：评论输入框（repositionDeps 切换菜单内容）。 */
  const [rewriteOpen, setRewriteOpen] = useState(false);
  /** 划词改写评论草稿。 */
  const [rewriteComment, setRewriteComment] = useState("");
  /** 待弹出的右键菜单：预览右键先进编辑模式，等编辑器就绪、光标/选区落到右键位置开始闪烁后再弹
   *  （菜单弹出时才据选区还原结果确定是否含剪切）。 */
  const [pendingMenu, setPendingMenu] = useState<{
    x: number;
    y: number;
    text: string;
  } | null>(null);
  /** 编辑器实例（MarkdownEditor 外抛）：剪切/粘贴按 CodeMirror 当前选区操作。 */
  const cmViewRef = useRef<EditorView | null>(null);
  /** 历史记录面板开关（「···」更多选项入口）。 */
  const [historyOpen, setHistoryOpen] = useState(false);
  /** 剪贴板操作内联提示（底部状态条展示，自动清除；失败是罕见边界，不为此引入 toast 基建）。 */
  const [clipHint, setClipHint] = useState<string | null>(null);
  const clipHintTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  /** 关闭划词右键菜单（两级共用）。 */
  const closeRewriteMenu = useCallback(() => {
    setContentMenu(null);
    setRewriteOpen(false);
  }, []);

  /** 剪贴板失败提示：底部状态条展示 2.5s 自动清除，重复失败重置计时。 */
  const showClipHint = useCallback((message: string) => {
    setClipHint(message);
    if (clipHintTimerRef.current) clearTimeout(clipHintTimerRef.current);
    clipHintTimerRef.current = setTimeout(() => setClipHint(null), 2500);
  }, []);
  useEffect(
    () => () => {
      if (clipHintTimerRef.current) clearTimeout(clipHintTimerRef.current);
    },
    [],
  );

  /** 正文区右键（含空白处）→ 弹菜单（复制/剪切/粘贴 + AI 处理）；预览/只读源码态右键先进入编辑模式，
   *  待光标/选区就位闪烁后再弹菜单（pendingMenu 流程，一套菜单通用）。仅 data-note-content 内容区接管
   *  （顶部条/属性区/反链区走浏览器默认菜单）；源码 textarea 的选区以 selectionStart/End 为准
   *  （window.getSelection 对 textarea 不可靠）。 */
  const handleContentContextMenu = (e: React.MouseEvent) => {
    const target = e.target instanceof Element ? e.target : null;
    if (!target?.closest("[data-note-content]")) return;
    let text = "";
    if (target instanceof HTMLTextAreaElement) {
      text = target.value.slice(target.selectionStart, target.selectionEnd);
    } else {
      text = window.getSelection()?.toString() ?? "";
    }
    e.preventDefault();
    setRewriteComment("");
    setRewriteOpen(false);
    if (preview) {
      // 预览/只读源码态：先进编辑模式，光标/选区就位后由 pendingMenu effect 弹菜单
      setPreview(false);
      setPendingMenu({ x: e.clientX, y: e.clientY, text });
    } else {
      setPendingMenu(null); // 取消仍在等待的 pending（60ms 窗口内再次右键），防旧坐标覆盖新菜单
      setContentMenu({ x: e.clientX, y: e.clientY, text, selectionLive: true });
    }
  };
  /** 编辑器根节点引用：点击编辑器外部 → 取消编辑模式（回渲染预览）。 */
  const editorRootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const onDocMouseDown = (e: MouseEvent) => {
      const root = editorRootRef.current;
      // 编辑器内部（内容区/属性区）不退出；portal 弹层（AI 改写菜单/「···」更多选项经
      // PopupLayer 挂 body，不在根节点内）也不退出——弹层内操作（点菜单项/评论输入框）
      // 不应把编辑态切回预览。preview 已是 true 时幂等无副作用。
      const target = e.target as Element | null;
      if (
        root &&
        !root.contains(e.target as Node) &&
        !target?.closest?.("[data-popup-layer]")
      ) {
        setPreview(true);
        // 60ms 窗口内点编辑器外：连同待弹菜单一起取消，防菜单弹在已回退的预览态上
        setPendingMenu(null);
      }
    };
    document.addEventListener("mousedown", onDocMouseDown);
    return () => document.removeEventListener("mousedown", onDocMouseDown);
  }, []);

  /** 待弹出右键菜单 → 延迟 PENDING_MENU_DELAY_MS 待编辑器挂载/重挂载/selectionchange 全部收敛后，
   *  一次性安置光标/选区并聚焦、紧接弹菜单——光标在菜单出现时真实闪烁，用户才看得到剪切/粘贴
   *  的作用位置（挂载瞬间的落位会被 StrictMode 重挂载/预览卸载的 selectionchange 吞掉，故不提前落）。
   *  预览选区原文在源码中匹配还原成编辑器选区（多处取离右键点最近的一处；渲染文本与源码有标记
   *  差异匹配不到时仅落光标，菜单随之不含剪切）。 */
  useEffect(() => {
    if (!pendingMenu) return;
    const pending = pendingMenu;
    let cancelled = false;
    /** 安置光标/选区并聚焦，返回菜单是否含剪切（编辑器未就绪返回 false，不阻塞弹菜单）。 */
    const restoreSelection = (): boolean => {
      if (sourceMode) {
        const ta = editorRootRef.current?.querySelector("textarea");
        if (!ta) return false;
        // 同元素只翻只读标志，光标/选区保留，聚焦即闪烁
        ta.focus();
        return true;
      }
      const view = cmViewRef.current;
      if (!view) return false;
      const docText = view.state.doc.toString();
      // precise=false：坐标未被视口 DOM 覆盖（如文末空白）时返回就近估算位置而非 null
      const refPos = view.posAtCoords({ x: pending.x, y: pending.y }, false);
      // 渲染文本与源码可能有差异（加粗标记/实体等），原文匹配不到退回去掉首尾空白再试
      const needle = docText.includes(pending.text) ? pending.text : pending.text.trim();
      const located = needle ? locateSelectionInDoc(docText, needle, refPos) : null;
      view.dispatch(
        located
          ? { selection: { anchor: located.from, head: located.to }, scrollIntoView: true }
          : { selection: { anchor: refPos }, scrollIntoView: true },
      );
      view.focus();
      return !!located;
    };
    const timer = setTimeout(() => {
      if (cancelled) return;
      const selectionLive = restoreSelection();
      setContentMenu({ x: pending.x, y: pending.y, text: pending.text, selectionLive });
      setPendingMenu(null);
    }, PENDING_MENU_DELAY_MS);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [pendingMenu, sourceMode]);

  // 切笔记：取消待弹的右键菜单（触发 pendingMenu effect cleanup 取消定时器），防旧坐标弹到新笔记
  useEffect(() => {
    setPendingMenu(null);
  }, [file]);

  // 卸载/切走：只清面板展示用的保存状态与冲突条请求（未落盘输入的 flush 归会话）；
  // 冲突标志按文件保留——它代表「有未决冲突 + 挂起输入」，面板卸载不代表冲突已解决
  useEffect(
    () => () => {
      useNoteStore.getState().setNoteSaveState(file, null);
      useNoteStore.getState().clearNoteConflictResolveReq(file);
    },
    [file],
  );

  /** 历史回滚完成：把会话拨到回滚后的内容（已落盘，记入磁盘基准）。 */
  const handleNoteRollback = useCallback(
    (content: string) => {
      session?.applyRollback(content);
    },
    [session],
  );

  /** 整篇提交（属性区合并 / 源码模式编辑 / 属性模板插入）：走会话的保存链。 */
  const handleChange = (v: string) => {
    session?.commitContent(v);
  };

  /** 当前编辑面选区（区间 + 源码原文）；编辑面不可用返回 null。 */
  const currentEditorSelection = (): { from: number; to: number; text: string } | null => {
    if (sourceMode) {
      const ta = editorRootRef.current?.querySelector("textarea");
      if (!ta) return null;
      return {
        from: ta.selectionStart,
        to: ta.selectionEnd,
        text: ta.value.slice(ta.selectionStart, ta.selectionEnd),
      };
    }
    const view = cmViewRef.current;
    if (!view) return null;
    const { from, to } = view.state.selection.main;
    return { from, to, text: view.state.sliceDoc(from, to) };
  };

  /** 编辑面区间替换原语：源码 textarea 以会话当前全文拼接走 handleChange（自动保存/冲突门控/
   *  协作 syncLocalBody 全复用；命令式取值防 await 剪贴板 IPC 窗口内的击键被旧闭包内容丢弃）；
   *  CodeMirror dispatch（经 onBodyChange → 自动保存/协作同步/撤销栈）。 */
  const editEditorRange = (from: number, to: number, ins: string) => {
    if (sourceMode) {
      const ta = editorRootRef.current?.querySelector("textarea");
      if (!ta) return;
      const current = session?.content() ?? "";
      handleChange(current.slice(0, from) + ins + current.slice(to));
      // React 提交新 value 后光标默认跳到末尾，恢复到插入末端
      const caret = from + ins.length;
      setTimeout(() => ta.setSelectionRange(caret, caret), 0);
    } else {
      const view = cmViewRef.current;
      if (!view) return;
      view.dispatch({
        changes: { from, to, insert: ins },
        selection: { anchor: from + ins.length },
        scrollIntoView: true,
      });
      view.focus();
    }
  };

  /** 无选区插入（空白处粘贴）：源码 textarea 插到光标处；CodeMirror 先把光标移到右键位置再插入
   *  （光标可能停在陈旧位置或刚进入编辑态的文档起点）。 */
  const insertAtCaret = (ins: string) => {
    if (sourceMode) {
      const ta = editorRootRef.current?.querySelector("textarea");
      if (!ta) return;
      editEditorRange(ta.selectionStart, ta.selectionStart, ins);
    } else {
      const view = cmViewRef.current;
      if (!view) return;
      // contentMenu 判空仅为 TS 收窄（菜单项点击时恒非空）；precise=false 让未覆盖坐标取就近估算
      const clicked = contentMenu
        ? view.posAtCoords({ x: contentMenu.x, y: contentMenu.y }, false)
        : null;
      const at = clicked ?? view.state.selection.main.head;
      editEditorRange(at, at, ins);
    }
  };

  /** 复制：菜单打开时捕获的选区原文（用户所见即所复制）写系统剪贴板；成功才关菜单，失败可重试。 */
  const copySelection = () => {
    if (!contentMenu) return;
    useAppStore
      .getState()
      .writeClipboardText(contentMenu.text)
      .then(() => closeRewriteMenu())
      .catch((e) => {
        console.warn("复制到剪贴板失败", e);
        showClipHint("复制失败，请重试");
      });
  };

  /** 剪切：实读编辑面当前选区写剪贴板——CM 装饰会把 DOM 选区映射为整个源码节点，菜单捕获的
   *  渲染文本 ≠ 删除范围，「剪贴板 = 被删内容」必须恒成立；先写成功再删，失败中止防文本丢失。 */
  const cutSelection = async () => {
    if (!contentMenu) return;
    const selected = currentEditorSelection();
    if (!selected?.text) return;
    try {
      await useAppStore.getState().writeClipboardText(selected.text);
    } catch (e) {
      console.warn("剪切写入剪贴板失败", e);
      showClipHint("剪切失败，请重试");
      return;
    }
    editEditorRange(selected.from, selected.to, "");
    closeRewriteMenu();
  };

  /** 粘贴：读系统剪贴板——有可操作选区则替换，否则插到光标/右键位置；空剪贴板/失败不动作且
   *  菜单保留（可重试），防误删选区。 */
  const pasteIntoSelection = async () => {
    let clip = "";
    try {
      clip = await useAppStore.getState().readClipboardText();
    } catch (e) {
      console.warn("读取剪贴板失败", e);
      showClipHint("粘贴失败，请重试");
      return;
    }
    if (!clip) return;
    if (contentMenu?.text.trim() && contentMenu.selectionLive) {
      const selected = currentEditorSelection();
      if (selected) editEditorRange(selected.from, selected.to, clip);
    } else {
      insertAtCaret(clip);
    }
    closeRewriteMenu();
  };

  /** 划词 AI 改写提交（评论框 Enter / 发送按钮共用）：入队面板后关菜单。 */
  const submitRewrite = () => {
    if (!contentMenu) return;
    useChatPanelStore.getState().queueNoteRewrite({
      noteFile: file,
      label: noteTitleFromFile(file),
      selectedText: contentMenu.text.trim(),
      comment: rewriteComment.trim(),
    });
    closeRewriteMenu();
  };

  /** Frontmatter 解析：content 变（输入/外部刷新）→ 面板数据即时重解析，形成「编辑/外部修改即刷新」闭环。 */
  const parsed = useMemo(() => parseFrontmatter(content), [content]);

  /** 笔记协作 presence：打开/关闭/切笔记时上报「正在看这篇笔记」，对端据此展示协作者。 */
  useEffect(() => {
    if (isCollab) useCollabStore.getState().notePresence(file);
    else useCollabStore.getState().notePresence(null);
    return () => useCollabStore.getState().notePresence(null);
  }, [isCollab, file]);

  /** 同看这篇笔记的在线协作者（presence 聚焦命中，或本端编辑面包含该笔记；卷标含用户色）。 */
  const collabPeers = useCollabStore((s) => s.peers);
  const notePeers = useMemo(
    () =>
      isCollab
        ? collabPeers.filter(
            (p) =>
              (p.presence?.file === file && p.presence?.view === "note") ||
              p.presence?.editingNotes?.includes(file),
          )
        : EMPTY_PEERS,
    [isCollab, collabPeers, file],
  );

  /** 面板编辑提交：新 data 拼回完整 content，走既有 handleChange（debounce 保存/冲突条/外部感知全复用，零新机制）。 */
  const handlePropertiesUpdate = (next: Record<string, unknown>) => {
    try {
      handleChange(stringifyFrontmatter(next, parsed.body));
    } catch (e) {
      // stringify 异常（不应发生）：不污染 content，记录日志便于排查（诊断）
      console.error("[frontmatter] stringify error:", e, next);
    }
  };

  const { tagCandidates, requestTagCandidates } = useVaultTagCandidates();

  /** 笔记链接打开/新建（公共接线簇，见 hooks/useVaultLinkHandlers；本编辑器不做画布定位）。 */
  const { handleOpenWikiNote, isVaultPathNote, handleOpenVaultPathNote, handleCreateNote } =
    useVaultLinkHandlers();
  // 统一渲染引擎的链接/定位回调（回调全部稳定化，防随输入重建装饰）
  const noteMarkdownLinks: MarkdownEditorLinks = useMemo(
    () => ({
      onOpenNote: handleOpenWikiNote,
      isVaultPathNote,
      onOpenVaultPathNote: handleOpenVaultPathNote,
      onCreateNote: handleCreateNote,
    }),
    [handleOpenWikiNote, isVaultPathNote, handleOpenVaultPathNote, handleCreateNote],
  );

  /** 反链：全仓库 .md 中引用本文档的笔记（自身排除）；索引缓存 + 指纹增量刷新，扫描开销毫秒级。
   * 只在「切换打开的笔记」时扫描——不随仓库文件变化重扫（根除全量风暴），磁盘为真相自愈。
   * 扫描失败静默降级留空，不阻塞编辑。 */
  const noteName = noteTitleFromFile(file);
  const [backlinks, setBacklinks] = useState<BacklinkRow[]>([]);
  useEffect(() => {
    if (!noteName) return;
    let cancelled = false;
    setBacklinks([]);
    void useVaultStore
      .getState()
      .scanWikiBacklinks(noteName, file)
      .then((rows) => {
        if (!cancelled) setBacklinks(rows.filter((r) => r.file !== file));
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [file, noteName]);

  /** 「添加笔记属性」= 一键插入 frontmatter 格式模板（`---\n---\n` 包裹区），用户自行填写；
   * 已有 frontmatter 或格式错误时不重复插入。CRLF 文件用 \r\n 模板，防换行混用。 */
  const addFrontmatterTemplate = () => {
    if (!parsed.ok || parsed.fmPrefix !== "") return;
    const eol = content.includes("\r\n") ? "\r\n" : "\n";
    handleChange("---" + eol + "---" + eol + eol + content);
  };

  /** 菜单打开时是否捕获到选区（第一级菜单形态：完整菜单 vs 仅粘贴）。 */
  const hasSelection = !!contentMenu?.text.trim();

  return (
    <div
      ref={editorRootRef}
      data-note-file={preview ? undefined : file}
      className="h-full flex flex-col"
      style={{ background: "var(--bg-primary)" }}
      onContextMenu={handleContentContextMenu}
    >
      {/* 顶部条：右侧编辑/预览切换（保存状态已移至面板 header）。高度与表格/画布工具栏统一（py-1.5）。 */}
      <div
        className="px-3 py-1.5 flex items-center gap-1.5 text-xs flex-shrink-0 select-none"
        style={{ borderBottom: "1px solid var(--border)", color: "var(--text-muted)" }}
      >
        <span className="ml-auto flex items-center gap-2 flex-shrink-0">
          {/* 插件贡献区：笔记工具条右侧（list 槽，priority 降序） */}
          <SlotListMount slot="toolbar/note/right" />
          {/* 协作协作者：同看这篇笔记的在线用户（用户色卷标，点击定位到其选中位——暂只展示） */}
          {notePeers.length > 0 && (
            <span className="flex items-center gap-1 flex-shrink-0">
              {notePeers.slice(0, 4).map((p) => (
                <span
                  key={p.peerId}
                  className="flex items-center gap-1 rounded px-1 py-0.5 text-[11px]"
                  style={{
                    color: p.color,
                    background: `${p.color}1f`,
                    border: `1px solid ${p.color}55`,
                  }}
                  title={`${p.nickname}${p.deviceName ? `（${p.deviceName}）` : ""} 打开了本笔记`}
                >
                  <span
                    className="inline-block w-1.5 h-1.5 rounded-full"
                    style={{ background: p.color }}
                  />
                  {p.nickname}
                </span>
              ))}
              {notePeers.length > 4 && (
                <span className="text-[11px]" style={{ color: "var(--text-muted)" }}>
                  +{notePeers.length - 4}
                </span>
              )}
            </span>
          )}
          {/* 预览模式提示：显示在切换按钮左侧 */}
          {preview && (
            <span className="text-[11px]" style={{ color: "var(--text-muted)" }}>
              双击内容进入编辑模式
            </span>
          )}
          {/* 固定笔图标：未进入编辑（预览态）淡色，进入编辑后金色高亮（表达激活态） */}
          <button
            onClick={() => setPreview((v) => !v)}
            title={preview ? "切换到编辑" : "切换到只读"}
            className="p-0.5 rounded hover:opacity-80"
            style={{ color: preview ? "var(--text-muted)" : "var(--accent)" }}
          >
            <Pencil size={14} />
          </button>
          {/* 「···」更多选项：笔记属性面板入口（统一弹层 PopupLayer：锚定 + 钳制 + Esc/外点关闭） */}
          <span className="flex-shrink-0">
            <button
              ref={menuTriggerRef}
              onClick={() => menu.toggle()}
              title="更多选项"
              className="p-0.5 rounded hover:opacity-80"
              style={{ color: menu.anchor ? "var(--accent)" : "var(--text-muted)" }}
            >
              <MoreHorizontal size={15} />
            </button>
            <PopupLayer
              anchor={menu.anchor}
              onClose={menu.close}
              triggerRef={menuTriggerRef}
              widthClass="w-36"
            >
              <button
                className="w-full flex items-center gap-2 px-2 py-1.5 text-xs hover:opacity-80"
                style={{ color: "var(--text-primary)" }}
                onClick={() => {
                  addFrontmatterTemplate();
                  menu.close();
                }}
                title="在内容顶部插入 frontmatter 格式模板（---\\n---\\n），自行填写属性"
              >
                {/* 图标列占位与「源码模式」对齐（Check 图标列同宽） */}
                <span className="w-3.5 flex-shrink-0" />
                添加笔记属性
              </button>
              <button
                className="w-full flex items-center gap-2 px-2 py-1.5 text-xs hover:opacity-80"
                style={{ color: "var(--text-primary)" }}
                onClick={() => {
                  setSourceMode((v) => !v);
                  menu.close();
                }}
                title="源码模式：编辑区显示 Markdown 源码"
              >
                <span className="w-3.5 flex-shrink-0">
                  {sourceMode && <Check size={12} style={{ color: "var(--accent)" }} />}
                </span>
                源码模式
              </button>
              <button
                className="w-full flex items-center gap-2 px-2 py-1.5 text-xs hover:opacity-80"
                style={{ color: "var(--text-primary)" }}
                onClick={() => {
                  menu.close();
                  setHistoryOpen(true);
                }}
                title="查看本笔记的历史版本并回滚"
              >
                <span className="w-3.5 flex-shrink-0" />
                历史记录
              </button>
            </PopupLayer>
          </span>
        </span>
      </div>

      {/* 属性区：胶囊行式融入正文顶部（可点击编辑）；渲染/实时预览编辑模式显示，源码模式由 textarea
          显示 YAML 原文不重复显示；无 frontmatter 时也显示空态「添加属性」行（内联添加首个属性）；格式错误时显示红条 */}
      {!sourceMode && !loadError && (
        <NotePropertiesView
          data={parsed.data}
          parseError={!parsed.ok}
          onUpdate={handlePropertiesUpdate}
          onOpenSource={() => setSourceMode(true)}
          tagCandidates={tagCandidates}
          onRequestTagCandidates={requestTagCandidates}
        />
      )}

      {loadError ? (
        <div className="flex-1 flex items-center justify-center text-sm" style={{ color: "#f87171" }}>
          读取笔记失败，请确认文件存在
        </div>
      ) : sourceMode ? (
        /* 源码模式：完整 Markdown 源码 textarea（含 frontmatter）；切换回实时预览编辑时内容经 content 双向同步。
           未激活编辑（preview）时只读（阅读/编辑分离，仅可查看源码），双击激活后进入可编辑源码模式 */
        <textarea
          data-note-content
          value={content}
          onChange={(e) => handleChange(e.target.value)}
          readOnly={preview}
          onDoubleClick={(e) => {
            // 双击激活编辑：清除浏览器默认的双击选中单词，光标留在双击位置，不选中文本
            const ta = e.currentTarget;
            const pos = ta.selectionStart;
            ta.setSelectionRange(pos, pos);
            window.getSelection()?.removeAllRanges();
            setPreview(false);
          }}
          spellCheck={false}
          placeholder="笔记内容（Markdown）"
          className="flex-1 w-full resize-none outline-none p-4 text-sm leading-relaxed"
          style={{
            background: "var(--bg-primary)",
            color: "var(--text-primary)",
            // 只读（未激活编辑）时光标用默认指针，非文本光标
            cursor: preview ? "default" : "text",
          }}
        />
      ) : (
        /* 只读实时视图 / 实时预览编辑：同一 CodeMirror 引擎，readOnly 动态切换（不重建 → 预览⇄编辑
           零跳变、选区/滚动/协作绑定全保留）。只读态 widget 恒渲染（表格/数学/HTML/勾选框等全部显示），
           双击/铅笔进入编辑；编辑器自身样式见 styles/index.css；border 与源码模式对齐（1px），
           accent 高亮 = 进入编辑模式（与源码模式聚焦时一致） */
        <div
          data-note-content
          className="markdown-body flex-1 overflow-auto"
          style={{
            background: "var(--bg-primary)",
            color: "var(--text-primary)",
            border: preview ? "1px solid var(--input-border)" : "1px solid var(--accent)",
          }}
          onDoubleClick={() => {
            // 只读态双击进入编辑：先清除浏览器默认的双击文本选中（选中单词），再切换，避免残留选中
            if (!preview) return;
            window.getSelection()?.removeAllRanges();
            setPreview(false);
          }}
        >
          <NoteBodyEditor
            file={file}
            content={content}
            syncSeq={view?.syncSeq ?? 0}
            binding={view?.binding ?? null}
            editorViewRef={cmViewRef}
            readOnly={preview}
            interactiveCheckbox
            links={noteMarkdownLinks}
            // 协作挂载分歧：干净 → 收敛会话基准到协作基线（不置脏不写盘——磁盘落盘只发生在
            // 真实内容变化：用户编辑/远端合入经 onBodyChange 保存链，挂载收敛不覆盖磁盘，
            // 防空/陈旧基线打开即清空笔记）；有未落盘编辑 → 本地正文写回 ytext（本地最新者胜）
            onCollabDivergence={(ytextText) => session?.handleCollabDivergence(ytextText)}
            onBodyChange={(md) => session?.applyBody(md)}
          />
        </div>
      )}

      {/* 反向链接区（编辑器内容区下方，独立于属性区）：引用本文档的笔记列表，点击打开引用方；
          空 = 无引用时也显示该区（空态提示） */}
      <div
        className="flex-shrink-0 px-3 py-2 select-none"
        style={{ borderTop: "1px solid var(--border)" }}
      >
        <div className="text-xs mb-1" style={{ color: "var(--text-muted)" }}>
          反向链接{backlinks.length > 0 ? `（${backlinks.length}）` : ""}
        </div>
        {backlinks.length > 0 ? (
          <div className="flex flex-col gap-0.5 max-h-40 overflow-auto">
            {backlinks.map((b) => (
              <button
                key={b.file}
                className="flex items-center gap-1 text-xs text-left truncate hover:opacity-80"
                style={{ color: "var(--accent)" }}
                onClick={() => useAppStore.getState().openNote(b.file, b.title)}
                title={`打开「${b.title}」`}
              >
                <span className="truncate">{b.title}</span>
              </button>
            ))}
          </div>
        ) : (
          <div className="text-xs" style={{ color: "var(--text-muted)" }}>
            暂无引用
          </div>
        )}
      </div>

      {/* 底部状态条：字数统计 + 剪贴板操作提示（2.5s 自动清除） */}
      <div
        className="px-3 py-1 text-[11px] flex-shrink-0 select-none"
        style={{ borderTop: "1px solid var(--border)", color: "var(--text-muted)" }}
      >
        {content.length} 字
        {clipHint && (
          <span className="ml-2" style={{ color: "#f87171" }}>
            {clipHint}
          </span>
        )}
      </div>

      {/* 正文右键菜单（预览切编辑后选区已还原，一套菜单通用）：有选区 = 复制/剪切/粘贴 + 分隔线 +
          AI 处理置底；空白处（无选区）= 仅粘贴（插到光标/右键位置）。AI 处理确认后进入评论输入框
          （repositionDeps 换内容），提交 → 改写请求入队面板（queueNoteRewrite） */}
      {contentMenu && (
        <Menu
          x={contentMenu.x}
          y={contentMenu.y}
          onClose={closeRewriteMenu}
          widthClass={rewriteOpen ? "w-72" : "w-40"}
          contentClassName="p-1.5"
          repositionDeps={[rewriteOpen]}
        >
          {!rewriteOpen ? (
            hasSelection ? (
              <>
                <MenuItem onClick={copySelection}>
                  <Copy size={14} className="flex-shrink-0" /> 复制
                </MenuItem>
                {contentMenu.selectionLive && (
                  <MenuItem onClick={cutSelection}>
                    <Scissors size={14} className="flex-shrink-0" /> 剪切
                  </MenuItem>
                )}
                <MenuItem onClick={pasteIntoSelection}>
                  <ClipboardPaste size={14} className="flex-shrink-0" /> 粘贴
                </MenuItem>
                <MenuDivider />
                <MenuItem onClick={() => setRewriteOpen(true)}>
                  <Wand2 size={14} className="flex-shrink-0" /> AI 处理
                </MenuItem>
              </>
            ) : (
              <MenuItem onClick={pasteIntoSelection}>
                <ClipboardPaste size={14} className="flex-shrink-0" /> 粘贴
              </MenuItem>
            )
          ) : (
            <div>
              <textarea
                autoFocus
                value={rewriteComment}
                onChange={(e) => setRewriteComment(e.target.value)}
                placeholder="追加评论/要求（可选，如：语气更专业）"
                rows={3}
                spellCheck={false}
                className="w-full resize-none outline-none rounded border px-2 py-1.5 text-xs leading-relaxed"
                style={{
                  background: "var(--input-bg)",
                  color: "var(--text-primary)",
                  borderColor: "var(--input-border)",
                }}
                onKeyDown={(e) => {
                  // Enter 确认 / Shift+Enter 换行；IME 组合期间 Enter 上屏不触发
                  if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                    e.preventDefault();
                    submitRewrite();
                  }
                }}
              />
              <div className="flex justify-end gap-1 mt-1.5">
                <button
                  onClick={closeRewriteMenu}
                  className="px-2 py-1 rounded text-xs hover:opacity-80"
                  style={{ color: "var(--text-secondary)" }}
                >
                  取消
                </button>
                <button
                  onClick={submitRewrite}
                  className="px-2 py-1 rounded text-xs"
                  style={{ background: "var(--accent)", color: "var(--accent-fg)" }}
                >
                  发送到面板
                </button>
              </div>
            </div>
          )}
        </Menu>
      )}

      {/* 历史面板（「···」→ 历史记录；画布/表格共用同一 HistoryModal） */}
      <HistoryModal
        kind="note"
        file={file}
        open={historyOpen}
        onClose={() => setHistoryOpen(false)}
        onRollback={handleNoteRollback}
      />
    </div>
  );
}
