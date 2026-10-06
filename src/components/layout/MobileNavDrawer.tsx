/**
 * 移动端导航抽屉：常驻窄图标栏（快捷切换）+ 可展开的带文字宽栏（覆盖在内容之上），用于设置页的 tab 切换（工作区视图切换见 MobileBottomBar）。
 * 展开态是浮层的一层：返回键先收起它（useBackHandler）；点选条目后自动收起。
 */
import { useState } from "react";
import type { ReactNode } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { useBackHandler } from "@/hooks/useBackHandler";

/** 抽屉条目（label/icon 由调用方解析提供）。 */
export interface MobileNavItem {
  key: string;
  label: string;
  icon: ReactNode;
}

/** 窄图标栏宽度（展开面板以此左贴）。 */
const RAIL_WIDTH = 52;

export function MobileNavDrawer({
  items,
  active,
  onSelect,
}: {
  items: MobileNavItem[];
  active: string | null;
  onSelect: (key: string) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  useBackHandler(expanded, () => {
    setExpanded(false);
    return true;
  });

  const pick = (key: string) => {
    onSelect(key);
    setExpanded(false);
  };

  return (
    <>
      <nav
        className="flex-shrink-0 flex flex-col items-stretch z-30"
        style={{
          width: RAIL_WIDTH,
          background: "var(--bg-secondary)",
          borderRight: "1px solid var(--border-subtle)",
        }}
      >
        <div className="flex-1 min-h-0 overflow-y-auto py-2">
          {items.map((item) => {
            const isActive = item.key === active;
            return (
              <button
                key={item.key}
                onClick={() => pick(item.key)}
                title={item.label}
                aria-current={isActive ? "page" : undefined}
                className="w-full h-12 flex items-center justify-center"
                style={{
                  color: isActive ? "var(--accent)" : "var(--text-secondary)",
                  background: isActive ? "var(--accent-soft)" : "transparent",
                }}
              >
                {item.icon}
              </button>
            );
          })}
        </div>
        <button
          onClick={() => setExpanded((v) => !v)}
          aria-label={expanded ? "收起导航栏" : "展开导航栏"}
          aria-expanded={expanded}
          className="h-11 flex items-center justify-center border-t flex-shrink-0"
          style={{ borderColor: "var(--border-subtle)", color: "var(--text-muted)" }}
        >
          {expanded ? <ChevronLeft size={16} /> : <ChevronRight size={16} />}
        </button>
      </nav>

      {expanded && (
        <>
          <div
            className="fixed inset-0 z-40"
            style={{ left: RAIL_WIDTH, background: "var(--scrim)" }}
            onClick={() => setExpanded(false)}
          />
          <div
            className="fixed top-0 bottom-0 z-40 flex flex-col border-r px-2.5 pt-3 pb-6 overflow-y-auto"
            style={{
              left: RAIL_WIDTH,
              width: 172,
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
            }}
          >
            {items.map((item) => {
              const isActive = item.key === active;
              return (
                <button
                  key={item.key}
                  onClick={() => pick(item.key)}
                  className="flex items-center gap-2.5 px-2.5 h-12 text-sm text-left flex-shrink-0 rounded-sm"
                  style={{
                    color: isActive ? "var(--accent)" : "var(--text-secondary)",
                    background: isActive ? "var(--accent-soft)" : "transparent",
                  }}
                >
                  {item.icon}
                  <span className="truncate">{item.label}</span>
                </button>
              );
            })}
          </div>
        </>
      )}
    </>
  );
}
