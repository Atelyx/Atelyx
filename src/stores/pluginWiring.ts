/**
 * 插件内核接线（宿主装配执行体）：把领域 store 数据源、槽位渲染宿主、能力变更事件
 * 注入内核各服务注入点。不 import pluginStore——插件行状态经 context 回调注入，保持装配 → 状态单向。
 */
import type { AppUiState, PluginGlobalShortcutDeclaration } from "@/types";
import {
  setPluginCollabAccess,
  setPluginFloatingLayerAccess,
  setPluginHistoryAccess,
  setPluginLayoutAccess,
  setPluginNotificationAccess,
  setPluginShortcutAccess,
  setPluginSlotHostComponent,
  setPluginUiStateAccess,
  setPluginVaultWriteAccess,
  setSettingsAccess,
} from "@/services/cordis/access";
import { setSlotWinnerOverrideSource } from "@/services/cordis/slots";
import { emitPluginEvent } from "@/services/cordis/events";
import { loadHistory } from "@/services/history";
import { layoutOp } from "@/services/layout";
import { pluginApplyDefaultLayout } from "@/services/plugins";
import { errText } from "@/utils/errors";
import {
  appendVaultFile,
  editVaultFile,
  writeVaultFile,
} from "@/services/vault/aiFiles";
import {
  useCollabStore,
  dispatchPluginChannel,
  getMyPeerInfo,
  publishPluginPresence,
  registerCollabChannel,
  sendPluginMessage,
} from "@/stores/collabStore";
import { useFloatingLayerStore } from "@/stores/floatingLayerStore";
import { useNotificationStore } from "@/stores/notificationStore";
import { useNoteStore } from "@/stores/noteStore";
import { useCanvasStore } from "@/stores/canvasStore";
import { useTableStore } from "@/stores/tableStore";
import { useRepoHistoryStore } from "@/stores/repoHistoryStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore } from "@/stores/vaultStore";
import { PluginSlotHost } from "@/components/plugins/SlotHost";

/** 接线所需、但状态归属 pluginStore 的两处数据源（由 pluginStore 注入，避免反向 import）。 */
export interface PluginWiringContext {
  /** 已装插件行的热键声明表（manifest.shortcuts；按调用时行状态现查）。 */
  pluginShortcuts(pluginId: string): PluginGlobalShortcutDeclaration[];
  /** 槽位手动胜者变化 → 推进对应槽修订号并 bump uiRevision（宿主订阅重取胜者）。 */
  bumpSlotRevisions(changed: Set<string>): void;
}

/** vault 写能力接线守卫：把仓库写方法暴露给 `vault` 服务（幂等一次）。
 *  复用 AI 文件工具同一批 service/store 语义（原子写/扩展名分发引用维护/树刷新）；
 *  `.md` 写入对打开的笔记会话就是一次磁盘内容变化（会话按内容事实收敛，不按调用方放行）；
 *  rename/move/delete/deleteDir/createFolder 走 vaultStore（扩展名分发 + loadFiles 刷新）。 */
let vaultWriteWired = false;
function ensureVaultWriteAccess(): void {
  if (vaultWriteWired) return;
  vaultWriteWired = true;
  setPluginVaultWriteAccess({
    writeFile: async (file, content) => {
      await writeVaultFile(file, content);
      return { ok: true, summary: `已写入「${file}」` };
    },
    editFile: (file, edits) => editVaultFile(file, edits),
    appendFile: (file, content) => appendVaultFile(file, content),
    renameFile: (oldPath, newName) => useVaultStore.getState().renameFile(oldPath, newName),
    moveFile: (oldPath, targetDir) => useVaultStore.getState().moveFile(oldPath, targetDir),
    deleteFile: (path) => useVaultStore.getState().deleteFile(path),
    deleteDir: async (dir, force) => {
      const r = await useVaultStore.getState().deleteFolder(dir, force);
      return {
        ok: r.deleted,
        summary: r.deleted
          ? `已删除目录「${dir}」`
          : r.needsConfirm
            ? `目录非空（${r.itemCount} 项），需确认后删除`
            : "删除目录失败",
        needsConfirm: r.needsConfirm,
        itemCount: r.itemCount,
      };
    },
    createFolder: async (dir) => {
      const path = await useVaultStore.getState().createFolder(dir);
      return { ok: true, summary: `已创建「${path}」`, path };
    },
  });
}

/** 协作能力接线守卫：把在线用户/presence/插件消息收发/本端身份/协作意愿声明暴露给内核 `collab` 服务（幂等一次）。 */
let collabRuntimeWired = false;
function ensureCollabRuntimeAccess(): void {
  if (collabRuntimeWired) return;
  collabRuntimeWired = true;
  setPluginCollabAccess({
    peers: () => useCollabStore.getState().peers,
    setPresence: (view, file) => publishPluginPresence(view, file),
    sendMessage: (channel, payload, to) => sendPluginMessage(channel, payload, to),
    myPeer: () => getMyPeerInfo(),
    acquire: () => useCollabStore.getState().retainPluginDemand(),
  });
}

/** 通知能力接线：把应用内通知运行时暴露给内核 `notification` 服务（幂等一次）。 */
let notificationWired = false;
function ensureNotificationAccess(): void {
  if (notificationWired) return;
  notificationWired = true;
  setPluginNotificationAccess({
    notify: (input) => useNotificationStore.getState().notify(input),
    dismiss: (id) => useNotificationStore.getState().dismiss(id),
  });
}

/** 全局热键解析数据源接线：manifest 声明表（已装插件行）+ 用户覆盖表（settingsStore）暴露给
 *  内核 `shortcuts` 按声明注册与设置页改键（幂等一次）。声明按调用时插件行现查——接线早于
 *  行集合写入，注册发生在挂载后，届时行已在 store。 */
let shortcutAccessWired = false;
function ensureShortcutAccess(ctx: PluginWiringContext): void {
  if (shortcutAccessWired) return;
  shortcutAccessWired = true;
  setPluginShortcutAccess({
    declarations: (pluginId) => ctx.pluginShortcuts(pluginId),
    overrides: () => useSettingsStore.getState().globalShortcuts,
  });
}

/** 浮层承载接线：把插件浮层运行时暴露给内核 `ui` 服务（幂等一次）。 */
let floatingLayerWired = false;
function ensureFloatingLayerAccess(): void {
  if (floatingLayerWired) return;
  floatingLayerWired = true;
  setPluginFloatingLayerAccess({
    open: (entry) => useFloatingLayerStore.getState().open(entry),
    close: (id) => useFloatingLayerStore.getState().close(id),
  });
}

/** AI 配置接线守卫：把供应商/模型/Agent 与默认目标解析暴露给内核 `ai` 服务（幂等一次）。
 *  providers 为运行时配置（apiKey 已由 settingsStore 填充——key 读取不进本层）。 */
let settingsAccessWired = false;
function ensureSettingsAccess(): void {
  if (settingsAccessWired) return;
  settingsAccessWired = true;
  setSettingsAccess(() => {
    const s = useSettingsStore.getState();
    return {
      providers: s.config.providers,
      agents: s.agents,
      resolveChatTarget: (sel) => s.resolveChatTarget(sel),
    };
  });
}

/** 领域历史访问接线守卫：把历史列表/回滚/仓库聚合暴露给内核 `history` 服务（幂等一次）。 */
let historyAccessWired = false;
function ensureHistoryAccess(): void {
  if (historyAccessWired) return;
  historyAccessWired = true;
  setPluginHistoryAccess({
    list: (kind, file) => loadHistory(kind, file),
    rollback: async (kind, file, seq) => {
      if (kind === "note") {
        const result = await useNoteStore.getState().noteHistoryRollback(file, seq);
        // 回滚被保存前钩子 veto（或目标版本不存在）：回滚未发生，如实抛错让调用插件感知
        if (result === null) throw new Error("回滚未执行");
      } else if (kind === "canvas") await useCanvasStore.getState().canvasHistoryRollback(file, seq);
      else await useTableStore.getState().tableHistoryRollback(file, seq);
    },
    repoHistory: () => {
      const s = useRepoHistoryStore.getState();
      return { entries: s.entries, dailyCounts: s.dailyCounts };
    },
  });
}

/** 布局访问接线守卫：把布局镜像 + 安全操作子集暴露给内核 `layout` 服务（幂等一次）。
 *  addView/op 均经 layout-op（Rust 是唯一变更入口）。 */
let layoutAccessWired = false;
function ensureLayoutAccess(): void {
  if (layoutAccessWired) return;
  layoutAccessWired = true;
  setPluginLayoutAccess({
    activeLayoutId: () => useUiStateStore.getState().activeLayoutId,
    layouts: () => useUiStateStore.getState().workspaceLayouts,
    addView: (panelId, view) => layoutOp({ op: "addView", panelId, view }),
    op: (op) => layoutOp(op),
    // 默认布局应用失败不阻断插件其余注册，但要用户可见（数据面之外的布局诉求落空）
    applyDefaultLayout: async (pluginId, spec) => {
      try {
        await pluginApplyDefaultLayout(pluginId, spec.name, spec.tree);
      } catch (e) {
        console.error("应用插件默认布局失败", e);
        useNotificationStore.getState().notify({
          level: "warning",
          title: "插件默认布局未生效",
          message: errText(e),
        });
      }
    },
  });
}

/** 应用级 UI 使用状态访问接线守卫：把非布局字段 + 布局镜像暴露给内核 `uiState` 服务（幂等一次）。 */
let uiStateAccessWired = false;
function ensureUiStateAccess(): void {
  if (uiStateAccessWired) return;
  uiStateAccessWired = true;
  setPluginUiStateAccess({
    read: () => {
      const s = useUiStateStore.getState();
      return {
        fileExplorerExpanded: [...s.fileExplorerExpanded],
        lastCanvasFile: s.lastCanvasFile ?? undefined,
        lastNoteFile: s.lastNoteFile ?? undefined,
        lastTableFile: s.lastTableFile ?? undefined,
        workspaceLayouts: s.workspaceLayouts,
        activeLayoutId: s.activeLayoutId ?? undefined,
        focusedPanelId: s.focusedPanelId ?? undefined,
        detachedWindows: s.detachedWindows,
        recentFiles: s.recentFiles,
        slotWinnerOverrides: s.slotWinnerOverrides,
      } as unknown as AppUiState;
    },
  });
}

/** 槽位宿主接线守卫：把插件侧槽位渲染宿主（ctx.slots.host 返回的组件）注入内核 slots 服务（幂等一次）。 */
let slotHostWired = false;
function ensureSlotHostAccess(): void {
  if (slotHostWired) return;
  slotHostWired = true;
  setPluginSlotHostComponent(PluginSlotHost);
}

/** 槽位胜者覆盖接线守卫：把应用级 ui-state 的「手动胜者」注入 resolveSlot（幂等一次）。
 *  覆盖变化（设置里切换胜者）按变化槽 bump 修订号——与 onSlotChange 同口径让既有按槽/全局
 *  订阅宿主自动重取胜者；覆盖是低频手动操作，diff 成本可忽略，不必给每个消费方加新订阅。 */
let slotOverrideWired = false;
function ensureSlotOverrideAccess(ctx: PluginWiringContext): void {
  if (slotOverrideWired) return;
  slotOverrideWired = true;
  setSlotWinnerOverrideSource((slot) => {
    // 槽名是插件任意字符串，读钉住须 hasOwn（裸索引会命中原型链，如 "constructor"）。
    const overrides = useUiStateStore.getState().slotWinnerOverrides;
    return Object.hasOwn(overrides, slot) ? overrides[slot] : null;
  });
  useUiStateStore.subscribe((state, prev) => {
    if (state.slotWinnerOverrides === prev.slotWinnerOverrides) return;
    const changed = new Set([
      ...Object.keys(state.slotWinnerOverrides),
      ...Object.keys(prev.slotWinnerOverrides),
    ]);
    if (changed.size === 0) return;
    ctx.bumpSlotRevisions(changed);
  });
}

/** 能力变更事件接线守卫：内核侧 store 变更 → emitPluginEvent 通知订阅插件（幂等一次）。
 *  canvas/table 变更事件随各自插件启停注册（见 canvasStore/tableStore 的 register*PluginWiring）；
 *  collab/vault 属内核数据访问，常驻。载荷为轻量信号（插件按需再调 snapshot()/取数据）。
 *  插件通用消息入站（plugin-msg 通道）同样在此接线：collabHost 通道 → 插件频道订阅注册表
 *  （ctx.collab.subscribe 的落点），handler 常驻内核不随插件启停（投递过滤由订阅注册表承担）。 */
let runtimeEventsWired = false;
function ensureRuntimeChangeEvents(): void {
  if (runtimeEventsWired) return;
  runtimeEventsWired = true;
  useCollabStore.subscribe((s, prev) => {
    if (s.peers !== prev.peers) emitPluginEvent("collab:changed", { peers: s.peers });
  });
  useVaultStore.subscribe((s, prev) => {
    if (s.tree !== prev.tree) emitPluginEvent("vault:changed", {});
  });
  // 入站帧的 file 槽承载插件线路频道名（插件id:逻辑频道），按订阅注册表投递（未订阅频道不投递）
  registerCollabChannel("plugin-msg", (peerId, file, payload) =>
    dispatchPluginChannel(peerId, file, payload),
  );
}

/** 插件内核接线总入口（幂等一次；pluginStore.load 首次进入时调用）。 */
export function wirePluginRuntime(ctx: PluginWiringContext): void {
  ensureVaultWriteAccess();
  ensureCollabRuntimeAccess();
  ensureNotificationAccess();
  ensureShortcutAccess(ctx);
  ensureFloatingLayerAccess();
  ensureSettingsAccess();
  ensureHistoryAccess();
  ensureLayoutAccess();
  ensureUiStateAccess();
  ensureSlotHostAccess();
  ensureSlotOverrideAccess(ctx);
  ensureRuntimeChangeEvents();
}
