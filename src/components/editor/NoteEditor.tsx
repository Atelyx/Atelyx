/**
 * `.md` 笔记编辑器（未打开画布时单击笔记打开）。
 *
 * 占据主编辑区（画布位置）：顶部文件操作条，正文 = 统一 CodeMirror 引擎
 * （默认只读实时视图，双击/铅笔进入实时预览编辑；「···」菜单切源码模式 textarea）。
 * - 加载：进入时读笔记正文（切换笔记重读）；加载完成前用户已输入则保留输入（不覆盖正在打的字）。
 * - 保存：输入 debounce 500ms 自动写回 `.md`；卸载/切走时 flush 未落盘输入（不静默丢弃）；
 *   写入完成时若已有更新输入则保持「保存中…」，避免误报「已自动保存」；状态写 vaultStore 由面板 header 展示。
 * - 分层：走 vaultStore（readNoteContent / saveNoteContent），不直调 service。
 */
import { Check, ClipboardPaste, Copy, MoreHorizontal, Pencil, Scissors, Wand2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { EditorView } from "@codemirror/view";
import { useVaultStore, lastFolderRenameTarget, lastNoteRenameTarget, isKnownNoteDiskContent, type NoteSaveStatus } from "@/stores/vaultStore";
import { useNoteUndoStore } from "@/stores/noteUndoStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useAppStore } from "@/stores/appStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { useCollabStore } from "@/stores/collabStore";
import { useNoteCollabStore } from "@/stores/noteCollabStore";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import type { BacklinkRow, CollabPeer } from "@/types";
import { parseFrontmatter, stringifyFrontmatter } from "@/utils/frontmatter";
import { noteTitleFromFile } from "@/utils/filename";
import { NotePropertiesView } from "@/components/editor/NotePropertiesView";
import { MarkdownEditor, type MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";
import { HistoryModal } from "@/components/history/HistoryModal";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useVaultLinkHandlers } from "@/hooks/useVaultLinkHandlers";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { useVaultTagCandidates } from "@/hooks/useVaultTagCandidates";
import { PopupLayer } from "@/components/common/PopupLayer";

type SaveState = NoteSaveStatus["state"];

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
  const readNoteContent = useVaultStore((s) => s.readNoteContent);
  const readNoteFresh = useVaultStore((s) => s.readNoteFresh);
  const saveNoteContent = useVaultStore((s) => s.saveNoteContent);
  // 外部修改感知：watcher note 事件 bump 序号（vaultStore.markNoteExternallyEdited），据此重读磁盘
  const externalEditSeq = useVaultStore((s) => s.externalNoteEdits[file] ?? 0);
  // 保存状态存 vaultStore（面板 header 展示；本组件只写不持）
  const noteSaveStatus = useVaultStore((s) => s.noteSaveStates[file]);
  const loadError = noteSaveStatus?.loadError ?? false;
  /** 协作态判定与应用身份：中转开关已开且已连接时，当前笔记进入 Yjs 协同编辑。 */
  const collabEnabled = useSettingsStore((s) => s.collabEnabled);
  const collabConnected = useCollabStore((s) => s.connected);
  const collabNickname = useSettingsStore((s) => s.collabNickname);
  const collabColor = useSettingsStore((s) => s.collabColor);
  const collabDevice = useSettingsStore((s) => s.deviceName);
  const isCollab = collabEnabled && collabConnected;
  /** 本端协作身份（协作态本地编辑计入参与作者集合用；id 规则与 history setHistoryAuthor 一致，展示可去重）。 */
  const localAuthor = useMemo<{ id: string; name: string; device: string }>(
    () => ({
      id: collabDevice || collabNickname || "用户",
      name: collabNickname || collabDevice || "用户",
      device: collabDevice || "",
    }),
    [collabNickname, collabDevice],
  );
  /** 当前笔记的协作文档绑定（后台 noteCollabStore 编排；下发给 MarkdownEditor 做 y-codemirror 绑定）。 */
  const collabBinding = useNoteCollabStore((s) => s.bindings[file]);
  const [content, setContent] = useState("");
  /** 编辑 / 预览切换（默认预览：打开即渲染；双击预览内容切回编辑；标题栏已显示文件名故顶部条不再重复）。 */
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
  /** 非用户编辑的 content 更新序号（加载完成/外部刷新/冲突重载时递增），MarkdownEditor 据此同步正文。 */
  const [editorSyncSeq, setEditorSyncSeq] = useState(0);
  /** 外部修改冲突：本地有未保存改动 + 磁盘已被外部改过。状态存 vaultStore 由面板 header 展示，期间暂停自动保存防覆盖。 */
  const conflictRef = useRef(false);
  const setConflictState = useCallback(
    (v: boolean) => {
      conflictRef.current = v;
      useVaultStore.getState().setNoteConflict(file, v);
    },
    [file],
  );
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  /** 保存状态写 store（面板 header 展示；组件不持 state）。 */
  const setSaveStatus = useCallback(
    (state: SaveState, isLoadError = false) =>
      useVaultStore.getState().setNoteSaveState(file, { state, loadError: isLoadError }),
    [file],
  );
  /** 最新输入（卸载时 flush 用，避免闭包拿到过期内容）。 */
  const contentRef = useRef("");
  /** 加载完成前用户是否已输入：输入优先，加载结果不覆盖正在打的字。 */
  const dirtyRef = useRef(false);
  /** 最后成功写盘的磁盘内容基准：外部修改感知据此区分「自写回放」与「真实外部变化」。 */
  const lastSavedRef = useRef("");
  /** 外部修改感知已处理到的序号（挂载时 = 当前值：加载 useEffect 已读到最新磁盘，只响应后续增量）。 */
  const processedSeqRef = useRef(externalEditSeq);
  /** 保存序号：写入完成时若已有更新的输入，保持「保存中…」而非误报「已自动保存」。 */
  const saveSeqRef = useRef(0);
  /** 卸载守卫：异步保存完成回调不再 setState（React 18 虽静默但属脏更新）。 */
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);
  /** 撤销/重做回放标记：applyNoteUndo 走 handleChange 复用保存/协作/冲突门控，
   *  但回放内容不得再记入撤销栈（防自我入栈形成撤销链）。 */
  const applyingUndoRef = useRef(false);
  /** 最近一次内容变化的来源作者（主作者）：协作远端合入 → 该协作者身份；本地编辑 → null（历史署当前用户）。
   *  历史按操作人署名用——远端合入落盘不署本端用户。 */
  const lastChangeAuthorRef = useRef<{ id: string; name: string; device: string } | null>(null);
  /** 自上次落盘以来参与内容变化的协作者集合（主作者 + 其余参与者，按 id 去重）：
   *  多协作者并发编辑合并进同一存档点时，历史版本以 coAuthors 记录全部操作人。
   *  落盘成功后清空（coalesce 合并由 history 层按 id 求并集，不在此累积）。 */
  const changeAuthorSetRef = useRef<Map<string, { id: string; name: string; device: string }>>(new Map());

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

  // 加载正文；切换笔记（file 变化）重新读取
  useEffect(() => {
    let cancelled = false;
    // 加载起始的序号快照：加载期间外部修改（序号移动）则丢弃本次结果，避免旧内容覆盖新磁盘
    const seqAtLoad = useVaultStore.getState().externalNoteEdits[file] ?? 0;
    setContent("");
    // 镜像清空 contentRef：与 content 恒同步（撤销栈以 contentRef 记「输入前全文」，切笔记不残留
    // 上一笔记内容被误记为 before；加载完成后再同步为真实内容，见下方 !dirty 分支）
    contentRef.current = "";
    // 切笔记：取消待弹的右键菜单（触发 pendingMenu effect cleanup 取消定时器），防旧坐标弹到新笔记
    setPendingMenu(null);
    // 编辑模式：清空编辑器（防加载窗口内旧笔记内容被误写到新文件，见 MarkdownEditor 同步机制）
    setEditorSyncSeq((s) => s + 1);
    setSaveStatus("idle");
    dirtyRef.current = false;
    void readNoteContent(file)
      .then((c) => {
        if (cancelled) return;
        // 加载期间外部已修改（序号移动）：放弃本次加载结果，外部感知 useEffect 会刷新（防旧内容覆盖新磁盘）
        if ((useVaultStore.getState().externalNoteEdits[file] ?? 0) !== seqAtLoad) return;
        // 基准 = 磁盘最新（即使输入优先不覆盖内容，后续自写回放/外部修改感知也以它为参照）
        lastSavedRef.current = c;
        if (!dirtyRef.current) {
          setContent(c);
          // 核心同步：contentRef 与加载内容一致——否则首次编辑 recordEdit 以残留 "" 记「输入前全文」，
          // Ctrl+Z 一步把整篇笔记清空（协作/非协作同一加载路径，行为一致）
          contentRef.current = c;
          // 编辑模式：加载完成同步编辑器（仅正文，frontmatter 不动）
          setEditorSyncSeq((s) => s + 1);
        }
      })
      .catch(() => {
        // 加载失败：若用户已输入（dirty），输入会随 debounce 写盘，保留编辑界面而非换错误页
        if (!cancelled && !dirtyRef.current) setSaveStatus("idle", true);
      });
    return () => {
      cancelled = true;
      // 卸载/切走：清除保存/冲突状态（面板 header 随视图不显示），再 flush 未落盘的输入（debounce 窗口内不静默丢弃）。
      // 组件已卸载不能再 setState，fire-and-forget 写盘即可。
      // 冲突未决时跳过：外部已修改且未明确选择，不覆盖外部修改（提示条已告知）
      useVaultStore.getState().setNoteSaveState(file, null);
      useVaultStore.getState().setNoteConflict(file, false);
      useVaultStore.getState().clearNoteConflictResolveReq(file);
      if (conflictRef.current) return;
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        // 文件已从仓库列表消失：软件内重命名（写到新文件）或真删除（跳过，防 writeNote 重建已删文件）
        const stillExists = useVaultStore
          .getState()
          .noteList.some((n) => n.file === file);
        const target =
          stillExists ? file : lastNoteRenameTarget(file) ?? lastFolderRenameTarget(file);
        if (target) {
          const pendingContent = contentRef.current;
          void saveNoteContent(target, pendingContent)
            .then(() => {
              // 清除挂起登记前比对内容：flush 在途期间若有新编辑重新登记（多面板/快速切回），
              // 不误清新值（与 vaultStore.flushPendingNotes「只清未被替换条目」同语义）
              if (useVaultStore.getState().pendingNoteContent[file] === pendingContent) {
                useVaultStore.getState().setPendingNoteContent(file, null);
              }
            })
            .catch((e) => console.error("笔记保存失败", e));
        }
      }
    };
  }, [file, readNoteContent, saveNoteContent, setSaveStatus]);

  // 外部修改感知：磁盘内容 ≠ 自身最后写盘基准（lastSavedRef） = 变化（自写回放或真实外部变化）。
  // 无本地改动 → 静默刷新为磁盘最新（实时同步，含应用内其他编辑面——画布文本节点——的写盘回波）；
  // 有本地改动 → 磁盘 = 应用级基线（应用内写盘）则静默保留本地输入，否则冲突提示 + 暂停自动保存，
  // 防覆盖外部修改
  useEffect(() => {
    if (externalEditSeq <= processedSeqRef.current) return;
    processedSeqRef.current = externalEditSeq;
    let cancelled = false;
    // 直读真实磁盘（绕过内容缓存——缓存可能滞后于在途写盘，用缓存比较会漏检/误判外部修改）
    void readNoteFresh(file)
      .then((disk) => {
        if (cancelled || !mountedRef.current || disk === lastSavedRef.current) return;
        if (isCollab) {
          // 多写者协作：对端/本端正收敛写盘是常态，不走单写者冲突模型。
          // 磁盘 = 本地收敛内容 → 对端写盘与本地一致：静默推进基准（后续增量跳过）不弹冲突；
          // 本地有未落盘编辑且磁盘不同 → 对端尚未收敛的写盘：保留本地，等 debounce 覆盖收敛；
          // 无本地编辑且磁盘不同 → 对端/外部刚落盘一版：收敛到磁盘（回退/合并），不弹冲突、
          // 不静默覆盖——内容随下次保存以「编辑」历史记录，外部内容不丢失。
          if (disk === contentRef.current) {
            lastSavedRef.current = disk;
            return;
          }
          if (dirtyRef.current) return;
          if (timerRef.current) {
            clearTimeout(timerRef.current);
            timerRef.current = null;
          }
          setContent(disk);
          contentRef.current = disk;
          lastSavedRef.current = disk;
          setSaveStatus("idle");
          // 编辑模式：以磁盘为基底重建 ytext（applyBody）并向房间重新收敛
          setEditorSyncSeq((s) => s + 1);
          return;
        }
        if (dirtyRef.current) {
          // 应用内其他编辑面写入（画布文本节点/保存为笔记）：静默保留本地输入，不弹「外部修改冲突」
          // （磁盘 = 应用最近已知内容，见 isKnownNoteDiskContent）；AI 文件工具写入不登记该基线，
          // 此处按真实外部修改弹冲突条（防静默覆盖 Agent 编辑，见 services/vault/aiFiles.writeVaultFile）；
          // 同时把 lastSavedRef 推进到该自写内容——挂起的 debounce 保存的写盘前校验
          // （handleChange 里「磁盘 ≠ lastSavedRef = 外部修改」）以此基线判定，不推进会把
          // 应用自写误判为外部修改弹冲突；推进后本地编辑按 LWW 覆盖应用自写（同编辑面语义）
          if (isKnownNoteDiskContent(file, disk)) {
            lastSavedRef.current = disk;
            return;
          }
          // 取消挂起的 debounce 保存（外部修改前已调度），防到点写盘覆盖外部修改
          if (timerRef.current) {
            clearTimeout(timerRef.current);
            timerRef.current = null;
          }
          setConflictState(true);
        } else {
          // 守卫：静默刷新仅当内存无更新的本地内容（contentRef === lastSavedRef）。当前不变量下
          // （handleChange 恒置 dirtyRef、所有内容变更路径同步 lastSavedRef）该分支不可达，属
          // 意图内防御——若未来出现不经 handleChange 的路径（直接 setContent）使内存新于基准
          // 而 dirty 为假，宁可弹冲突也不静默覆盖内存内容
          if (contentRef.current !== lastSavedRef.current) {
            setConflictState(true);
            return;
          }
          setContent(disk);
          contentRef.current = disk;
          lastSavedRef.current = disk;
          setSaveStatus("idle");
          // 编辑模式：外部静默刷新同步编辑器
          setEditorSyncSeq((s) => s + 1);
        }
      })
      .catch(() => {
        // 外部删除由窗口联动（loadFiles 后不在列表关闭，此处忽略
      });
    return () => {
      cancelled = true;
    };
  }, [externalEditSeq, file, readNoteFresh, setSaveStatus, setConflictState, isCollab]);

  /** 冲突「重新加载」：丢弃本地改动，恢复磁盘最新内容并解除冲突（直读真实磁盘，防命中陈旧缓存）。 */
  const reloadFromDisk = useCallback(() => {
    void readNoteFresh(file)
      .then((disk) => {
        if (!mountedRef.current) return;
        setContent(disk);
        contentRef.current = disk;
        lastSavedRef.current = disk;
        dirtyRef.current = false;
        setConflictState(false);
        setSaveStatus("idle");
        // 编辑模式：冲突「重新加载」同步编辑器
        setEditorSyncSeq((s) => s + 1);
      })
      .catch(() => {});
  }, [file, readNoteFresh, setConflictState, setSaveStatus]);

  /** 历史回滚完成：把编辑器拨到回滚后内容（记入磁盘基准，编辑器/预览/属性区刷新）。 */
  const handleNoteRollback = useCallback(
    (content: string) => {
      setContent(content);
      contentRef.current = content;
      lastSavedRef.current = content;
      dirtyRef.current = false;
      setConflictState(false);
      setSaveStatus("saved");
      // 编辑模式：回滚同步编辑器（协作态经 applyBody 重建 ytext）
      setEditorSyncSeq((s) => s + 1);
    },
    [setSaveStatus, setConflictState],
  );
  const saveLocalOverExternal = useCallback(() => {
    const v = contentRef.current;
    setConflictState(false);
    const seq = ++saveSeqRef.current;
    setSaveStatus("saving");
    void saveNoteContent(file, v)
      .then(() => {
        lastSavedRef.current = v;
        // 保存期间又输入了（新 seq 已接管）：dirty 保持 true，防后续外部修改静默覆盖正在打的字
        if (seq === saveSeqRef.current) dirtyRef.current = false;
        if (mountedRef.current && seq === saveSeqRef.current) setSaveStatus("saved");
      })
      .catch(() => {
        if (mountedRef.current) setSaveStatus("error");
      });
  }, [file, saveNoteContent, setConflictState, setSaveStatus]);

  /** 面板 header 冲突条按钮 → vaultStore 序号请求 → 本组件订阅执行（与 externalNoteEdits 同构）。
   * 首帧以当前序号为基线：只响应本实例挂载后发出的请求（防处理卸载前残留请求）。 */
  const resolveReq = useVaultStore((s) => s.noteConflictResolveReq[file]);
  const processedResolveSeqRef = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (processedResolveSeqRef.current === undefined) {
      processedResolveSeqRef.current = resolveReq?.seq ?? 0;
      return;
    }
    const cur = resolveReq?.seq ?? 0;
    if (cur <= processedResolveSeqRef.current) return;
    processedResolveSeqRef.current = cur;
    if (resolveReq?.keepLocal) saveLocalOverExternal();
    else reloadFromDisk();
  }, [resolveReq, reloadFromDisk, saveLocalOverExternal]);

  const handleChange = (v: string) => {
    // 无变化短路：属性面板改回原值/协作收敛内容一致等「空步」
    // 不入撤销栈、不置脏不抖状态（内容未变，无任何必要副作用）
    if (v === contentRef.current) return;
    // 捕获内容变化来源作者（历史按操作人署名）：协作远端合入 → 该协作者；
    // 本地编辑 → null（历史署当前用户）。debounce 落盘前若又本地编辑会覆盖为 null（本地者胜），
    // 与保存内容（最后一次 handleChange 的 v）一致。
    const remoteAuthor =
      isCollab && useNoteCollabStore.getState().isRemoteApplying()
        ? useNoteCollabStore.getState().lastRemoteAuthor(file)
        : null;
    lastChangeAuthorRef.current = remoteAuthor;
    // 参与作者集合（多协作者并发编辑同存档点 → 历史 coAuthors）：远端合入记该协作者，
    // 本地编辑记本端身份（仅协作态；非协作单作者不记）。
    if (isCollab) {
      changeAuthorSetRef.current.set(remoteAuthor?.id ?? localAuthor.id, remoteAuthor ?? localAuthor);
    }
    // 用户输入登记（撤销栈 + 挂起输入）：撤销回放（applyNoteUndo）不记栈；
    // 撤销栈记「本次输入前全文」（连续输入合并为一步，见 services/noteUndo）；
    // pendingNoteContent 供 flushAllPending/切仓库前把未落盘输入统一落盘 + 补历史
    if (!applyingUndoRef.current) {
      useNoteUndoStore.getState().recordEdit(file, contentRef.current);
      useVaultStore.getState().setPendingNoteContent(file, v);
    }
    dirtyRef.current = true;
    contentRef.current = v;
    setContent(v);
    // 协作态源码编辑同步：源码模式改动只更新 content（不经 yCollab），把正文写回 ytext 防切回
    // 实时预览被陈旧 ytext 回退（实时预览编辑 ytext 已由 yCollab 同步、frontmatter 面板不改正文，
    // 均无需在此同步；绑定残留期间持续同步，协作重开时 ytext 即当前正文）
    if (collabBinding && sourceMode) {
      useNoteCollabStore
        .getState()
        .syncLocalBody(file, parseFrontmatter(v).body.replace(/\r\n/g, "\n"));
    }
    // 冲突中：仅更新本地内容，暂停自动保存（等用户选「重新加载」或「保留本地并保存」，防静默覆盖外部修改）
    if (conflictRef.current) return;
    // 输入即有未落盘改动：显示「未保存」；真正写盘时才切「保存中…」
    setSaveStatus("edited");
    if (timerRef.current) clearTimeout(timerRef.current);
    const seq = ++saveSeqRef.current;
    timerRef.current = setTimeout(() => {
      // 保存前同步快照本版作者：异步写盘期间 refs 可能被下一窗口 handleChange 推进，
      // 快照保证版本署名 = 本次保存内容来源（防重叠保存窗口把下一窗口作者错署到本版）。
      // 主作者 = 最后内容来源（远端合入署该协作者、本地署本端）；coAuthors = 窗口内其余参与者；
      // 落盘成功且无新输入时清集合（见各分支 seq 守卫），coalesce 合并由 history 层按 id 求并集。
      const mainAuthor = lastChangeAuthorRef.current;
      const coAuthors = [...changeAuthorSetRef.current.values()].filter(
        (a) => a.id !== (mainAuthor?.id ?? localAuthor.id),
      );
      const commitHistory = () =>
        useVaultStore.getState().noteHistoryRecord(file, v, "edit", {
          ...(mainAuthor ? { authorOverride: mainAuthor } : {}),
          ...(coAuthors.length ? { coAuthors } : {}),
        });
      // 协作态：多写者落盘内容收敛一致，远端/对端正收敛写盘不应走「磁盘≠基准=外部修改」冲突预检
      // （会误报），只做增量跳过 + 直接保存收敛全文；真实外部整文件写入由外部感知 effect 兜底
      // （见其 collab 分支：不静默覆盖）。
      if (isCollab) {
        if (v === lastSavedRef.current) {
          dirtyRef.current = false;
          if (mountedRef.current && seq === saveSeqRef.current) setSaveStatus("saved");
          return;
        }
        setSaveStatus("saving");
        void saveNoteContent(file, v)
          .then(() => {
            lastSavedRef.current = v;
            // 协作态落盘完成：通知磁盘基线收敛（重建 doc 的挂起复位，见 noteDoc#markNoteDiskWrite）
            useNoteCollabStore.getState().notifyNoteDiskWrite(file);
            if (seq === saveSeqRef.current) dirtyRef.current = false;
            if (mountedRef.current && seq === saveSeqRef.current) setSaveStatus("saved");
            // 保存完成且无新输入：清除挂起登记（期间又有新输入则保留，由下一轮保存接管）
            if (seq === saveSeqRef.current) useVaultStore.getState().setPendingNoteContent(file, null);
            if (seq === saveSeqRef.current) changeAuthorSetRef.current.clear();
            commitHistory();
          })
          .catch(() => {
            if (mountedRef.current) setSaveStatus("error");
          });
        return;
      }
      // 写盘前校验磁盘基准：期间磁盘已被外部改动（disk ≠ lastSavedRef）则放弃本次写盘并转冲突提示，
      // 防 debounce 到点把外部修改覆盖掉（自写回放磁盘 = lastSavedRef，不受影响）。
      // 直读真实磁盘（绕过内容缓存——缓存可能滞后于在途写盘，用缓存比较会漏检外部修改）
      void readNoteFresh(file)
        .then((disk) => {
          if (!mountedRef.current || seq !== saveSeqRef.current) return;
          if (disk !== lastSavedRef.current) {
            setConflictState(true);
            return;
          }
          // 增量跳过：磁盘已包含与当前输入完全相同的内容（如输入后撤销回已保存状态），
          // 写盘纯属无操作——省去一次全量 fsync 写盘与随之而来的 watcher 回波
          if (v === lastSavedRef.current) {
            dirtyRef.current = false;
            if (mountedRef.current && seq === saveSeqRef.current) setSaveStatus("saved");
            return;
          }
          setSaveStatus("saving");
          void saveNoteContent(file, v)
            .then(() => {
              lastSavedRef.current = v;
              // 保存完成且无新输入：无未保存改动（dirtyRef 重置，后续外部修改恢复静默刷新语义）
              if (seq === saveSeqRef.current) dirtyRef.current = false;
              // 期间又有新输入（新定时器已接管）→ 不显示「已自动保存」；组件已卸载不再写状态
              if (mountedRef.current && seq === saveSeqRef.current) setSaveStatus("saved");
              // 保存完成且无新输入：清除挂起登记
              if (seq === saveSeqRef.current) useVaultStore.getState().setPendingNoteContent(file, null);
              if (seq === saveSeqRef.current) changeAuthorSetRef.current.clear();
              commitHistory();
            })
            .catch(() => {
              if (mountedRef.current) setSaveStatus("error");
            });
        })
        .catch(() => {
          // 写盘前读磁盘失败：文件可能已被外部删除，放弃写盘（不覆盖不重建），提示保存失败
          if (mountedRef.current && seq === saveSeqRef.current) setSaveStatus("error");
        });
    }, 500);
  };
  /** 最新 handleChange（属性面板请求 effect 用）：effect 依赖不追每次渲染重建的函数（与 applyUndoRef 同模式）。 */
  const handleChangeRef = useRef(handleChange);
  handleChangeRef.current = handleChange;

  /**
   * 撤销/重做应用：从当前文件撤销栈取目标全文 → 走 handleChange（复用保存/协作/冲突门控 +
   * debounce 落盘），置 applyingUndoRef 防回放内容自我入栈；再 bump editorSyncSeq 让
   * MarkdownEditor 以新正文重建（applyBody，不进 CM 撤销栈）。栈按 file 键隔离，
   * 只作用于当前编辑器文件，各文件互不混淆。
   */
  const applyNoteUndo = (dir: "undo" | "redo") => {
    const target =
      dir === "undo"
        ? useNoteUndoStore.getState().undo(file, contentRef.current)
        : useNoteUndoStore.getState().redo(file, contentRef.current);
    if (target === null) return;
    applyingUndoRef.current = true;
    handleChange(target);
    applyingUndoRef.current = false;
    // 撤销/重做是应持久化的编辑：登记挂起输入（handleChange 在 applyingUndoRef 下跳过登记，
    // 而 recordEdit 跳过与 pending 登记是两个维度——漏登记会在 500ms 内 flush 时落盘撤销前旧内容）
    useVaultStore.getState().setPendingNoteContent(file, target);
    setEditorSyncSeq((s) => s + 1);
  };

  /**
   * 笔记撤销/重做窗口级快捷键（编辑态 preview=false 时生效，协作/非协作一致）：
   * Mod-z / Mod-y / Mod-Shift-z → applyNoteUndo。撤销入口不依赖编辑器聚焦、不依赖
   * CM keymap——CM 内置 history 与 y-undo 均随 EditorView 销毁丢失（退出编辑→重进即清），
   * 持久栈按文件驻留 store；CM 与源码 textarea 均不再绑定撤销键（MarkdownEditor 分支
   * 已去除），无双撤销。画布快捷键仅在画布面板聚焦时启用，编辑态与画布聚焦互斥
   * （点画布即退出编辑），无冲突。
   * 目标过滤：属性面板/弹层输入框（input/select、非源码 textarea）交给浏览器原生撤销；
   * 其余目标（CM content、源码 textarea、双击重进后焦点落 body、工具栏）一律接管——
   * 编辑态下笔记即活动编辑面（点其它面板/弹窗即退出编辑），无越权撤销。
   */
  const applyUndoRef = useRef(applyNoteUndo);
  applyUndoRef.current = applyNoteUndo;
  useEffect(() => {
    if (preview) return;
    const onKeyDown = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      const key = e.key.toLowerCase();
      if (key !== "z" && key !== "y") return;
      const t = e.target as Element | null;
      if (t instanceof HTMLInputElement || t instanceof HTMLSelectElement) return;
      if (t instanceof HTMLTextAreaElement && !t.hasAttribute("data-note-content")) return;
      e.preventDefault();
      if (key === "z") {
        if (e.shiftKey) applyUndoRef.current("redo");
        else applyUndoRef.current("undo");
      } else {
        applyUndoRef.current("redo");
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [preview]);

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

  /** 编辑面区间替换原语：源码 textarea 以 contentRef 拼接走 handleChange（自动保存/冲突门控/
   *  协作 syncLocalBody 全复用；用 ref 防 await 剪贴板 IPC 窗口内的击键被旧闭包 content 丢弃）；
   *  CodeMirror dispatch（经 onBodyChange → 自动保存/协作同步/撤销栈）。 */
  const editEditorRange = (from: number, to: number, ins: string) => {
    if (sourceMode) {
      const ta = editorRootRef.current?.querySelector("textarea");
      if (!ta) return;
      handleChange(contentRef.current.slice(0, from) + ins + contentRef.current.slice(to));
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

  /** 属性面板属性编辑请求（序号制，同 resolveReq）→ 合并新 frontmatter 到编辑器实时正文，走
   * handleChange 全保存链（防抖/冲突/撤销/挂起输入/历史/协作全复用）——面板不直写，防未落盘
   * 正文被整文件覆盖、改动被挂起保存静默回滚。编辑器未挂载时请求无人消费（面板以
   * noteSaveStates 判定改走直写路径）。首帧以当前序号为基线，只响应本实例挂载后的请求。 */
  const propsEditReq = useVaultStore((s) => (file ? s.notePropsEditReq[file] : undefined));
  const processedPropsSeqRef = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (processedPropsSeqRef.current === undefined) {
      processedPropsSeqRef.current = propsEditReq?.seq ?? 0;
      return;
    }
    const cur = propsEditReq?.seq ?? 0;
    if (cur <= processedPropsSeqRef.current) return;
    processedPropsSeqRef.current = cur;
    if (!propsEditReq) return;
    // 正文不可信（加载失败 / 源模式手写坏 YAML）时拒绝合并：防覆盖正文或产生双重 frontmatter
    if (loadError || !parsed.ok) return;
    let merged: string;
    try {
      merged = stringifyFrontmatter(propsEditReq.data, parsed.body);
    } catch (e) {
      console.error("[frontmatter] stringify error:", e, propsEditReq.data);
      return;
    }
    handleChangeRef.current(merged);
    // 预写缓存：属性面板经 noteContents 订阅即时刷新（落盘由 handleChange 的 debounce 保存负责）
    useVaultStore.getState().stageNoteContent(file, merged);
  }, [propsEditReq, parsed, loadError, file]);

  /** 协作文档绑定：进入协作态且内容已加载时，以正文（body，LF）为基线绑定 Y.Doc；
   * 绑定幂等——已绑定（collabBinding 非空）不重复建 doc，多面板共享同一实例。
   * 身份（昵称/用户色）随设置变化可重设（bind 内部幂等更新 awareness）。 */
  useEffect(() => {
    if (!isCollab || collabBinding || !content) return;
    useNoteCollabStore.getState().bind(
      file,
      parsed.body.replace(/\r\n/g, "\n"),
      {
        name: collabNickname || collabDevice || "用户",
        color: collabColor || "#30bced",
      },
    );
  }, [isCollab, collabBinding, file, content, parsed.body, collabNickname, collabColor, collabDevice]);

  /** 解绑协作文档：切笔记/卸载时释放一个引用（多面板各释放一次）。 */
  useEffect(() => {
    return () => {
      if (useNoteCollabStore.getState().bindings[file]) {
        useNoteCollabStore.getState().unbind(file);
      }
    };
  }, [file]);

  /** 笔记协作 presence：打开/关闭/切笔记时上报「正在看这篇笔记」，对端据此展示协作者。 */
  useEffect(() => {
    if (isCollab) useCollabStore.getState().notePresence(file);
    else useCollabStore.getState().notePresence(null);
    return () => useCollabStore.getState().notePresence(null);
  }, [isCollab, file]);

  /** 同看这篇笔记的在线协作者（presence.file 命中；卷标含用户色）。 */
  const collabPeers = useCollabStore((s) => s.peers);
  const notePeers = useMemo(
    () =>
      isCollab
        ? collabPeers.filter((p) => p.presence?.file === file && p.presence?.view === "note")
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
                  title={`${p.nickname}${p.deviceName ? `（${p.deviceName}）` : ""} 正在编辑本笔记`}
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
          <MarkdownEditor
            body={parsed.body}
            syncSeq={editorSyncSeq}
            editorViewRef={cmViewRef}
            readOnly={preview}
            interactiveCheckbox
            links={noteMarkdownLinks}
            // 协作挂载分歧：干净 → 收敛 content 到协作基线（不置脏不写盘——磁盘落盘只发生在
            // 真实内容变化：用户编辑/远端合入经 onBodyChange→handleChange 保存链，挂载收敛不覆盖磁盘，
            // 防空/陈旧基线打开即清空笔记）；有未落盘编辑 → 本地正文写回 ytext（本地最新者胜）
            onCollabDivergence={(ytextText) => {
              const bodyLF = parsed.body.replace(/\r\n/g, "\n");
              if (contentRef.current === lastSavedRef.current) {
                const body = parsed.body.includes("\r\n")
                  ? ytextText.replace(/\n/g, "\r\n")
                  : ytextText;
                const full = parsed.fmPrefix + body;
                if (full === contentRef.current) return;
                contentRef.current = full;
                lastSavedRef.current = full;
                setContent(full);
                setSaveStatus("idle");
              } else {
                useNoteCollabStore.getState().syncLocalBody(file, bodyLF);
              }
            }}
            // 协作态门控：仅协作激活时进入 Yjs 编辑（collabBinding 可能因协作关闭/断线残留，若不过滤，
            // 残留绑定的陈旧 ytext 会成为编辑模型源，源码模式编辑（只改 content）切回实时预览被回退）
            collab={isCollab ? collabBinding : undefined}
            onBodyChange={(md) => {
              // CRLF 文件：编辑器统一输出 LF，拼回前转回文件原有换行，防 frontmatter/正文混用
              const body = parsed.body.includes("\r\n") ? md.replace(/\n/g, "\r\n") : md;
              handleChange(parsed.fmPrefix + body);
            }}
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
