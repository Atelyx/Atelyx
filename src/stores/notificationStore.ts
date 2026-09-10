/**
 * 应用内通知运行时（内核 UI 服务 `ctx.notification` 的数据源）。
 *
 * 插件与宿主都只 push/dismiss；渲染在 NotificationHost（每个窗口各挂一个、各自独立实例
 * ——撕裂窗口是独立 webview，通知不跨窗口共享）。自动消失由宿主组件按 timeoutMs 计时。
 */
import { create } from "zustand";
import type { NotificationInput, NotificationLevel } from "@/services/cordis/types";

export type { NotificationLevel };

export interface NotificationItem {
  id: string;
  level: NotificationLevel;
  /** 可选标题（无标题时只显示正文）。 */
  title?: string;
  message: string;
  /** 自动消失毫秒数（宿主组件据此计时）。 */
  timeoutMs: number;
}

/** 自动消失时长：错误留久一些（8s），其余 4s。 */
const AUTO_DISMISS_MS: Record<NotificationLevel, number> = {
  info: 4000,
  success: 4000,
  warning: 6000,
  error: 8000,
};

interface NotificationState {
  items: NotificationItem[];
  /** 弹出一条通知，返回 id。 */
  notify(input: NotificationInput): string;
  /** 关闭一条通知（不存在 = no-op）。 */
  dismiss(id: string): void;
}

export const useNotificationStore = create<NotificationState>()((set) => ({
  items: [],

  notify: ({ message, title, level = "info" }) => {
    const id = crypto.randomUUID();
    set((s) => ({
      items: [...s.items, { id, level, ...(title ? { title } : {}), message, timeoutMs: AUTO_DISMISS_MS[level] }],
    }));
    return id;
  },

  dismiss: (id) =>
    set((s) => {
      if (!s.items.some((n) => n.id === id)) return s;
      return { items: s.items.filter((n) => n.id !== id) };
    }),
}));
