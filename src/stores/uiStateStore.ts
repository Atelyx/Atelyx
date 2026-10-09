/**
 * 应用级 UI 使用状态（app_data_dir/ui-state.json，Rust `layout.rs` 单一写者持久化，schema `atelyx-ui-state/v2`）。
 * 布局字段 Rust 权威：本 store 只持镜像，一切布局操作经 `services/layout.ts` 的 `layout_op` 命令、
 * 由 Rust 全量广播 `layout-broadcast` 收敛；非布局字段 JS 权威：防抖经 `ui_state_patch` 补丁合并落盘。
 * 应用级、跨仓库共享（app_data_dir 本机独有），切仓库不清空不重载；组件不直调 `services`。
 */
import { create } from "zustand";
import { remapDirKey } from "@/utils/filename";
import { createPersistController } from "@/utils/persist";
import { layoutBootstrap, layoutFlush, layoutOp, onLayoutBroadcast, uiStatePatch } from "@/services/layout";
import {
  type DetachedWindow,
  type LayoutNode,
  type Scene,
  type SplitDirection,
  type ViewKind,
  type WorkspaceLayout,
} from "@/types/workspaceLayout";
import { createDefaultScenes } from "@/utils/workspaceLayout";
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
  /** 场景列表镜像（Rust 权威；布局之上的容器，至少一项，load 前为默认场景）。 */
  scenes: Scene[];
  /** 激活场景 id 镜像（缺省 = 列表第一个）。 */
  activeSceneId: string | null;
  /** 激活场景的布局列表镜像（Rust 权威；至少一项，load 前为默认布局）。 */
  workspaceLayouts: WorkspaceLayout[];
  /** 激活布局 id 镜像（激活场景内；缺省 = 列表第一个）。 */
  activeLayoutId: string | null;
  /** 聚焦面板 id（画布快捷键门控；面板可能已被关闭/布局切换，渲染兜底聚焦第一个）。 */
  focusedPanelId: string | null;
  /** 撕裂窗口列表镜像（Rust 权威；应用级、跨布局共享）。 */
  detachedWindows: DetachedWindow[];
  /** 最近打开的文件（跨仓库记录、按 file+仓库身份去重置顶、上限截断；主页面板按当前仓库过滤）。 */
  recentFiles: RecentFileEntry[];
  /** single 槽手动胜者覆盖（槽 → 钉住的贡献 id；设置 → 插件里的冲突裁决写入，覆盖 priority 决胜）。 */
  slotWinnerOverrides: Record<string, string>;
  /** 当前 ui-state 是否已 bootstrap（恢复 effect 依赖它避免在加载前误清状态）。 */
  loaded: boolean;
  /** bootstrap 失败标志（失败时以默认值渲染，但禁止后续 patch 落盘防覆盖磁盘）。 */
  loadFailed: boolean;

  /** 应用启动（appStore.init）/撕裂窗口启动（initPanel）时调用：拉取快照 + 订阅布局广播（每窗口各自实例）。 */
  load: () => Promise<void>;
  /** 文件面板展开/收起文件夹（toggle）。 */
  toggleExpanded: (path: string) => void;
  /** 展开指定文件夹（移动文件到目标后让其可见；已展开的不重复处理）。 */
  expandDirs: (paths: string[]) => void;
  /** 「展开/收起全部」：dirPaths = 当前树全部文件夹路径；全部展开时切换为收起。 */
  toggleExpandAll: (dirPaths: string[]) => void;
  /** 记录打开的画布/笔记/表格文件（lastCanvasFile/lastNoteFile/lastTableFile，kind 区分）。 */
  recordOpenFile: (kind: LastOpenFileKind, file: string) => void;
  /** 记录最近打开的文件（recentFiles：去重置顶 + 截断；kind 与仓库身份键由调用方提供）。 */
  recordRecentFile: (file: string, kind: RecentFileEntry["kind"], vaultKey: string) => void;
  /** 画布/笔记/表格重命名/移动后同步上次打开记录（旧路径命中才更新，kind 区分）。 */
  renameLastFile: (kind: LastOpenFileKind, oldFile: string, newFile: string) => void;
  /** 文件夹重命名/移动后同步展开集合/上次打开文件（目录键精确或 `oldDir/` 前缀命中 → 换新路径）。 */
  renameByDir: (oldDir: string, newDir: string) => void;
  /** 文件夹删除后清理展开集合中该目录及子目录条目。 */
  removeExpandedByDir: (dir: string) => void;
  /** 关闭画布/笔记/表格：清空对应的上次打开记录（kind 区分）。 */
  closeFile: (kind: LastOpenFileKind) => void;
  /** 设置聚焦面板（点击面板时；null = 无聚焦）。 */
  setFocusedPanel: (panelId: string | null) => void;
  /** 钉住/取消 single 槽胜者（contribId = 钉住的贡献 id；null = 跟随 priority 决胜）。 */
  setSlotWinner: (slot: string, contribId: string | null) => void;

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
  /** 拖拽调宽回写 Split 子树尺寸比例（百分数，和 = 100，长度 = children 长度；前端防抖提交）。 */
  setLayoutSizes: (splitId: string, sizes: number[]) => void;
  /** 新建布局（单个空面板占位），命名「布局 N」自动去重，并激活。 */
  addLayout: () => void;
  /** 重命名布局。 */
  renameLayout: (id: string, name: string) => void;
  /** 删除布局（最后一个不可删）。 */
  deleteLayout: (id: string) => void;
  /** 激活布局（切换布局：仅替换面板网格，文件状态与撕裂窗口不动）。 */
  activateLayout: (id: string) => void;
  /** 调整布局顺序（布局 tab 拖拽排序）。 */
  moveLayout: (fromIndex: number, toIndex: number) => void;
  /** 新建场景（复制当前激活场景），命名「场景 N」自动去重，并激活。 */
  addScene: () => void;
  /** 重命名场景。 */
  renameScene: (id: string, name: string) => void;
  /** 删除场景（默认场景不可删；场景内布局一并删除）。 */
  deleteScene: (id: string) => void;
  /** 激活场景（切换场景：恢复该场景记忆的激活布局，文件状态与撕裂窗口不动）。 */
  activateScene: (id: string) => void;
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
    if (Object.keys(pendingPatch).length === 0) return;
    const patch = pendingPatch;
    pendingPatch = {};
    try {
      await uiStatePatch(patch);
    } catch (e) {
      console.error("补丁应用级 UI 状态失败", e);
      // 发送失败把本轮字段并回（等待期间的新标记后写胜出），下次防抖窗口重发
      pendingPatch = { ...patch, ...pendingPatch };
    }
  },
  delay: 400,
});

/** 待发送的非布局字段补丁（增量：各 setter 只标记自己变更的字段，防抖后合并发送）。 */
let pendingPatch: import("@/types").UiStatePatch = {};

/** 增量标记非布局补丁字段并调度发送。
 *  撕裂窗口各自持有独立 store 实例且收不到其他窗口的非布局字段更新——整包回写会让
 *  本窗口的陈旧副本覆盖主窗口较新的值；只发本窗口真正变更的字段，其余字段不落笔。 */
function markPatch(fields: import("@/types").UiStatePatch): void {
  pendingPatch = { ...pendingPatch, ...fields };
  persistDebounced();
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

/** 场景快照 → 布局镜像字段：workspaceLayouts = 主页（激活场景专属）+ 激活场景布局列表，
 *  activeLayoutId = 顶层激活布局 id。消费方（布局 tab 条/面板网格/进仓自动切场景）只看这两个
 *  派生字段，不感知场景结构。入参来自 Rust normalize 后的快照/广播，激活态由 normalize 保证。 */
function deriveLayoutMirror(active: Scene, activeLayoutId: string) {
  return {
    workspaceLayouts: [active.homeLayout, ...active.layouts],
    activeLayoutId,
  };
}

/** 广播 → 镜像（只应用布局字段；非布局字段 JS 是权威，不随广播覆盖）。
 * 收到广播即证明 Rust 存活且有真实布局 → 清除 bootstrap 失败标记，恢复后续 patch 落盘。 */
function applyLayoutMirror(state: AppUiState): void {
  if (!state || !Array.isArray(state.scenes) || state.scenes.length === 0) return;
  const scenes = state.scenes;
  const activeSceneId = state.activeSceneId;
  const active = scenes.find((s) => s.id === activeSceneId)!;
  useUiStateStore.setState({
    scenes,
    activeSceneId,
    ...deriveLayoutMirror(active, state.activeLayoutId),
    detachedWindows: Array.isArray(state.detachedWindows)
      ? state.detachedWindows.filter(isValidDetached)
      : [],
    loadFailed: false,
  });
}

export const useUiStateStore = create<UiStateStore>((set, get) => {
  // load 前的渲染兜底种子（与 load 失败路径同源；单个实例保证字段间 id 一致）
  const bootScenes = createDefaultScenes();
  return {
    fileExplorerExpanded: new Set(),
    lastCanvasFile: null,
    lastNoteFile: null,
    lastTableFile: null,
    scenes: bootScenes,
    activeSceneId: bootScenes[0].id,
    workspaceLayouts: [bootScenes[0].homeLayout, ...bootScenes[0].layouts],
    activeLayoutId: bootScenes[0].activeLayoutId,
    focusedPanelId: null,
    detachedWindows: [],
    recentFiles: [],
    slotWinnerOverrides: {},
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
        // 快照来自 Rust normalize 后的模型：scenes 恒非空、激活态恒有效
        const scenes = disk.scenes;
        const activeSceneId = disk.activeSceneId;
        set({
          fileExplorerExpanded: new Set(disk.fileExplorerExpanded ?? []),
          lastCanvasFile: disk.lastCanvasFile ?? null,
          lastNoteFile: disk.lastNoteFile ?? null,
          lastTableFile: disk.lastTableFile ?? null,
          scenes,
          activeSceneId,
          ...deriveLayoutMirror(
            scenes.find((s) => s.id === activeSceneId)!,
            disk.activeLayoutId,
          ),
          focusedPanelId: disk.focusedPanelId ?? null,
          detachedWindows: Array.isArray(disk.detachedWindows)
            ? disk.detachedWindows.filter(isValidDetached)
            : [],
          recentFiles: Array.isArray(disk.recentFiles) ? disk.recentFiles : [],
          slotWinnerOverrides:
            disk.slotWinnerOverrides &&
            typeof disk.slotWinnerOverrides === "object" &&
            !Array.isArray(disk.slotWinnerOverrides)
              ? disk.slotWinnerOverrides
              : {},
          loaded: true,
          loadFailed: false,
        });
      } catch (e) {
        console.error("读取应用级 UI 状态失败", e);
        // 失败时以默认值渲染（恢复 effect 依赖 loaded=true 才跑），但标记错误态禁止
        // 后续 patch 落盘：默认值一旦经补丁进 Rust 会覆盖磁盘 recentFiles/expanded
        const scenes = createDefaultScenes();
        set({
          fileExplorerExpanded: new Set(),
          lastCanvasFile: null,
          lastNoteFile: null,
          lastTableFile: null,
          scenes,
          activeSceneId: scenes[0].id,
          ...deriveLayoutMirror(scenes[0], scenes[0].activeLayoutId),
          focusedPanelId: null,
          detachedWindows: [],
          recentFiles: [],
          slotWinnerOverrides: {},
          loaded: true,
          loadFailed: true,
        });
      }
    },

    toggleExpanded: (path) => {
      const next = new Set(get().fileExplorerExpanded);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      set({ fileExplorerExpanded: next });
      markPatch({ fileExplorerExpanded: [...next] });
    },

    expandDirs: (paths) => {
      if (paths.length === 0) return;
      const next = new Set(get().fileExplorerExpanded);
      for (const p of paths) next.add(p);
      set({ fileExplorerExpanded: next });
      markPatch({ fileExplorerExpanded: [...next] });
    },

    toggleExpandAll: (dirPaths) => {
      const allExpanded =
        dirPaths.length > 0 && dirPaths.every((p) => get().fileExplorerExpanded.has(p));
      const next = allExpanded ? new Set<string>() : new Set(dirPaths);
      set({ fileExplorerExpanded: next });
      markPatch({ fileExplorerExpanded: [...next] });
    },

    recordOpenFile: (kind, file) => setLastFile(set, kind, file),

    recordRecentFile: (file, kind, vaultKey) => {
      const next = [
        { file, kind, vaultKey, openedAt: Date.now() },
        ...get().recentFiles.filter((r) => !(r.file === file && r.vaultKey === vaultKey)),
      ].slice(0, MAX_RECENT_FILES);
      set({ recentFiles: next });
      markPatch({ recentFiles: next });
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
        const next = remapDirKey(p, oldDir, newDir);
        if (next !== p) expandedChanged = true;
        expanded.add(next);
      }
      const lastCanvasFile = s.lastCanvasFile ? remapDirKey(s.lastCanvasFile, oldDir, newDir) : null;
      const lastNoteFile = s.lastNoteFile ? remapDirKey(s.lastNoteFile, oldDir, newDir) : null;
      const lastTableFile = s.lastTableFile ? remapDirKey(s.lastTableFile, oldDir, newDir) : null;
      const changed =
        expandedChanged ||
        lastCanvasFile !== s.lastCanvasFile ||
        lastNoteFile !== s.lastNoteFile ||
        lastTableFile !== s.lastTableFile;
      if (!changed) return;
      set({ fileExplorerExpanded: expanded, lastCanvasFile, lastNoteFile, lastTableFile });
      // 只标记实际变化的字段；last*File 为 null 的字段不标记（陈旧 null 副本不得误清其他窗口记录）。
      const patch: import("@/types").UiStatePatch = { fileExplorerExpanded: [...expanded] };
      if (lastCanvasFile !== s.lastCanvasFile && lastCanvasFile !== null) patch.lastCanvasFile = lastCanvasFile;
      if (lastNoteFile !== s.lastNoteFile && lastNoteFile !== null) patch.lastNoteFile = lastNoteFile;
      if (lastTableFile !== s.lastTableFile && lastTableFile !== null) patch.lastTableFile = lastTableFile;
      markPatch(patch);
    },

    removeExpandedByDir: (dir) => {
      const prefix = `${dir}/`;
      const next = new Set([...get().fileExplorerExpanded].filter((p) => p !== dir && !p.startsWith(prefix)));
      if (next.size === get().fileExplorerExpanded.size) return;
      set({ fileExplorerExpanded: next });
      markPatch({ fileExplorerExpanded: [...next] });
    },

    closeFile: (kind) => {
      setLastFile(set, kind, null);
      // 用户显式关闭：定向清除补丁让 Rust 侧「上次打开」记录立即失效（否则重启仍会恢复
      // 已关闭的文件）；常规 setLastFile 路径不携带 null（见 setLastFile），防陈旧副本误清。
      if (get().loaded && !get().loadFailed) {
        markPatch({ [LAST_FILE_KEYS[kind]]: null });
      }
    },

    setFocusedPanel: (panelId) => {
      if (get().focusedPanelId === panelId) return;
      set({ focusedPanelId: panelId });
      markPatch({ focusedPanelId: panelId });
    },

    setSlotWinner: (slot, contribId) => {
      const cur = get().slotWinnerOverrides;
      const next = { ...cur };
      if (contribId === null) {
        // 槽名是插件任意字符串，用 hasOwn（`in` 会命中原型链属性，如 "constructor"）。
        if (!Object.hasOwn(next, slot)) return;
        delete next[slot];
      } else {
        if (next[slot] === contribId) return;
        next[slot] = contribId;
      }
      set({ slotWinnerOverrides: next });
      markPatch({ slotWinnerOverrides: next });
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
        markPatch({ focusedPanelId: null });
      }
    },

    detachedAddView: (windowId, view) => sendLayoutOp({ op: "detachedAddView", windowId, view }),
    detachedSetActive: (windowId, tabId) => sendLayoutOp({ op: "detachedSetActive", windowId, tabId }),
    detachedCloseTab: (windowId, tabId) => sendLayoutOp({ op: "detachedCloseTab", windowId, tabId }),
    detachedSetLocked: (windowId, tabId, locked) =>
      sendLayoutOp({ op: "detachedSetLocked", windowId, tabId, locked }),
    detachedSetTabView: (windowId, tabId, view) =>
      sendLayoutOp({ op: "detachedSetTabView", windowId, tabId, view }),
    detachedMoveTab: (windowId, tabId, toIndex) =>
      sendLayoutOp({ op: "detachedMoveTab", windowId, tabId, toIndex }),

    // 拖拽调宽：resize 拖拽高频，前端防抖提交（Rust 仍是最终权威）
    setLayoutSizes: debounceSetSizes(),

    addLayout: () => {
      sendLayoutOp({ op: "addLayout" });
      set({ focusedPanelId: null });
      markPatch({ focusedPanelId: null });
    },
    renameLayout: (id, name) => sendLayoutOp({ op: "renameLayout", id, name }),
    deleteLayout: (id) => {
      sendLayoutOp({ op: "deleteLayout", id });
      if (get().activeLayoutId === id) {
        set({ focusedPanelId: null });
        markPatch({ focusedPanelId: null });
      }
    },
    activateLayout: (id) => {
      sendLayoutOp({ op: "activateLayout", id });
      set({ focusedPanelId: null });
      markPatch({ focusedPanelId: null });
    },
    moveLayout: (fromIndex, toIndex) => sendLayoutOp({ op: "moveLayout", fromIndex, toIndex }),

    addScene: () => {
      sendLayoutOp({ op: "addScene" });
      set({ focusedPanelId: null });
      markPatch({ focusedPanelId: null });
    },
    renameScene: (id, name) => sendLayoutOp({ op: "renameScene", id, name }),
    deleteScene: (id) => {
      sendLayoutOp({ op: "deleteScene", id });
      // 删除激活场景时 Rust 回退默认场景 → 聚焦面板失效，本地同步清空（非布局字段）
      if (get().activeSceneId === id) {
        set({ focusedPanelId: null });
        markPatch({ focusedPanelId: null });
      }
    },
    activateScene: (id) => {
      sendLayoutOp({ op: "activateScene", id });
      set({ focusedPanelId: null });
      markPatch({ focusedPanelId: null });
    },

    flush: async () => {
      // 布局字段由 Rust 侧已调度落盘；非布局字段补丁立即发送 + 强制 Rust 落盘
      await persistCtl.flush();
      await layoutFlush().catch((e) => console.error("布局状态落盘失败", e));
    },
  };
});

/** 写上次打开文件字段（kind → 字段映射统一出口）。null 只改本地（显式清除由 closeFile 定向
 * 标记补丁）——非显式关闭路径不得把 null 落盘，防撕裂窗口的陈旧空副本误清其他窗口记录。 */
function setLastFile(
  set: (partial: Partial<import("zustand").StoreApi<UiStateStore>["getState"]>) => void,
  kind: LastOpenFileKind,
  file: string | null,
): void {
  const patch: Partial<Pick<UiStateStore, "lastCanvasFile" | "lastNoteFile" | "lastTableFile">> = {};
  patch[LAST_FILE_KEYS[kind]] = file;
  set(patch);
  if (file !== null) markPatch({ [LAST_FILE_KEYS[kind]]: file });
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
      // 长度与当前激活树里该 Split 的 children 数不符 = 树已在拖拽期间被改动（分割/关面板/切布局）：
      // 该帧是过期观测，丢弃即可省一次注定被拒的 IPC。
      if (!sizesMatchSplit(id, sz)) return;
      sendLayoutOp({ op: "setLayoutSizes", splitId: id, sizes: sz });
    }, 250);
  };
}

/** 给定 Split id 在**当前激活布局**里的 children 数量（不是本布局/未命中返回 null）。 */
function splitChildCount(splitId: string): number | null {
  const s = useUiStateStore.getState();
  const layout = s.workspaceLayouts.find((l) => l.id === s.activeLayoutId) ?? s.workspaceLayouts[0];
  if (!layout) return null;
  const walk = (node: LayoutNode): number | null => {
    if (node.kind === "panel") return null;
    if (node.id === splitId) return node.children.length;
    for (const child of node.children) {
      const hit = walk(child);
      if (hit !== null) return hit;
    }
    return null;
  };
  return walk(layout.tree);
}

/** 尺寸帧形状是否与当前树一致（长度 + 有限非负 + 和 > 0；与 Rust 侧 `sizes_valid_for` 同口径）。 */
function sizesMatchSplit(splitId: string, sizes: number[]): boolean {
  const count = splitChildCount(splitId);
  if (count === null || sizes.length !== count || sizes.length === 0) return false;
  if (!sizes.every((v) => Number.isFinite(v) && v >= 0)) return false;
  return sizes.reduce((a, b) => a + b, 0) > 0;
}
