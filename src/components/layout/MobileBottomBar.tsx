/**
 * 移动端底部导航栏：横向图标栏（取前若干个视图）+「更多」展开完整列表（底部抽屉浮层）。
 * 视图顺序由应用级偏好决定（`utils/mobileNav`，设置 → 通用可调）；返回键先收起抽屉（useBackHandler）。
 */
import { useState } from "react";
import { MoreHorizontal } from "lucide-react";
import { useBackHandler } from "@/hooks/useBackHandler";
import type { MobileNavItem } from "@/components/layout/MobileNavDrawer";
import { MOBILE_NAV_BAR_SIZE } from "@/utils/mobileNav";

export function MobileBottomBar({
  items,
  active,
  onSelect,
}: {
  items: MobileNavItem[];
  active: string | null;
  onSelect: (key: string) => void;
}) {
  const [sheetOpen, setSheetOpen] = useState(false);
  useBackHandler(sheetOpen, () => {
    setSheetOpen(false);
    return true;
  });

  /** 是否还有底栏放不下的视图（有则追加「更多」入口）。 */
  const overflow = items.length > MOBILE_NAV_BAR_SIZE;
  const barItems = overflow ? items.slice(0, MOBILE_NAV_BAR_SIZE) : items;
  /** 激活视图落在「更多」里：底栏上没有任何项高亮，改由「更多」入口承担高亮 */
  const activeInOverflow = !barItems.some((item) => item.key === active);

  const pick = (key: string) => {
    onSelect(key);
    setSheetOpen(false);
  };

  return (
    <>
      <nav
        className="flex-shrink-0 flex items-stretch border-t"
        style={{
          background: "var(--bg-secondary)",
          borderColor: "var(--border-subtle)",
          paddingBottom: "env(safe-area-inset-bottom)",
        }}
      >
        {barItems.map((item) => {
          const isActive = item.key === active;
          return (
            <button
              key={item.key}
              onClick={() => onSelect(item.key)}
              title={item.label}
              aria-current={isActive ? "page" : undefined}
              className="flex-1 min-h-[52px] flex items-center justify-center"
              style={{
                color: isActive ? "var(--accent)" : "var(--text-secondary)",
                background: isActive ? "var(--accent-soft)" : "transparent",
              }}
            >
              {item.icon}
            </button>
          );
        })}
        {overflow && (
          <button
            onClick={() => setSheetOpen(true)}
            title="全部视图"
            aria-label="全部视图"
            aria-expanded={sheetOpen}
            className="flex-1 min-h-[52px] flex items-center justify-center"
            style={{
              color: sheetOpen || activeInOverflow ? "var(--accent)" : "var(--text-secondary)",
              background: activeInOverflow && !sheetOpen ? "var(--accent-soft)" : "transparent",
            }}
          >
            <MoreHorizontal size={16} />
          </button>
        )}
      </nav>

      {/* 完整列表：底部抽屉（内容超高自行滚动） */}
      {sheetOpen && (
        <>
          <div
            className="fixed inset-0 z-40"
            style={{ background: "var(--scrim)" }}
            onClick={() => setSheetOpen(false)}
          />
          <div
            className="fixed left-0 right-0 bottom-0 z-40 flex flex-col overflow-y-auto rounded-t-[var(--radius-lg)] border-t px-2.5 pt-3"
            style={{
              background: "var(--bg-primary)",
              borderColor: "var(--border)",
              maxHeight: "70vh",
              paddingBottom: "calc(env(safe-area-inset-bottom) + 16px)",
            }}
          >
            <div className="px-2.5 pb-1.5 text-micro font-medium" style={{ color: "var(--text-muted)" }}>
              全部视图
            </div>
            {items.map((item) => {
              const isActive = item.key === active;
              return (
                <button
                  key={item.key}
                  onClick={() => pick(item.key)}
                  className="flex items-center gap-2.5 px-2.5 min-h-12 text-sm text-left flex-shrink-0 rounded-[var(--radius-sm)]"
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
