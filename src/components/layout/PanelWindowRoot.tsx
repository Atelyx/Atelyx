/**
 * 撕裂窗口根（label `panel-<id>` 的独立窗口）。
 *
 * 与主窗口同代码入口，但只渲染单面板：自定义标题栏（窗口控制 + 拖动区）+ 标签头
 * （PanelTabBar，可多标签）+ 视图承载（ViewHost）。≡ 菜单锁定 = 整块窗口锁定。
 *
 * - 启动：panelStore.initPanel——bootstrap 拉布局快照（ui-state）+ 订阅 layout-broadcast
 *   广播镜像 + drag-session，未就绪前渲染 LoadingScreen
 * - 状态：标签组镜像自 uiStateStore（Rust 广播权威布局）；视图离开本窗口即 releaseView（flush + 清内存）
 * - 上下文：请求当前仓库/打开文件（emitRequestOpenFileState，应答经 open-file-changed 广播，
 *   按需加载仓库级配置/文件树/AI 会话）
 * - 关闭：installPanelCloseGuard（flush 托管视图 → notifyPanelClosed 上报 Rust → 销毁，守卫收在 panelStore）
 * - 外观/配置：与主窗口一致（useAppearance + settingsStore.load() 读盘）
 * - watcher：订阅仓库文件变化（画布/表格/笔记跨窗口写盘经 watcher + 乐观合并收敛）
 */
import { useEffect, useMemo } from "react";
import { LayoutTemplate } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import { titleOfTabs, usePanelStore } from "@/stores/panelStore";
import { usePluginStore } from "@/stores/pluginStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore } from "@/stores/vaultStore";
import { PanelTabBar } from "@/components/layout/PanelTabBar";
import { ViewHost, ViewStatusIndicator } from "@/components/layout/ViewHost";
import { DragGhost } from "@/components/layout/DragGhost";
import { TitleBarControls } from "@/components/common/TitleBarControls";
import { PanelPlaceholder } from "@/components/layout/PanelPlaceholder";
import { LoadingScreen } from "@/components/common/LoadingScreen";
import { useAppearance } from "@/hooks/useAppearance";
import { collectAllViews } from "@/utils/workspaceLayout";

export function PanelWindowRoot() {
  useAppearance();
  const panelReady = usePanelStore((s) => s.panelReady);
  const tabs = usePanelStore((s) => s.panelTabs);
  const activeTabId = usePanelStore((s) => s.panelActiveTabId);
  const windowId = usePanelStore((s) => s.windowId);
  const dropTarget = usePanelStore((s) => s.dropTarget);
  const layoutMirror = usePanelStore((s) => s.layoutMirror);

  const minimizeWindow = useAppStore((s) => s.minimizeWindow);
  const toggleMaximizeWindow = useAppStore((s) => s.toggleMaximizeWindow);
  const closeWindow = useAppStore((s) => s.closeWindow);

  // 初始化：面板角色 bootstrap（布局快照 + 广播订阅）+ 外观/配置读盘 + 插件运行时 + watcher 订阅
  useEffect(() => {
    void usePanelStore.getState().initPanel();
    void useSettingsStore.getState().load();
    // 撕裂窗口是独立 webview：本窗口的插件运行时（各行视图贡献）须各自 load 拉起——
    // 切仓库时 panelStore 会再按 open-file-changed load，此处覆盖冷启动（启动页恢复的窗口）。
    void usePluginStore.getState().load().catch((e) => console.error("撕裂窗口加载插件失败", e));
  }, []);

  useEffect(() => {
    useVaultStore.getState().startFileWatcher(true);
    return () => useVaultStore.getState().startFileWatcher(false);
  }, []);

  // 关闭守卫（收进 panelStore.installPanelCloseGuard：flush 托管视图 → 上报关闭 → 销毁；幂等防重复订阅）
  useEffect(() => {
    void usePanelStore.getState().installPanelCloseGuard();
  }, []);

  // 聚焦门控（画布/表格快捷键）：激活标签即聚焦本窗口——setFocusedPanel 会把
  // focusedPanelId 经防抖持久化到 ui-state（与主窗口同一字段），重启后恢复
  useEffect(() => {
    useUiStateStore.getState().setFocusedPanel(windowId);
  }, [activeTabId, windowId]);

  const activeTab = tabs.find((t) => t.id === activeTabId) ?? tabs[0] ?? null;
  const title = titleOfTabs(tabs, activeTabId);

  const usedViews = useMemo(() => {
    if (!layoutMirror) return [];
    return collectAllViews(layoutMirror.activeTree, layoutMirror.detachedWindows);
  }, [layoutMirror]);

  const isDropTarget = dropTarget?.window === windowId && dropTarget.zone === "center";

  if (!panelReady) {
    return <LoadingScreen />;
  }

  return (
    <div
      className="h-full w-full flex flex-col"
      style={{ background: "var(--bg-primary)" }}
      data-panel-drop-root
    >
      {/* 自定义标题栏（与主窗口一致：拖动区 + 窗口控制） */}
      <div
        className="h-9 flex items-center gap-1 px-2 flex-shrink-0 select-none"
        style={{ background: "var(--bg-secondary)", borderBottom: "1px solid var(--border)" }}
        data-tauri-drag-region
      >
        <span
          className="text-xs truncate"
          style={{ color: "var(--text-secondary)" }}
          data-tauri-drag-region
        >
          {title}
        </span>
        <div className="ml-auto h-full flex items-center" data-tauri-drag-region>
          <TitleBarControls
            onMinimize={() => void minimizeWindow()}
            onMaximize={() => void toggleMaximizeWindow()}
            onClose={() => void closeWindow()}
          />
        </div>
      </div>

      {/* 标签头 + 视图承载 */}
      <PanelTabBar
        hostId={windowId}
        isPanel
        allowSplit={false}
        tabs={tabs}
        activeTabId={activeTabId}
        usedViews={usedViews}
        canDeletePanel
        status={activeTab ? <ViewStatusIndicator view={activeTab.view} /> : null}
        onPickView={(view) => usePanelStore.getState().panelAddView(view)}
        onActivate={(tabId) => usePanelStore.getState().panelSetActive(tabId)}
        onCloseTab={(tabId) => usePanelStore.getState().panelCloseTab(tabId)}
        onCloseFile={(view) => {
          // 关闭文件（标签保留）：文件状态全局唯一，按视图清全局当前文件状态
          const app = useAppStore.getState();
          if (view === "canvas") app.closeCanvas();
          else if (view === "note") app.closeNote();
          else if (view === "table") app.closeTable();
        }}
        onSetTabView={(tabId, view) => usePanelStore.getState().panelSetTabView(tabId, view)}
        onTogglePanelLock={() => {
          // 整块锁定/解锁撕裂窗口：所有标签统一设同一锁定值（空窗口无标签，无操作）
          const target = !(tabs.length > 0 && tabs.every((t) => t.locked));
          tabs.forEach((t) => usePanelStore.getState().panelSetLocked(t.id, target));
        }}
        onDeletePanel={() => void closeWindow()}
        onFocusHost={() => useUiStateStore.getState().setFocusedPanel(windowId)}
      />
      <div className="flex-1 min-h-0">
        {activeTab ? (
          <ViewHost view={activeTab.view} hostId={windowId} />
        ) : (
          <PanelPlaceholder
            icon={<LayoutTemplate size={64} strokeWidth={1.5} />}
            title="空面板"
            description="右键头部添加视图，或从主窗口拖入标签。"
          />
        )}
      </div>

      {/* drop 指示器（中部 = 加标签） */}
      {isDropTarget && (
        <div
          style={{
            position: "fixed",
            inset: 0,
            pointerEvents: "none",
            zIndex: 40,
            background: "color-mix(in srgb, var(--accent) 18%, transparent)",
            outline: "1px solid color-mix(in srgb, var(--accent) 60%, transparent)",
          }}
        />
      )}

      {/* 拖拽 ghost 影子（跨窗口跟随光标） */}
      <DragGhost />
    </div>
  );
}
