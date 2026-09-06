/**
 * 聊天消息气泡（画布对话节点 / AI 对话面板共用）。
 *
 * 收敛两处的重复实现：气泡容器 + @引用胶囊（按原文位置内嵌）+ 附件缩略 + 思考折叠 +
 * Markdown 渲染 + 操作按钮组（复制/回到此处/分支/重新生成）。
 * user/assistant 消息统一 Markdown 渲染；差异由 props 表达：@胶囊 点击行为
 * （画布 = 定位节点，面板 = 打开笔记）、附件（仅画布有）、分支/重新生成（入口各自）。
 * 气泡配色/内边距/流式占位两入口恒同值，内联为模块级常量（调用点不再传，memo 引用恒稳定）。
 *
 * memo 生效前提：markdownLinks 必须 useMemo 稳定化、onRollback/onBranch 等
 * 回调 useCallback——流式期间历史消息靠引用不变跳过重渲染（assistant 消息无 refs/
 * 附件，引用天然稳定，重渲染最贵的 MarkdownView 得以跳过）。
 */
import { memo, useMemo, useState, type CSSProperties, type ReactNode } from "react";
import {
  AlertCircle,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  FileSearch,
  FileText,
  FilePlus,
  GitBranch,
  Globe,
  History,
  Link,
  Loader2,
  PenLine,
  RefreshCw,
  Search,
  Wrench,
} from "lucide-react";
import { ThinkingBlock } from "@/components/common/ThinkingBlock";
import { groupAgentSteps } from "@/utils/agentSteps";
import type { AgentStep, Attachment, ToolRun } from "@/types";
import { MarkdownView, type MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";

/** 两入口恒同值的气泡样式（画布/面板同款）：模块级单例，memo 浅比较引用恒稳定。 */
const USER_BUBBLE_CLASS = "bg-[var(--bg-tertiary)]";
const ASSISTANT_BUBBLE_STYLE: CSSProperties = {
  background: "var(--bg-primary)",
  border: "1px solid var(--border)",
};
const BUBBLE_PADDING_CLASS = "px-3 py-2 text-sm leading-relaxed min-w-0";
/** 流式且无内容时的占位（画布/面板同款）。 */
const STREAMING_PLACEHOLDER = (
  <span
    className="inline-flex items-center gap-1 text-xs"
    style={{ color: "var(--text-muted)" }}
  >
    <Loader2 size={12} className="animate-spin" /> 生成中…
  </span>
);

interface ChatMessageBubbleProps {
  /** 消息归属：user = 右对齐 + 用户底色；assistant = 左对齐。 */
  role: "user" | "assistant";
  /** user 气泡显示文本（原始输入含 @标签，组件内按原文位置渲染胶囊）。 */
  displayContent?: string;
  /**
   * 消息原始 refs（直接传消息对象自带数组，**不要 map 出新数组**——
   * 否则父组件每次渲染新引用，memo 对所有气泡失效，流式期间历史消息的 Markdown 被反复重解析）。
   * key 由 refKeyOf 提取；label 两处消息类型同构，直接读。
   */
  refs?: { label: string }[];
  /** 从 ref 提取 chip 去重键（画布 = nodeId，面板 = file）；模块级稳定函数。 */
  refKeyOf?: (ref: { label: string }) => string;
  /** @chip 点击（画布 = 定位节点；面板 = 打开笔记）。 */
  onRefChipClick?: (refKey: string, label: string) => void;
  /** assistant 气泡 Markdown 原文（空时显示占位/思考块）。 */
  content?: string;
  /** Agent 步进（assistant 消息：思考与工具按序交错，每步上的思考可见；工具步可点开看详情）。 */
  steps?: AgentStep[];
  /** 是否进行中的流式消息（思考块折叠态显示等待动画）。 */
  isStreaming: boolean;
  /** 历史消息附件缩略（仅画布消息有；面板消息无附件）。 */
  attachments?: Attachment[];
  /** 附件图片右键 → 拉出为媒体节点（仅画布传）。 */
  onMediaExtract?: (att: Attachment) => void;
  /** assistant Markdown 的链接/定位回调（调用方 useMemo 稳定化；缺省 = 纯渲染无链接拦截）。 */
  markdownLinks?: MarkdownEditorLinks;
  /** 复制内容（气泡所见原文，调用方算好：user = displayContent ?? content，assistant = content）。 */
  copyText: string;
  messageId: string;
  /** 仅完整 AI 回复可「回到此处」。 */
  canRollback: boolean;
  onRollback?: (messageId: string) => void;
  /** 分支（仅画布对话节点有）。 */
  onBranch?: (messageId: string) => void;
  /** 重新生成（仅 AI 对话面板有）。 */
  onRegenerate?: () => void;
  /** React Flow 内使用时阻止事件冒泡（画布传 true，防拖动/连线误触）。 */
  stopPropagation?: boolean;
}

export const ChatMessageBubble = memo(function ChatMessageBubble({
  role,
  displayContent,
  refs,
  refKeyOf,
  onRefChipClick,
  content,
  steps,
  isStreaming,
  attachments,
  onMediaExtract,
  markdownLinks,
  copyText,
  messageId,
  canRollback,
  onRollback,
  onBranch,
  onRegenerate,
  stopPropagation,
}: ChatMessageBubbleProps) {
  const isUser = role === "user";
  // 步骤流 → 步骤组（思考与背后的工具归组，最终纯思考自成一组的 thinking）；steps 引用变化才重算
  const stepGroups = useMemo(
    () => (steps ? groupAgentSteps(steps) : []),
    [steps],
  );
  // 复制反馈 1.5s 后复原（复制内容 = 气泡所见原文）
  const [copied, setCopied] = useState(false);
  const copyMessage = () => {
    void navigator.clipboard.writeText(copyText);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };
  const atts = attachments ?? [];
  // refs → {key, label} 归一化：refs 为消息对象自带数组（引用稳定），refKeyOf 为模块级稳定函数，
  // 该 useMemo 只在消息自身 refs 变化时重算——历史消息 memo 跳过渲染；同时供用户消息 MarkdownView
  // 的 @引用 胶囊（mentions）使用
  const mentionRefs = useMemo(() => {
    if (!refs || !refKeyOf) return undefined;
    return refs.map((r) => ({ key: refKeyOf(r), label: r.label }));
  }, [refs, refKeyOf]);
  const stopProps = stopPropagation
    ? {
        onClick: (e: React.MouseEvent) => e.stopPropagation(),
        onPointerDown: (e: React.PointerEvent) => e.stopPropagation(),
      }
    : {};

  return (
    <div className={`group relative flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`relative max-w-[85%] rounded-lg ${BUBBLE_PADDING_CLASS} ${isUser ? USER_BUBBLE_CLASS : ""}`}
        style={{
          userSelect: "text",
          WebkitUserSelect: "text",
          // assistant 底色/边框来自样式常量（原实现 bg-tertiary 被其覆盖，净效果一致），字色恒 text-primary
          ...(isUser ? {} : { ...ASSISTANT_BUBBLE_STYLE, color: "var(--text-primary)" }),
        }}
      >
        {atts.length > 0 && (
          <div className="flex flex-wrap gap-1.5 mb-1.5">
            {atts.map((att, i) =>
              att.kind === "image" && att.payload ? (
                <img
                  key={i}
                  src={att.payload}
                  alt={att.filename ?? ""}
                  className="max-h-32 rounded border"
                  style={{ borderColor: "var(--border)", background: "var(--bg-card)" }}
                  draggable={false}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    onMediaExtract?.(att);
                  }}
                  title="右键：拉出为媒体节点"
                />
              ) : (
                <span
                  key={i}
                  className="inline-flex items-center gap-1 text-xs rounded px-1.5 py-0.5"
                  style={{ background: "rgba(128,128,128,.2)" }}
                  title={att.filename}
                >
                  <FileText size={12} className="flex-shrink-0" /> {att.filename || "文件"}
                </span>
              )
            )}
          </div>
        )}
        {isUser ? (
          <div className="markdown-body max-w-none break-words">
            {/* 用户消息整段统一引擎渲染；@引用 经 mention widget 原地渲染为胶囊（点击定位/打开） */}
            <MarkdownView
              text={displayContent ?? ""}
              links={markdownLinks}
              mentions={mentionRefs}
              onMentionClick={onRefChipClick}
            />
          </div>
        ) : (
          <div className="markdown-body max-w-none break-words">
            {stepGroups.length > 0 && (
              <div className="mb-1.5 flex flex-col gap-1.5">
                {stepGroups.map((g, idx) => (
                  <div key={idx} className="flex flex-col gap-1">
                    {g.thinkings.map((tk, ti) =>
                      tk.kind === "reasoning" ? (
                        <ThinkingBlock
                          key={ti}
                          text={tk.text}
                          streaming={isStreaming && idx === stepGroups.length - 1}
                        />
                      ) : (
                        <NarrationLine key={ti} text={tk.text} links={markdownLinks} />
                      ),
                    )}
                    {g.tools.map((run) => (
                      <ToolRunRow key={run.id} run={run} />
                    ))}
                  </div>
                ))}
              </div>
            )}
            {isStreaming && !content && stepGroups.length === 0 ? (
              STREAMING_PLACEHOLDER
            ) : (
              <>
                {/* 步骤流与最终回复的分隔（有工具步骤且已产出回复时才显示，避免悬空分隔线） */}
                {stepGroups.length > 0 && content ? (
                  <div className="my-1.5 border-t" style={{ borderColor: "var(--border)" }} />
                ) : null}
                <MarkdownView
                  text={content || (stepGroups.length === 0 ? "..." : "")}
                  links={markdownLinks}
                />
              </>
            )}
          </div>
        )}
      </div>
      {/* 气泡下方操作按钮组：复制（全部消息）+ 回到此处（完整 AI 回复），hover 浮现 */}
      <div
        className={`nodrag absolute top-full mt-0.5 flex items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-60 hover:!opacity-100 z-10 ${isUser ? "right-0" : "left-0"}`}
        {...stopProps}
      >
        <button
          onClick={copyMessage}
          title="复制消息"
          aria-label="复制消息"
          className="flex items-center gap-0.5 rounded px-1 py-0.5 text-[11px]"
          style={{ color: "var(--text-muted)" }}
        >
          {copied ? <Check size={12} className="flex-shrink-0" /> : <Copy size={12} className="flex-shrink-0" />}
          {copied ? "已复制" : "复制"}
        </button>
        {canRollback && (
          <button
            onClick={() => onRollback?.(messageId)}
            title="截断此消息之后的全部消息，在此处继续对话（可撤销）"
            className="flex items-center gap-0.5 rounded px-1 py-0.5 text-[11px]"
            style={{ color: "var(--text-muted)" }}
          >
            <History size={12} className="flex-shrink-0" />
            回到此处
          </button>
        )}
        {onBranch && (
          <button
            onClick={() => onBranch(messageId)}
            title="在此处创建分支对话节点"
            className="flex items-center gap-0.5 rounded px-1 py-0.5 text-[11px]"
            style={{ color: "var(--text-muted)" }}
          >
            <GitBranch size={12} className="flex-shrink-0" />
            分支
          </button>
        )}
        {onRegenerate && (
          <button
            onClick={onRegenerate}
            title="重新生成最后一条回复"
            aria-label="重新生成"
            className="flex items-center gap-0.5 rounded px-1 py-0.5 text-[11px]"
            style={{ color: "var(--text-muted)" }}
          >
            <RefreshCw size={12} className="flex-shrink-0" /> 重新生成
          </button>
        )}
      </div>
    </div>
  );
});

/** 工具轮叙述行：模型在工具轮里说的正文，作为该步的叙述行展示（不进最终回复），与最终回复同 Markdown 正文渲染。 */
function NarrationLine({ text, links }: { text: string; links?: MarkdownEditorLinks }) {
  return (
    <div className="markdown-body max-w-none break-words rounded px-1.5 py-1">
      <MarkdownView text={text} links={links} />
    </div>
  );
}

/** 工具图标（按工具名分发；未知工具 = 通用扳手）。 */
function toolIcon(name: string, size: number) {
  switch (name) {
    case "web_search":
      return <Globe size={size} className="flex-shrink-0" />;
    case "web_fetch":
      return <Link size={size} className="flex-shrink-0" />;
    case "read_file":
      return <FileText size={size} className="flex-shrink-0" />;
    case "glob":
      return <FileSearch size={size} className="flex-shrink-0" />;
    case "grep":
      return <Search size={size} className="flex-shrink-0" />;
    case "edit_file":
      return <PenLine size={size} className="flex-shrink-0" />;
    case "write_file":
      return <FilePlus size={size} className="flex-shrink-0" />;
    default:
      return <Wrench size={size} className="flex-shrink-0" />;
  }
}

/** 工具调用行：可点开详情。折叠 = 图标 + 参数摘要 + 状态 + 结果摘要；展开 = 完整参数与结果。 */
function ToolRunRow({ run }: { run: ToolRun }) {
  const [expanded, setExpanded] = useState(false);
  const statusColor =
    run.status === "error"
      ? "#f87171"
      : run.status === "done"
        ? "var(--accent)"
        : "var(--text-muted)";
  return (
    <div
      className="rounded overflow-hidden"
      style={{
        background: "color-mix(in srgb, var(--accent) 8%, transparent)",
        border: "1px solid color-mix(in srgb, var(--accent) 22%, transparent)",
      }}
    >
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="flex w-full items-center gap-1.5 text-[11px] leading-snug rounded px-1.5 py-1 text-left"
        style={{ cursor: "pointer" }}
        title={expanded ? "收起详情" : "展开详情"}
      >
        <span
          style={{ color: statusColor, display: "inline-flex" }}
        >
          {expanded ? (
            <ChevronDown size={12} className="flex-shrink-0" />
          ) : (
            <ChevronRight size={12} className="flex-shrink-0" />
          )}
        </span>
        <span style={{ color: statusColor, display: "inline-flex" }}>{toolIcon(run.name, 12)}</span>
        <span className="min-w-0 flex-1 truncate" style={{ color: "var(--text-secondary)" }}>
          {run.argsSummary}
          {run.status !== "running" && run.resultSummary ? (
            <span className="ml-1" style={{ color: statusColor }}>
              · {run.resultSummary}
            </span>
          ) : null}
        </span>
        {run.status === "running" ? (
          <Loader2 size={11} className="animate-spin flex-shrink-0" style={{ color: "var(--text-muted)" }} />
        ) : run.status === "error" ? (
          <AlertCircle size={11} className="flex-shrink-0" style={{ color: "#f87171" }} />
        ) : (
          <Check size={11} className="flex-shrink-0" style={{ color: "var(--accent)" }} />
        )}
      </button>
      {expanded && (
        <div className="flex flex-col gap-1 px-1.5 pb-1.5 text-[11px] leading-snug">
          {run.args != null && (
            <DetailSection label="参数">
              <pre>{prettyJson(run.args)}</pre>
            </DetailSection>
          )}
          {run.status === "running" ? (
            <DetailSection label="结果">
              <div className="opacity-70">进行中…</div>
            </DetailSection>
          ) : run.result != null ? (
            <DetailSection label="结果">
              <pre>{run.result}</pre>
            </DetailSection>
          ) : null}
        </div>
      )}
    </div>
  );
}

/** 展开详情小节：左竖条 + 标签 + 内容（超高滚动，防撑爆气泡）。 */
function DetailSection({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div
      className="rounded px-1.5 py-1"
      style={{ background: "color-mix(in srgb, var(--text-primary) 6%, transparent)" }}
    >
      <div className="mb-0.5 font-medium" style={{ color: "var(--text-muted)" }}>
        {label}
      </div>
      <div
        className="max-h-48 overflow-auto whitespace-pre-wrap break-words"
        style={{ color: "var(--text-secondary)", cursor: "text", userSelect: "text" }}
      >
        {children}
      </div>
    </div>
  );
}

/** 参数原始 JSON → 美化（非法 JSON 原文兜底）。 */
function prettyJson(json: string): string {
  try {
    return JSON.stringify(JSON.parse(json), null, 2);
  } catch {
    return json;
  }
}

