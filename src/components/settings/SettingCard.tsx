import { AlertTriangle } from "lucide-react";

/** 设置项卡片（设置页统一样式基准）：宽屏 = 左标题描述 + 右控件；窄屏 = 上下堆叠。
 *  danger = 危险操作变体（危险色描边与底色 + 标题前置警示图标）。 */
export function SettingCard({
  title,
  description,
  danger,
  children,
}: {
  title: string;
  description: React.ReactNode;
  danger?: boolean;
  children: React.ReactNode;
}) {
  return (
    <div
      className="flex flex-col items-stretch gap-2 p-3 rounded-[var(--radius-md)] border sm:flex-row sm:items-center sm:justify-between sm:gap-3"
      style={{
        background: danger
          ? "color-mix(in srgb, var(--danger) 6%, transparent)"
          : "var(--bg-tertiary)",
        borderColor: danger
          ? "color-mix(in srgb, var(--danger) 28%, transparent)"
          : "var(--border)",
      }}
    >
      <div className="min-w-0">
        <div
          className="text-sm font-medium flex items-center gap-1.5"
          style={{ color: danger ? "var(--danger)" : "var(--text-primary)" }}
        >
          {danger && <AlertTriangle size={13} className="shrink-0" />}
          {title}
        </div>
        <div className="text-xs mt-0.5" style={{ color: "var(--text-muted)" }}>
          {description}
        </div>
      </div>
      <div className="flex-shrink-0">{children}</div>
    </div>
  );
}
