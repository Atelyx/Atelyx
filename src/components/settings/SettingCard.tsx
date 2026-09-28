/** 设置项卡片（设置页统一样式基准）：宽屏 = 左标题描述 + 右控件；窄屏 = 上下堆叠。 */
export function SettingCard({
  title,
  description,
  children,
}: {
  title: string;
  description: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div
      className="flex flex-col items-stretch gap-2 p-3 rounded-lg border sm:flex-row sm:items-center sm:justify-between sm:gap-3"
      style={{
        background: "var(--bg-primary)",
        borderColor: "var(--border)",
      }}
    >
      <div className="min-w-0">
        <div
          className="text-sm font-medium"
          style={{ color: "var(--text-primary)" }}
        >
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
