/**
 * 应用内通知宿主（右下角堆叠）：每窗口各挂一个（主窗口与撕裂窗口是独立 webview，
 * 通知列表各自独立、不跨窗口共享）。
 *
 * 列表来自 notificationStore，自动消失与手动关闭都只调 store.dismiss。
 */
import { useEffect } from "react";
import { AlertTriangle, CheckCircle2, Info, X, XCircle } from "lucide-react";
import { useNotificationStore, type NotificationItem } from "@/stores/notificationStore";
import { Button, IconButton } from "@/components/common/Button";
import type { NotificationLevel } from "@/services/cordis/types";

/** 级别 → 图标与强调色（颜色走主题语义色变量，随深浅主题与强调色体系一致）。 */
const LEVEL_STYLES: Record<NotificationLevel, { icon: typeof Info; color: string }> = {
  info: { icon: Info, color: "var(--text-secondary)" },
  success: { icon: CheckCircle2, color: "var(--success)" },
  warning: { icon: AlertTriangle, color: "var(--warning)" },
  error: { icon: XCircle, color: "var(--danger)" },
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

/** 单条通知：挂载即按 timeoutMs 计时自动消失；带动作的通知不自动消失；手关闭立即消失。 */
function NotificationCard({ item }: { item: NotificationItem }) {
  const dismiss = useNotificationStore((s) => s.dismiss);
  useEffect(() => {
    // 带动作的通知不自动消失：用户需看到并处理动作入口（关闭由用户手动触发）
    if (item.action) return;
    const timer = setTimeout(() => dismiss(item.id), item.timeoutMs);
    return () => clearTimeout(timer);
  }, [item.id, item.timeoutMs, item.action, dismiss]);

  const { icon: Icon, color } = LEVEL_STYLES[item.level];
  return (
    <div
      role="status"
      className="pointer-events-auto flex max-w-sm items-start gap-2 rounded-md border px-3 py-2 shadow-lg"
      style={{
        background: "var(--bg-secondary)",
        borderColor: "var(--border)",
        // 通知压在内容之上：背景模糊由皮肤决定（默认 none = 不模糊）
        backdropFilter: "var(--glass-filter)",
        WebkitBackdropFilter: "var(--glass-filter)",
      }}
    >
      <Icon size={16} style={{ color, marginTop: 2, flexShrink: 0 }} />
      <div className="min-w-0 flex-1">
        {item.title && (
          <div className="text-caption font-medium" style={{ color: "var(--text-primary)" }}>
            {item.title}
          </div>
        )}
        <div className="text-caption break-words" style={{ color: "var(--text-secondary)" }}>
          {item.message}
        </div>
        {item.action && (
          <Button
            type="button"
            onClick={() => {
              item.action?.onClick();
              dismiss(item.id);
            }}
            variant="primary"
            size="xs"
            className="mt-1.5"
          >
            {item.action.label}
          </Button>
        )}
      </div>
      <IconButton
        type="button"
        onClick={() => dismiss(item.id)}
        icon={<X size={13} />}
        label="关闭"
        size="xs"
        variant="subtle"
      />
    </div>
  );
}
