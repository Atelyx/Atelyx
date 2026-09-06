/** 面板消息 refs → chip 去重键（file）：模块级稳定函数，供 ChatMessageBubble memo 生效 */
const refKeyOfPanelRef = (r: { label: string }) =>
  (r as unknown as { file: string }).file;

/**
 * 右侧边栏 AI 对话面板。
 *
 * IDE 式侧边聊天，无头样式：
 * - 顶部一行：左侧面板内联错误提示（仅出错时占位）+ 右侧「新建会话 / 历史会话」图标按钮
 * - 中部消息流：Markdown 公共渲染、流式指示、自动滚底
 * - 底部输入区：textarea（Enter 发送 / Shift+Enter 换行，支持 @引用标签）
 *   + Agent 选择（图标 + Agent 名）+ 模型选择（图标 + 模型名）+ 发送/停止按钮
 *
 * 分层：组件只走 chatPanelStore / settingsStore / vaultStore，不直调 service。
 * 当前打开笔记不经本组件传递：发送时由 chatPanelStore 以尾部上下文块随请求注入（见 runExchange）。
 */
import {
  AlertCircle,
  Bot,
  Cpu,
  FilePlus,
  History,
  Loader2,
  MessageSquare,
  RefreshCw,
  SendHorizontal,
  Square,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useCallback, useMemo, useRef, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useChatPanelStore } from "@/stores/chatPanelStore";
import { useSettingsStore, selectDefaultModelDisplay } from "@/stores/settingsStore";
import { useAutoScrollFollow } from "@/hooks/useAutoScrollFollow";
import {
  splitMentions,
  type MentionSeg,
} from "@/utils/text";
import { ChatMessageBubble } from "@/components/common/ChatMessageBubble";
import type { MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";
import { MentionTextarea } from "@/components/common/MentionTextarea";
import { JumpToBottomButton } from "@/components/common/JumpToBottomButton";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { ModelSelect } from "@/components/common/ModelSelect";
import { PopupLayer } from "@/components/common/PopupLayer";
import { ERROR_PREFIX } from "@/constants/chat";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { VaultAtPicker, type VaultPickTarget } from "@/components/common/VaultAtPicker";
import { openVaultPath } from "@/components/common/FileKindIcon";
import { noteTitleFromFile } from "@/utils/filename";
import { assistantReplyText } from "@/utils/agentSteps";
import { useVaultLinkHandlers } from "@/hooks/useVaultLinkHandlers";
import type { EditorChatMessage, EditorChatMessageRef } from "@/types";

/** 空消息数组（模块级常量：避免 selector 新引用导致无限重渲染）。 */
const EMPTY_MESSAGES: EditorChatMessage[] = [];

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
  const newSession = useChatPanelStore((s) => s.newSession);
  const openSession = useChatPanelStore((s) => s.openSession);
  const deleteSession = useChatPanelStore((s) => s.deleteSession);
  const setAgentId = useChatPanelStore((s) => s.setAgentId);
  const setModelOverride = useChatPanelStore((s) => s.setModelOverride);
  const setEffortOverride = useChatPanelStore((s) => s.setEffortOverride);
  const clearError = useChatPanelStore((s) => s.clearError);
  const vaultRoot = useAppStore((s) => s.vaultRoot);

  const active = sessions.find((s) => s.id === activeSessionId);
  const messages = active?.messages ?? EMPTY_MESSAGES;
  // 顶部标题：激活会话名（新对话态无会话 → 「新对话」）
  const activeTitle = active?.title || "新对话";
  // Agent：激活会话的会话级引用；新对话态（无激活会话）读 draft（发送首条消息时固化）
  const draftAgentId = useChatPanelStore((s) => s.draftAgentId);
  const agentId = active?.agentId ?? draftAgentId;

  const [input, setInput] = useState("");
  // 历史会话浮层：锚定 History 按钮（PopupLayer 统一壳，外点/Esc 关闭）
  const historyBtnRef = useRef<HTMLButtonElement>(null);
  const {
    anchor: historyAnchor,
    toggle: toggleHistory,
    close: closeHistory,
  } = usePopupAnchor(historyBtnRef);
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
  const { handleOpenWikiNote, isVaultPathNote, handleOpenVaultPathNote, handleCreateNote } =
    useVaultLinkHandlers();
  const chatMarkdownLinks = useMemo<MarkdownEditorLinks>(
    () => ({
      onOpenNote: handleOpenWikiNote,
      isVaultPathNote,
      onOpenVaultPathNote: handleOpenVaultPathNote,
      onCreateNote: handleCreateNote,
    }),
    [handleOpenWikiNote, isVaultPathNote, handleOpenVaultPathNote, handleCreateNote],
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
  // sessionVaultId 不匹配，load 自会完整重读盘（覆盖 selectVault 中途异常跳过的场景）
  useEffect(() => {
    if (!vaultRoot) return;
    closeHistory();
    void useChatPanelStore.getState().load(useAppStore.getState().vaultId);
  }, [vaultRoot, closeHistory]);

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
    if (!text || streaming) return;
    setInput("");
    setMentions([]);
    setPicker(null);
    setAtIdx(-1);
    void send(text, mentions);
  };

  // 仓库选择器选中 → @标签 插入（@ 到光标间过滤词替换、分隔空格、尾随空格、光标复位，与画布同语义）
  const handleVaultPick = (t: VaultPickTarget) => {
    const caret = textareaRef.current?.selectionStart ?? input.length;
    const insertAt = Math.min(Math.max(atIdx, 0), input.length);
    const end = Math.max(caret, insertAt);
    const before = input.slice(0, insertAt);
    const sep = before && !/\s$/.test(before) ? " " : "";
    const label = t.name.toLowerCase().endsWith(".md") ? noteTitleFromFile(t.path) : t.name;
    const mentionText = `@${label}`;
    setInput((prev) => prev.slice(0, insertAt) + sep + mentionText + " " + prev.slice(end));
    setMentions((prev) => [...prev, { file: t.path, label }]);
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (ta) {
        ta.focus();
        const pos = insertAt + sep.length + mentionText.length + 1;
        ta.setSelectionRange(pos, pos);
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

  return (
    <div
      className="h-full flex flex-col overflow-hidden relative"
      style={{ background: "var(--bg-secondary)", color: "var(--text-primary)" }}
    >
      {/* 顶部无头行：左侧会话标题（出错时显示错误提示）+ 右侧会话管理按钮 */}
      <div
        className="px-2 py-1.5 border-b flex items-center gap-2 flex-shrink-0 min-h-9"
        style={{ borderColor: "var(--border)" }}
        data-tauri-drag-region
      >
        <div className="flex-1 min-w-0 flex items-center gap-1.5">
          <MessageSquare
            size={13}
            className="flex-shrink-0"
            style={{ color: "var(--text-muted)" }}
          />
          {error ? (
            <span
              className="flex items-center gap-1 min-w-0 text-xs"
              style={{ color: "#f87171" }}
            >
              <AlertCircle size={13} className="flex-shrink-0" />
              <span className="truncate">{error}</span>
              <button
                onClick={clearError}
                title="清除"
                className="p-0.5 hover:opacity-70 flex-shrink-0"
              >
                <X size={12} />
              </button>
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
                <button
                  onClick={() => void handleRename()}
                  disabled={renaming}
                  title={renaming ? "正在生成标题…" : "重新命名（按全部会话记录生成标题）"}
                  aria-label="重新命名"
                  className="p-0.5 rounded hover:opacity-80 flex-shrink-0 disabled:opacity-60 disabled:cursor-default disabled:hover:opacity-60"
                  style={{ color: "var(--text-muted)" }}
                >
                  {renaming ? (
                    <Loader2 size={12} className="animate-spin" />
                  ) : (
                    <RefreshCw size={12} />
                  )}
                </button>
              )}
            </>
          )}
        </div>
        <div className="flex items-center gap-0.5 flex-shrink-0" data-tauri-drag-region="false">
          <button
            onClick={newSession}
            title="新建会话"
            aria-label="新建会话"
            className="p-1.5 rounded hover:opacity-80"
            style={{ color: "var(--text-secondary)" }}
          >
            <FilePlus size={15} />
          </button>
          <button
            ref={historyBtnRef}
            onClick={toggleHistory}
            title="历史会话"
            aria-label="历史会话"
            className="p-1.5 rounded hover:opacity-80"
            style={{ color: historyAnchor ? "var(--accent)" : "var(--text-secondary)" }}
          >
            <History size={15} />
          </button>
        </div>
      </div>

      {/* 历史会话浮层（当前面板全部会话，按最近使用倒序；点击切换、可删除）——PopupLayer 锚定 History 按钮 */}
      <PopupLayer
        anchor={historyAnchor}
        onClose={closeHistory}
        triggerRef={historyBtnRef}
        widthClass="w-64"
      >
        <div className="max-h-72 overflow-auto">
          <div className="px-3 py-1 text-[11px]" style={{ color: "var(--text-muted)" }}>
            历史会话
          </div>
          {[...sessions].sort((a, b) => b.updatedAt - a.updatedAt).map((s) => (
            <div
              key={s.id}
              className="flex items-center gap-1 px-2 py-1.5 text-xs"
              style={{
                background: s.id === activeSessionId ? "var(--bg-tertiary)" : undefined,
                color: "var(--text-primary)",
              }}
            >
              <button
                onClick={() => {
                  openSession(s.id);
                  closeHistory();
                }}
                className="flex-1 text-left truncate min-w-0 hover:opacity-80"
                title={s.title ?? "未命名对话"}
              >
                {s.title ?? "未命名对话"}
              </button>
              <button
                onClick={() => deleteSession(s.id)}
                title="删除会话"
                aria-label={`删除会话 ${s.title ?? ""}`}
                className="p-0.5 hover:opacity-70 flex-shrink-0"
                style={{ color: "var(--text-muted)" }}
              >
                <Trash2 size={12} />
              </button>
            </div>
          ))}
          {sessions.length === 0 && (
            <div className="px-3 py-2 text-[11px]" style={{ color: "var(--text-muted)" }}>
              暂无历史会话
            </div>
          )}
        </div>
      </PopupLayer>

      {/* 消息流（relative 容器承载回底按钮，结构与画布对话节点一致） */}
      <div className="relative flex-1 min-h-0 flex flex-col">
        <div
          ref={scrollRef}
          onScroll={handleScroll}
          className="flex-1 overflow-auto px-3 py-3 space-y-3 min-h-0"
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
                <ChatMessageBubble
                  key={m.id}
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
              );
            })
          )}
        </div>
        {showJumpToBottom && <JumpToBottomButton onClick={jumpToBottom} />}
      </div>

      {/* 底部输入区：textarea + 内部底部工具条（左：提示词/模型；右：发送） */}
      <div className="border-t flex-shrink-0" style={{ borderColor: "var(--border)" }}>
        {/* textarea + 内部工具条（absolute 叠放，textarea 有 pb 留白） */}
        <div className="relative">
          {/* 输入框内底部工具条：左 Agent/模型；右 发送/停止 */}
          <div className="absolute inset-x-0 bottom-3 z-10 px-1.5 flex items-center gap-0.5">
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
            className="px-1.5 py-1 rounded text-xs hover:opacity-80 w-28 min-w-0"
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
            className="px-1.5 py-1 rounded text-xs hover:opacity-80 w-28 min-w-0"
            style={{ color: "var(--text-secondary)" }}
          />

          <div className="flex-1" />
          {/* 右：发送 / 停止（图标 only，金色圆钮，流式中切换为停止）——mr-1 右缘留白不顶格 */}
          <button
            onClick={streaming ? stop : handleSend}
            disabled={!streaming && !input.trim()}
            title={streaming ? "停止" : "发送 (Enter)"}
            aria-label={streaming ? "停止" : "发送"}
            className="p-1.5 rounded flex-shrink-0 mr-1 disabled:opacity-40"
            style={{
              background: streaming ? "var(--bg-tertiary)" : "var(--accent)",
              color: streaming ? "var(--text-secondary)" : "var(--accent-fg)",
            }}
          >
            {streaming ? <Square size={13} /> : <SendHorizontal size={13} />}
          </button>
        </div>

        {/* 输入框（data-chat-input = 文件面板拖拽文件/文件夹的落点：拖入即 @引用）：
        overlay 渲染 @标签（透明 textarea 承载输入，滚动同步 transform）；键入 @ 唤起仓库选择器 */}
        <div className="relative" data-chat-input ref={inputWrapRef}>
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
            overlayClassName="z-0 px-2 pt-3 pb-12 text-sm leading-relaxed"
            textareaClassName="w-full px-2 pt-3 pb-12 text-sm leading-relaxed"
          />
        </div>
        </div>
      </div>
    </div>
  );
}

