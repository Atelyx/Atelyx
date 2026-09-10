/**
 * 应用内通知宿主（右下角堆叠）。
 *
 * 每个窗口各挂一个（主窗口与撕裂窗口是独立 webview，通知列表各自独立、不跨窗口共享）；
 * 列表来自 notificationStore，自动消失与手关闭都只调 store.dismiss。
 * 颜色走主题 CSS 变量，图标用 lucide 线性图标。
 */
import { useEffect } from "react";
import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";
import { useNotificationStore, type NotificationItem } from "@/stores/notificationStore";
import type { NotificationLevel } from "@/services/cordis/types";

/** 级别 → 图标与强调色（色值尽量走主题变量，状态色直给以保语义清晰）。 */
const LEVEL_STYLES: Record<NotificationLevel, { icon: typeof Info; color: string }> = {
  info: { icon: Info, color: "var(--text-secondary)" },
  success: { icon: CheckCircle2, color: "#4ade80" },
  warning: { icon: AlertTriangle, color: "#fbbf24" },
  error: { icon: XCircle, color: "#f87171" },
};

export function NotificationHost() {
  const items = useNotificationStore((s) => s.items);
  return (
    <div className="fixed bottom-4 right-4 z-[1200] flex flex-col gap-2 pointer-events-none">
      {items.map((item) => (
        <NotificationCard key={item.id} item={item} />
      ))}
    </div>
  );
}

/** 单条通知：挂载即按 timeoutMs 计时自动消失；手关闭立即消失。 */
function NotificationCard({ item }: { item: NotificationItem }) {
  const dismiss = useNotificationStore((s) => s.dismiss);
  useEffect(() => {
    const timer = setTimeout(() => dismiss(item.id), item.timeoutMs);
    return () => clearTimeout(timer);
  }, [item.id, item.timeoutMs, dismiss]);

  const { icon: Icon, color } = LEVEL_STYLES[item.level];
  return (
    <div
      role="status"
      className="pointer-events-auto flex max-w-sm items-start gap-2 rounded-md border px-3 py-2 shadow-lg"
      style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
    >
      <Icon size={16} style={{ color, marginTop: 2, flexShrink: 0 }} />
      <div className="min-w-0 flex-1">
        {item.title && (
          <div className="text-[12px] font-medium" style={{ color: "var(--text-primary)" }}>
            {item.title}
          </div>
        )}
        <div className="text-[12px] break-words" style={{ color: "var(--text-secondary)" }}>
          {item.message}
        </div>
      </div>
      <button
        type="button"
        onClick={() => dismiss(item.id)}
        title="关闭"
        className="flex-shrink-0 rounded p-0.5 hover:opacity-80"
        style={{ color: "var(--text-muted)" }}
      >
        <X size={13} />
      </button>
    </div>
  );
}
