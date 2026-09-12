/**
 * 日历（主页）store：手动日程（`.atelyx/calendar.json` 仓库级）+ 带日期笔记（只读）。
 *
 * 手动日程 CRUD 防抖落盘；写盘按「已加载仓库」归属校验（loadedForVault），
 * 切仓库前须先 flush（appStore.selectVault 已接）防防抖窗口内把旧仓库日程写进新仓库。
 * 带日期笔记来自 `services/home.listDatedNotes`（frontmatter date/due），随 load 一并刷新。
 * 性能：同仓库会话内缓存（loadedForVault），主页面板随布局切换反复挂载时，已缓存仓库
 * 仅后台静默重扫带日期笔记；手动日程 items 为 store 实时态（防抖落盘），刷新不重读磁盘
 * 防覆盖在途编辑。加载失败静默降级为空（尽力而为，不阻塞面板）。
 */
import { create } from "zustand";
import { CALENDAR_FILE, CALENDAR_SCHEMA } from "@/constants/calendar";
import { readVaultFile, writeVaultFile } from "@/services/vault/aiFiles";
import { listDatedNotes, type DatedNote } from "@/services/home";
import { createPersistController } from "@/utils/persist";
import { useAppStore } from "@/stores/appStore";
import type { CalendarItem } from "@/types";

/** `.atelyx/calendar.json` 磁盘格式。 */
interface CalendarFile {
  schema: typeof CALENDAR_SCHEMA;
  items: CalendarItem[];
}

interface CalendarState {
  items: CalendarItem[];
  datedNotes: DatedNote[];
  /** 已加载的仓库 id（null = 未加载；persist 归属校验用）。 */
  loadedForVault: string | null;
  /** 重载（切仓库/面板挂载时调用；先清空防旧仓库数据闪现）。 */
  load: () => Promise<void>;
  addItem: (date: string, title: string, note?: string, color?: string) => void;
  updateItem: (
    id: string,
    patch: Partial<Pick<CalendarItem, "date" | "title" | "note" | "color">>,
  ) => void;
  removeItem: (id: string) => void;
  /** 立即落盘（切仓库/退出前 flush，防 debounce 窗口内丢改动）。 */
  flush: () => Promise<void>;
}

/** 读手动日程（缺失/损坏 → 空列表）。 */
async function readCalendarItems(): Promise<CalendarItem[]> {
  try {
    const raw = await readVaultFile(CALENDAR_FILE);
    const parsed = JSON.parse(raw) as CalendarFile;
    return Array.isArray(parsed.items) ? parsed.items : [];
  } catch {
    return [];
  }
}

/** 上一次成功落盘的日程序列化快照（脏门控基线；null = 无基线，下次必写）。
 * 手动日程 CRUD 每次都 `schedule()`，但同一日程可能被多轮触发（更新回显、切面板重挂），
 * 无差异时写盘纯属浪费且会让撕裂窗口/外部同步场景产生无意义写入。 */
let persistedItems: string | null = null;

const persistCtl = createPersistController({
  persist: async () => {
    const s = useCalendarStore.getState();
    // 跨仓库守卫：未加载（首启/已清空）不写
    if (!s.loadedForVault) return;
    const snapshot = JSON.stringify(s.items);
    // 无脏：内存日程与上次落盘一致，不写盘（首载基线由 load 建立，故「加载后立即写」不会发生）
    if (snapshot === persistedItems) return;
    try {
      const payload: CalendarFile = { schema: CALENDAR_SCHEMA, items: s.items };
      await writeVaultFile(CALENDAR_FILE, JSON.stringify(payload, null, 2));
      persistedItems = snapshot;
    } catch (e) {
      console.error("保存日历日程失败", e);
    }
  },
  delay: 400,
});

export const useCalendarStore = create<CalendarState>((set, get) => ({
  items: [],
  datedNotes: [],
  loadedForVault: null,

  load: async () => {
    const vaultId = useAppStore.getState().vaultId;
    if (!vaultId) return;
    if (get().loadedForVault === vaultId) {
      // 同仓库已缓存：再进主页不清空不转圈，仅后台静默重扫带日期笔记（只读视图可能滞后）；
      // 手动日程 items 为 store 实时态（防抖落盘），不重读磁盘，防覆盖在途编辑。
      try {
        const datedNotes = await listDatedNotes();
        if (useAppStore.getState().vaultId === vaultId) set({ datedNotes });
      } catch (e) {
        console.error("刷新日历笔记失败", e);
      }
      return;
    }
    // 首载/切仓库：清残留 debounce（双保险防旧 timer 写新仓库）+ 清空防旧仓库数据闪现
    persistCtl.cancel();
    set({ loadedForVault: null, items: [], datedNotes: [] });
    persistedItems = null;
    try {
      const [items, datedNotes] = await Promise.all([readCalendarItems(), listDatedNotes()]);
      // 竞态守卫：等待期间用户可能已切仓库
      if (useAppStore.getState().vaultId !== vaultId) return;
      set({ items, datedNotes, loadedForVault: vaultId });
      // 脏门控基线 = 刚落盘的磁盘内容：加载本身不产生写盘
      persistedItems = JSON.stringify(items);
    } catch (e) {
      console.error("加载日历失败", e);
      if (useAppStore.getState().vaultId === vaultId) {
        set({ items: [], datedNotes: [], loadedForVault: vaultId });
      }
    }
  },

  addItem: (date, title, note, color) => {
    const trimmed = title.trim();
    if (!trimmed) return;
    const item: CalendarItem = {
      id: crypto.randomUUID(),
      date,
      title: trimmed,
      ...(note?.trim() ? { note: note.trim() } : {}),
      ...(color ? { color } : {}),
      createdAt: Date.now(),
    };
    set((s) => ({ items: [...s.items, item] }));
    persistCtl.schedule();
  },

  updateItem: (id, patch) => {
    set((s) => ({
      items: s.items.map((it) => (it.id === id ? { ...it, ...patch } : it)),
    }));
    persistCtl.schedule();
  },

  removeItem: (id) => {
    set((s) => ({ items: s.items.filter((it) => it.id !== id) }));
    persistCtl.schedule();
  },

  flush: async () => {
    await persistCtl.flush();
  },
}));
