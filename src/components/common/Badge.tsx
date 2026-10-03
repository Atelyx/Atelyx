/**
 * 状态徽标 / 标签胶囊（`Badge` / `Tag`）：面板与列表里的计数、状态、作用域标记。
 * 状态不靠颜色单独表达——`StatusPill` 之类的语义色须配文字或图标。
 */
import type { ReactNode } from "react";

export type BadgeTone = "neutral" | "accent" | "success" | "warning" | "danger" | "info";

/** 语义色调。neutral 走中性底，accent 用于「当前/选中」，其余与状态语义色一致。 */
const TONE_STYLE: Record<BadgeTone, { background: string; color: string; border: string }> = {
  neutral: {
    background: "var(--bg-tertiary)",
    color: "var(--text-secondary)",
    border: "var(--border)",
  },
  accent: {
    background: "var(--accent-soft)",
    color: "var(--accent)",
    border: "color-mix(in srgb, var(--accent) 35%, transparent)",
  },
  success: {
    background: "color-mix(in srgb, var(--success) 12%, transparent)",
    color: "var(--success)",
    border: "color-mix(in srgb, var(--success) 32%, transparent)",
  },
  warning: {
    background: "color-mix(in srgb, var(--warning) 12%, transparent)",
    color: "var(--warning)",
    border: "color-mix(in srgb, var(--warning) 32%, transparent)",
  },
  danger: {
    background: "color-mix(in srgb, var(--danger) 12%, transparent)",
    color: "var(--danger)",
    border: "color-mix(in srgb, var(--danger) 32%, transparent)",
  },
  info: {
    background: "color-mix(in srgb, var(--info) 12%, transparent)",
    color: "var(--info)",
    border: "color-mix(in srgb, var(--info) 32%, transparent)",
  },
};

/** 徽标：计数、状态点文字、作用域标记等短文本。计数建议传 `mono` 以便扫读。 */
export function Badge({
  children,
  tone = "neutral",
  mono,
  className,
  title,
}: {
  children: ReactNode;
  tone?: BadgeTone;
  /** 数字/编号用等宽，便于纵向扫读对齐。 */
  mono?: boolean;
  className?: string;
  title?: string;
}) {
  const t = TONE_STYLE[tone];
  return (
    <span
      title={title}
      className={`inline-flex items-center gap-1 px-1.5 py-0.5 rounded-[var(--radius-xs)]
        border text-micro leading-none whitespace-nowrap flex-shrink-0
        ${mono ? "font-mono" : ""} ${className ?? ""}`}
      style={{ background: t.background, color: t.color, borderColor: t.border }}
    >
      {children}
    </span>
  );
}

/**
 * 状态语义胶囊（`StatusPill`）：保存中/已落盘/失败/断链/停用这类
 * 「系统处于什么状态」的统一表达。色 + 圆点形状 + 文字三重编码，
 * 保证不辨色也能读出状态。
 */
export function StatusPill({
  status,
  label,
  className,
}: {
  status: "ok" | "pending" | "error" | "off";
  /** 状态文字（必填：状态须有文字，不能只靠色点）。 */
  label: string;
  className?: string;
}) {
  const map = {
    ok: { color: "var(--success)", shape: "rounded-full" },
    pending: { color: "var(--warning)", shape: "rounded-full" },
    error: { color: "var(--danger)", shape: "rounded-[1px]" },
    off: { color: "var(--text-muted)", shape: "rounded-[1px]" },
  }[status];
  return (
    <span
      className={`inline-flex items-center gap-1.5 text-micro whitespace-nowrap flex-shrink-0
        ${className ?? ""}`}
      style={{ color: map.color }}
    >
      {/* 形状随状态：ok/pending 为圆点，error/off 为方点 */}
      <span className={`w-1.5 h-1.5 flex-shrink-0 ${map.shape}`} style={{ background: map.color }} />
      {label}
    </span>
  );
}
