/**
 * 仓库历史只读聚合 store（主页「仓库历史」面板 + 日历活动计数共用数据源）。
 *
 * 数据来自 `services/home.listRepoHistory`（经内容面按身份分派：本地聚合历史侧文件，
 * 协作空间走服务端聚合端点）：
 * - `entries`：版本流（ts 倒序、上限）→ 仓库历史面板；
 * - `dailyCounts`：全量版本按日计数 → 日历活动密度。
 *
 * 性能：同仓库会话内缓存（`loadedFor` 存仓库身份键），主页面板随布局切换反复挂载时，
 * 已缓存仓库只做后台静默刷新（不清空、不转圈，async 命令不阻塞 UI），避免每次
 * 进主页都整树重扫；`inflight` 单飞去重（主页日历/仓库历史两面板同帧并发触发）。
 * 切仓库时由面板经仓库身份 effect 触发重载（未缓存仓库走首载：清空 + loading）。
 * 加载失败静默降级为空（尽力而为，不阻塞面板）。
 */
import { create } from "zustand";
import { listRepoHistory, type DailyCount, type RepoHistoryEntry } from "@/services/home";
import { useAppStore, selectVaultIdentityKey } from "@/stores/appStore";

interface RepoHistoryState {
  loading: boolean;
  /** 版本流（ts 倒序，上限）。 */
  entries: RepoHistoryEntry[];
  /** 按日版本计数（活动日历）。 */
  dailyCounts: DailyCount[];
  /** 已加载的仓库身份键（null = 未加载；同仓库再进主页仅后台静默刷新，不重扫）。 */
  loadedFor: string | null;
  /** 重载（切仓库/面板挂载时调用；同仓库已缓存走后台刷新）。 */
  load: () => Promise<void>;
}

/** 加载代数：切仓库时旧请求在途 → 递增代数使其结果作废（loading 恒由最新代数复位，防卡死）。 */
let loadSeq = 0;
/** 同仓库在途加载（单飞去重）：主页日历/仓库历史两面板同帧挂载同时触发 load，避免重复扫盘。 */
const inflight = new Map<string, Promise<void>>();

export const useRepoHistoryStore = create<RepoHistoryState>((set, get) => ({
  loading: false,
  entries: [],
  dailyCounts: [],
  loadedFor: null,
  load: () => {
    // 身份键（local = root；space = space:<serverUrl>#<spaceId>）：空间无本地 root，按 root 会让加载永不发生
    const identity = selectVaultIdentityKey(useAppStore.getState());
    if (identity === "none") return Promise.resolve();
    const running = inflight.get(identity);
    if (running) return running;
    // 已缓存：同仓库再进主页不清空不转圈，仅后台静默刷新（async 命令不阻塞 UI，数据就位后原位更新）
    const cached = get().loadedFor === identity;
    const seq = ++loadSeq;
    const p = (async () => {
      if (!cached) set({ loading: true, entries: [], dailyCounts: [] });
      try {
        const res = await listRepoHistory();
        // 竞态守卫：等待期间已触发新加载（seq 落后）或已切仓库 → 丢弃陈旧结果
        if (seq !== loadSeq || selectVaultIdentityKey(useAppStore.getState()) !== identity) return;
        set({ entries: res.entries, dailyCounts: res.dailyCounts, loadedFor: identity });
      } catch (e) {
        console.error("加载仓库历史失败", e);
        // 首载失败清空兜底；后台刷新失败保留已缓存数据（不闪空）
        if (seq === loadSeq && !cached) set({ entries: [], dailyCounts: [] });
      } finally {
        // 首载路径才复位 loading；后台刷新不动 loading，防缓存数据上闪 spinner
        if (seq === loadSeq && !cached) set({ loading: false });
      }
    })();
    inflight.set(identity, p);
    return p.finally(() => inflight.delete(identity));
  },
}));
