/** 面板消息 refs → chip 去重键（file）：模块级稳定函数，供 ChatMessageBubble memo 生效 */
const refKeyOfPanelRef = (r: { label: string }) =>
  (r as unknown as { file: string }).file;

/**
 * AI 对话面板。
 *
 * IDE 式侧边聊天，无头样式：
 * - 左侧会话列表：搜索 + 会话行（点击切换、行内删除）；面板够宽时与对话区并列，
 *   拖窄即改为浮层弹出（覆盖对话区，不挤压）；顶部「历史会话」按钮手动开关
 * - 顶部一行：左侧面板内联错误提示（仅出错时占位）+ 右侧「新建会话 / 历史会话 / 压缩」图标按钮
 * - 中部消息流：Markdown 公共渲染、流式指示、自动滚底
 * - 底部输入区：textarea（Enter 发送 / Shift+Enter 换行，支持 @引用标签）
 *   + Agent 选择（图标 + Agent 名）+ 模型选择（图标 + 模型名）+ 发送/停止按钮
 *
 * 分层：组件只走 chatPanelStore / settingsStore / vaultStore，不直调 service。
 * 当前打开笔记不经本组件传递：发送时由 chatPanelStore 以尾部上下文块随请求注入（见 runExchange）。
 */
import {
  AlertCircle,
  AlertTriangle,
  ArrowUp,
  Bot,
  Cpu,
  FilePlus,
  History,
  Layers,
  MessageSquare,
  RefreshCw,
  Square,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { useSettingsStore, selectDefaultModelDisplay } from "@/stores/settingsStore";
import { useAutoScrollFollow } from "@/hooks/useAutoScrollFollow";
import {
  insertMentionTag,
  splitMentions,
  type MentionSeg,
} from "@/utils/text";
import { Spinner } from "@/components/common/primitives";
import { Input } from "@/components/common/Input";
import { ChatMessageBubble } from "@/components/common/ChatMessageBubble";
import { Button, IconButton } from "@/components/common/Button";
import { SlotListMount } from "@/components/plugins/SlotHost";
import type { MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";
import { MentionTextarea } from "@/components/common/MentionTextarea";
import { JumpToBottomButton } from "@/components/common/JumpToBottomButton";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { ModelSelect } from "@/components/common/ModelSelect";
import { CHAT_UNAVAILABLE_TEXT, ERROR_PREFIX } from "@/constants/chat";
import { useChatRuntime } from "@/hooks/useChatRuntime";
import { relTime } from "@/utils/time";
import { useBackHandler } from "@/hooks/useBackHandler";
import { VaultAtPicker, type VaultPickTarget } from "@/components/common/VaultAtPicker";
import { openVaultPath } from "@/components/common/FileKindIcon";
import { noteTitleFromFile } from "@/utils/filename";
import { assistantReplyText } from "@/utils/agentSteps";
import { compactionMarkerIndex } from "@/utils/compaction";
import { CompactionMarker } from "@/components/common/CompactionMarker";
import { useVaultLinkHandlers } from "@/hooks/useVaultLinkHandlers";
import type { EditorChatMessage, EditorChatMessageRef } from "@/types";

/** 空消息数组（模块级常量：避免 selector 新引用导致无限重渲染）。 */
const EMPTY_MESSAGES: EditorChatMessage[] = [];

/** 面板宽度低于此值即改为浮层弹出会话列表（侧栏 228px，再窄就没地方并列两列了）。 */
const SIDEBAR_MIN_PANEL_WIDTH = 560;

/**
 * 输入框追加 @标签：前文非空且不以空格结尾时才补一个分隔空格，标签后恒带一个尾随空格。
 */
function appendMentionTags(prev: string, tags: string[]): string {
  const sep = prev && !prev.endsWith(" ") ? " " : "";
  return prev + sep + tags.join(" ") + " ";
}

/**
 * 划词 → 面板输入框指令文本（中性化描述）：笔记路径 + 划词原文 + 用户要求。
 * 不预设「改写」意图——用户划词提出要求，AI 自行判断用工具修改、解释还是其他；
 * 工具可用性由 Agent 模式开关决定。注入仓库路径帮助 AI 精确匹配目标笔记
 * （edit_file 的 path 参数支持路径匹配，同名笔记不混淆）。
 */
function buildRewritePrompt(r: {
  noteFile: string;
  selectedText: string;
  comment: string;
}): string {
  const lines = [
    `用户发来笔记（${r.noteFile}）中的以下文本：`,
    "",
    r.selectedText,
  ];
  if (r.comment.trim()) {
    lines.push("", "要求：", r.comment.trim());
  }
  return lines.join("\n");
}

export function AiChatPanel() {
  const sessions = useChatPanelStore((s) => s.sessions);
  const activeSessionId = useChatPanelStore((s) => s.activeSessionId);
  const streaming = useChatPanelStore((s) => s.streaming);
  const modelOverride = useChatPanelStore((s) => s.modelOverride);
  const effortOverride = useChatPanelStore((s) => s.effortOverride);
  const error = useChatPanelStore((s) => s.error);
  const send = useChatPanelStore((s) => s.send);
  const stop = useChatPanelStore((s) => s.stop);
  const renameSession = useChatPanelStore((s) => s.renameSession);
  const rollbackTo = useChatPanelStore((s) => s.rollbackTo);
  const regenerate = useChatPanelStore((s) => s.regenerate);
  const compactSession = useChatPanelStore((s) => s.compactSession);
  /** 本会话是否正在压缩（store 存的是「压缩中的会话 id」；其他会话压缩时本会话不显示转圈/不禁用）。 */
  const compactingThis = useChatPanelStore(
    (s) => s.compacting !== null && s.compacting === s.activeSessionId,
  );
  const compactingAny = useChatPanelStore((s) => s.compacting !== null);
  const newSession = useChatPanelStore((s) => s.newSession);
  const openSession = useChatPanelStore((s) => s.openSession);
  const deleteSession = useChatPanelStore((s) => s.deleteSession);
  const setAgentId = useChatPanelStore((s) => s.setAgentId);
  const setModelOverride = useChatPanelStore((s) => s.setModelOverride);
  const setEffortOverride = useChatPanelStore((s) => s.setEffortOverride);
  const clearError = useChatPanelStore((s) => s.clearError);
  // 会话落盘失败状态条（退避重试期间持续可见；成功落盘自动清除）
  const persistError = useChatPanelStore((s) => s.persistError);

  const active = sessions.find((s) => s.id === activeSessionId);
  const messages = active?.messages ?? EMPTY_MESSAGES;
  // 对话能力（对话核心插件提供）：未启用时整面板降级为提示占位
  const chatRuntime = useChatRuntime();
  // 压缩标记行插入位（-1 = 无注解/注解失效）：与历史重建同判定
  const compactionMarkerIdx = compactionMarkerIndex(messages, active?.compaction);
  // 顶部标题：激活会话名（新对话态无会话 → 「新对话」）
  const activeTitle = active?.title || "新对话";
  // Agent：激活会话的会话级引用；新对话态（无激活会话）读 draft（发送首条消息时固化）
  const draftAgentId = useChatPanelStore((s) => s.draftAgentId);
  const agentId = active?.agentId ?? draftAgentId;

  const [input, setInput] = useState("");
  // 会话列表侧栏：默认展开，顶部「历史会话」按钮开关
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [sessionQuery, setSessionQuery] = useState("");
  /**
   * 面板是否窄到放不下「侧栏 + 对话区」两列：窄面板里侧栏改为浮层弹出覆盖对话区
   * （并列会把对话区挤成一条），并在拖窄的那一刻自动收起，避免浮层突然盖住对话。
   *
   * 面板根节点经回调 ref 存 state（对话能力未启用时渲染占位、不挂根节点，恢复后才挂上，
   * 节点变化要能重新起观察器）；初始宽度在布局阶段先量一次，避免首帧按宽面板渲染出侧栏。
   */
  const [panelRoot, setPanelRoot] = useState<HTMLDivElement | null>(null);
  const [narrow, setNarrow] = useState(false);
  /** 上一帧是否为窄面板：只在「宽 → 窄」的跨越那一刻收起侧栏，
   *  否则窄态下任何一次尺寸微调都会把用户刚打开的浮层关掉。 */
  const wasNarrowRef = useRef(false);
  useLayoutEffect(() => {
    if (!panelRoot) return;
    const apply = (width: number) => {
      if (width <= 0) return;
      const next = width < SIDEBAR_MIN_PANEL_WIDTH;
      setNarrow(next);
      if (next && !wasNarrowRef.current) setSidebarOpen(false);
      wasNarrowRef.current = next;
    };
    apply(panelRoot.clientWidth);
    const ro = new ResizeObserver((entries) => apply(entries[0]?.contentRect.width ?? 0));
    ro.observe(panelRoot);
    return () => ro.disconnect();
  }, [panelRoot]);
  /** 浮层态（窄面板）下的关闭路径：返回键与 Esc 都要能关（浮层盖住对话区，只靠点遮罩太钝）。 */
  const overlayOpen = narrow && sidebarOpen;
  useBackHandler(overlayOpen, () => {
    setSidebarOpen(false);
    return true;
  });
  useEffect(() => {
    if (!overlayOpen) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setSidebarOpen(false);
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [overlayOpen]);
  /** 侧栏列表：最近使用倒序 + 按列表里显示的标题过滤（无标题的会话按「未命名对话」参与匹配）。 */
  const listedSessions = useMemo(() => {
    const q = sessionQuery.trim().toLowerCase();
    const sorted = [...sessions].sort((a, b) => b.updatedAt - a.updatedAt);
    if (!q) return sorted;
    return sorted.filter((s) => (s.title ?? "未命名对话").toLowerCase().includes(q));
  }, [sessions, sessionQuery]);

  /** 切换会话：窄面板（浮层态）下顺手收起浮层——浮层盖着对话区，不收起会让人以为没切成功。 */
  const pickSession = (id: string) => {
    openSession(id);
    if (narrow) setSidebarOpen(false);
  };
  // 手动重新命名请求是否进行中（按钮旋转反馈 + 防重复点击）
  const [renaming, setRenaming] = useState(false);
  const handleRename = async () => {
    setRenaming(true);
    try {
      await renameSession();
    } finally {
      setRenaming(false);
    }
  };
  // assistant/user 消息的链接/定位回调：hook 统一 useMemo 稳定化（气泡 memo 生效前提）。
  // 回调全部来自 useVaultLinkHandlers（useCallback 稳定 + 内部 getState 实时读 noteList），无需响应 noteList 变化重建
  const {
    resolveWikiNote,
    handleOpenWikiNote,
    isVaultPathNote,
    handleOpenVaultPathNote,
    handleCreateNote,
    openCreatedNote,
  } = useVaultLinkHandlers();
  const chatMarkdownLinks = useMemo<MarkdownEditorLinks>(
    () => ({
      onOpenNote: handleOpenWikiNote,
      resolveWikiNote,
      isVaultPathNote,
      onOpenVaultPathNote: handleOpenVaultPathNote,
      onCreateNote: handleCreateNote,
      onOpenCreatedNote: openCreatedNote,
    }),
    [resolveWikiNote, handleOpenWikiNote, isVaultPathNote, handleOpenVaultPathNote, handleCreateNote, openCreatedNote],
  );
  // 气泡操作回调稳定化（memo 生效前提）；rollbackTo 为 store action 引用恒稳定，onRollback 直传
  const handleRegenerate = useCallback(() => void regenerate(), [regenerate]);
  // @chip 点击按类型打开引用目标（画布/笔记/表格应用内打开，其他文件/文件夹在文件管理器中打开；
  // 稳定引用，气泡 memo 生效前提）
  const handleRefChipClick = useCallback((file: string) => {
    openVaultPath(file);
  }, []);
  // 输入框内的 @引用（拖入/键入 @ 选择）：@标签 随 input 文本渲染，发送时按命中实例注入路径
  const [mentions, setMentions] = useState<EditorChatMessageRef[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // 键入 @ 唤起仓库选择器：atIdx = @ 位置（query = @ 之后的内容），坐标相对输入容器
  const [picker, setPicker] = useState<{
    x: number;
    y: number;
    openUp: boolean;
    yBottom: number;
    query: string;
  } | null>(null);
  const [atIdx, setAtIdx] = useState(-1);
  const inputWrapRef = useRef<HTMLDivElement>(null);

  // 拖入的笔记引用队列（FileExplorerPanel 拖拽笔记到本输入框）→ 输入框追加 @标签（去重）
  const pendingMentions = useChatPanelStore((s) => s.pendingMentions);
  useEffect(() => {
    if (pendingMentions.length === 0) return;
    const added = pendingMentions.filter((r) => !mentions.some((x) => x.file === r.file));
    if (added.length) {
      setMentions((prev) => [...prev, ...added]);
      setInput((prev) => appendMentionTags(prev, added.map((r) => `@${r.label}`)));
    }
    useChatPanelStore.getState().clearPendingMentions();
  }, [pendingMentions, mentions]);

  // 笔记划词改写请求队列（NoteEditor 划词右键确认）→ 输入框追加改写指令文本块
  const pendingRewrites = useChatPanelStore((s) => s.pendingRewrites);
  useEffect(() => {
    if (pendingRewrites.length === 0) return;
    const prompts = pendingRewrites.map((r) => buildRewritePrompt(r));
    setInput((prev) => prev + (prev.trim() ? "\n\n" : "") + prompts.join("\n\n"));
    useChatPanelStore.getState().clearPendingRewrites();
  }, [pendingRewrites]);

  // 挂载/重挂对齐会话（不 force，load 幂等守卫兜底）：面板重挂（布局切换/关闭再打开）不得
  // 清空进行中会话——流式引擎在 store 层持续运行，重挂后原样续上；仓库真实切换时
  // sessionVaultKey 不匹配，load 自会完整重读盘（覆盖 selectVault/selectSpace 中途异常跳过的场景）。
  // 门控按仓库身份（空间模式下 vaultRoot 恒 null，按 root 门控会让空间内面板不加载会话）
  const vaultIdentity = useAppStore((s) => s.vaultIdentity);
  useEffect(() => {
    if (!vaultIdentity) return;
    void useChatPanelStore.getState().load();
  }, [vaultIdentity]);

  // 智能滚动跟随：贴底自动跟随新消息；上翻停止跟随 + 「新消息」回底按钮（与画布对话节点共用 hook）
  const scrollRef = useRef<HTMLDivElement>(null);
  const lastContent = messages.length ? messages[messages.length - 1].content : "";
  const { handleScroll, jumpToBottom, showJumpToBottom } = useAutoScrollFollow(scrollRef, [
    messages.length,
    lastContent,
  ]);

  const providers = useSettingsStore((s) => s.config.providers);
  const defaultModelDisplay = useSettingsStore(selectDefaultModelDisplay);
  // Agent 候选（配置在 设置 → Agent，仓库级 .atelyx/agents.json；发送时实时解析系统提示词/工具）
  const agents = useSettingsStore((s) => s.agents);

  // 输入框 overlay 分段：@引用 → 圆角标签段（可删除），其余普通文本段
  const segments = splitMentions(
    input,
    mentions.map((r) => ({ nodeId: r.file, text: `@${r.label}` }))
  );

  const handleSend = () => {
    const text = input.trim();
    // 压缩进行中拦截在清空草稿之前（与画布 handleSend 同口径；store 侧另有竞态兜底）
    if (!text || streaming || compactingAny) return;
    setInput("");
    setMentions([]);
    setPicker(null);
    setAtIdx(-1);
    void send(text, mentions);
  };

  // 仓库选择器选中 → @标签 插入（@ 到光标间过滤词替换、分隔空格、尾随空格、光标复位，与画布同语义）。
  // `atIdx`/光标是「待替换区间」的渲染期事实；插入位置在 `setInput(prev => ...)` 内按 `prev` 计算——
  // 渲染期闭包的 `input` 已含上一次入队结果，两次插入同 tick 到达时会互相覆盖。
  const handleVaultPick = (t: VaultPickTarget) => {
    const caret = textareaRef.current?.selectionStart ?? input.length;
    const insertAt = Math.min(Math.max(atIdx, 0), input.length);
    const end = Math.max(caret, insertAt);
    const label = t.name.toLowerCase().endsWith(".md") ? noteTitleFromFile(t.path) : t.name;
    const mentionText = `@${label}`;
    let caretAfter = 0;
    setInput((prev) => {
      const { text, caret: next } = insertMentionTag(prev, insertAt, end, mentionText);
      caretAfter = next;
      return text;
    });
    setMentions((prev) => [...prev, { file: t.path, label }]);
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (ta) {
        ta.focus();
        ta.setSelectionRange(caretAfter, caretAfter);
      }
    });
    setPicker(null);
    setAtIdx(-1);
  };

  // 胶囊被移除（MentionTextarea 已删文本 + 复位光标）→ 引用层清理：按实例移出 mentions（同路径多枚胶囊只删一处）
  const removeMention = (seg: MentionSeg) => {
    if (seg.mention) {
      const file = seg.mention.nodeId;
      setMentions((prev) => {
        const idx = prev.findIndex((m) => m.file === file);
        if (idx < 0) return prev;
        return prev.filter((_, i) => i !== idx);
      });
    }
  };

  // 对话能力未启用（对话核心插件停用）：整面板降级为提示占位（历史会话仍在磁盘，恢复后照常可见）
  if (!chatRuntime) {
    return (
      <div
        className="h-full flex flex-col items-center justify-center gap-2 px-6 text-center"
        style={{ background: "var(--bg-primary)", color: "var(--text-secondary)" }}
      >
        <AlertTriangle size={18} />
        <span className="text-xs">{CHAT_UNAVAILABLE_TEXT}</span>
      </div>
    );
  }

  /** 侧栏内容（宽面板内联 / 窄面板浮层共用同一份）：搜索 + 会话行（点击切换、行内删除）。 */
  const sidebarBody = (
    <>
      <div
        className="px-2.5 py-2 flex-shrink-0"
        style={{ borderBottom: "1px solid var(--border-subtle)" }}
      >
        <Input
          value={sessionQuery}
          onChange={(e) => setSessionQuery(e.target.value)}
          placeholder="搜索会话"
          spellCheck={false}
        />
      </div>
      <div className="flex-1 min-h-0 overflow-y-auto p-2 flex flex-col gap-0.5">
        {listedSessions.map((s) => {
          const isActiveSession = s.id === activeSessionId;
          return (
            <div
              key={s.id}
              className={`group flex items-center rounded-[var(--radius-sm)] ${
                isActiveSession ? "" : "hover:bg-[var(--bg-tertiary)]"
              }`}
              style={
                isActiveSession
                  ? { background: "var(--accent-soft)", boxShadow: "inset 2px 0 0 var(--accent)" }
                  : undefined
              }
            >
              <Button
                variant="ghost"
                size="sm"
                className="flex-1 min-w-0 justify-start items-start"
                onClick={() => pickSession(s.id)}
                title={s.title ?? "未命名对话"}
              >
                <span
                  className="block truncate text-xs font-medium"
                  style={{ color: isActiveSession ? "var(--accent)" : "var(--text-primary)" }}
                >
                  {s.title ?? "未命名对话"}
                </span>
                <span
                  className="block text-micro mt-0.5"
                  style={{ color: "var(--text-muted)", fontFamily: "var(--font-mono)" }}
                >
                  {relTime(s.updatedAt)}
                </span>
              </Button>
              {/* focus-visible:opacity-100：默认 hidden 态下仍可 Tab 到，键盘用户必须看得见焦点 */}
              <IconButton
                className="mr-1 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 [@media(hover:none)]:opacity-100"
                variant="danger"
                size="xs"
                icon={<Trash2 size={12} />}
                label={`删除会话 ${s.title ?? ""}`}
                onClick={() => deleteSession(s.id)}
              />
            </div>
          );
        })}
        {listedSessions.length === 0 && (
          <div className="px-2 py-2 text-micro" style={{ color: "var(--text-muted)" }}>
            {sessionQuery.trim() ? "没有匹配的会话" : "暂无历史会话"}
          </div>
        )}
      </div>
    </>
  );

  return (
    <div
      ref={setPanelRoot}
      className="h-full flex overflow-hidden relative"
      style={{ background: "var(--bg-primary)", color: "var(--text-primary)" }}
    >
      {/* 会话列表：宽面板与对话区并列；窄面板改为浮层弹出（覆盖对话区，不再挤压） */}
      {sidebarOpen &&
        (narrow ? (
          <>
            <div
              className="absolute inset-0 z-10"
              style={{ background: "var(--scrim)" }}
              onClick={() => setSidebarOpen(false)}
            />
            <aside
              className="absolute left-0 top-0 bottom-0 z-20 w-[228px] flex flex-col min-h-0 border-r shadow-[var(--shadow-pop)]"
              style={{ borderColor: "var(--border-subtle)", background: "var(--bg-secondary)" }}
            >
              {sidebarBody}
            </aside>
          </>
        ) : (
          <aside
            className="w-[228px] flex-shrink-0 flex flex-col min-h-0 border-r"
            style={{ borderColor: "var(--border-subtle)", background: "var(--bg-secondary)" }}
          >
            {sidebarBody}
          </aside>
        ))}

      {/* 对话列：顶部行 + 消息流 + 输入区 */}
      <div className="flex-1 min-w-0 flex flex-col overflow-hidden relative">
        {/* 顶部无头行：左侧会话标题（出错时显示错误提示）+ 右侧会话管理按钮 */}
        <div
          className="px-2 py-1.5 border-b flex items-center gap-2 flex-shrink-0 min-h-9"
          style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
          data-tauri-drag-region
        >
          <div className="flex-1 min-w-0 flex items-center gap-1.5">
            <MessageSquare
              size={13}
              className="flex-shrink-0"
              style={{ color: "var(--text-muted)" }}
            />
            {error || persistError ? (
              <span
                className="flex items-center gap-1 min-w-0 text-xs"
                style={{ color: "var(--danger)" }}
                title={persistError && !error ? `失败于 ${new Date(persistError.at).toLocaleTimeString()}` : undefined}
              >
                <AlertCircle size={13} className="flex-shrink-0" />
                <span className="truncate">{error ?? persistError?.message}</span>
                {error && (
                  <IconButton
                    className="flex-shrink-0"
                    variant="danger"
                    size="xs"
                    icon={<X size={12} />}
                    label="清除"
                    onClick={clearError}
                  />
                )
              }
              </span>
            ) : (
              <>
                <span
                  className="truncate text-xs font-medium"
                  style={{ color: "var(--text-primary)" }}
                  title={activeTitle}
                >
                  {activeTitle}
                </span>
                {/* 手动重新命名：按全部会话记录请求 LLM 生成标题（新对话态/流式中禁用）；请求中旋转 + 防重复点击 */}
                {active && !streaming && (
                  <IconButton
                    className="flex-shrink-0"
                    variant="subtle"
                    size="xs"
                    icon={
                      renaming ? (
                        <Spinner size={12} />
                      ) : (
                        <RefreshCw size={12} />
                      )
                    }
                    label={renaming ? "正在生成标题…" : "重新命名（按全部会话记录生成标题）"}
                    onClick={() => void handleRename()}
                    disabled={renaming}
                  />
                )}
              </>
            )}
          </div>
          <div className="flex items-center gap-0.5 flex-shrink-0" data-tauri-drag-region="false">
            <IconButton
              onClick={newSession}
              variant="ghost"
              size="md"
              icon={<FilePlus size={15} />}
              label="新建会话"
            />
            <IconButton
              onClick={() => setSidebarOpen((v) => !v)}
              variant="ghost"
              size="md"
              icon={<History size={15} />}
              label={sidebarOpen ? "收起会话列表" : "展开会话列表"}
              aria-expanded={sidebarOpen}
              style={{ color: sidebarOpen ? "var(--accent)" : "var(--text-secondary)" }}
            />
            {/* 压缩会话历史：把对话总结为检查点，压缩后的请求历史由摘要代替（消息本体不动）；
                仅激活会话有历史时可用；流式/压缩进行中禁用；转圈只显示在真正压缩的会话上 */}
            <IconButton
              onClick={() => void compactSession()}
              disabled={!active || streaming || compactingAny}
              variant="ghost"
              size="md"
              icon={
                compactingThis ? (
                  <Spinner size={15} />
                ) : (
                  <Layers size={15} />
                )
              }
              label={compactingThis ? "正在压缩会话历史" : "压缩会话历史"}
            />
            {/* 插件贡献区：AI 对话面板顶部右侧动作区（list 槽，priority 降序） */}
            <SlotListMount slot="toolbar/aichat/right" />
          </div>
        </div>

        {/* 消息流（relative 容器承载回底按钮，结构与画布对话节点一致） */}
        <div className="relative flex-1 min-h-0 flex flex-col">
          <div
            ref={scrollRef}
            onScroll={handleScroll}
            className="flex-1 overflow-auto px-3 py-3 space-y-1 min-h-0"
          >
            {messages.length === 0 ? (
              <div className="h-full flex flex-col items-center justify-center gap-2 text-xs select-none" style={{ color: "var(--text-muted)" }}>
                <MessageSquare size={24} strokeWidth={1.5} className="opacity-60" />
                开始新的 AI 对话
              </div>
            ) : (
              messages.map((m, i) => {
                // 流式指示：最后一条 assistant 占位且正在流式
                const isStreamingMsg = streaming && m.role === "assistant" && i === messages.length - 1;
                // 重新生成：仅最后一条完整 AI 回复可用（同画布 canRegenerate）
                const canRegenerate =
                  !streaming &&
                  m.role === "assistant" &&
                  i === messages.length - 1 &&
                  assistantReplyText(m).trim() !== "" &&
                  !m.content.startsWith(ERROR_PREFIX);
                return (
                  <div key={m.id} className="space-y-3">
                    {/* 压缩标记：插在折叠块之后首条消息之前（被压缩的原文仍显示在标记上方） */}
                    {i === compactionMarkerIdx && active?.compaction && (
                      <CompactionMarker compaction={active.compaction} />
                    )}
                    <ChatMessageBubble
                      role={m.role}
                    displayContent={m.role === "user" ? m.displayContent ?? m.content : undefined}
                    refs={m.refs}
                    refKeyOf={refKeyOfPanelRef}
                    onRefChipClick={handleRefChipClick}
                    content={m.content}
                    steps={m.steps}
                    isStreaming={isStreamingMsg}
                    markdownLinks={chatMarkdownLinks}
                    copyText={m.role === "user" ? (m.displayContent ?? m.content) : assistantReplyText(m)}
                    messageId={m.id}
                    canRollback={!isStreamingMsg && m.role === "assistant" && assistantReplyText(m).trim() !== ""}
                    onRollback={rollbackTo}
                    onRegenerate={canRegenerate ? handleRegenerate : undefined}
                  />
                  </div>
                );
              })
            )}

            {/* 压缩覆盖到对话末尾（刚压缩完的常态）：标记行渲染在列表末尾 */}
            {compactionMarkerIdx === messages.length && active?.compaction && (
              <CompactionMarker compaction={active.compaction} />
            )}
          </div>
          {showJumpToBottom && <JumpToBottomButton onClick={jumpToBottom} />}
        </div>

        {/* 底部输入区：不铺底色、只用一条 1px 上边分隔——输入框是这里唯一的「盒子」，
            底栏若再铺一层底色就与输入框叠成两个盒子 */}
        <div
          className="border-t flex-shrink-0 px-3 pt-2.5 pb-3"
          style={{ borderColor: "var(--border-subtle)" }}
        >
          {/* 输入框（data-chat-input = 文件面板拖拽文件/文件夹的落点：拖入即 @引用）：
          overlay 渲染 @标签（透明 textarea 承载输入，滚动同步 transform）；键入 @ 唤起仓库选择器。
          输入框是输入区唯一的盒子，内部纵向三段常规流：输入面 → 分隔线 → 工具排
          （工具排走常规流而非绝对定位——绝对定位会与超出的输入文字叠在一起） */}
          <div
            // 边框恒定不改：聚焦转金边在深底上呈"发光"观感，且叠加 2px 光环会形成同心双线，故聚焦不做边框变化
            className="relative rounded-[var(--radius-md)] border border-[var(--border)]"
            data-chat-input
            ref={inputWrapRef}
            style={{ background: "var(--bg-sunken)" }}
          >
            {picker && (
              <VaultAtPicker
                x={picker.x}
                y={picker.y}
                openUp={picker.openUp}
                yBottom={picker.yBottom}
                query={picker.query}
                onPick={handleVaultPick}
                onClose={() => setPicker(null)}
              />
            )}
            <MentionTextarea
              textareaRef={textareaRef}
              value={input}
              onChange={(v) => {
                setInput(v);
                // @ 后继续输入 → 实时过滤候选（query = @ 位置之后的内容）；
                // @ 锚字符已被删（退格/整体替换）→ 关闭选择器，防陈旧 atIdx 错位插入
                if (picker && atIdx >= 0) {
                  if (v[atIdx] !== "@") {
                    setPicker(null);
                    setAtIdx(-1);
                  } else {
                    setPicker((p) => (p ? { ...p, query: v.slice(atIdx + 1) } : p));
                  }
                }
              }}
              segments={segments}
              onRemoveMention={removeMention}
              onKeyDown={(e) => {
                // 键入 @ 唤起仓库文件/文件夹选择器（坐标相对输入容器；下方视口不足 → 向上弹出）
                if (e.key === "@") {
                  setAtIdx(textareaRef.current?.selectionStart ?? 0);
                  const taRect = textareaRef.current?.getBoundingClientRect();
                  const wrapRect = inputWrapRef.current?.getBoundingClientRect();
                  const x = (taRect?.left ?? 0) - (wrapRect?.left ?? 0);
                  const y = (taRect?.bottom ?? 0) - (wrapRect?.top ?? 0);
                  const yBottom = (wrapRect?.bottom ?? 0) - (taRect?.bottom ?? 0);
                  const openUp = window.innerHeight - (taRect?.bottom ?? 0) < 264;
                  setPicker({ x, y, openUp, yBottom, query: "" });
                }
                // Enter 发送 / Shift+Enter 换行；IME 组合期间 Enter 是「上屏候选词」而非发送
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  handleSend();
                }
              }}
              spellCheck={false}
              placeholder="输入消息，@ 引用文件，Enter 发送，Shift+Enter 换行"
              rows={5}
              overlayClassName="z-0 px-2 pt-3 pb-2 text-sm leading-relaxed"
              // focus:shadow-none 压掉全局 textarea:focus 的 2px 焦点环：输入面撑满输入盒内部，
              // 该环会紧贴盒子的 1px 金边形成同心双线；此处的聚焦提示由外层盒的边转金承担
              textareaClassName="w-full px-2 pt-3 pb-2 text-sm leading-relaxed focus:shadow-none"
            />

            {/* 工具排（盒子内底部，与输入面同属一块、不画分隔线）：左 Agent/模型；右 发送/停止 */}
            <div className="flex items-center gap-2 px-2 pt-1 pb-2">
              {/* Agent 选择：选中的 Agent 提供系统提示词与工具（发送时实时解析）；缺省「对话」= 普通对话 */}
              <DropdownSelect
                value={agentId ?? ""}
                onChange={(v) => setAgentId(v || undefined)}
                options={agents.map((a) => ({ value: a.id, label: a.name }))}
                // 未选择（旧数据/清空）= 缺省「对话」：占位显示对话、运行时按「对话」解析
                placeholder="对话"
                emptyText="暂无 Agent（设置 → Agent 新建）"
                prefixIcon={<Bot size={13} className="flex-shrink-0" />}
                title={agentId ? `Agent：${agents.find((a) => a.id === agentId)?.name ?? ""}` : "Agent：对话（缺省，普通对话；系统提示词与工具在 设置 → Agent 中配置）"}
                // 无边框幽灵态 + 按内容定宽（宽度上限 = 文字自然宽度，不撑满剩余空间）；
                // 不参与收缩——否则面板一窄，长模型名会把只有两个字的 Agent 名挤成「对…」
                // （max-w-45% 只是超长 Agent 名的兜底，正常名不会触到）
                className="h-6 px-2 rounded-[var(--radius-sm)] text-micro hover:bg-[var(--bg-tertiary)] w-fit max-w-[45%] flex-shrink-0 min-w-0"
                style={{ color: "var(--text-secondary)" }}
              />

              {/* 模型选择：两级菜单（模型 / 推理等级子面板，PopupLayer 统一弹层壳） */}
              <ModelSelect
                providers={providers}
                providerId={modelOverride?.providerId}
                model={modelOverride?.model}
                effort={effortOverride ?? undefined}
                onSelectModel={(sel) =>
                  sel
                    ? setModelOverride({ providerId: sel.providerId, model: sel.model })
                    : setModelOverride(null)
                }
                onSelectEffort={(effort) => setEffortOverride(effort ?? null)}
                defaultModelDisplay={defaultModelDisplay}
                prefixIcon={<Cpu size={13} className="flex-shrink-0" />}
                title={modelOverride ? `模型：${modelOverride.model}` : "模型：跟随仓库默认（点击选择/设置推理等级）"}
                // 同按内容定宽（上限 = 文字自然宽度）；空间不足时由它承担收缩（模型名最长，截断损失最小）
                className="h-6 px-2 rounded-[var(--radius-sm)] text-micro hover:bg-[var(--bg-tertiary)] w-fit min-w-0"
                style={{ color: "var(--text-secondary)" }}
              />
              {/* 右：发送 / 停止（图标 only，金色圆钮，流式中切换为停止）——mr-1 右缘留白不顶格；
                  任一会话压缩进行中即禁用（压缩与发送共用一个中止句柄，避免静默无效点击） */}
              <IconButton
                className="ml-auto"
                onClick={streaming ? stop : handleSend}
                disabled={compactingAny || (!streaming && !input.trim())}
                variant={streaming ? "secondary" : "primary"}
                size="md"
                icon={streaming ? <Square size={12} /> : <ArrowUp size={13} />}
                label={
                  compactingAny ? "正在压缩会话历史" : streaming ? "停止" : "发送 (Enter)"
                }
              />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

