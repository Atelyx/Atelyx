/**
 * 面板占位引导（无文件/空视图时显示）。
 * 图标 + 标题 + 描述，承载各视图的空状态引导文案。
 */
import type { ReactNode } from "react";

export function PanelPlaceholder({
  icon,
  title,
  description,
}: {
  icon: ReactNode;
  title: string;
  description: string;
}) {
  return (
    <div
      className="h-full w-full flex items-center justify-center"
      style={{ background: "var(--bg-primary)" }}
    >
      <div className="flex flex-col items-center gap-4 max-w-sm text-center px-6">
        <div className="opacity-60">{icon}</div>
        <h2 className="text-xl font-semibold" style={{ color: "var(--text-primary)" }}>
          {title}
        </h2>
        <p className="text-sm" style={{ color: "var(--text-muted)" }}>
          {description}
        </p>
      </div>
    </div>
  );
}
