import { create } from "zustand";
import {
  listCanvasesVault,
  createCanvasVault,
  deleteCanvasVault,
  renameCanvasVault,
  moveCanvasVault,
  readCanvasVault,
  writeCanvasVault,
  openVault,
  convertWhiteboardToAtlx,
  remapSideloads,
} from "@/services/vault";
import { activateContentVault, createSpaceBackendRegistration, identityKeyOf } from "@/services/content/factory";
import { createSpaceContentBackend } from "@/services/content/spaceContent";
import type { VaultIdentity } from "@/services/content/contract";
import {
  readGlobalConfig,
  updateGlobalConfig,
  bumpRecentVault,
  bumpRecentSpace,
  removeRecentVault as dropVaultFromRecents,
} from "@/services/global";
import { useSettingsStore } from "@/stores/settingsStore";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useCollabStore } from "@/stores/collabStore";
import { migrateHistoryFile } from "@/services/history";
import { markSelfSave } from "@/utils/selfSave";
import {
  flushAllDomains,
  notifyVaultEntered,
  notifyVaultLeaving,
  releaseView,
} from "@/utils/kernelLifecycle";
import { emitVaultEvent } from "@/utils/vaultEvents";
import { baseName, dedupeFilename, parentDir, remapDirPrefix, sanitizeFilename, siblingPath, stripExt } from "@/utils/filename";
import { getAppVersion as getVersionSvc } from "@/services/app";
import { openInExplorer as openInExplorerSvc, openUrl as openUrlSvc } from "@/services/shell";
import { readClipboardText as readClipboardTextSvc, writeClipboardText as writeClipboardTextSvc } from "@/services/clipboard";
import { pickDirectory as pickDirectorySvc } from "@/services/dialog";
import { applyWorkspaceWindow as applyWorkspaceWindowSvc, closeWindow as closeWindowSvc, minimizeWindow as minimizeWindowSvc, onCloseRequested as onCloseRequestedSvc, toggleFullscreen as toggleFullscreenSvc, toggleMaximizeWindow as toggleMaximizeWindowSvc } from "@/services/window";
import { checkAndAutoUpdate as checkAndAutoUpdateSvc, checkForUpdate as checkForUpdateSvc, installUpdate as installUpdateSvc } from "@/services/updater";
import { emitPluginEvent } from "@/services/cordis/events";
import { usePluginStore } from "@/stores/pluginStore";
import { useNotificationStore } from "@/stores/notificationStore";
import type { CanvasFileRow, RecentSpace, RecentVault, VaultSettingsTarget } from "@/types";

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

/** 全局配置损坏（原文已备份）的用户可见提示：读到空配置会连带重置最近仓库与外观，
 *  只写日志等于用户看到「东西全没了」却不知原因。备份文件在应用数据目录（与 global.json 同目录）。 */
function notifyGlobalConfigCorrupt(backup: string | null): void {
  if (!backup) return;
  useNotificationStore.getState().notify({
    level: "error",
    message: `全局配置已损坏，原文备份为 ${backup}（应用数据目录）：最近仓库与外观设置已重置`,
  });
}

/** selectSpace 结果：ok = 已进入；need-login = 该服务器无有效会话（停留未激活，调用方引导登录）；
 *  error = 服务端不可达/非成员（已通知）或切换重入被拒。 */
export type SpaceEnterResult = "ok" | "need-login" | "error";

/** init 自动进入目标：本地仓库按 root 进入，协作空间按条目进入（boot 据此分派 selectVault/selectSpace）。 */
export type AutoEnterTarget =
  | { kind: "local"; root: string }
  | { kind: "space"; entry: RecentSpace };

/** 重命名/移动后的历史侧文件迁移失败不阻塞主流程，但会让版本记录孤儿化——错误须用户可见，不能静默吞掉。 */
function notifySidecarFailure(what: string, error: unknown): void {
  console.error(`${what}失败`, error);
  useNotificationStore.getState().notify({ level: "warning", message: `${what}失败，历史记录可能不完整` });
}

/**
 * 应用级状态：当前仓库 + 画布列表 CRUD。
 *
 * 启动即工作区：仓库的入口与管理在文件面板仓库树（仓库为顶级条目），
 * 无激活仓库时树区空态引导创建；当前仓库由 selectVault 单点进入（flush → 清理 → 换 root）。
 */
interface AppState {
  /** 插件应用页面 id（非空 = 插件全页接管，渲染注册的插件页面替代工作区；app 页面/模式）。 */
  pluginPage: string | null;
  /** 当前仓库根路径（workspace 期间有效；协作空间仓库无本地 root，恒为 null） */
  vaultRoot: string | null;
  /** 当前激活仓库身份（与 vaultRoot 并列：local 时 root 一致；space 时 root 为 null）。
   *  撕裂窗口经 open-file-changed 广播镜像此身份自建内容后端。 */
  vaultIdentity: VaultIdentity | null;
  /** 当前仓库名（显示用） */
  vaultName: string;
  /** 全屏加载进行中（boot 异步 + selectVault 全程；App 据此渲染加载屏，插件逐项上报据此门控）。 */
  entryLoading: boolean;
  /** 面板内切换进行中的目标仓库 root（null = 空闲）。仓库树据此在目标行显示加载动画并禁点；
   *  兼作 selectVault 的重入守卫（切换不再有整屏加载屏兜底，防并发重入）。 */
  switchingVaultRoot: string | null;
  /** 加载步骤清单（已完成 + 当前进行中的标签，顺序；LoadingScreen 渲染步骤提示）。 */
  loadSteps: string[];
  /** 开始一次加载会话（幂等：已激活不重置；boot 先 begin，selectVault 复用）。 */
  beginLoad: () => void;
  /** 上报一个加载步骤（追加到步骤清单，最后一项 = 当前进行中）。 */
  reportLoad: (label: string) => void;
  /** 结束加载会话（幂等；只收 active，步骤清单保留到下一次 beginLoad 重置）。 */
  endLoad: () => void;
  /** 最近打开的仓库列表（按最近打开倒序） */
  recentVaults: RecentVault[];
  /** 最近打开的协作空间仓库列表（按最近打开倒序；global.json `spaces` 字段镜像） */
  recentSpaces: RecentSpace[];
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

  /** 应用挂载时调用一次：读取最近仓库列表。返回本次应自动进入的目标（null = 无仓库，停留空态）。 */
  init: () => Promise<AutoEnterTarget | null>;
  /** 设自动检查更新（应用级，写 global.json；不随仓库同步）。 */
  setAutoUpdate: (enabled: boolean) => Promise<void>;
  /** 手动检查新版本（设置页「关于」）；结果写入 updateStatus/updateLatestVersion/updateError。 */
  checkForUpdates: () => Promise<void>;
  /** 下载并安装已发现的新版本；成功后 relaunch 重启。 */
  installUpdate: () => Promise<void>;
  /** 打开仓库：openVault + 登记最近 + 进画布工作区（占位态）。成功返回 true。 */
  selectVault: (root: string) => Promise<boolean>;
  /**
   * 进入协作空间仓库（无本地 root，不调 openVault）：flush 旧仓库 → 校验会话 → 拉树预检 →
   * 激活空间内容后端 → 清旧仓库状态 → 加载（文件树；画布列表空间不支持跳过）。
   * need-login = 无有效会话（未激活，UI 引导登录）；error = 服务端不可达/非成员（已通知，停留未激活）。
   */
  selectSpace: (entry: { serverUrl: string; spaceId: string; name: string }) => Promise<SpaceEnterResult>;
  /** 从最近列表移除某仓库（不删文件；移除当前激活仓库不影响激活态）。 */
  removeRecentVault: (root: string) => Promise<void>;
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
  /** 在系统文件管理器中打开路径（失败弹通知，不向上抛）。 */
  openInExplorer: (path: string) => Promise<void>;
  /** 用系统默认程序打开外部 URL（webview 不导航；失败弹通知，不向上抛）。 */
  openUrl: (url: string) => Promise<void>;
  /** 读系统剪贴板纯文本（无文本返回空串）。 */
  readClipboardText: () => Promise<string>;
  /** 写纯文本到系统剪贴板。 */
  writeClipboardText: (text: string) => Promise<void>;
  /** 应用版本号（关于页展示用）。 */
  getAppVersion: () => Promise<string>;
  /** 窗口控制（自定义标题栏按钮/全屏）：全部经 services 层转发。 */
  minimizeWindow: () => Promise<void>;
  toggleMaximizeWindow: () => Promise<void>;
  closeWindow: () => Promise<void>;
  toggleFullscreen: () => Promise<void>;
  /** 窗口形态应用（boot 末尾统一调用一次）：可调整 + 最小尺寸 = 默认。 */
  applyWorkspaceWindow: () => Promise<void>;
  /** 设置弹窗状态（null = 关闭；tab 为可选初始 tab，缺省 = 通用）。 */
  settingsModal: { tab?: string } | null;
  /** 打开设置弹窗（可指定初始 tab）。 */
  openSettings: (tab?: string) => void;
  /** 关闭设置弹窗。 */
  closeSettings: () => void;
  /** 仓库设置弹窗状态（null = 关闭；target = 被编辑的仓库，可为列表里未激活的仓库）。 */
  vaultSettingsModal: { target: VaultSettingsTarget } | null;
  /** 打开仓库设置（文件面板的仓库行 / 空间行右键入口）：
   *  记录目标并让 settingsStore 建编辑会话（目标是激活仓库时直接编辑激活态，不建会话）。 */
  openVaultSettings: (target: VaultSettingsTarget) => void;
  /** 关闭仓库设置（会话在途写落盘后销毁，见 settingsStore.closeVaultSettingsSession）。 */
  closeVaultSettings: () => void;

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
  // 画布写操作组（新建/重命名/移动/复制/删除）的失败边界 = 落盘那一步：只有落盘失败才抛错（调用方据此提示）。
  // 其后的同步步骤（自写抑制/侧文件随迁/事件分发/列表刷新）不影响成败——订阅方异常已在事件总线逐个隔离、
  // 两条列表加载各自吞错；不得把它们纳入失败判定，否则文件已改却被报成「操作失败」，重试会重复建或撞旧路径。
  /** 新建画布到 dir（相对仓库根，空 = 根目录），返回 { id, file, title }（title 可能被同名去重）；失败抛错。 */
  createCanvas: (title?: string, dir?: string) => Promise<{ id: string; file: string; title: string }>;
  /** 重命名画布（同目录改文件名，按当前 file），返回实际标题；失败抛错。 */
  renameCanvas: (row: CanvasFileRow, title: string) => Promise<string>;
  /** 移动画布文件到目标文件夹（保持文件名，目标同名自动加序号；同目录 no-op），返回实际 file。 */
  moveCanvas: (row: CanvasFileRow, targetDir: string) => Promise<string>;
  /**
   * 复制画布为同目录副本（基于磁盘当前内容；title 同名自动加序号 + id 重新生成，
   * 副本保持「标题即文件名」规范），返回实际标题（调用方据此提示）。
   * 副本不自动打开。
   */
  duplicateCanvas: (row: CanvasFileRow) => Promise<string>;
  /** 删除画布（按 file）；失败抛错。 */
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

/** 系统「在文件管理器中打开」只接受绝对路径：仓库相对路径（@chip、`file:` 引用、图片/路径链接）
 *  按当前仓库根补全，否则会被 shell 的 scope 校验拒绝。已是绝对路径（Unix `/`、盘符、UNC、`file://`）原样返回。 */
function explorerAbsolutePath(path: string, vaultRoot: string | null): string {
  const isAbsolute =
    path.startsWith("/") || path.startsWith("\\\\") || path.startsWith("file://") || /^[A-Za-z]:[\\/]/.test(path);
  if (isAbsolute || !vaultRoot) return path;
  return `${vaultRoot.replace(/[\\/]+$/, "")}/${path}`;
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
  pluginPage: null,
  vaultRoot: null,
  vaultIdentity: null,
  vaultName: "",
  entryLoading: false,
  switchingVaultRoot: null,
  loadSteps: [],
  recentVaults: [],
  recentSpaces: [],
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

  init: async (): Promise<AutoEnterTarget | null> => {
    get().reportLoad("读取全局配置");
    let recents: RecentVault[] = [];
    let spaces: RecentSpace[] = [];
    let autoEnter: AutoEnterTarget | null = null;
    let autoUpdate = false;
    try {
      const { config: cfg, corruptBackup } = await readGlobalConfig();
      recents = cfg.recentVaults;
      spaces = cfg.spaces ?? [];
      autoUpdate = cfg.autoUpdate ?? false;
      notifyGlobalConfigCorrupt(corruptBackup);
    } catch (e) {
      console.error("读取全局配置失败", e);
      // 读失败（含「全局配置损坏且原文备份失败」被后端拒绝）会让最近仓库与外观本次不可用，必须可见
      useNotificationStore.getState().notify({
        level: "error",
        message: `全局配置读取失败，本次以空配置启动：${e instanceof Error ? e.message : String(e)}`,
      });
    }
    // 空间自动进入前先恢复登录态（restore 幂等；本地-only 用户服务器清单为空，零网络开销）。
    // 恢复失败（网络等）视同无会话：自动进入静默跳过，不报错刷屏
    if (spaces.length > 0) {
      get().reportLoad("恢复协作空间登录");
      try {
        await useSpaceAuthStore.getState().restore();
      } catch (e) {
        console.error("恢复协作空间会话失败", e);
      }
    }
    // 本地最近仓库与最近空间按打开时间取最近；最近是空间且会话无效时静默跳过自动进入
    //（停留未激活态，文件面板空态引导重新进入），不弹错误通知
    const lastVault = recents[0];
    const lastSpace = spaces[0];
    const vaultAt = lastVault?.lastOpenedAt ?? -1;
    const spaceAt = lastSpace?.openedAt ?? -1;
    if (lastSpace && (spaceAt > vaultAt || !lastVault)) {
      if (useSpaceAuthStore.getState().getServer(lastSpace.serverUrl)) {
        autoEnter = { kind: "space", entry: lastSpace };
      }
    } else if (lastVault) {
      // recentVaults[0] = 最近打开（selectVault 时置顶）= 上次所在仓库，启动时直接进入；
      // 仓库路径失效时 selectVault 失败停留未激活态（文件面板树区空态引导）
      autoEnter = { kind: "local", root: lastVault.root };
    }
    set({
      recentVaults: recents,
      recentSpaces: spaces,
      autoUpdate: autoUpdate,
    });
    // 应用级 UI 使用状态（布局/展开/上次文件）启动加载一次，之后跨仓库共享。
    // 等待完成：进仓门控「全部加载完再进入」涵盖布局状态，恢复上次打开文件依赖 loaded。
    get().reportLoad("加载布局与使用状态");
    await useUiStateStore.getState().load();
    return autoEnter;
  },

  setAutoUpdate: async (enabled) => {
    set({ autoUpdate: enabled });
    try {
      notifyGlobalConfigCorrupt(await updateGlobalConfig({ autoUpdate: enabled }));
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
      // （协作连接无需在此 dispose：安装成功后进程退出，服务端按 TCP 断开即移除 peer）
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
    // 重入守卫：切换进行中的再次调用直接忽略——面板内切换工作区保持可见，
    // 防并发重入的屏障职责由此承担（仓库树同时据 switchingVaultRoot 禁点全部行）
    if (get().switchingVaultRoot) return false;
    const seq = ++vaultSwitchSeq;
    set({ switchingVaultRoot: root });
    // 全屏加载会话：boot 期间渲染加载屏；面板内切换不再整屏替换，仅插件步骤上报仍经此门控
    get().beginLoad();
    get().reportLoad("打开仓库");
    try {
      // 切换前先落盘旧仓库的全部领域编辑并**等待写盘完成**：openVault 会把 VaultState.root 切到新仓库，
      // 若 fire-and-forget 直接放行，写盘可能晚于 open_vault 执行、把旧仓库内容写进新仓库（跨仓库污染）。
      // 领域 store 无改动则不写（脏门控，见各自 flush）；chatPanel 额外传当前仓库 root 做归属校验。
      // 经领域生命周期注册表分发（canvas/table/aichat/calendar/note 各钩子按注册序执行，失败快速传播）
      await flushAllDomains({ vaultRoot: get().vaultRoot });
      const info = await openVault(root);
      // 激活内容面仓库身份（root 绝对路径）：此后内容 I/O 按激活仓库取后端
      activateContentVault({ kind: "local", root: info.root });
      // 配置损坏已由 open_vault 备份（它紧接着就会重写原路径的配置文件，之后再读只会读到合法内容）
      if (info.configCorruptBackup) {
        useNotificationStore.getState().notify({
          level: "error",
          message: `仓库配置文件已损坏，原文备份为 .atelyx/${info.configCorruptBackup}：供应商、默认模型与 API key 需重新配置`,
        });
      }
      const now = Math.floor(Date.now() / 1000);
      const recents = bumpRecentVault(get().recentVaults, info, now);
      // set + 清空须在下一个 await 之前同步完成，双保险防跨仓库写入：
      // 1) NoteEditor debounce timer 是 macrotask，openVault→set 间无 await 则无隙可乘；
      // 2) 清空 noteList 先于 React 提交卸载（见下方注释），cleanup 的 stillExists 守卫必跳过。
      set({
        vaultRoot: info.root,
        vaultIdentity: { kind: "local", root: info.root },
        vaultName: info.name,
        recentVaults: recents,
        canvases: [],
        currentCanvasId: null,
        currentCanvasFile: null,
        currentNoteFile: null,
        currentNoteTitle: "",
        currentTableFile: null,
        currentTableTitle: "",
      });
      // 仓库设置弹窗对应「打开时那个仓库」：激活态已变，弹窗与会话一并关闭（同批同步执行）
      if (get().vaultSettingsModal) get().closeVaultSettings();
      // 立即清空旧仓库文件树 + 撤销栈/笔记运行时态（**必须在任何 await 之前**）：
      // NoteEditor 随 currentNoteFile 置空而卸载，其 cleanup 按「noteList 是否仍含该文件」决定是否
      // flush——若此处落后于下一个 await（React 提交卸载），noteList 还是旧仓库列表，cleanup 会把
      // 旧仓库内容经已切换的 root 写进新仓库同路径文件（跨仓库污染）。笔记挂起输入已在 openVault 前
      // flush 落盘旧仓库，残留（含 flush 后、切仓库前新输入）由下方 notifyVaultLeaving 同步清掉，不丢数据。
      useVaultStore.setState({ tree: [], noteList: [], tableList: [] });
      // 切仓库同步清态（笔记运行时态/撤销栈/画布运行时，经领域生命周期注册表分发）——同步执行，
      // 保住防跨仓库写入守卫：清空须在下一个 await 之前完成（与上方 set 同批，React 提交卸载前 noteList 已清空）
      notifyVaultLeaving();
      // 登记最近仓库失败不阻塞切换：global.json 写入异常（权限/磁盘）只影响最近列表，
      // 若放行抛错会被下方 catch 吞掉，导致后续重载（配置/画布列表/文件树/AI 会话）全部跳过
      try {
        notifyGlobalConfigCorrupt(await updateGlobalConfig({ recentVaults: recents }));
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
        // 强制重读盘（防会话身份键巧合等于目标时被幂等守卫跳过，面板停留在旧仓库会话）
        await notifyVaultEntered({ vaultRoot: info.root });
      } catch (e) {
        console.error("加载领域仓库上下文失败", e);
      }
      // 插件平台：切仓库后全量重载（load 内部先卸载旧贡献，再按新仓库上下文重建 app+vault 插件）；
      // 加载完成后再广播 vault:switch，保证订阅方是已就绪的后台插件。
      get().reportLoad("加载插件");
      try {
        await usePluginStore.getState().load();
      } catch (e) {
        console.error("加载插件失败", e);
      }
      emitPluginEvent("vault:switch", { root: info.root });
      return true;
    } catch (e) {
      console.error("打开仓库失败", e);
      // 失败必须可见：无激活仓库时文件面板停留树区空态，用户只看通知
      useNotificationStore.getState().notify({
        level: "error",
        message: `打开仓库失败：${e instanceof Error ? e.message : String(e)}`,
      });
      return false;
    } finally {
      if (seq === vaultSwitchSeq) {
        get().endLoad();
        set({ switchingVaultRoot: null });
      }
    }
  },

  /**
   * 进入协作空间仓库。时序与 selectVault 对齐：
   * flush 旧仓库（await，落盘在途先清零）→ 会话校验（网络 await，期间新输入进各领域挂起缓冲）
   * → 拉树预检（网络 await，失败停留原状态）→ 激活空间后端 → **同步**（下一个 await 之前）
   * set 切换态 + 清空文件树/当前文件 + notifyVaultLeaving 清 per-file 运行时——
   * 与 selectVault 同一防跨仓库守卫。空间路径不调 openVault（Rust 本地 root 不变），
   * 即便有漏网写入也落在旧仓库，不会污染空间。
   */
  selectSpace: async (entry) => {
    // 重入守卫（同 selectVault）：切换进行中直接拒绝；守卫值无本地 root，
    // 仓库树按 root 禁点匹配不到该行也无碍（守卫职责是防并发重入，不是 UI 定位）
    if (get().switchingVaultRoot) return "error";
    const seq = ++vaultSwitchSeq;
    set({ switchingVaultRoot: `space:${entry.serverUrl}#${entry.spaceId}` });
    get().beginLoad();
    get().reportLoad("打开协作空间");
    try {
      // 切换前先落盘旧仓库全部领域编辑并等待写盘完成（同 selectVault：flush 失败快速传播，中止切换）
      await flushAllDomains({ vaultRoot: get().vaultRoot });
      // 会话校验：restore 幂等（已恢复过直接返回），无有效令牌即 need-login（不进入激活态，UI 引导登录）
      get().reportLoad("校验空间会话");
      try {
        await useSpaceAuthStore.getState().restore();
      } catch (e) {
        console.error("恢复协作空间会话失败", e);
      }
      if (!useSpaceAuthStore.getState().getServer(entry.serverUrl)) {
        return "need-login";
      }
      // 拉树预检：服务端不可达/非成员在这里暴露，激活态保持原样（停留未激活或旧仓库）。
      // 预检用临时后端实例，激活走 createSpaceBackendRegistration（按身份 key 复用共享后端）
      get().reportLoad("连接协作空间");
      try {
        await createSpaceContentBackend(entry.serverUrl, entry.spaceId).listTree();
      } catch (e) {
        console.error("连接协作空间失败", e);
        useNotificationStore.getState().notify({
          level: "error",
          message: `无法打开协作空间：${e instanceof Error ? e.message : String(e)}`,
        });
        return "error";
      }
      // 成功：激活空间内容后端，随后同步完成全部切换态——set 与清理都在下一个 await 之前
      const registration = createSpaceBackendRegistration(entry.serverUrl, entry.spaceId);
      registration.activate();
      const now = Math.floor(Date.now() / 1000);
      const spaces = bumpRecentSpace(get().recentSpaces, { ...entry, openedAt: now }, now);
      set({
        vaultIdentity: registration.identity,
        vaultRoot: null,
        vaultName: entry.name,
        recentSpaces: spaces,
        canvases: [],
        currentCanvasId: null,
        currentCanvasFile: null,
        currentNoteFile: null,
        currentNoteTitle: "",
        currentTableFile: null,
        currentTableTitle: "",
      });
      // 立即清空旧仓库文件树 + 撤销栈/笔记运行时态（同步执行，同 selectVault 防跨仓库守卫）
      useVaultStore.setState({ tree: [], noteList: [], tableList: [] });
      notifyVaultLeaving();
      // 仓库设置弹窗对应「打开时那个仓库」：激活态已变，弹窗与会话一并关闭
      if (get().vaultSettingsModal) get().closeVaultSettings();
      // recentSpaces 落盘 global.json（失败不阻塞切换，同 recentVaults）
      try {
        notifyGlobalConfigCorrupt(await updateGlobalConfig({ spaces }));
      } catch (e) {
        console.error("登记最近空间失败", e);
      }
      // 仓库级配置按身份分流（metadata 层）：local = config.json；space = 服务端团队元数据
      get().reportLoad("加载仓库配置");
      try {
        await useSettingsStore.getState().loadVaultConfig();
      } catch (e) {
        console.error("加载仓库配置失败", e);
      }
      // 文件树与画布列表随切换等待完成（空间后端支持全量方法，与本地同链路）：
      // 门控「全部加载完再进入」，加载屏覆盖到数据就绪。加载失败不连带跳过其余步骤
      get().reportLoad("加载文件树与画布列表");
      try {
        await refreshCanvasAndTree();
      } catch (e) {
        console.error("加载文件树/画布列表失败", e);
      }
      // 领域仓库上下文（AI 会话读盘等）：空间无本地 root，传 null（会话域按未激活处理）
      get().reportLoad("加载 AI 会话");
      try {
        await notifyVaultEntered({ vaultRoot: null });
      } catch (e) {
        console.error("加载领域仓库上下文失败", e);
      }
      // 插件平台：全量重载（同 selectVault；加载完成后再广播 vault:switch）
      get().reportLoad("加载插件");
      try {
        await usePluginStore.getState().load();
      } catch (e) {
        console.error("加载插件失败", e);
      }
      emitPluginEvent("vault:switch", { root: null });
      return "ok";
    } finally {
      if (seq === vaultSwitchSeq) {
        get().endLoad();
        set({ switchingVaultRoot: null });
      }
    }
  },

  removeRecentVault: async (root) => {
    const recents = dropVaultFromRecents(get().recentVaults, root);
    try {
      notifyGlobalConfigCorrupt(await updateGlobalConfig({ recentVaults: recents }));
    } catch (e) {
      console.error("更新最近仓库列表失败", e);
    }
    set({ recentVaults: recents });
  },

  openPluginPage: (id) => set({ pluginPage: id }),

  closePluginPage: () => set({ pluginPage: null }),

  flushAllPending: async () => {
    // 领域 store 全部 pending 改动（画布/表格/AI 会话/日历/笔记挂起输入）经生命周期注册表分发；
    // 笔记编辑器挂起的 debounce 输入（组件内 timer，不走 store）也在笔记钩子内统一落盘 +
    // 补历史存档点，防关窗/切仓库/AI 重命名移动删除前丢最后 500ms 输入
    await flushAllDomains({ vaultRoot: get().vaultRoot });
    await useUiStateStore.getState().flush();
    await useSettingsStore.getState().flush();
    // 协作连接收尾不在此处（本函数启动自动更新检查时也会调用）：dispose 会断开会话内协作连接
    // 且清空 runtimeCfg，之后 applyConfig 全部失效、状态永久未连接。dispose 只由关窗守卫
    // （真退出）显式调用发 bye；更新 relaunch 场景进程退出即断，服务端按 TCP 断开立即移除 peer
  },

  installCloseGuard: () => {
    if (closeGuardInstalled) return;
    closeGuardInstalled = true;
    void onCloseRequestedSvc(async () => {
      await useAppStore.getState().flushAllPending();
      // 关窗 = 真退出：发 bye 离开协作房间并停止重连，防服务端 30s 心跳残留幽灵在线
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
  openInExplorer: async (path) => {
    try {
      await openInExplorerSvc(explorerAbsolutePath(path, get().vaultRoot));
    } catch (e) {
      // 系统打开失败必须可见：shell 的 scope 校验/权限拒绝只抛错，吞掉等于点击无反应
      console.error("在文件管理器中打开失败", e);
      useNotificationStore.getState().notify({ level: "error", message: "无法在文件管理器中打开该路径" });
    }
  },
  openUrl: async (url) => {
    try {
      await openUrlSvc(url);
    } catch (e) {
      console.error("打开链接失败", e);
      useNotificationStore.getState().notify({ level: "error", message: "无法用系统默认程序打开该链接" });
    }
  },
  readClipboardText: () => readClipboardTextSvc(),
  writeClipboardText: (text) => writeClipboardTextSvc(text),
  getAppVersion: () => getVersionSvc(),
  minimizeWindow: () => minimizeWindowSvc(),
  toggleMaximizeWindow: () => toggleMaximizeWindowSvc(),
  closeWindow: () => closeWindowSvc(),
  toggleFullscreen: () => toggleFullscreenSvc(),
  applyWorkspaceWindow: () => applyWorkspaceWindowSvc(),

  settingsModal: null,
  openSettings: (tab) => set({ settingsModal: tab ? { tab } : {} }),
  closeSettings: () => set({ settingsModal: null }),

  vaultSettingsModal: null,
  openVaultSettings: (target) => {
    set({ vaultSettingsModal: { target } });
    void useSettingsStore.getState().openVaultSettingsSession(target);
  },
  closeVaultSettings: () => {
    set({ vaultSettingsModal: null });
    void useSettingsStore.getState().closeVaultSettingsSession();
  },

  loadList: async () => {
    // 竞态守卫按仓库身份比较（非 vaultRoot）：空间仓库无本地 root（恒 null），
    // root 比较防不住「列表在途时切到另一个空间」的旧数据覆盖
    const identity = identityKeyOf(get().vaultIdentity);
    try {
      const canvases = await listCanvasesVault();
      // 等待期间用户可能已切到新仓库（后台填充链与面板快速切换并发），
      // 旧仓库的扫描结果不得覆盖新仓库的列表
      if (identityKeyOf(get().vaultIdentity) !== identity) return;
      set({ canvases });
    } catch (e) {
      console.error("加载画布列表失败", e);
    }
  },
  openCanvas: (row) => {
    set({ currentCanvasId: row.id, currentCanvasFile: row.file });
    // 记录「上次打开」供下次进入仓库恢复（画布窗口已无标签概念，打开即唯一文件状态）
    useUiStateStore.getState().recordOpenFile("canvas", row.file);
    // 记录最近打开（主页面板「最近打开」数据源；按仓库身份键归属——空间无本地 root）
    const identity = identityKeyOf(get().vaultIdentity);
    if (get().vaultIdentity) useUiStateStore.getState().recordRecentFile(row.file, "canvas", identity);
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
    if (get().vaultIdentity) {
      useUiStateStore
        .getState()
        .recordRecentFile(file, "note", identityKeyOf(get().vaultIdentity));
    }
    emitPluginEvent("note:opened", { file });
  },
  closeNote: () => {
    set({ currentNoteFile: null, currentNoteTitle: "" });
    useUiStateStore.getState().closeFile("note");
    emitPluginEvent("note:opened", { file: null });
  },
  openTable: (file, title) => {
    set({ currentTableFile: file, currentTableTitle: title });
    useUiStateStore.getState().recordOpenFile("table", file);
    if (get().vaultIdentity) {
      useUiStateStore
        .getState()
        .recordRecentFile(file, "table", identityKeyOf(get().vaultIdentity));
    }
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
    let created: { id: string; file: string };
    try {
      created = await createCanvasVault(actual, dir);
    } catch (e) {
      // 不吞错误：调用方据此提示失败（与其余画布写操作同一失败契约）
      console.error("新建画布失败", e);
      throw e;
    }
    markSelfSave(created.file);
    await refreshCanvasAndTree();
    return { id: created.id, file: created.file, title: actual };
  },
  renameCanvas: async (row, title) => {
    // 同名自动加序号（排除自身，同目录），返回实际标题供 UI 提醒
    const siblings = canvasesInDir(parentDir(row.file))
      .map((c) => c.title)
      .filter((t) => t !== row.title);
    const actual = dedupeFilename(title, siblings);
    try {
      await renameCanvasVault(row.file, actual);
    } catch (e) {
      // 不吞错误：调用方据此提示失败，磁盘未变时不得让 UI 以为改名成功
      console.error("重命名失败", e);
      throw e;
    }
    const newFile = siblingPath(row.file, `${sanitizeFilename(actual)}.atlx`);
    // 重命名后文件名变了：先算新路径再标记自写（旧路径删除 + 新路径创建事件一并抑制）
    markSelfSave([row.file, newFile]);
    // 侧文件先确保在新编码名下，再随重命名迁移（同笔记/表格路径）
    await migrateHistoryFile("canvas", row.file).catch((e) => notifySidecarFailure("画布重命名后的历史迁移", e));
    // 历史侧文件随迁（画布 kind 目录）；失败不阻塞重命名主流程
    await remapSideloads(row.file, newFile).catch((e) => notifySidecarFailure("画布重命名后的历史迁移", e));
    // 画布运行时引用同步（打开路径）经仓库事件分发
    emitVaultEvent({ kind: "canvas:renamed", oldPath: row.file, newPath: newFile });
    useUiStateStore.getState().renameLastFile("canvas", row.file, newFile);
    await refreshCanvasAndTree();
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
    } catch (e) {
      // 不吞错误：调用方（FileExplorerPanel.handleMoveFile）据此提示「移动文件失败」
      console.error("移动画布失败", e);
      throw e;
    }
    markSelfSave([row.file, newFile]);
    // 侧文件先确保在新编码名下，再随移动迁移（同 renameCanvas）
    await migrateHistoryFile("canvas", row.file).catch((e) => notifySidecarFailure("画布移动后的历史迁移", e));
    // 历史侧文件随迁（画布 kind 目录）；失败不阻塞移动主流程
    await remapSideloads(row.file, newFile).catch((e) => notifySidecarFailure("画布移动后的历史迁移", e));
    // 画布运行时引用同步（打开路径）经仓库事件分发
    emitVaultEvent({ kind: "canvas:moved", oldPath: row.file, newPath: newFile });
    useUiStateStore.getState().renameLastFile("canvas", row.file, newFile);
    await refreshCanvasAndTree();
    return newFile;
  },
  duplicateCanvas: async (row) => {
    // 同名自动加序号（同目录），返回实际标题供 UI 提醒
    const siblings = canvasesInDir(parentDir(row.file)).map((c) => c.title);
    const actual = dedupeFilename(row.title, siblings);
    let target: string;
    try {
      // 读磁盘原文 → 重写 id/title → 写新文件（write 的落盘路径由 title 决定，与 siblingPath 一致）
      const canvas = await readCanvasVault(row.file);
      canvas.id = crypto.randomUUID();
      canvas.title = actual;
      target = siblingPath(row.file, `${sanitizeFilename(actual)}.atlx`);
      await writeCanvasVault(canvas, target);
    } catch (e) {
      console.error("复制画布失败", e);
      throw e;
    }
    markSelfSave(target);
    await refreshCanvasAndTree();
    return actual;
  },
  deleteCanvas: async (row) => {
    try {
      await deleteCanvasVault(row.file);
    } catch (e) {
      // 不吞错误：调用方据此提示失败（与其余画布写操作同一失败契约）
      console.error("删除失败", e);
      throw e;
    }
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

/** 激活仓库的身份键（local = `local:<root>`；space = `space:<serverUrl>#<spaceId>`；未激活 = `"none"`）。
 *  组件/面板按此过滤跨仓库记录（最近打开），空间无本地 root 不能按 root 比对。 */
export function selectVaultIdentityKey(s: Pick<AppState, "vaultIdentity">): string {
  return identityKeyOf(s.vaultIdentity);
}
