/**
 * 应用级 UI 使用状态。
 *
 * 承载跨会话恢复的使用数据，磁盘 `app_data_dir/ui-state.json` 由 Rust `layout.rs`
 * 迷你窗口管理器**单一写者**持久化（schema `atelyx-ui-state/v1`，本 store 不直接写盘）：
 * - **布局（Rust 权威）**：布局列表 + 激活布局 + 撕裂窗口。本 store 只持有镜像——
 *   一切布局操作经 `services/layout.ts` 的 `layout_op` 命令发给 Rust，模型变更后
 *   Rust 全量广播 `layout-broadcast`，各窗口据此更新自身镜像并渲染。
 * - **非布局（JS 权威）**：上次打开文件 + 文件面板展开 + 最近打开 + 聚焦面板。
 *   本 store 持有并变更，防抖后经 `ui_state_patch` 补丁进 Rust 模型合并落盘。
 *
 * **应用级、跨仓库共享**：app_data_dir 本机独有、不随仓库同步；切仓库不清空、不重载。
 * `load` 在应用启动时调用一次（appStore.init）；撕裂窗口在其 `initPanel` 也调用一次
 * （每窗口各自持有本 store 实例，bootstrap + 订阅广播）。
 *
 * 分层：FileExplorerPanel / ProjectWorkspacePage / WorkspaceGrid / panelStore 走本 store，
 * 不直调 `services`。防抖 400ms（同 settingsStore.persistDebounced 模式）。
 */
import { create } from "zustand";
import { remapDirPrefix } from "@/utils/filename";
import { createPersistController } from "@/utils/persist";
import { layoutBootstrap, layoutFlush, layoutOp, onLayoutBroadcast, uiStatePatch } from "@/services/layout";
import {
  createDefaultLayouts,
  type DetachedWindow,
  type SplitDirection,
  type ViewKind,
  type WorkspaceLayout,
} from "@/types/workspaceLayout";
import { type AppUiState, type LayoutOp, type LayoutOpResult, type RecentFileEntry } from "@/types";

interface UiStateStore {
  /** 文件面板展开的文件夹相对路径集合（不可变更新，防 selector 无限重渲染）。 */
  fileExplorerExpanded: Set<string>;
  /** 上次打开的画布文件（相对仓库根；关闭/删除后清空）。 */
  lastCanvasFile: string | null;
  /** 上次打开的笔记文件（相对仓库根；关闭/删除后清空）。 */
  lastNoteFile: string | null;
  /** 上次打开的表格文件（相对仓库根；关闭/删除后清空）。 */
  lastTableFile: string | null;
  /** 工作区布局列表镜像（Rust 权威；至少一项，load 前为默认布局）。 */
  workspaceLayouts: WorkspaceLayout[];
  /** 激活布局 id 镜像（缺省 = 列表第一个）。 */
  activeLayoutId: string | null;
  /** 聚焦面板 id（画布快捷键门控；面板可能已被关闭/布局切换，渲染兜底聚焦第一个）。 */
  focusedPanelId: string | null;
  /** 撕裂窗口列表镜像（Rust 权威；应用级、跨布局共享）。 */
  detachedWindows: DetachedWindow[];
  /** 最近打开的文件（跨仓库记录、按 file+vaultId 去重置顶、上限截断；主页面板按当前仓库过滤）。 */
  recentFiles: RecentFileEntry[];
  /** 当前 ui-state 是否已 bootstrap（恢复 effect 依赖它避免在加载前误清状态）。 */
  loaded: boolean;
  /** bootstrap 失败标志（失败时以默认值渲染，但禁止后续 patch 落盘防覆盖磁盘）。 */
  loadFailed: boolean;

  /** 应用启动/撕裂窗口启动时调用：拉取快照 + 订阅布局广播。 */
  load: () => Promise<void>;
  /** 文件面板展开/收起文件夹（toggle）。 */
  toggleExpanded: (path: string) => void;
  /** 展开指定文件夹（移动文件到目标后让其可见；已展开的不重复处理）。 */
  expandDirs: (paths: string[]) => void;
  /** 「展开/收起全部」：dirPaths = 当前树全部文件夹路径；全部展开时切换为收起。 */
  toggleExpandAll: (dirPaths: string[]) => void;
  /** 记录打开的画布/笔记/表格文件（lastCanvasFile/lastNoteFile/lastTableFile，kind 区分）。 */
  recordOpenFile: (kind: LastOpenFileKind, file: string) => void;
  /** 记录最近打开的文件（recentFiles：去重置顶 + 截断；kind 与 vaultId 由调用方提供）。 */
  recordRecentFile: (file: string, kind: RecentFileEntry["kind"], vaultId: string) => void;
  /** 画布/笔记/表格重命名/移动后同步上次打开记录（旧路径命中才更新，kind 区分）。 */
  renameLastFile: (kind: LastOpenFileKind, oldFile: string, newFile: string) => void;
  /** 文件夹重命名后同步展开集合/上次打开文件（`oldDir/` 前缀 → `newDir/`）。 */
  renameByDir: (oldDir: string, newDir: string) => void;
  /** 文件夹删除后清理展开集合中该目录及子目录条目。 */
  removeExpandedByDir: (dir: string) => void;
  /** 关闭画布/笔记/表格：清空对应的上次打开记录（kind 区分）。 */
  closeFile: (kind: LastOpenFileKind) => void;
  /** 设置聚焦面板（点击面板时；null = 无聚焦）。 */
  setFocusedPanel: (panelId: string | null) => void;

  /** 添加视图到面板（下拉入口）：组内已有该视图 = 激活；否则新建标签并激活。 */
  addViewToPanel: (panelId: string, view: ViewKind) => void;
  /** 激活面板中的标签。 */
  setActiveTab: (panelId: string, tabId: string) => void;
  /** 关闭面板中的标签（标签右键菜单）：锁定标签拒关；最后一个标签关闭 → 面板留空。 */
  closeTab: (panelId: string, tabId: string) => void;
  /** 锁定/解锁面板中的标签（锁定 = 固定：禁拖/禁撕裂/禁关闭）。 */
  setTabLocked: (panelId: string, tabId: string, locked: boolean) => void;
  /** 切换面板中某标签的视图（标签右键「切换标签视图」；视图全局唯一约束由 Rust 校验）。 */
  setTabView: (panelId: string, tabId: string, view: ViewKind) => void;
  /** 面板标签组内排序。 */
  moveTabWithinPanel: (panelId: string, tabId: string, toIndex: number) => void;
  /** 窗口内跨面板移动标签（面板 A → 面板 B 标签组，默认尾部）。 */
  moveTabBetweenPanels: (
    fromPanelId: string,
    toPanelId: string,
    tabId: string,
    index?: number,
  ) => void;
  /** 分割激活布局中的面板：父 split 方向匹配时同级插入新空面板（多叉），否则嵌套回退。返回新面板 id。 */
  splitPanel: (
    panelId: string,
    direction: SplitDirection,
    position?: "before" | "after",
  ) => Promise<string | null>;
  /** 删除面板 = 整块移除（含其全部标签）并合并到父 Split 兄弟；最后一个面板不可删。 */
  closePanel: (panelId: string) => void;
  /** 撕裂标签：从面板移除（面板留空）→ 挂到应用级 detachedWindows，返回新窗口条目。 */
  tearOffTab: (
    panelId: string,
    tabId: string,
    bounds: DetachedWindow["bounds"],
  ) => Promise<DetachedWindow | null>;
  /** 撕裂窗口再撕裂：把标签从撕裂窗口移到新的撕裂窗口条目（源窗口拖空后由调用方回收）。 */
  tearOffFromDetached: (
    windowId: string,
    tabId: string,
    bounds: DetachedWindow["bounds"],
  ) => Promise<DetachedWindow | null>;
  /** 拖回：把撕裂窗口中的标签停靠进主窗口面板（默认尾部并激活；源窗口拖空自动移除）。 */
  dockTabIntoPanel: (panelId: string, tabId: string, index?: number) => void;
  /** 拖入：把标签停靠进撕裂窗口（来源 = 树面板或另一撕裂窗口；同窗口 = 组内排序）。 */
  dockTabIntoDetached: (windowId: string, tabId: string, index?: number) => void;
  /** 向撕裂窗口添加新视图标签（视图全局唯一，已占用则忽略；面板窗口「添加视图」入口）。 */
  detachedAddView: (windowId: string, view: ViewKind) => void;
  /** 激活撕裂窗口中的标签。 */
  detachedSetActive: (windowId: string, tabId: string) => void;
  /** 关闭撕裂窗口中的标签（锁定拒关；拖空后窗口条目移除，OS 窗口关闭由 panelStore 处理）。 */
  detachedCloseTab: (windowId: string, tabId: string) => void;
  /** 锁定/解锁撕裂窗口中的标签。 */
  detachedSetLocked: (windowId: string, tabId: string, locked: boolean) => void;
  /** 切换撕裂窗口中某标签的视图（标签右键「切换标签视图」）。 */
  detachedSetTabView: (windowId: string, tabId: string, view: ViewKind) => void;
  /** 撕裂窗口标签组内排序。 */
  detachedMoveTab: (windowId: string, tabId: string, toIndex: number) => void;
  /** 移除撕裂窗口条目（OS 窗口已关闭/拖空自动关窗时调用）。 */
  removeDetachedWindow: (windowId: string) => void;
  /** 拖拽调宽回写 Split 子树尺寸比例（百分数，和 = 100，长度 = children 长度；前端防抖提交）。 */
  setLayoutSizes: (splitId: string, sizes: number[]) => void;
  /** 新建布局（复制当前激活布局），命名「布局 N」自动去重，并激活。 */
  addLayout: () => void;
  /** 重命名布局。 */
  renameLayout: (id: string, name: string) => void;
  /** 删除布局（最后一个不可删）。 */
  deleteLayout: (id: string) => void;
  /** 激活布局（切换布局：仅替换面板网格，文件状态与撕裂窗口不动）。 */
  activateLayout: (id: string) => void;
  /** 调整布局顺序（布局 tab 拖拽排序）。 */
  moveLayout: (fromIndex: number, toIndex: number) => void;
  /** 立即落盘（应用退出/切页面前 flush 用，防 debounce 窗口内丢状态）。 */
  flush: () => Promise<void>;
}

/** 上次打开文件记录类别（画布/笔记/表格）。 */
type LastOpenFileKind = "canvas" | "note" | "table";

/** 类别 → 上次打开文件字段名映射（record/rename/close 三个 action 经此写各自字段）。 */
const LAST_FILE_KEYS: Record<LastOpenFileKind, "lastCanvasFile" | "lastNoteFile" | "lastTableFile"> = {
  canvas: "lastCanvasFile",
  note: "lastNoteFile",
  table: "lastTableFile",
};

/** 最近打开文件列表上限（去重置顶后截断）。 */
const MAX_RECENT_FILES = 50;

/** 撕裂窗口条目合法性校验（bootstrap 时过滤损坏条目，与 Rust 侧一致）。 */
function isValidDetached(w: DetachedWindow): boolean {
  return !!w && typeof w.id === "string" && Array.isArray(w.tabs) && !!w.bounds;
}

/** 防抖持久化控制器：非布局字段防抖 patch 到 Rust（400ms；写盘由 Rust 侧统一防抖）。 */
const persistCtl = createPersistController({
  persist: async () => {
    const s = useUiStateStore.getState();
    // 未加载或 bootstrap 失败时不补丁：失败时没有磁盘基线可依赖，默认值若落盘会覆盖
    // 磁盘 recentFiles/expanded（取舍：宁可丢弃本次会话补丁，不覆盖既有数据）
    if (!s.loaded || s.loadFailed) return;
    try {
      await uiStatePatch(nonLayoutPatch(s));
    } catch (e) {
      console.error("补丁应用级 UI 状态失败", e);
    }
  },
  delay: 400,
});

/** 非布局字段 → Rust 补丁（只发 JS 拥有的字段）。 */
function nonLayoutPatch(s: UiStateStore): import("@/types").UiStatePatch {
  return {
    fileExplorerExpanded: [...s.fileExplorerExpanded],
    lastCanvasFile: s.lastCanvasFile,
    lastNoteFile: s.lastNoteFile,
    lastTableFile: s.lastTableFile,
    focusedPanelId: s.focusedPanelId,
    recentFiles: s.recentFiles,
  };
}

function persistDebounced(): void {
  persistCtl.schedule();
}

/** 布局命令 fire-and-forget（错误静默：布局模型由 Rust 收敛，失败仅影响一次操作）。 */
function sendLayoutOp(op: LayoutOp): void {
  void layoutOp(op).catch((e) => console.error("布局操作失败", e));
}

/** 布局广播订阅守卫（每窗口实例只订阅一次）。 */
let broadcastSubscribed = false;

/** 广播 → 镜像（只应用布局字段；非布局字段 JS 是权威，不随广播覆盖）。
 * 收到广播即证明 Rust 存活且有真实布局 → 清除 bootstrap 失败标记，恢复后续 patch 落盘。 */
function applyLayoutMirror(state: AppUiState): void {
  if (!state || !Array.isArray(state.workspaceLayouts) || state.workspaceLayouts.length === 0) return;
  useUiStateStore.setState({
    workspaceLayouts: state.workspaceLayouts,
    activeLayoutId: state.activeLayoutId ?? state.workspaceLayouts[0].id ?? null,
    detachedWindows: Array.isArray(state.detachedWindows)
      ? state.detachedWindows.filter(isValidDetached)
      : [],
    loadFailed: false,
  });
}

export const useUiStateStore = create<UiStateStore>((set, get) => {
  return {
    fileExplorerExpanded: new Set(),
    lastCanvasFile: null,
    lastNoteFile: null,
    lastTableFile: null,
    workspaceLayouts: createDefaultLayouts(),
    activeLayoutId: null,
    focusedPanelId: null,
    detachedWindows: [],
    recentFiles: [],
    loaded: false,
    loadFailed: false,

    load: async () => {
      persistCtl.cancel();
      // 订阅广播（先订阅后拉快照：快照是某一时刻的一致状态，其后广播更新；顺序应用无竞态）
      if (!broadcastSubscribed) {
        broadcastSubscribed = true;
        void onLayoutBroadcast(applyLayoutMirror).catch((e) => {
          console.error("订阅布局广播失败", e);
          broadcastSubscribed = false;
        });
      }
      try {
        const disk = await layoutBootstrap();
        set({
          fileExplorerExpanded: new Set(disk.fileExplorerExpanded ?? []),
          lastCanvasFile: disk.lastCanvasFile ?? null,
          lastNoteFile: disk.lastNoteFile ?? null,
          lastTableFile: disk.lastTableFile ?? null,
          workspaceLayouts:
            Array.isArray(disk.workspaceLayouts) && disk.workspaceLayouts.length > 0
              ? disk.workspaceLayouts
              : [],
          activeLayoutId: disk.activeLayoutId ?? disk.workspaceLayouts?.[0]?.id ?? null,
          focusedPanelId: disk.focusedPanelId ?? null,
          detachedWindows: Array.isArray(disk.detachedWindows)
            ? disk.detachedWindows.filter(isValidDetached)
            : [],
          recentFiles: Array.isArray(disk.recentFiles) ? disk.recentFiles : [],
          loaded: true,
          loadFailed: false,
        });
      } catch (e) {
        console.error("读取应用级 UI 状态失败", e);
        // 失败时以默认值渲染（恢复 effect 依赖 loaded=true 才跑），但标记错误态禁止
        // 后续 patch 落盘：默认值一旦经补丁进 Rust 会覆盖磁盘 recentFiles/expanded
        set({
          fileExplorerExpanded: new Set(),
          lastCanvasFile: null,
          lastNoteFile: null,
          lastTableFile: null,
          workspaceLayouts: createDefaultLayouts(),
          activeLayoutId: null,
          focusedPanelId: null,
          detachedWindows: [],
          recentFiles: [],
          loaded: true,
          loadFailed: true,
        });
      }
    },

    toggleExpanded: (path) => {
      set((s) => {
        const next = new Set(s.fileExplorerExpanded);
        if (next.has(path)) next.delete(path);
        else next.add(path);
        return { fileExplorerExpanded: next };
      });
      persistDebounced();
    },

    expandDirs: (paths) => {
      if (paths.length === 0) return;
      set((s) => {
        const next = new Set(s.fileExplorerExpanded);
        for (const p of paths) next.add(p);
        return { fileExplorerExpanded: next };
      });
      persistDebounced();
    },

    toggleExpandAll: (dirPaths) => {
      set((s) => {
        const allExpanded =
          dirPaths.length > 0 && dirPaths.every((p) => s.fileExplorerExpanded.has(p));
        return { fileExplorerExpanded: allExpanded ? new Set() : new Set(dirPaths) };
      });
      persistDebounced();
    },

    recordOpenFile: (kind, file) => setLastFile(set, kind, file),

    recordRecentFile: (file, kind, vaultId) => {
      const next = [
        { file, kind, vaultId, openedAt: Date.now() },
        ...get().recentFiles.filter((r) => !(r.file === file && r.vaultId === vaultId)),
      ].slice(0, MAX_RECENT_FILES);
      set({ recentFiles: next });
      persistDebounced();
    },

    renameLastFile: (kind, oldFile, newFile) => {
      if (get()[LAST_FILE_KEYS[kind]] !== oldFile) return;
      setLastFile(set, kind, newFile);
    },

    renameByDir: (oldDir, newDir) => {
      const s = get();
      let expandedChanged = false;
      const expanded = new Set<string>();
      for (const p of s.fileExplorerExpanded) {
        const next = remapDirPrefix(p, oldDir, newDir);
        if (next !== p) expandedChanged = true;
        expanded.add(next);
      }
      const lastCanvasFile = s.lastCanvasFile ? remapDirPrefix(s.lastCanvasFile, oldDir, newDir) : null;
      const lastNoteFile = s.lastNoteFile ? remapDirPrefix(s.lastNoteFile, oldDir, newDir) : null;
      const lastTableFile = s.lastTableFile ? remapDirPrefix(s.lastTableFile, oldDir, newDir) : null;
      const changed =
        expandedChanged ||
        lastCanvasFile !== s.lastCanvasFile ||
        lastNoteFile !== s.lastNoteFile ||
        lastTableFile !== s.lastTableFile;
      if (!changed) return;
      set({ fileExplorerExpanded: expanded, lastCanvasFile, lastNoteFile, lastTableFile });
      persistDebounced();
    },

    removeExpandedByDir: (dir) => {
      const prefix = `${dir}/`;
      const next = new Set([...get().fileExplorerExpanded].filter((p) => p !== dir && !p.startsWith(prefix)));
      if (next.size === get().fileExplorerExpanded.size) return;
      set({ fileExplorerExpanded: next });
      persistDebounced();
    },

    closeFile: (kind) => setLastFile(set, kind, null),

    setFocusedPanel: (panelId) => {
      if (get().focusedPanelId === panelId) return;
      set({ focusedPanelId: panelId });
      persistDebounced();
    },

    // ---- 布局操作：全部发命令，模型由 Rust 变更 + 广播收敛 ----
    addViewToPanel: (panelId, view) => sendLayoutOp({ op: "addView", panelId, view }),
    setActiveTab: (panelId, tabId) => sendLayoutOp({ op: "setActive", panelId, tabId }),
    closeTab: (panelId, tabId) => sendLayoutOp({ op: "closeTab", panelId, tabId }),
    setTabLocked: (panelId, tabId, locked) => sendLayoutOp({ op: "setLocked", panelId, tabId, locked }),
    setTabView: (panelId, tabId, view) => sendLayoutOp({ op: "setTabView", panelId, tabId, view }),
    moveTabWithinPanel: (panelId, tabId, toIndex) =>
      sendLayoutOp({ op: "moveTabWithin", panelId, tabId, toIndex }),

    moveTabBetweenPanels: (fromPanelId, toPanelId, tabId, index) =>
      sendLayoutOp({ op: "moveTabBetween", fromPanelId, toPanelId, tabId, index }),

    splitPanel: async (panelId, direction, position = "after") => {
      try {
        const r: LayoutOpResult = await layoutOp({ op: "splitPanel", panelId, direction, position });
        return r.splitPanelId ?? null;
      } catch (e) {
        console.error("分割面板失败", e);
        return null;
      }
    },

    closePanel: (panelId) => {
      sendLayoutOp({ op: "closePanel", panelId });
      // 聚焦面板指向被删面板 → 清空（非布局字段，本地收敛）
      if (get().focusedPanelId === panelId) {
        set({ focusedPanelId: null });
        persistDebounced();
      }
    },

    tearOffTab: async (panelId, tabId, bounds) => {
      try {
        const r: LayoutOpResult = await layoutOp({ op: "tearOff", panelId, tabId, bounds });
        return r.detachedWindow ?? null;
      } catch (e) {
        console.error("撕裂标签失败", e);
        return null;
      }
    },

    tearOffFromDetached: async (windowId, tabId, bounds) => {
      try {
        const r: LayoutOpResult = await layoutOp({ op: "tearOffFromDetached", windowId, tabId, bounds });
        return r.detachedWindow ?? null;
      } catch (e) {
        console.error("撕裂窗口再撕裂失败", e);
        return null;
      }
    },

    dockTabIntoPanel: (panelId, tabId, index) =>
      sendLayoutOp({ op: "dockIntoPanel", panelId, tabId, index }),

    dockTabIntoDetached: (windowId, tabId, index) =>
      sendLayoutOp({ op: "dockIntoDetached", windowId, tabId, index }),

    detachedAddView: (windowId, view) => sendLayoutOp({ op: "detachedAddView", windowId, view }),
    detachedSetActive: (windowId, tabId) => sendLayoutOp({ op: "detachedSetActive", windowId, tabId }),
    detachedCloseTab: (windowId, tabId) => sendLayoutOp({ op: "detachedCloseTab", windowId, tabId }),
    detachedSetLocked: (windowId, tabId, locked) =>
      sendLayoutOp({ op: "detachedSetLocked", windowId, tabId, locked }),
    detachedSetTabView: (windowId, tabId, view) =>
      sendLayoutOp({ op: "detachedSetTabView", windowId, tabId, view }),
    detachedMoveTab: (windowId, tabId, toIndex) =>
      sendLayoutOp({ op: "detachedMoveTab", windowId, tabId, toIndex }),
    removeDetachedWindow: (windowId) => sendLayoutOp({ op: "removeDetachedWindow", windowId }),

    // 拖拽调宽：resize 拖拽高频，前端防抖提交（Rust 仍是最终权威）
    setLayoutSizes: debounceSetSizes(),

    addLayout: () => {
      sendLayoutOp({ op: "addLayout" });
      set({ focusedPanelId: null });
      persistDebounced();
    },
    renameLayout: (id, name) => sendLayoutOp({ op: "renameLayout", id, name }),
    deleteLayout: (id) => {
      sendLayoutOp({ op: "deleteLayout", id });
      if (get().activeLayoutId === id) {
        set({ focusedPanelId: null });
        persistDebounced();
      }
    },
    activateLayout: (id) => {
      sendLayoutOp({ op: "activateLayout", id });
      set({ focusedPanelId: null });
      persistDebounced();
    },
    moveLayout: (fromIndex, toIndex) => sendLayoutOp({ op: "moveLayout", fromIndex, toIndex }),

    flush: async () => {
      // 布局字段由 Rust 侧已调度落盘；非布局字段补丁立即发送 + 强制 Rust 落盘
      await persistCtl.flush();
      await layoutFlush().catch((e) => console.error("布局状态落盘失败", e));
    },
  };
});

/** 写上次打开文件字段（kind → 字段映射统一出口：set 后统一走防抖补丁）。 */
function setLastFile(
  set: (partial: Partial<import("zustand").StoreApi<UiStateStore>["getState"]>) => void,
  kind: LastOpenFileKind,
  file: string | null,
): void {
  const patch: Partial<Pick<UiStateStore, "lastCanvasFile" | "lastNoteFile" | "lastTableFile">> = {};
  patch[LAST_FILE_KEYS[kind]] = file;
  set(patch);
  persistDebounced();
}

/** setLayoutSizes 防抖（trailing）：resize 拖拽期间最多几百 ms 一次 IPC。 */
function debounceSetSizes(): (splitId: string, sizes: number[]) => void {
  let timer: number | null = null;
  let pending: { splitId: string; sizes: number[] } | null = null;
  return (splitId, sizes) => {
    pending = { splitId, sizes };
    if (timer !== null) return;
    timer = window.setTimeout(() => {
      timer = null;
      if (!pending) return;
      const { splitId: id, sizes: sz } = pending;
      pending = null;
      sendLayoutOp({ op: "setLayoutSizes", splitId: id, sizes: sz });
    }, 250);
  };
}
