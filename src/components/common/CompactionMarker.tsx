/**
 * 会话压缩标记行：画布对话节点与 AI 对话面板共用，插在「压缩边界之后」首条消息之前。
 *
 * 只作提示与摘要展开——被压缩的原始消息仍完整显示在标记上方（非破坏注解），
 * 模型请求历史则由摘要代替该区间（见 `utils/compaction` 的 `splitByCompaction`）。
 */
import { useState } from "react";
import { ChevronDown, ChevronRight, Layers } from "lucide-react";
import type { ConversationCompaction } from "@/types";

/** 压缩标记：图标 + 「上下文已压缩」+ 覆盖条数；可展开查看检查点摘要。 */
export function CompactionMarker({ compaction }: { compaction: ConversationCompaction }) {
  const [expanded, setExpanded] = useState(false);
  return (
    <div
      className="rounded overflow-hidden"
      style={{
        background: "color-mix(in srgb, var(--text-primary) 5%, transparent)",
        border: "1px dashed color-mix(in srgb, var(--text-primary) 20%, transparent)",
      }}
    >
      <button
        type="button"
        onClick={() => setExpanded((v) => !v)}
        className="flex w-full items-center gap-1.5 text-[11px] leading-snug rounded px-1.5 py-1 text-left"
        style={{ cursor: "pointer" }}
        title={expanded ? "收起检查点摘要" : "展开检查点摘要"}
      >
        <span style={{ color: "var(--text-muted)", display: "inline-flex" }}>
          {expanded ? (
            <ChevronDown size={12} className="flex-shrink-0" />
          ) : (
            <ChevronRight size={12} className="flex-shrink-0" />
          )}
        </span>
        <Layers size={12} className="flex-shrink-0" style={{ color: "var(--text-muted)" }} />
        <span className="min-w-0 flex-1 truncate" style={{ color: "var(--text-secondary)" }}>
          上下文已压缩
          <span className="ml-1" style={{ color: "var(--text-muted)" }}>
            · 以上 {compaction.messageCount} 条已折叠为摘要
          </span>
        </span>
      </button>
      {expanded && (
        <pre
          className="max-h-64 overflow-auto whitespace-pre-wrap break-words px-1.5 pb-1.5 m-0 text-[11px] leading-snug"
          style={{ color: "var(--text-secondary)", fontFamily: "inherit" }}
        >
          {compaction.summary}
        </pre>
      )}
    </div>
  );
}
