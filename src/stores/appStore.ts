import { create } from "zustand";
import {
  listCanvasesVault,
  createCanvasVault,
  deleteCanvasVault,
  renameCanvasVault,
  moveCanvasVault,
  readCanvasVault,
  writeCanvasVault,
  ensureDefaultVault,
  openVault,
  convertWhiteboardToAtlx,
  remapSideloads,
} from "@/services/vault";
import {
  readGlobalConfig,
  updateGlobalConfig,
  bumpRecentVault,
  removeRecentVault as dropVaultFromRecents,
} from "@/services/global";
import { useSettingsStore } from "@/stores/settingsStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useCollabStore } from "@/stores/collabStore";
import { migrateHistoryFile } from "@/services/history";
import { markSelfSave } from "@/utils/selfSave";
import {
  flushAllDomains,
  notifyVaultEntered,
  notifyVaultExit,
  notifyVaultLeaving,
  releaseView,
} from "@/utils/kernelLifecycle";
import { emitVaultEvent } from "@/utils/vaultEvents";
import { baseName, dedupeFilename, parentDir, remapDirPrefix, sanitizeFilename, siblingPath, stripExt } from "@/utils/filename";
import { getAppVersion as getVersionSvc } from "@/services/app";
import { openInExplorer as openInExplorerSvc, openUrl as openUrlSvc } from "@/services/shell";
import { readClipboardText as readClipboardTextSvc, writeClipboardText as writeClipboardTextSvc } from "@/services/clipboard";
import { pickDirectory as pickDirectorySvc } from "@/services/dialog";
import { applyStartupWindow as applyStartupWindowSvc, applyWorkspaceWindow as applyWorkspaceWindowSvc, closeWindow as closeWindowSvc, minimizeWindow as minimizeWindowSvc, onCloseRequested as onCloseRequestedSvc, toggleFullscreen as toggleFullscreenSvc, toggleMaximizeWindow as toggleMaximizeWindowSvc } from "@/services/window";
import { checkAndAutoUpdate as checkAndAutoUpdateSvc, checkForUpdate as checkForUpdateSvc, installUpdate as installUpdateSvc } from "@/services/updater";
import { emitPluginEvent } from "@/services/cordis/events";
import { usePluginStore } from "@/stores/pluginStore";
import type { CanvasFileRow, RecentVault } from "@/types";

/** 手动检查更新状态（设置页「关于」tab 用）。 */
type UpdateStatus =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "error";

/** 窗口关闭守卫已注册标志（installCloseGuard 幂等，防 React StrictMode 双挂载重复订阅）。 */
let closeGuardInstalled = false;

/** selectVault 并发序号：快速连续切换时仅最后一次调用有权清除切换读条（finally 守卫）。 */
let vaultSwitchSeq = 0;

/**
 * 应用级状态：路由 + 当前仓库 + 画布列表 CRUD。
 *
 * 两视图路由：
 * - vaultSelect：启动页 = 仓库选择，展示最近仓库，打开/新建仓库
 * - workspace：画布工作区（左文件面板 + 右画布；无当前画布时占位引导）
 *
 * 选仓库后直达 workspace（currentCanvasId=null 占位），画布管理收进左栏文件面板。
 * 首启无最近仓库时 ensureDefaultVault 建默认仓库并登记。
 */
type View = "vaultSelect" | "workspace";

interface AppState {
  view: View;
  /** 插件应用页面 id（非空 = 插件全页接管，渲染注册的插件页面替代工作区；app 页面/模式）。 */
  pluginPage: string | null;
  /** 当前仓库根路径（workspace 期间有效） */
  vaultRoot: string | null;
  /** 当前仓库名（显示用） */
  vaultName: string;
  /** 全屏加载进行中（boot 异步 + selectVault 全程；App 据此渲染加载屏，插件逐项上报据此门控）。 */
  entryLoading: boolean;
  /** 加载步骤清单（已完成 + 当前进行中的标签，顺序；LoadingScreen 渲染步骤提示）。 */
  loadSteps: string[];
  /** 开始一次加载会话（幂等：已激活不重置；boot 先 begin，selectVault 复用）。 */
  beginLoad: () => void;
  /** 上报一个加载步骤（追加到步骤清单，最后一项 = 当前进行中）。 */
  reportLoad: (label: string) => void;
  /** 结束加载会话（幂等；只收 active，步骤清单保留到下一次 beginLoad 重置）。 */
  endLoad: () => void;
  /** 当前仓库稳定 ID（`.atelyx/config.json` 的 vaultId；chatPanelStore 等据此识别仓库归属）。 */
  vaultId: string | null;
  /** 最近打开的仓库列表（按最近打开倒序） */
  recentVaults: RecentVault[];
  currentCanvasId: string | null;
  /** 当前画布磁盘路径（相对仓库根；打开/保存/重命名/删除按此路径）。 */
  currentCanvasFile: string | null;
  /** 当前打开的笔记文件（相对仓库根；与画布/表格并存，各一个）。 */
  currentNoteFile: string | null;
  /** 当前打开笔记的显示标题（去 .md 后缀；打开文件时携带）。 */
  currentNoteTitle: string;
  /** 当前打开的表格文件（相对仓库根；与画布/笔记并存，各一个）。 */
  currentTableFile: string | null;
  /** 当前打开表格的显示标题（去 .atb 后缀）。 */
  currentTableTitle: string;
  canvases: CanvasFileRow[];
  /** 自动检查更新（应用级，存 global.json；缺省 false = 关闭，关闭时完全不联网检查）。 */
  autoUpdate: boolean;
  /** 手动检查更新状态（设置页「关于」tab；运行期状态不持久化）。 */
  updateStatus: UpdateStatus;
  /** 检查到的新版本号（status = available 时有效）。 */
  updateLatestVersion: string;
  /** 检查/安装失败信息（status = error 时有效）。 */
  updateError: string;
  /** 新版本下载安装中（available 后点「下载并安装」）。 */
  installing: boolean;

  /** 应用挂载时调用一次：加载最近仓库，首启建默认仓库。返回本次应自动进入的仓库 root（null = 不自动进入）。 */
  init: () => Promise<string | null>;
  /** 设自动检查更新（应用级，写 global.json；不随仓库同步）。 */
  setAutoUpdate: (enabled: boolean) => Promise<void>;
  /** 手动检查新版本（设置页「关于」）；结果写入 updateStatus/updateLatestVersion/updateError。 */
  checkForUpdates: () => Promise<void>;
  /** 下载并安装已发现的新版本；成功后 relaunch 重启。 */
  installUpdate: () => Promise<void>;
  /** 打开仓库：openVault + 登记最近 + 进画布工作区（占位态）。成功返回 true。 */
  selectVault: (root: string) => Promise<boolean>;
  /** 从最近列表移除某仓库（不删文件）。 */
  removeRecentVault: (root: string) => Promise<void>;
  /** 返回仓库选择页（VaultSwitcher「管理仓库」入口）。 */
  backToVaultSelect: () => void;
  /** 打开插件应用页面（全页接管；仅工作区视图生效）。 */
  openPluginPage: (id: string) => void;
  /** 退出插件应用页面（回到工作区）。 */
  closePluginPage: () => void;
  /** 立即落盘全部 store 的 pending 改动（画布/表格/面板会话/UI 状态/配置；关窗与更新重启前调用）。
   *  不含协作连接收尾（本函数也会被启动自动更新检查调用，dispose 会误杀会话内协作连接）；
   *  协作 dispose 只在关窗守卫（真退出）调用，见 installCloseGuard。 */
  flushAllPending: () => Promise<void>;
  /** 注册窗口关闭守卫：关窗前先 flushAllPending 再销毁（幂等，App 挂载时调用一次）。 */
  installCloseGuard: () => void;
  /** 静默自动更新链路（启动时 autoUpdate 开启才调用）：先落盘再检查安装，失败静默降级。 */
  runAutoUpdate: () => Promise<void>;

  /** 调系统目录选择器，选中路径（用户取消返回 null）。 */
  pickVaultDirectory: () => Promise<string | null>;
  /** 在系统文件管理器中打开路径。 */
  openInExplorer: (path: string) => Promise<void>;
  /** 用系统默认程序打开外部 URL（webview 不导航）。 */
  openUrl: (url: string) => Promise<void>;
  /** 读系统剪贴板纯文本（无文本返回空串）。 */
  readClipboardText: () => Promise<string>;
  /** 写纯文本到系统剪贴板。 */
  writeClipboardText: (text: string) => Promise<void>;
  /** 应用版本号（启动页展示用）。 */
  getAppVersion: () => Promise<string>;
  /** 窗口控制（自定义标题栏按钮/全屏）：全部经 services 层转发。 */
  minimizeWindow: () => Promise<void>;
  toggleMaximizeWindow: () => Promise<void>;
  closeWindow: () => Promise<void>;
  toggleFullscreen: () => Promise<void>;
  /** 窗口形态切换（view effect 用）：启动页固定 960×640 不可调整 / 工作区恢复可调整。 */
  applyStartupWindow: () => Promise<void>;
  applyWorkspaceWindow: () => Promise<void>;
  /** 设置弹窗状态（null = 关闭；tab 为可选初始 tab，缺省 = 通用）。 */
  settingsModal: { tab?: string } | null;
  /** 打开设置弹窗（可指定初始 tab）。 */
  openSettings: (tab?: string) => void;
  /** 关闭设置弹窗。 */
  closeSettings: () => void;

  loadList: () => Promise<void>;
  /** 打开画布（树行携带 id + file）：设置全局文件状态并记录「上次打开」（uiState）。 */
  openCanvas: (row: CanvasFileRow) => void;
  /** 关闭当前画布（标签保留，回到未打开文件状态）：清全局文件状态与「上次打开」，先落盘再清内存态。 */
  closeCanvas: () => void;
  /** 打开笔记（文件面板/搜索/属性定位等入口）：设置全局文件状态并记录「上次打开」。 */
  openNote: (file: string, title: string) => void;
  /** 关闭笔记窗口：清当前笔记文件状态。 */
  closeNote: () => void;
  /** 打开表格：设置全局文件状态并记录「上次打开」；已打开的表格不重复加载。 */
  openTable: (file: string, title: string) => void;
  /** 关闭表格窗口：先落盘再清内存态（防 debounce 窗口内丢改动）。 */
  closeTable: () => void;
  /**
   * 表格文件已被删除/外部移动（从列表消失）时的静默关闭：**不 flush**
   * （防写回重建已删文件），只清内存态与保存定时器。
   */
  closeTableSilent: () => void;
  /** 新建画布到 dir（相对仓库根，空 = 根目录），返回 { id, file, title }（title 可能被同名去重）。 */
  createCanvas: (title?: string, dir?: string) => Promise<{ id: string | null; file: string | null; title: string }>;
  /** 重命名画布（同目录改文件名，按当前 file），返回实际标题。 */
  renameCanvas: (row: CanvasFileRow, title: string) => Promise<string>;
  /** 移动画布文件到目标文件夹（保持文件名，目标同名自动加序号；同目录 no-op），返回实际 file。 */
  moveCanvas: (row: CanvasFileRow, targetDir: string) => Promise<string>;
  /**
   * 复制画布为同目录副本（基于磁盘当前内容；title 同名自动加序号 + id 重新生成，
   * 副本保持「标题即文件名」规范），返回实际标题（调用方据此提示）。
   * 副本不自动打开。
   */
  duplicateCanvas: (row: CanvasFileRow) => Promise<string>;
  /** 删除画布（按 file）。 */
  deleteCanvas: (row: CanvasFileRow) => Promise<void>;
  /**
   * 删除文件夹联动：目录内画布全部消失——当前打开的画布在目录内则复位画布运行时状态
   * （防残留 saveTimer 重写已删文件，同 deleteCanvas）并清空画布槽/标签。供 vaultStore.deleteFolder 调用。
   * 返回目录内是否有画布（调用方据此决定是否重扫画布列表）。
   */
  closeCanvasIfInDir: (dir: string) => boolean;
  /** 文件夹重命名联动：当前打开画布位于 `oldDir/` 下时同步 currentCanvasFile。供 vaultStore.renameFolder 调用。 */
  renameCurrentCanvasFile: (oldDir: string, newDir: string) => void;
  /**
   * 把外部白板文件（.canvas）转换为同目录 .atlx 画布（原文件保留，单向转换）。
   * 成功后刷新画布列表与文件树，返回画布行（页面层打开）；失败返回 null。
   */
  convertWhiteboard: (file: string) => Promise<CanvasFileRow | null>;
}

/** 画布 CRUD 后统一刷新两个数据源：canvases 列表（appStore）+ 文件树（vaultStore.tree 含 .atlx 行）。
 * 漏刷会导致文件面板不显示新画布，直到重进仓库——所有画布写操作后必须走这里。 */
async function refreshCanvasAndTree(): Promise<void> {
  await useAppStore.getState().loadList();
  await useVaultStore.getState().loadFiles();
}

/** 同目录现有画布行（画布 CRUD 防重名 siblings 计算，五处共用）：parentDir 命中 dir；
 * excludeFile = 排除自身（移动场景为移动前的旧路径）。map 出 title 还是 baseName 由调用方定——
 * title（无扩展名）与 baseName（含扩展名）属两个去重空间，不在此混同。 */
function canvasesInDir(dir: string, excludeFile?: string): CanvasFileRow[] {
  return useAppStore
    .getState()
    .canvases.filter(
      (c) => parentDir(c.file) === dir && (excludeFile === undefined || c.file !== excludeFile),
    );
}

export const useAppStore = create<AppState>((set, get) => ({
  view: "vaultSelect",
  pluginPage: null,
  vaultRoot: null,
  vaultName: "",
  entryLoading: false,
  loadSteps: [],
  vaultId: null,
  recentVaults: [],
  currentCanvasId: null,
  currentCanvasFile: null,
  currentNoteFile: null,
  currentNoteTitle: "",
  currentTableFile: null,
  currentTableTitle: "",
  canvases: [],
  autoUpdate: false,
  updateStatus: "idle",
  updateLatestVersion: "",
  updateError: "",
  installing: false,

  // 加载会话三方法：进仓/启动期间由 App boot 与 selectVault 调用，
  // LoadingScreen 订阅 loadSteps 渲染步骤清单（已完成打勾、最后一项 = 当前进行中）。
  // endLoad 只收 active 不清 steps：boot 内 selectVault 先收尾时步骤仍保留展示，
  // 下一次 beginLoad 才重置清单。
  beginLoad: () =>
    set((s) => (s.entryLoading ? s : { entryLoading: true, loadSteps: [] })),
  reportLoad: (label) => set((s) => ({ loadSteps: [...s.loadSteps, label] })),
  endLoad: () => set({ entryLoading: false }),

  init: async (): Promise<string | null> => {
    get().reportLoad("读取全局配置");
    let recents: RecentVault[] = [];
    let autoEnterRoot: string | null = null;
    let autoUpdate = false;
    try {
      const cfg = await readGlobalConfig();
      recents = cfg.recentVaults;
      autoUpdate = cfg.autoUpdate ?? false;
    } catch (e) {
      console.error("读取全局配置失败", e);
    }
    if (recents.length === 0) {
      // 首启无最近仓库 → ensureDefaultVault 建默认仓库并登记；
      // 本次不自动进入，展示启动页让用户选择/新建仓库
      try {
        const info = await ensureDefaultVault();
        const now = Math.floor(Date.now() / 1000);
        recents = bumpRecentVault(recents, info, now);
        await updateGlobalConfig({ recentVaults: recents });
      } catch (e) {
        console.error("初始化默认仓库失败", e);
      }
    } else {
      // 非首启：recentVaults[0] = 最近打开（selectVault 时置顶）= 上次所在仓库，
      // 启动时跳过启动页直接进入；仓库路径失效时 selectVault 失败自动回退启动页
      autoEnterRoot = recents[0].root;
    }
    set({
      recentVaults: recents,
      view: "vaultSelect",
      autoUpdate: autoUpdate,
    });
    // 应用级 UI 使用状态（布局/展开/上次文件）启动加载一次，之后跨仓库共享。
    // 等待完成：进仓门控「全部加载完再进入」涵盖布局状态，恢复上次打开文件依赖 loaded。
    get().reportLoad("加载布局与使用状态");
    await useUiStateStore.getState().load();
    return autoEnterRoot;
  },

  setAutoUpdate: async (enabled) => {
    set({ autoUpdate: enabled });
    try {
      await updateGlobalConfig({ autoUpdate: enabled });
    } catch (e) {
      console.error("保存自动更新配置失败", e);
    }
  },

  checkForUpdates: async () => {
    set({ updateStatus: "checking", updateError: "" });
    try {
      const result = await checkForUpdateSvc();
      set(
        result
          ? {
              updateStatus: "available",
              updateLatestVersion: result.latestVersion,
            }
          : { updateStatus: "upToDate", updateLatestVersion: "" },
      );
    } catch (e) {
      console.error("检查更新失败", e);
      set({
        updateStatus: "error",
        updateError: e instanceof Error ? e.message : String(e),
      });
    }
  },

  installUpdate: async () => {
    set({ installing: true, updateError: "" });
    try {
      // 更新安装后 relaunch 重启：先落盘全部 pending 改动，防 debounce 保存随 webview 销毁丢失
      // （协作连接无需在此 dispose：安装成功后进程退出，relay 按 TCP 断开即移除 peer）
      await useAppStore.getState().flushAllPending();
      await installUpdateSvc();
    } catch (e) {
      console.error("安装更新失败", e);
      set({
        installing: false,
        updateStatus: "error",
        updateError: e instanceof Error ? e.message : String(e),
      });
    }
  },

  selectVault: async (root) => {
    // 快速连续切换时仅最后一次调用有权 endLoad（finally 守卫）
    const seq = ++vaultSwitchSeq;
    // 全屏加载会话：进入仓库期间逐步上报加载项；boot 已 begin 时幂等不重置
    get().beginLoad();
    get().reportLoad("打开仓库");
    try {
      // 切换前先落盘旧仓库的全部领域编辑并**等待写盘完成**：openVault 会把 VaultState.root 切到新仓库，
      // 若 fire-and-forget 直接放行，写盘可能晚于 open_vault 执行、把旧仓库内容写进新仓库（跨仓库污染）。
      // 领域 store 无改动则不写（脏门控，见各自 flush）；chatPanel 额外传当前仓库 vaultId 做归属校验。
      // 经领域生命周期注册表分发（canvas/table/aichat/calendar/note 各钩子按注册序执行，失败快速传播）
      await flushAllDomains({ vaultId: get().vaultId });
      const info = await openVault(root);
      const now = Math.floor(Date.now() / 1000);
      const recents = bumpRecentVault(get().recentVaults, info, now);
      // set + 清空须在下一个 await 之前同步完成，双保险防跨仓库写入：
      // 1) NoteEditor debounce timer 是 macrotask，openVault→set 间无 await 则无隙可乘；
      // 2) 清空 noteList 先于 React 提交卸载（见下方注释），cleanup 的 stillExists 守卫必跳过。
      // 注意 view 不在此切：进仓门控「全部加载完（含插件）再进入仓库」，工作区视图在末尾统一切换
      set({
        vaultRoot: info.root,
        vaultName: info.name,
        vaultId: info.id,
        recentVaults: recents,
        canvases: [],
        currentCanvasId: null,
        currentCanvasFile: null,
        currentNoteFile: null,
        currentNoteTitle: "",
        currentTableFile: null,
        currentTableTitle: "",
      });
      // 立即清空旧仓库文件树 + 笔记内容缓存/挂起输入 + 撤销栈（**必须在任何 await 之前**）：
      // NoteEditor 随 currentNoteFile 置空而卸载，其 cleanup 按「noteList 是否仍含该文件」决定是否
      // flush——若此处落后于下一个 await（React 提交卸载），noteList 还是旧仓库列表，cleanup 会把
      // 旧仓库内容经已切换的 root 写进新仓库同路径文件（跨仓库污染）。挂起输入已在 openVault 前
      // flush 落盘旧仓库，此处清残留（含 flush 后、切仓库前新输入），不丢数据。
      useVaultStore.setState({ tree: [], noteList: [], tableList: [], noteContents: {}, pendingNoteContent: {} });
      // 切仓库同步清态（笔记撤销栈/画布运行时，经注册表分发）——同步执行，保住防跨仓库写入守卫：
      // 清空须在下一个 await 之前完成（与上方 set 同批，React 提交卸载前 noteList 已清空）
      notifyVaultLeaving();
      // 登记最近仓库失败不阻塞切换：global.json 写入异常（权限/磁盘）只影响最近列表，
      // 若放行抛错会被下方 catch 吞掉，导致后续重载（配置/画布列表/文件树/AI 会话）全部跳过
      try {
        await updateGlobalConfig({ recentVaults: recents });
      } catch (e) {
        console.error("登记最近仓库失败", e);
      }
      // 加载仓库级配置覆盖（.atelyx/config.json），需在发消息前完成
      get().reportLoad("加载仓库配置");
      await useSettingsStore.getState().loadVaultConfig();
      // 文件树/画布列表/AI 会话随切换等待完成：门控「全部加载完再进入」，加载屏覆盖到数据就绪。
      // 各自独立 try——任一加载失败不连带跳过其余（尤其 AI 会话加载不能被文件树失败跳过，
      // 否则历史/当前会话停留在旧仓库）
      get().reportLoad("加载文件树与画布列表");
      try {
        await refreshCanvasAndTree();
      } catch (e) {
        console.error("加载文件树/画布列表失败", e);
      }
      get().reportLoad("加载 AI 会话");
      try {
        // 领域仓库上下文（AI 会话读盘等）经注册表分发；aichat 钩子 force：真实仓库切换，
        // 强制重读盘（防 sessionVaultId 巧合等于目标时被幂等守卫跳过，面板停留在旧仓库会话）
        await notifyVaultEntered({ vaultId: info.id });
      } catch (e) {
        console.error("加载领域仓库上下文失败", e);
      }
      // 插件平台：切仓库后全量重载（load 内部先卸载旧贡献，再按新仓库上下文重建 app+vault 插件）；
      // 加载完成后再切工作区视图 + 广播 vault:switch，保证订阅方是已就绪的后台插件。
      get().reportLoad("加载插件");
      try {
        await usePluginStore.getState().load();
      } catch (e) {
        console.error("加载插件失败", e);
      }
      // 全部加载完成（含插件）才进入仓库：切换工作区视图，窗口形态随 view 统一应用
      set({ view: "workspace" });
      emitPluginEvent("vault:switch", { root: info.root, id: info.id });
      return true;
    } catch (e) {
      console.error("打开仓库失败", e);
      return false;
    } finally {
      if (seq === vaultSwitchSeq) get().endLoad();
    }
  },

  removeRecentVault: async (root) => {
    const recents = dropVaultFromRecents(get().recentVaults, root);
    try {
      await updateGlobalConfig({ recentVaults: recents });
    } catch (e) {
      console.error("更新最近仓库列表失败", e);
    }
    set({ recentVaults: recents });
  },

  backToVaultSelect: () => {
    // 回启动页：AI 会话/日历日程/笔记挂起输入/表格改动落盘防 debounce 丢改动（root 未切换，写旧仓库
    // 安全；冲突未决/已删文件条目由 flushPendingNotes 内部保留）。经注册表分发（fire-and-forget）。
    void notifyVaultExit().catch((e) => console.error("退出仓库领域清理失败", e));
    useSettingsStore.getState().clearVaultConfig();
    // 清设置弹窗（防止下次进入工作区残留重开）
    set({ settingsModal: null });
    set({
      view: "vaultSelect",
      vaultRoot: null,
      vaultName: "",
      vaultId: null,
      canvases: [],
      currentCanvasId: null,
      currentCanvasFile: null,
      currentNoteFile: null,
      currentNoteTitle: "",
      currentTableFile: null,
      currentTableTitle: "",
      pluginPage: null,
    });
    // 插件事件先发（存活插件先收到「清上下文」通知），再卸载重载——
    // 否则 load 开头会同步卸载全部 runtime，vault:clear 发出时已无订阅者。
    emitPluginEvent("vault:clear", {});
    // 插件平台：回启动页后卸载 vault 级插件、保留 app 级插件（load 全量重载，root 已空则只扫 app 目录）。
    void usePluginStore.getState().load().catch(() => {});
  },

  openPluginPage: (id) => set({ pluginPage: id }),

  closePluginPage: () => set({ pluginPage: null }),

  flushAllPending: async () => {
    // 领域 store 全部 pending 改动（画布/表格/AI 会话/日历/笔记挂起输入）经生命周期注册表分发；
    // 笔记编辑器挂起的 debounce 输入（组件内 timer，不走 store）也在笔记钩子内统一落盘 +
    // 补历史存档点，防关窗/切仓库/AI 重命名移动删除前丢最后 500ms 输入
    await flushAllDomains({ vaultId: get().vaultId });
    await useUiStateStore.getState().flush();
    await useSettingsStore.getState().flush();
    // 协作连接收尾不在此处（本函数启动自动更新检查时也会调用）：dispose 会断开会话内协作连接
    // 且清空 runtimeCfg，之后 applyConfig 全部失效、状态永久未连接。dispose 只由关窗守卫
    // （真退出）显式调用发 bye；更新 relaunch 场景进程退出即断，relay 按 TCP 断开立即移除 peer
  },

  installCloseGuard: () => {
    if (closeGuardInstalled) return;
    closeGuardInstalled = true;
    void onCloseRequestedSvc(async () => {
      await useAppStore.getState().flushAllPending();
      // 关窗 = 真退出：发 bye 离开协作房间并停止重连，防 relay 侧 30s 心跳残留幽灵在线
      useCollabStore.getState().dispose();
    });
  },

  runAutoUpdate: async () => {
    try {
      // 更新重启前先落盘：relaunch 会销毁 webview，pending 的 debounce 保存随之中断；
      // dev 跳过守卫在 service（checkAndAutoUpdateSvc）内；协作连接收尾不在此处见 flushAllPending
      await useAppStore.getState().flushAllPending();
      await checkAndAutoUpdateSvc();
    } catch (e) {
      console.error("自动更新失败（静默降级，下次启动再试）", e);
    }
  },

  pickVaultDirectory: () => pickDirectorySvc(),
  openInExplorer: (path) => openInExplorerSvc(path),
  openUrl: (url) => openUrlSvc(url),
  readClipboardText: () => readClipboardTextSvc(),
  writeClipboardText: (text) => writeClipboardTextSvc(text),
  getAppVersion: () => getVersionSvc(),
  minimizeWindow: () => minimizeWindowSvc(),
  toggleMaximizeWindow: () => toggleMaximizeWindowSvc(),
  closeWindow: () => closeWindowSvc(),
  toggleFullscreen: () => toggleFullscreenSvc(),
  applyStartupWindow: () => applyStartupWindowSvc(),
  applyWorkspaceWindow: () => applyWorkspaceWindowSvc(),

  settingsModal: null,
  openSettings: (tab) => set({ settingsModal: tab ? { tab } : {} }),
  closeSettings: () => set({ settingsModal: null }),

  loadList: async () => {
    const vaultId = get().vaultId;
    try {
      const canvases = await listCanvasesVault();
      // 切仓库竞态守卫：等待期间用户可能已切到新仓库（后台填充链与 VaultSwitcher 快速切换并发），
      // 旧仓库的扫描结果不得覆盖新仓库的列表
      if (get().vaultId !== vaultId) return;
      set({ canvases });
    } catch (e) {
      console.error("加载画布列表失败", e);
    }
  },
  openCanvas: (row) => {
    set({ currentCanvasId: row.id, currentCanvasFile: row.file });
    // 记录「上次打开」供下次进入仓库恢复（画布窗口已无标签概念，打开即唯一文件状态）
    useUiStateStore.getState().recordOpenFile("canvas", row.file);
    // 记录最近打开（主页面板「最近打开」数据源）
    const vaultId = get().vaultId;
    if (vaultId) useUiStateStore.getState().recordRecentFile(row.file, "canvas", vaultId);
  },
  closeCanvas: () => {
    set({ currentCanvasId: null, currentCanvasFile: null });
    useUiStateStore.getState().closeFile("canvas");
    // 先落盘（防 debounce 窗口内丢改动）再清内存态（经注册表分发，与撕裂视图交接同语义）；清空后不可写回
    void releaseView("canvas").catch((e) => console.error("关闭画布落盘失败", e));
  },
  openNote: (file, title) => {
    set({ currentNoteFile: file, currentNoteTitle: title });
    useUiStateStore.getState().recordOpenFile("note", file);
    const vaultId = get().vaultId;
    if (vaultId) useUiStateStore.getState().recordRecentFile(file, "note", vaultId);
  },
  closeNote: () => {
    set({ currentNoteFile: null, currentNoteTitle: "" });
    useUiStateStore.getState().closeFile("note");
  },
  openTable: (file, title) => {
    set({ currentTableFile: file, currentTableTitle: title });
    useUiStateStore.getState().recordOpenFile("table", file);
    const vaultId = get().vaultId;
    if (vaultId) useUiStateStore.getState().recordRecentFile(file, "table", vaultId);
    // 内容加载由 TableView 自载（同 CanvasView：openCanvas 不加载，视图挂载时按文件读盘）——
    // 撕裂窗口只镜像文件路径，须由视图统一承担加载；此处不 load 防主窗口切表时重复读盘
  },
  closeTable: () => {
    set({ currentTableFile: null, currentTableTitle: "" });
    useUiStateStore.getState().closeFile("table");
    // 先落盘（防 debounce 窗口内丢改动）再清内存态（经注册表分发）；清空后不可写回
    void releaseView("table").catch((e) => console.error("关闭表格落盘失败", e));
  },
  closeTableSilent: () => {
    const file = get().currentTableFile;
    // 文件已删：flush 会写回重建，只清内存态与保存定时器（经仓库事件分发，须在置空当前文件前发出）
    if (file) emitVaultEvent({ kind: "table:deleted", path: file });
    set({ currentTableFile: null, currentTableTitle: "" });
    useUiStateStore.getState().closeFile("table");
  },
  createCanvas: async (title = "未命名画布", dir = "") => {
    // 同名自动加序号（标题即文件名，保证同目录不重名），返回实际标题供 UI 提醒
    const siblings = canvasesInDir(dir).map((c) => c.title);
    const actual = dedupeFilename(title, siblings);
    try {
      const { id, file } = await createCanvasVault(actual, dir);
      markSelfSave(file);
      await refreshCanvasAndTree();
      return { id, file, title: actual };
    } catch (e) {
      console.error("新建画布失败", e);
      return { id: null, file: null, title: actual };
    }
  },
  renameCanvas: async (row, title) => {
    // 同名自动加序号（排除自身，同目录），返回实际标题供 UI 提醒
    const siblings = canvasesInDir(parentDir(row.file))
      .map((c) => c.title)
      .filter((t) => t !== row.title);
    const actual = dedupeFilename(title, siblings);
    try {
      await renameCanvasVault(row.file, actual);
      // 重命名后文件名变了：先算新路径再标记自写（旧路径删除 + 新路径创建事件一并抑制），
      // 当前画布磁盘 .atlx 已被 Rust 改（title + 同目录改文件名），同步乐观锁基准防下次保存误冲突
      const newFile = siblingPath(row.file, `${sanitizeFilename(actual)}.atlx`);
      markSelfSave([row.file, newFile]);
      // 侧文件先确保在新编码名下，再随重命名迁移（同笔记/表格路径）
      await migrateHistoryFile("canvas", row.file).catch(() => {});
      // 历史侧文件随迁（画布 kind 目录）；失败静默降级，不阻塞重命名主流程
      await remapSideloads(row.file, newFile).catch(() => {});
      // 画布运行时引用同步（乐观锁基准 + 打开路径）经仓库事件分发
      emitVaultEvent({ kind: "canvas:renamed", oldPath: row.file, newPath: newFile });
      useUiStateStore.getState().renameLastFile("canvas", row.file, newFile);
      await refreshCanvasAndTree();
    } catch (e) {
      console.error("重命名失败", e);
    }
    return actual;
  },
  moveCanvas: async (row, targetDir) => {
    // 保持文件名，目标文件夹同名自动加序号（排除自身 = 同目录移动 no-op）
    const name = baseName(row.file);
    const siblings = canvasesInDir(targetDir, row.file).map((c) => baseName(c.file));
    const safe = dedupeFilename(name, siblings);
    const newFile = targetDir ? `${targetDir}/${safe}` : safe;
    if (newFile === row.file) return row.file;
    try {
      await moveCanvasVault(row.file, newFile);
      markSelfSave([row.file, newFile]);
      // 侧文件先确保在新编码名下，再随移动迁移（同 renameCanvas）
      await migrateHistoryFile("canvas", row.file).catch(() => {});
      // 历史侧文件随迁（画布 kind 目录）；失败静默降级，不阻塞移动主流程
      await remapSideloads(row.file, newFile).catch(() => {});
      // 画布运行时引用同步（乐观锁基准 + 打开路径）经仓库事件分发
      emitVaultEvent({ kind: "canvas:moved", oldPath: row.file, newPath: newFile });
      useUiStateStore.getState().renameLastFile("canvas", row.file, newFile);
      await refreshCanvasAndTree();
      return newFile;
    } catch (e) {
      // 不吞错误：调用方（FileExplorerPanel.handleMoveFile）据此提示「移动文件失败」
      console.error("移动画布失败", e);
      throw e;
    }
  },
  duplicateCanvas: async (row) => {
    // 同名自动加序号（同目录），返回实际标题供 UI 提醒
    const siblings = canvasesInDir(parentDir(row.file)).map((c) => c.title);
    const actual = dedupeFilename(row.title, siblings);
    try {
      // 读磁盘原文 → 重写 id/title → 写新文件（write 的落盘路径由 title 决定，与 siblingPath 一致）
      const canvas = await readCanvasVault(row.file);
      canvas.id = crypto.randomUUID();
      canvas.title = actual;
      const target = siblingPath(row.file, `${sanitizeFilename(actual)}.atlx`);
      await writeCanvasVault(canvas, target);
      markSelfSave(target);
      await refreshCanvasAndTree();
      return actual;
    } catch (e) {
      console.error("复制画布失败", e);
      throw e;
    }
  },
  deleteCanvas: async (row) => {
    try {
      await deleteCanvasVault(row.file);
      markSelfSave(row.file);
      const { currentCanvasId, currentCanvasFile } = get();
      // 删除的是当前画布（id 或路径命中——AI 工具等调用方可能只有 file 无真实 id）：清空 canvasStore
      // （含未落盘 saveTimer / 进行中的流），否则残留 timer 会重写已删文件、watcher 事件匹配旧 id 产生误导 reload
      const isCurrent = row.id === currentCanvasId || row.file === currentCanvasFile;
      // 当前画布复位运行时（防残留 saveTimer 重写已删文件）：经仓库事件分发（handler 按当前文件匹配，
      // 须在下方置空 currentCanvasFile 前发出）
      emitVaultEvent({ kind: "canvas:deleted", path: row.file });
      // 删除的是「上次打开」的画布：清空 uiState 记录（否则下次进入仓库尝试恢复已删文件）
      if (useUiStateStore.getState().lastCanvasFile === row.file) {
        useUiStateStore.getState().closeFile("canvas");
      }
      set({
        currentCanvasId: isCurrent ? null : currentCanvasId,
        currentCanvasFile: isCurrent ? null : currentCanvasFile,
      });
      await refreshCanvasAndTree();
    } catch (e) {
      console.error("删除失败", e);
    }
  },
  closeCanvasIfInDir: (dir) => {
    const { canvases, currentCanvasId } = get();
    const affectedIds = canvases
      .filter((c) => c.file.startsWith(`${dir}/`))
      .map((c) => c.id);
    if (affectedIds.length > 0 && currentCanvasId && affectedIds.includes(currentCanvasId)) {
      // 当前画布位于被删目录内：复位运行时（经仓库事件分发，须在置空当前文件前发出）
      const file = get().currentCanvasFile;
      if (file) emitVaultEvent({ kind: "canvas:deleted", path: file });
      useUiStateStore.getState().closeFile("canvas");
      set({ currentCanvasId: null, currentCanvasFile: null });
    }
    return affectedIds.length > 0;
  },
  renameCurrentCanvasFile: (oldDir, newDir) => {
    const file = get().currentCanvasFile;
    if (!file || !file.startsWith(`${oldDir}/`)) return;
    set({ currentCanvasFile: remapDirPrefix(file, oldDir, newDir) });
  },
  /**
   * 把外部白板文件（.canvas）转换为同目录 .atlx 画布（原文件保留，单向转换），
   * 成功后刷新画布列表与文件树并直接打开新画布。失败返回 null（画布错误条提示）。
   */
  convertWhiteboard: async (file) => {
    const title = stripExt(baseName(file));
    // 同名自动加序号（同目录现有 .atlx 标题）
    const siblings = canvasesInDir(parentDir(file)).map((c) => c.title);
    try {
      const row = await convertWhiteboardToAtlx(file, title, siblings);
      markSelfSave(row.file);
      // 转换生成了新 .atlx：刷新两个数据源后打开新画布
      await refreshCanvasAndTree();
      get().openCanvas(row);
      return row;
    } catch (e) {
      console.error("转换为画布失败", e);
      emitVaultEvent({ kind: "canvas:error", message: "转换为画布失败，请重试" });
      return null;
    }
  },
}));
