/**
 * 移动端底部导航条：单栏壳的视图切换入口。
 * 标签 = 当前可用视图贡献（组合行启停驱动，与桌面同一分派来源），
 * 固定常用序在前、其余（含插件视图）按 id 追加；全部停用时壳显示空态提示。
 */
import type { ReactNode } from "react";

/** 底部导航单个标签（label/icon 由视图元信息解析提供）。 */
export interface MobileTab {
  kind: string;
  label: string;
  icon: ReactNode;
}

export function MobileTabBar({
  tabs,
  active,
  onSelect,
}: {
  tabs: MobileTab[];
  active: string | null;
  onSelect: (kind: string) => void;
}) {
  return (
    <nav
      className="flex-shrink-0 flex items-stretch"
      style={{
        background: "var(--bg-secondary)",
        borderTop: "1px solid var(--border)",
        paddingBottom: "env(safe-area-inset-bottom)",
      }}
    >
      {tabs.map((tab) => {
        const isActive = tab.kind === active;
        return (
          <button
            key={tab.kind}
            onClick={() => onSelect(tab.kind)}
            className="flex-1 min-w-0 flex flex-col items-center justify-center gap-0.5 py-2"
            style={{
              color: isActive ? "var(--accent)" : "var(--text-secondary)",
              background: isActive ? "color-mix(in srgb, var(--accent) 10%, transparent)" : "transparent",
            }}
            aria-current={isActive ? "page" : undefined}
          >
            {tab.icon}
            <span className="text-[10px] leading-none truncate max-w-full px-1">{tab.label}</span>
          </button>
        );
      })}
    </nav>
  );
}
