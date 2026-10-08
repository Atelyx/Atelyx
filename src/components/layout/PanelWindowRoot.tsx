/**
 * 撕裂窗口根（label `panel-<id>` 的独立窗口）：与主窗口同代码入口，但只渲染单面板——
 * 自定义标题栏（窗口控制 + 拖动区）+ 标签头（PanelTabBar，可多标签）+ 视图承载（ViewHost）；
 * ≡ 菜单锁定 = 整块窗口锁定。启动/状态镜像/关闭守卫等生命周期见各 effect 与 panelStore 注释。
 */
import { useEffect, useMemo } from "react";
import { LayoutTemplate, TriangleAlert } from "lucide-react";
import { SlotReplaceMount } from "@/components/plugins/SlotHost";
import { useAppStore } from "@/stores/appStore";
import { titleOfTabs, usePanelStore } from "@/stores/panelStore";
import { usePluginStore } from "@/stores/pluginStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { PanelTabBar } from "@/components/layout/PanelTabBar";
import { ViewHost, ViewStatusIndicator } from "@/components/layout/ViewHost";
import { DragGhost } from "@/components/layout/DragGhost";
import { TitleBarControls } from "@/components/common/TitleBarControls";
import { PanelPlaceholder } from "@/components/layout/PanelPlaceholder";
import { LoadingScreen } from "@/components/common/LoadingScreen";
import { Spinner } from "@/components/common/primitives";
import { useAppearance } from "@/hooks/useAppearance";
import { collectAllViews } from "@/utils/workspaceLayout";

export function PanelWindowRoot() {
  useAppearance();
  const panelReady = usePanelStore((s) => s.panelReady);
  const panelError = usePanelStore((s) => s.panelError);
  const tabs = usePanelStore((s) => s.panelTabs);
  const activeTabId = usePanelStore((s) => s.panelActiveTabId);
  const windowId = usePanelStore((s) => s.windowId);
  const dropTarget = usePanelStore((s) => s.dropTarget);
  const layoutMirror = usePanelStore((s) => s.layoutMirror);
  const switchGate = usePanelStore((s) => s.switchGate);

  const minimizeWindow = useAppStore((s) => s.minimizeWindow);
  const toggleMaximizeWindow = useAppStore((s) => s.toggleMaximizeWindow);
  const closeWindow = useAppStore((s) => s.closeWindow);

  // 初始化：面板角色 bootstrap（布局快照 + 广播订阅）+ 外观/配置读盘 + 插件运行时
  useEffect(() => {
    void usePanelStore.getState().initPanel();
    void useSettingsStore.getState().load();
    // 撕裂窗口是独立 webview：本窗口的插件运行时（各行视图贡献）须各自 load 拉起——
    // 切仓库时 panelStore 会再按仓库上下文广播 load，此处覆盖冷启动（boot 自动进仓前恢复的窗口）。
    void usePluginStore.getState().load().catch((e) => console.error("撕裂窗口加载插件失败", e));
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

  // 仓库切换门遮罩：切换准备/上下文加载期间整窗禁写（flush 后到新仓库数据就绪前，
  // 任何写入都会打进已切换的新仓库）；abort 或加载链完成时随门状态清空自动消失
  const switchGateMask = switchGate ? (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center flex-col gap-3"
      style={{ background: "var(--bg-primary)" }}
    >
      <Spinner />
      <span className="text-sm" style={{ color: "var(--text-secondary)" }}>
        正在切换仓库…
      </span>
    </div>
  ) : null;

  // 插件声明了窗口选项（自定形态）的撕裂窗口：无标题栏/标签条的裸渲染——插件视图
  // 自带头部（拖动区 + 窗口动作），错误态保留最小拖动条 + 重试（否则窗口既拖不动
  // 也无处重试）。关闭语义由窗口选项声明，Rust/守卫按选项拦截。
  // 判定 = 窗口声明了任一非默认选项（即创建方要求的特殊形态）→ 裸渲染插件视图。
  const isStandaloneWindow = useUiStateStore((s) =>
    (s.detachedWindows ?? []).some(
      (w) => w.id === windowId && Object.values(w.options ?? {}).some(Boolean),
    ),
  );

  if (!panelReady) {
    return <LoadingScreen />;
  }

  if (isStandaloneWindow) {
    return (
      <div className="h-full w-full flex flex-col" data-panel-drop-root>
        {panelError ? (
          <>
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
            <div className="flex-1 min-h-0">
              <PanelPlaceholder
                icon={<TriangleAlert size={64} strokeWidth={1.5} />}
                title="面板未能加载"
                description={panelError}
                action={
                  <button
                    onClick={() => void usePanelStore.getState().retryPanelInit()}
                    className="px-3 py-1.5 rounded text-xs border hover:bg-[var(--hover)]"
                    style={{ borderColor: "var(--border)", color: "var(--text-primary)" }}
                  >
                    重试
                  </button>
                }
              />
            </div>
          </>
        ) : activeTab ? (
          <div className="flex-1 min-h-0">
            <ViewHost view={activeTab.view} hostId={windowId} />
          </div>
        ) : null}
        {switchGateMask}
        <DragGhost />
      </div>
    );
  }

  return (
    // 不画底色：与 html/body 的底色同值，重画只会让半透明皮肤（极光）多叠一层、氛围底透不上来
    <div className="h-full w-full flex flex-col" data-panel-drop-root>
      {/* 标题栏（默认实现）：拖动区 + 窗口控制。错误态同样渲染——否则窗口既不能拖动也无法关闭，
          而错误态是「布局服务未响应」这类可达界面。插件可经 shell/titlebar 单槽替换本窗口标题栏。 */}
      <SlotReplaceMount slot="shell/titlebar">
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
      </SlotReplaceMount>

      {panelError ? (
        // bootstrap 失败/超时：可见错误态 + 重试入口（不静默停在加载屏）
        <div className="flex-1 min-h-0">
          <PanelPlaceholder
            icon={<TriangleAlert size={64} strokeWidth={1.5} />}
            title="面板未能加载"
            description={panelError}
            action={
              <button
                onClick={() => void usePanelStore.getState().retryPanelInit()}
                className="px-3 py-1.5 rounded text-xs border hover:bg-[var(--hover)]"
                style={{ borderColor: "var(--border)", color: "var(--text-primary)" }}
              >
                重试
              </button>
            }
          />
        </div>
      ) : (
        <>
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
        </>
      )}

      {/* 仓库切换门遮罩（含独立形态窗口的提前返回分支） */}
      {switchGateMask}

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
