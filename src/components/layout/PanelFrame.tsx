/**
 * 面板框：标签头（PanelTabBar）+ 视图承载（ViewHost）。标签交互与 ≡ 菜单见 PanelTabBar。
 * 点击面板任意处聚焦（画布快捷键门控依据）；空面板内容区右键同样可添加视图（与头部空白右键一致）。
 */
import { memo, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { usePluginStore } from "@/stores/pluginStore";
import { PanelTabBar, ViewPickerMenu } from "@/components/layout/PanelTabBar";
import { ViewHost, ViewStatusIndicator } from "@/components/layout/ViewHost";
import type { PanelNode, ViewKind } from "@/types";

export const PanelFrame = memo(function PanelFrame({
  node,
  onFocus,
  usedKey,
  panelCount,
}: {
  node: PanelNode;
  onFocus: (id: string) => void;
  /** 全局已占用视图集合的稳定键（WorkspaceGrid 计算，含撕裂窗口；resize 时不变，memo 跳过重渲染）。 */
  usedKey: string;
  /** 当前布局面板总数（= 1 时「删除面板」禁用）。 */
  panelCount: number;
}) {
  const addViewToPanel = useUiStateStore((s) => s.addViewToPanel);
  const setActiveTab = useUiStateStore((s) => s.setActiveTab);
  const closeTab = useUiStateStore((s) => s.closeTab);
  const setTabView = useUiStateStore((s) => s.setTabView);
  const setTabLocked = useUiStateStore((s) => s.setTabLocked);
  const splitPanel = useUiStateStore((s) => s.splitPanel);
  const closePanel = useUiStateStore((s) => s.closePanel);
  const setFocusedPanel = useUiStateStore((s) => s.setFocusedPanel);
  const closeCanvas = useAppStore((s) => s.closeCanvas);
  const closeNote = useAppStore((s) => s.closeNote);
  const closeTable = useAppStore((s) => s.closeTable);

  const used = new Set(usedKey ? (usedKey.split(",") as ViewKind[]) : []);
  const activeTab = node.tabs.find((t) => t.id === node.activeTabId) ?? node.tabs[0] ?? null;

  // 视图候选 = 内建 + 插件面板（订阅插件 UI 注册变化刷新）。
  usePluginStore((s) => s.uiRevision);
  const viewKinds: string[] = usePluginStore.getState().pluginViewKinds();

  // 空面板内容区右键视图菜单（与头部空白右键一致）
  const [emptyMenu, setEmptyMenu] = useState<{ x: number; y: number } | null>(null);

  return (
    <div
      className="h-full flex flex-col min-h-0"
      data-drop-panel={node.id}
      onClick={() => onFocus(node.id)}
    >
      <PanelTabBar
        hostId={node.id}
        tabs={node.tabs}
        activeTabId={node.activeTabId}
        usedViews={[...used]}
        canDeletePanel={panelCount > 1}
        status={activeTab ? <ViewStatusIndicator view={activeTab.view} /> : null}
        onPickView={(view) => addViewToPanel(node.id, view)}
        onActivate={(tabId) => setActiveTab(node.id, tabId)}
        onCloseTab={(tabId) => closeTab(node.id, tabId)}
        onCloseFile={(view) => {
          // 关闭文件（标签保留）：按视图清全局当前文件状态
          if (view === "canvas") closeCanvas();
          else if (view === "note") closeNote();
          else if (view === "table") closeTable();
        }}
        onSetTabView={(tabId, view) => setTabView(node.id, tabId, view)}
        onTogglePanelLock={() => {
          // 整块锁定/解锁：面板所有标签统一设同一锁定值（空面板无标签，无操作）
          const target = !(node.tabs.length > 0 && node.tabs.every((t) => t.locked));
          node.tabs.forEach((t) => setTabLocked(node.id, t.id, target));
        }}
        onSplit={(dir) => splitPanel(node.id, dir)}
        onDeletePanel={() => closePanel(node.id)}
        onFocusHost={() => setFocusedPanel(node.id)}
      />
      {/* 面板内容（空面板渲染占位引导，右键可添加视图） */}
      <div
        className="flex-1 min-h-0"
        onContextMenu={
          activeTab
            ? undefined
            : (e) => {
                e.preventDefault();
                e.stopPropagation();
                setEmptyMenu({ x: e.clientX, y: e.clientY });
              }
        }
      >
        <ViewHost view={activeTab?.view ?? "empty"} hostId={node.id} />
      </div>
      {emptyMenu && (
        <ViewPickerMenu
          x={emptyMenu.x}
          y={emptyMenu.y}
          onClose={() => setEmptyMenu(null)}
          tabs={node.tabs}
          usedViews={[...used]}
          kinds={viewKinds}
          onPickView={(view) => addViewToPanel(node.id, view)}
        />
      )}
    </div>
  );
});
