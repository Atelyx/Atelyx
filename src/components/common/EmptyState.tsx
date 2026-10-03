/**
 * 空态（`EmptyState`）：无内容时的统一表达——图标 + 标题 + 说明 + 主行动。
 * 说明文字承载「为什么空」与「下一步做什么」，避免用户把空态误读为加载失败。
 */
import type { ReactNode } from "react";

export function EmptyState({
  icon,
  title,
  description,
  action,
  className,
}: {
  /** 顶部图标（lucide 图标元素）。 */
  icon?: ReactNode;
  title: string;
  /** 说明：为什么是空的、下一步做什么。 */
  description?: ReactNode;
  /** 主行动按钮（传`<Button>`）。 */
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={`flex flex-col items-center justify-center text-center gap-2 px-6 py-10
        ${className ?? ""}`}
    >
      {icon && (
        <div
          className="w-10 h-10 rounded-[var(--radius-md)] flex items-center justify-center flex-shrink-0"
          style={{ background: "var(--bg-tertiary)", color: "var(--text-muted)" }}
        >
          {icon}
        </div>
      )}
      <div className="text-body font-medium" style={{ color: "var(--text-primary)" }}>
        {title}
      </div>
      {description && (
        <div className="text-ui max-w-[42ch] leading-relaxed" style={{ color: "var(--text-muted)" }}>
          {description}
        </div>
      )}
      {action && <div className="mt-1">{action}</div>}
    </div>
  );
}
