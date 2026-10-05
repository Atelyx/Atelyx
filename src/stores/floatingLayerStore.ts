/**
 * 插件浮层运行时（内核 UI 服务 `ctx.ui` 的数据源）。
 *
 * 插件经 ctx.ui.showFloatingLayer 登记、句柄 close 收起；渲染在 FloatingLayerHost（每个窗口
 * 各挂一个、各自独立实例——撕裂窗口是独立 webview，浮层不跨窗口共享）。定位/层级/Esc 收起
 * 语义由宿主组件代管，登记方只贡献组件与选项。
 */
import { create } from "zustand";
import type { FloatingLayerEntry } from "@/services/cordis/types";

export type { FloatingLayerEntry };

interface FloatingLayerState {
  layers: FloatingLayerEntry[];
  /** 登记一个浮层，返回浮层 id。 */
  open(entry: Omit<FloatingLayerEntry, "id">): string;
  /** 收起浮层（不存在 = no-op；触发该浮层的 onClose 至多一次）。 */
  close(id: string): void;
}

export const useFloatingLayerStore = create<FloatingLayerState>()((set, get) => ({
  layers: [],

  open: (entry) => {
    const id = crypto.randomUUID();
    set((s) => ({ layers: [...s.layers, { ...entry, id }] }));
    return id;
  },

  close: (id) => {
    const entry = get().layers.find((l) => l.id === id);
    if (!entry) return;
    set((s) => ({ layers: s.layers.filter((l) => l.id !== id) }));
    entry.onClose?.();
  },
}));
