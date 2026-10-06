/**
 * 消息气泡内的「思考过程」折叠块（reasoning_content 展示）：默认折叠，折叠态显示一行思考摘要。
 *
 * 摘要取首行（完成）/ 最新一行（流式）；思考仅作即时展示，不进 API 历史上下文、不参与复制。
 */
import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

/** 摘要 = 文本首行（完成态：一眼看到思考主题）。 */
function firstLine(text: string): string {
  const newline = text.indexOf("\n");
  return newline === -1 ? text : text.slice(0, newline);
}

/** 摘要 = 文本最新一行（流式态：跟随思考进度，trimEnd 去末尾换行残片）。 */
function latestLine(text: string): string {
  const visible = text.trimEnd();
  const newline = visible.lastIndexOf("\n");
  return newline === -1 ? visible : visible.slice(newline + 1);
}

export function ThinkingBlock({
  text,
  streaming = false,
}: {
  text: string;
  streaming?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const summary = streaming ? latestLine(text) : firstLine(text);
  // 流式摘要跟随最新一行：水平滚动到末尾，新写的思考直接可见（不再从行首被截断）
  const summaryRef = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const el = summaryRef.current;
    if (el) el.scrollLeft = streaming ? el.scrollWidth - el.clientWidth : 0;
  }, [summary, streaming]);
  return (
    <div
      className="mb-2 overflow-hidden rounded-[var(--radius-md)] border text-xs"
      style={{ borderColor: "var(--border-subtle)", background: "var(--bg-secondary)" }}
    >
      {/* 头部条（下沉面）：折叠态即思考摘要行。保留原生 button（不归基元）：整行 w-full 承载摘要、
          relative 定位扫光动画、select-none 禁选，语义是行而非按钮 */}
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        className="relative flex h-7 w-full items-center gap-1.5 px-2.5 text-left select-none"
        style={{ background: "var(--bg-sunken)", color: "var(--text-muted)" }}
      >
        {streaming && !open && <span className="thinking-sweep-bar" aria-hidden />}
        {open ? (
          <ChevronDown size={12} className="flex-shrink-0" />
        ) : (
          <ChevronRight size={12} className="flex-shrink-0" />
        )}
        <span className="flex-shrink-0">思考过程</span>
        {summary && (
          <>
            <span
              aria-hidden
              className="h-0.5 w-0.5 flex-shrink-0 rounded-full"
              style={{ background: "var(--text-muted)" }}
            />
            <span
              ref={summaryRef}
              className="min-w-0 flex-1 overflow-hidden whitespace-nowrap"
              style={{
                color: "var(--text-secondary)",
                textOverflow: streaming ? "clip" : "ellipsis",
              }}
            >
              {summary}
            </span>
          </>
        )}
      </button>
      {open && (
        <div
          className="whitespace-pre-wrap break-words max-h-48 overflow-y-auto px-3 py-2 leading-5"
          style={{ color: "var(--text-secondary)", borderTop: "1px solid var(--border-subtle)" }}
        >
          {text}
        </div>
      )}
    </div>
  );
}
