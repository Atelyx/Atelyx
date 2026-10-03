/**
 * 辅助基元：分隔线、键盘按键提示、滚动容器、头像（`Divider` / `Kbd` /
 * `ScrollArea` / `Avatar`）。
 */
import type { HTMLAttributes, ReactNode } from "react";
import { Hash } from "lucide-react";

/** 分隔线。`vertical` 为竖向（同行内分组之间）。 */
export function Divider({ vertical, className }: { vertical?: boolean; className?: string }) {
  return vertical ? (
    <span
      aria-hidden
      className={`inline-block w-px self-stretch flex-shrink-0 ${className ?? ""}`}
      style={{ background: "var(--border)" }}
    />
  ) : (
    <hr className={`border-0 border-t w-full ${className ?? ""}`} style={{ borderColor: "var(--border)" }} />
  );
}

/** 键盘按键提示（`⌘` `Ctrl` `S` 等），用于快捷键说明。 */
export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd
      className="inline-flex items-center justify-center min-w-[18px] h-[18px] px-1
        rounded-[var(--radius-xs)] border text-micro font-mono leading-none flex-shrink-0"
      style={{
        background: "var(--bg-tertiary)",
        borderColor: "var(--border)",
        color: "var(--text-secondary)",
      }}
    >
      {children}
    </kbd>
  );
}

/**
 * 滚动容器：两端轴向都可滚动。滚动条配色由 `styles/index.css` 的全局
 * `::-webkit-scrollbar` 规则统一给色（WebView 下滚动条不跟随系统主题），
 * 故本组件只负责布局与滚动行为，不自带配色。
 */
export function ScrollArea({
  children,
  className,
  ...rest
}: {
  children: ReactNode;
  className?: string;
} & HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={`overflow-auto ${className ?? ""}`} {...rest}>
      {children}
    </div>
  );
}

/**
 * 协作者头像（`Avatar`）：以首字母 + 稳定色环表示在线成员。
 * `name` 缺失（如设备名）时退化为图标占位。色环取自`color`（协作通道分配的稳定色）。
 */
export function Avatar({
  name,
  color,
  size = 20,
  className,
}: {
  /** 成员名（取首字母）或设备名（缺省退化为图标）。 */
  name?: string;
  /** 稳定标识色（协作通道按 id 分配），缺省用中性色。 */
  color?: string;
  size?: number;
  className?: string;
}) {
  const initial = name?.trim().charAt(0) || "";
  return (
    <span
      className={`inline-flex items-center justify-center rounded-full flex-shrink-0 ${className ?? ""}`}
      style={{
        width: size,
        height: size,
        background: color ? `color-mix(in srgb, ${color} 20%, var(--bg-tertiary))` : "var(--bg-tertiary)",
        color: color ?? "var(--text-muted)",
        border: `1px solid ${color ?? "var(--border)"}`,
        // 首字母字号随圆尺寸等比缩放，不用 rem：字要跟着圆走，
        // 若随「字体大小」设置放大，用户调到 20px 时字母会撑破固定尺寸的圆。
        fontSize: Math.max(10, Math.round(size * 0.5)),
        lineHeight: 1,
      }}
      title={name}
    >
      {initial ? initial.toUpperCase() : <Hash size={Math.round(size * 0.55)} />}
    </span>
  );
}

/**
 * 加载指示（`Spinner` / `ProgressBar`）：
 * - `Spinner` 不确定进度（转圈）
 * - `ProgressBar` 确定进度（0–100 的细条）
 *
 * `Spinner` 默认只作视觉指示（`aria-hidden`）：绝大多数调用点在按钮内或紧邻
 * 「正在检查权限…」这类文案，状态已由禁用态与文案表达，再让读屏播报一次
 * 「加载中」是重复信息。独立使用（周围无任何状态文案）时传 `label` 开启播报。
 *
 * 环宽随尺寸按比例给（`size/8`）：固定 `border-2` 在 11–13px 的密集工具条里
 * 会占到直径两成以上，看起来是一块实心圆饼而不是环。
 */
export function Spinner({
  size = 14,
  label,
  className,
}: {
  size?: number;
  /** 供屏幕阅读器朗读的状态文案；给出即开启 `role="status"`。 */
  label?: string;
  className?: string;
}) {
  return (
    <span
      role={label ? "status" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : "true"}
      className={`inline-block rounded-full animate-spin flex-shrink-0 ${className ?? ""}`}
      style={{
        width: size,
        height: size,
        // 环色走`--spinner-*` 变量而非直写 token：强调色按钮内（底色即--accent）
        // 直写会让旋转顶弧与底色同色、看着不转，调用点需按所在表面覆写这两个变量。
        border: `${Math.max(1.5, size / 8)}px solid var(--spinner-track, var(--border))`,
        borderTopColor: "var(--spinner-head, var(--accent))",
      }}
    />
  );
}

/** 确定进度条（0–100）。`label` 供屏幕阅读器朗读进度语义。 */
export function ProgressBar({
  value,
  label,
  className,
}: {
  /** 0–100。 */
  value: number;
  label?: string;
  className?: string;
}) {
  const pct = Math.max(0, Math.min(100, value));
  return (
    <div
      role="progressbar"
      aria-valuenow={Math.round(pct)}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-label={label}
      className={`w-full h-1 rounded-full overflow-hidden ${className ?? ""}`}
      style={{ background: "var(--bg-sunken)" }}
    >
      <div
        className="h-full transition-[width]"
        style={{
          width: `${pct}%`,
          background: "var(--accent-grad)",
          transitionDuration: "var(--dur-base)",
        }}
      />
    </div>
  );
}
