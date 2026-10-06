/**
 * 插件浮层运行时（内核 UI 服务 `ctx.ui` 的数据源）：插件经 ctx.ui.showFloatingLayer 登记、句柄 close 收起。
 * 定位/层级/Esc 收起语义由宿主组件 FloatingLayerHost 代管，登记方只贡献组件与选项。
 */
import { create } from "zustand";
import type { FloatingLayerEntry } from "@/services/cordis/types";

export type { FloatingLayerEntry };

interface FloatingLayerState {
  /** 本窗口的浮层列表（撕裂窗口是独立 webview，各挂各的 host 实例，浮层不跨窗口共享）。 */
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
