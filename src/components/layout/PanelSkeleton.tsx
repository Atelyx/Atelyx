/**
 * 面板启动骨架：面板 bootstrap 与视图贡献装载期间的同形占位（标签条 + 内容区）。
 * 形状（标签占位宽 / 内容行宽 / 纵向档位）与首帧内联骨架同源，见 `constants/panelSkeleton`。
 */
import {
  PANEL_SKELETON_CONTENT_ROWS,
  PANEL_SKELETON_TAB_PILLS,
  PANEL_SKELETON_TABBAR_REM,
} from "@/constants/panelSkeleton";

/** 标签条骨架：与 PanelTabBar 同形——档位取同一常量，底边线画在背景层（写成 border 会占盒高）。 */
function TabStripSkeleton() {
  return (
    <div
      className="flex items-center gap-1 px-1.5 flex-shrink-0"
      style={{
        height: `${PANEL_SKELETON_TABBAR_REM}rem`,
        backgroundColor: "var(--bg-secondary)",
        backgroundImage: "linear-gradient(var(--border), var(--border))",
        backgroundSize: "100% 1px",
        backgroundPosition: "bottom",
        backgroundRepeat: "no-repeat",
      }}
    >
      {PANEL_SKELETON_TAB_PILLS.map((width, i) => (
        <div key={i} className="rounded" style={{ width, height: 14, background: "var(--bg-tertiary)" }} />
      ))}
    </div>
  );
}

/** 内容区骨架：铺满父容器；行块复用既有扫光表达「进行中」（不新增动画）。 */
export function ViewBootSkeleton() {
  return (
    <div
      className="h-full w-full flex flex-col gap-2.5 px-5 py-4 overflow-hidden"
      style={{ background: "var(--bg-primary)" }}
    >
      {PANEL_SKELETON_CONTENT_ROWS.map((width, i) => (
        <div
          key={i}
          className="relative overflow-hidden rounded"
          style={{ width: `${width}%`, height: 12, background: "var(--bg-tertiary)" }}
        >
          <div className="thinking-sweep-bar" />
        </div>
      ))}
    </div>
  );
}

/** 面板启动骨架（不含标题栏：启动期渲染真标题栏，窗口立即可拖动/关闭）。 */
export function PanelBootSkeleton() {
  return (
    <>
      <TabStripSkeleton />
      <div className="flex-1 min-h-0">
        <ViewBootSkeleton />
      </div>
    </>
  );
}
