/**
 * 面板占位引导（无文件/空视图时显示）：图标 + 标题 + 描述，承载各视图的空状态引导文案。
 * 传 `viewKind` 时走 `empty/<viewKind>` 槽（见 viewKind 注释），否则纯本组件兜底引导。
 */
import type { ReactNode } from "react";
import { EmptyStateMount } from "@/components/plugins/SlotHost";

export function PanelPlaceholder({
  icon,
  title,
  description,
  action,
  viewKind,
}: {
  icon: ReactNode;
  title: string;
  description: string;
  /** 可选操作区（如失败态的重试按钮）。 */
  action?: ReactNode;
  /** 空态槽 kind（empty/<viewKind>；传了才走槽分派，否则纯宿主引导）。 */
  viewKind?: string;
}) {
  const fallback = (
    <div className="flex flex-col items-center gap-4 max-w-sm text-center px-6">
      <div className="opacity-60">{icon}</div>
      <h2 className="text-xl font-semibold" style={{ color: "var(--text-primary)" }}>
        {title}
      </h2>
      <p className="text-sm" style={{ color: "var(--text-muted)" }}>
        {description}
      </p>
      {action}
    </div>
  );
  return (
    <div
      className="h-full w-full flex items-center justify-center"
      style={{ background: "var(--bg-primary)" }}
    >
      {viewKind ? <EmptyStateMount viewKind={viewKind} fallback={fallback} /> : fallback}
    </div>
  );
}
