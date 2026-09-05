/**
 * 拖拽 ghost 影子：拖拽标签时显示跟随鼠标的半透明标签预览。
 *
 * 各窗口各自渲染一份：订阅 Rust 广播的活跃拖拽会话（panelStore.dragSession，屏幕坐标），
 * 换算为本地 client 坐标，仅在坐标位于本窗口视口内时显示——跨窗口拖动时影子跟随光标所在窗口；
 * 拖到桌面（无窗口）时影子不显示（无跨窗口层叠能力）。
 */
import { usePanelStore } from "@/stores/panelStore";
import { viewMetaFor } from "@/components/layout/ViewHost";

export function DragGhost() {
  const session = usePanelStore((s) => s.dragSession);
  const windowPos = usePanelStore((s) => s.windowPos);
  if (!session || !session.active || typeof session.screenX !== "number" || typeof session.screenY !== "number" || !session.view) {
    return null;
  }
  const x = session.screenX - windowPos.x;
  const y = session.screenY - windowPos.y;
  // 超出本窗口视口（光标在别处）→ 本窗口不显示
  if (x < -50 || y < -50 || x > window.innerWidth + 50 || y > window.innerHeight + 50) return null;
  const meta = viewMetaFor(session.view);
  return (
    <div
      style={{
        position: "fixed",
        left: x + 14,
        top: y + 18,
        pointerEvents: "none",
        zIndex: 60,
        opacity: 0.85,
      }}
    >
      <div
        className="flex items-center gap-1.5 px-2.5 py-1 rounded-md text-xs border"
        style={{
          background: "var(--bg-secondary)",
          borderColor: "var(--accent)",
          color: "var(--text-primary)",
          boxShadow: "0 4px 12px rgba(0,0,0,0.25)",
        }}
      >
        {meta.icon}
        <span className="whitespace-nowrap">{meta.label}</span>
      </div>
    </div>
  );
}
