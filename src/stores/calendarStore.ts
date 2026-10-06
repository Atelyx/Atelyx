/**
 * 日历（主页）store：手动日程（仓库级）+ 带日期笔记（只读）。
 * 手动日程 CRUD 防抖落盘，落盘位置按激活仓库身份分流（services/metadata）；写盘按已加载仓库身份
 * 归属校验，切仓库前须先 flush（appStore.selectVault/selectSpace 已接），防防抖窗口内把旧仓库日程写进新仓库。
 */
import { create } from "zustand";
import { CALENDAR_SCHEMA } from "@/constants/calendar";
import { SPACE_TEAM_META } from "@/constants/spaceMeta";
import { readCalendarRaw, writeCalendarRaw } from "@/services/metadata";
import { identityKeyOf } from "@/services/content/factory";
import { listDatedNotes, type DatedNote } from "@/services/home";
import { createPersistController } from "@/utils/persist";
import { registerCollabMetaChanged } from "@/utils/collabHost";
import { useAppStore } from "@/stores/appStore";
import type { CalendarItem } from "@/types";

/** 日历磁盘格式（个人仓库 `.atelyx/calendar.json` / 空间团队 meta `calendar` 共用）。 */
interface CalendarFile {
  schema: typeof CALENDAR_SCHEMA;
  items: CalendarItem[];
}

/** 写盘/竞态守卫键：local = `local:<root>`；space = `space:<serverUrl>#<spaceId>`（未激活 = "none"）。 */
function calendarGuardKey(): string {
  return identityKeyOf(useAppStore.getState().vaultIdentity);
}

interface CalendarState {
  items: CalendarItem[];
  /** 带日期笔记（`services/home.listDatedNotes`，frontmatter date/due），随 load 一并刷新。 */
  datedNotes: DatedNote[];
  /** 已加载的仓库守卫键（null = 未加载；persist 归属校验用）。 */
  loadedFor: string | null;
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

/** 读手动日程 JSON 原文解析为条目（原文缺失 → 空列表；损坏 → 空列表，与 load 的容错同口径）。 */
function parseCalendarItems(raw: string | null): CalendarItem[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as CalendarFile;
    return Array.isArray(parsed.items) ? parsed.items : [];
  } catch {
    return [];
  }
}

/** 读手动日程（缺失/损坏 → 空列表）。 */
async function readCalendarItems(): Promise<CalendarItem[]> {
  try {
    return parseCalendarItems(await readCalendarRaw());
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
    if (!s.loadedFor) return;
    const snapshot = JSON.stringify(s.items);
    // 无脏：内存日程与上次落盘一致，不写盘（首载基线由 load 建立，故「加载后立即写」不会发生）
    if (snapshot === persistedItems) return;
    try {
      const payload: CalendarFile = { schema: CALENDAR_SCHEMA, items: s.items };
      await writeCalendarRaw(JSON.stringify(payload, null, 2));
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
  loadedFor: null,

  load: async () => {
    // 未激活仓库不加载（守卫键需有身份才有判别意义）
    if (!useAppStore.getState().vaultIdentity) return;
    const guardKey = calendarGuardKey();
    if (get().loadedFor === guardKey) {
      // 同仓库已缓存：再进主页不清空不转圈，仅后台静默重扫带日期笔记（只读视图可能滞后）；
      // 手动日程 items 为 store 实时态（防抖落盘），不重读磁盘，防覆盖在途编辑。
      try {
        const datedNotes = await listDatedNotes();
        if (calendarGuardKey() === guardKey) set({ datedNotes });
      } catch (e) {
        console.error("刷新日历笔记失败", e);
      }
      return;
    }
    // 首载/切仓库：清残留 debounce（双保险防旧 timer 写新仓库）+ 清空防旧仓库数据闪现
    persistCtl.cancel();
    set({ loadedFor: null, items: [], datedNotes: [] });
    persistedItems = null;
    try {
      const [items, datedNotes] = await Promise.all([readCalendarItems(), listDatedNotes()]);
      // 竞态守卫：等待期间用户可能已切仓库（按身份键比对——空间切换 vaultRoot 恒 null，root 比对失效）
      if (calendarGuardKey() !== guardKey) return;
      set({ items, datedNotes, loadedFor: guardKey });
      // 脏门控基线 = 刚落盘的磁盘内容：加载本身不产生写盘
      persistedItems = JSON.stringify(items);
    } catch (e) {
      console.error("加载日历失败", e);
      if (calendarGuardKey() === guardKey) {
        set({ items: [], datedNotes: [], loadedFor: guardKey });
        // 基线对齐内存空态：加载失败后远端变更帧仍可采纳（自愈入口），否则脏检查恒不通过
        persistedItems = JSON.stringify([]);
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

/**
 * 采纳团队层远端日历（`meta-changed` 帧）：本空间已加载且整个读盘窗口内基线无推进时，回读磁盘
 * 真源采纳（快照一致 = 自回声/无变化，不重渲染）。「基线无推进」= 发起读前记下落盘基线，
 * 读返回后基线未变且内存仍等于该基线——期间发生过落盘（本地编辑写盘/并发采纳）或产生未落盘
 * 编辑，本次读发起早于落盘、结果不可信，一律丢弃；丢弃后由本次落盘触发的服务端自回声帧再次
 * 进入本函数，以干净状态采纳最新磁盘真源自愈。读失败保持现状（等下一帧或重新加载，不因一次
 * 网络抖动清空视图）。写盘被服务端拒绝（viewer）且本地脏态未消除期间，脏检查恒不通过、
 * 推送被忽略，视图与远端脱节直至编辑成功重试或重新加载——保存失败已有通知可见。
 */
async function adoptRemoteCalendar(): Promise<void> {
  const s = useCalendarStore.getState();
  const guardKey = s.loadedFor;
  if (!guardKey) return;
  const baselineAtStart = persistedItems;
  if (JSON.stringify(s.items) !== baselineAtStart) return;
  let raw: string | null;
  try {
    raw = await readCalendarRaw();
  } catch (e) {
    console.error("同步共享日历失败", e);
    return;
  }
  const latest = useCalendarStore.getState();
  if (latest.loadedFor !== guardKey) return;
  if (persistedItems !== baselineAtStart) return;
  if (JSON.stringify(latest.items) !== baselineAtStart) return;
  const items = parseCalendarItems(raw);
  const snapshot = JSON.stringify(items);
  if (snapshot === persistedItems) return;
  useCalendarStore.setState({ items });
  persistedItems = snapshot;
}

/** 日历域协作接线（builtin.calendar 载荷调用，随插件启停）：订阅团队日历落地广播帧。 */
export function registerCalendarCollabWiring(): () => void {
  return registerCollabMetaChanged((key) => {
    if (key !== SPACE_TEAM_META.calendar) return;
    void adoptRemoteCalendar();
  });
}
