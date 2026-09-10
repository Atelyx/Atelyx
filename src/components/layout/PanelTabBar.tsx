/**
 * 面板标签条（主窗口面板头与撕裂窗口头共用）：标签组 + 标签右键菜单 + 空白右键视图切换菜单 + ≡ 菜单 + 拖拽源。
 *
 * - 标签：点击激活；锁定标签显示锁标记且不可拖；pointer 拖拽 = 撕裂/停靠/排序（经 panelStore 会话）
 * - 标签右键菜单：**关闭该标签** + **切换标签视图 ›**（钻入子菜单列出全部视图类型，受全局唯一约束）
 * - 空白右键：弹出视图选择菜单（添加视图）——组内已有 = 激活，未占用 = 添加为本组标签，撕裂窗口占用 = 禁用
 * - ≡ 菜单（右）：锁定/解锁（整块面板，锁定 = 不可移动/关闭标签/删除面板）、**删除面板**（整块删除含标签）、左右/上下分割
 * - 布局操作全部经 props 回调注入宿主（PanelFrame → uiStateStore；PanelWindowRoot → panelStore）
 */
import { ChevronLeft, ChevronRight, Lock, LockOpen, Menu, X } from "lucide-react";
import { memo, useRef, useState, type ReactNode } from "react";
import { useAppStore } from "@/stores/appStore";
import { usePanelStore } from "@/stores/panelStore";
import { usePluginStore } from "@/stores/pluginStore";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";
import { PopupLayer } from "@/components/common/PopupLayer";
import { Menu as MenuShell, MenuDivider, MenuItem } from "@/components/common/Menu";
import { VIEW_META, viewMetaFor } from "@/components/layout/ViewHost";
import { noteTitleFromFile, tableTitleFromFile } from "@/utils/filename";
import { type SplitDirection, type TabItem, type ViewKind } from "@/types";

export interface PanelTabBarProps {
  /** 宿主标识：面板 id（主窗口）或撕裂窗口 id。 */
  hostId: string;
  /** 是否为撕裂窗口（拖拽命中窗口判定用）。 */
  isPanel?: boolean;
  /** 是否显示分割菜单项（撕裂窗口无面板树，不提供分割）。 */
  allowSplit?: boolean;
  tabs: TabItem[];
  activeTabId: string | null;
  /** 全局已占用视图（含本组/其他面板/撕裂窗口），切换视图菜单据此禁用。 */
  usedViews: ViewKind[];
  /** 删除面板可用性（最后一个面板不可删；撕裂窗口恒可删 = 关闭窗口）。 */
  canDeletePanel: boolean;
  /** 状态指示（画布/表格/笔记保存/冲突等，宿主传入）。 */
  status?: ReactNode;
  /** 视图切换/添加：组内已有该视图 = 激活；否则添加为本组标签。 */
  onPickView: (view: ViewKind) => void;
  onActivate: (tabId: string) => void;
  onCloseTab: (tabId: string) => void;
  /** 关闭文件（画布/笔记/表格，标签保留回到未打开状态）。 */
  onCloseFile: (view: ViewKind) => void;
  /** 切换标签视图（标签右键「切换标签视图」子菜单；视图全局唯一约束由菜单禁用层保证）。 */
  onSetTabView: (tabId: string, view: ViewKind) => void;
  /** 锁定/解锁整个面板（整块锁定，所有标签一起；锁定 = 不可移动标签/不可删除面板/不可关闭标签）。 */
  onTogglePanelLock: () => void;
  /** 左右/上下分割（仅 allowSplit 时使用；撕裂窗口无面板树不传）。 */
  onSplit?: (direction: SplitDirection) => void;
  /** 删除面板（整块删除含标签；撕裂窗口 = 关闭整个窗口）。 */
  onDeletePanel: () => void;
  onFocusHost: () => void;
}

/**
 * 视图选择菜单（头部空白右键弹出）：组内已有 = 高亮「本组」（点击激活）；未占用 = 添加；
 * 其他位置占用 = 禁用。
 */
export function ViewPickerMenu({
  x,
  y,
  onClose,
  tabs,
  usedViews,
  kinds,
  onPickView,
}: {
  x: number;
  y: number;
  onClose: () => void;
  tabs: TabItem[];
  usedViews: ViewKind[];
  kinds: string[];
  onPickView: (view: ViewKind) => void;
}) {
  return (
    <MenuShell x={x} y={y} onClose={onClose} widthClass="w-40">
      {kinds.map((v) => {
        const inGroup = tabs.some((t) => t.view === v);
        const occupiedElsewhere = usedViews.includes(v as ViewKind) && !inGroup;
        const meta = viewMetaFor(v);
        return (
          <MenuItem
            key={v}
            disabled={occupiedElsewhere}
            noDisabledCursor
            onClick={() => {
              onPickView(v as ViewKind);
              onClose();
            }}
            className="text-xs"
            style={{ color: inGroup ? "var(--accent)" : undefined }}
            title={occupiedElsewhere ? "该视图已在其他位置（全局唯一）" : meta.label}
          >
            {meta.icon}
            {meta.label}
            {inGroup && <span className="ml-auto text-[10px]">本组</span>}
            {occupiedElsewhere && <span className="ml-auto text-[10px]">已占用</span>}
          </MenuItem>
        );
      })}
    </MenuShell>
  );
}

/** 标签右键菜单的钻入子面板。 */
type TabMenuPane = "root" | "views";

export const PanelTabBar = memo(function PanelTabBar({
  hostId,
  isPanel = false,
  allowSplit = true,
  tabs,
  activeTabId,
  usedViews,
  canDeletePanel,
  status,
  onPickView,
  onActivate,
  onCloseTab,
  onCloseFile,
  onSetTabView,
  onTogglePanelLock,
  onSplit,
  onDeletePanel,
  onFocusHost,
}: PanelTabBarProps) {
  const activeTab = tabs.find((t) => t.id === activeTabId) ?? tabs[0] ?? null;

  /** 面板是否整体锁定（所有标签 locked；空面板无标签视为未锁定）。 */
  const panelLocked = tabs.length > 0 && tabs.every((t) => t.locked);

  const currentCanvasFile = useAppStore((s) => s.currentCanvasFile);
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const currentTableFile = useAppStore((s) => s.currentTableFile);

  // 视图候选 = 内建 + 插件面板（订阅插件 UI 注册变化；插件视图 label/icon 走 viewMetaFor 兜底）
  usePluginStore((s) => s.uiRevision);
  const viewKinds: string[] = usePluginStore.getState().pluginViewKinds();

  /** 标签标题：画布/笔记/表格打开文件时显示文件名，否则视图名。 */
  const tabTitle = (tab: TabItem): string => {
    if (tab.view === "canvas") {
      return currentCanvasFile ? noteTitleFromFile(currentCanvasFile) : VIEW_META.canvas.label;
    }
    if (tab.view === "note") {
      return currentNoteFile ? noteTitleFromFile(currentNoteFile) : VIEW_META.note.label;
    }
    if (tab.view === "table") {
      return currentTableFile ? tableTitleFromFile(currentTableFile) : VIEW_META.table.label;
    }
    return viewMetaFor(tab.view).label;
  };

  /** 文件视图当前是否打开文件（X 关闭按钮仅打开时显示）。 */
  const hasFile = (tab: TabItem): boolean => {
    if (tab.view === "canvas") return !!currentCanvasFile;
    if (tab.view === "note") return !!currentNoteFile;
    if (tab.view === "table") return !!currentTableFile;
    return false;
  };

  // ---------- 拖拽源（pointer 按下 → 候选 → 超阈值转正为拖拽会话） ----------
  const handlePointerDown = (e: React.PointerEvent, tab: TabItem) => {
    e.stopPropagation();
    if (tab.locked) return;
    // 捕获：指针离开标签按钮（含拖出窗口）后事件仍送达本按钮；配合 OS 按下期间隐式捕获
    e.currentTarget.setPointerCapture(e.pointerId);
    usePanelStore.getState().beginDragCandidate(tab, hostId, e.clientX, e.clientY);
  };
  const handlePointerMove = (e: React.PointerEvent) => {
    const ps = usePanelStore.getState();
    if (ps.dragActive) ps.updateDrag(e.clientX, e.clientY);
    else ps.moveDragCandidate(e.clientX, e.clientY);
  };
  const handlePointerUp = (e: React.PointerEvent, tab: TabItem) => {
    const ps = usePanelStore.getState();
    const hadDrag = ps.dragActive;
    if (hadDrag) {
      ps.finishDrag(e.clientX, e.clientY, false);
    } else if (ps.dragCandidate) {
      ps.cancelDrag();
      onActivate(tab.id);
    } else {
      onActivate(tab.id);
    }
  };

  // ---------- 标签右键菜单（关闭 + 切换标签视图子菜单） ----------
  const [tabMenu, setTabMenu] = useState<{
    tabId: string;
    x: number;
    y: number;
    pane: TabMenuPane;
  } | null>(null);
  const menuTab = tabMenu ? tabs.find((t) => t.id === tabMenu.tabId) ?? null : null;

  // ---------- 空白右键视图切换菜单（添加视图） ----------
  const [viewMenu, setViewMenu] = useState<{ x: number; y: number } | null>(null);

  // ---------- ≡ 菜单 ----------
  const menuTriggerRef = useRef<HTMLButtonElement>(null);
  const menu = usePopupAnchor(menuTriggerRef);

  // ---------- tab 排序 drop 指示（本标签条为命中目标时显示插入位） ----------
  const dropTarget = usePanelStore((s) => s.dropTarget);
  const isTabTarget =
    dropTarget?.zone === "tab" && (isPanel ? dropTarget.window === hostId : dropTarget.window === "main");
  const insertIndex = isTabTarget ? (dropTarget.tabIndex ?? tabs.length) : -1;

  return (
    <div
      className="h-7 flex items-center gap-1 px-1.5 border-b flex-shrink-0 select-none min-w-0"
      style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
      onClick={onFocusHost}
      onContextMenu={(e) => {
        // 空白处右键 = 视图选择菜单（标签上的右键已被 button 拦截，不达此处）
        e.preventDefault();
        e.stopPropagation();
        setViewMenu({ x: e.clientX, y: e.clientY });
      }}
      data-panel-tabbar={isPanel ? "" : undefined}
    >
      {/* 标签组（拖拽源；锁定标签显示锁标记） */}
      <div className="flex items-stretch gap-0.5 flex-1 min-w-0 overflow-x-auto no-scrollbar">
        {tabs.map((tab, i) => (
          <div key={tab.id} className="flex items-center flex-shrink-0">
            {i === insertIndex && <InsertionMarker />}
            <button
              data-tab-id={tab.id}
              onPointerDown={(e) => handlePointerDown(e, tab)}
              onPointerMove={handlePointerMove}
              onPointerUp={(e) => handlePointerUp(e, tab)}
              onPointerCancel={() => usePanelStore.getState().cancelDrag()}
              onContextMenu={(e) => {
                // 标签右键 = 标签菜单（关闭 + 切换标签视图）；阻止冒泡到空白右键
                e.preventDefault();
                e.stopPropagation();
                // 锁定标签：右键菜单无对应选项（不可关闭/切换），整块锁定后每个标签均如此
                if (tab.locked) return;
                setViewMenu(null);
                setTabMenu({ tabId: tab.id, x: e.clientX, y: e.clientY, pane: "root" });
              }}
              className="flex items-center gap-1 px-2 py-0.5 rounded-t text-xs border-b-2 transition-colors"
              style={{
                borderColor: tab.id === activeTabId ? "var(--accent)" : "transparent",
                color: tab.id === activeTabId ? "var(--accent)" : "var(--text-secondary)",
                background: tab.id === activeTabId ? "rgba(255,255,255,0.04)" : "transparent",
              }}
              title={`${viewMetaFor(tab.view).label}${tab.locked ? "（已锁定：不可移动/关闭/删除面板）" : ""}`}
            >
              {viewMetaFor(tab.view).icon}
              <span className="max-w-[120px] truncate">{tabTitle(tab)}</span>
              {tab.locked && <Lock size={9} className="flex-shrink-0" />}
              {!tab.locked && hasFile(tab) && (
                <span
                  role="button"
                  aria-label="关闭文件"
                  title="关闭文件（标签保留，回到未打开状态）"
                  className="flex-shrink-0 p-0.5 rounded hover:opacity-70"
                  style={{ color: "var(--text-muted)" }}
                  onClick={(e) => {
                    e.stopPropagation();
                    onCloseFile(tab.view);
                  }}
                  onPointerDown={(e) => e.stopPropagation()}
                  onPointerMove={(e) => e.stopPropagation()}
                  onPointerUp={(e) => e.stopPropagation()}
                  onPointerCancel={(e) => e.stopPropagation()}
                >
                  <X size={11} />
                </span>
              )}
            </button>
          </div>
        ))}
        {tabs.length > 0 && insertIndex === tabs.length && <InsertionMarker />}
      </div>

      {/* 状态指示 */}
      {status}
      {/* 插件贡献区：面板头状态区（list 槽，priority 降序） */}
      <SlotListMount slot="panelhead/status" />

      {/* ≡ 菜单（右侧；锁定 + 删除面板 + 分割） */}
      <div className="flex-shrink-0">
        <button
          ref={menuTriggerRef}
          onClick={(e) => {
            e.stopPropagation();
            menu.toggle();
          }}
          className="flex-shrink-0 px-1.5 py-0.5 rounded hover:opacity-70"
          style={{ color: "var(--text-muted)" }}
          title={activeTab ? `面板菜单（${viewMetaFor(activeTab.view).label}）` : "面板菜单"}
          aria-label="面板菜单"
        >
          <Menu size={13} />
        </button>
        <PopupLayer
          anchor={menu.anchor}
          onClose={menu.close}
          triggerRef={menuTriggerRef}
          widthClass="w-40"
          repositionDeps={[activeTabId, tabs.length]}
        >
          {activeTab && (
            <MenuItem
              onClick={() => {
                onTogglePanelLock();
                menu.close();
              }}
              className="text-xs"
            >
              {panelLocked ? <LockOpen size={13} /> : <Lock size={13} />}
              {panelLocked ? "解锁" : "锁定"}
            </MenuItem>
          )}
          {!panelLocked && (
            <>
              <MenuItem
                danger
                disabled={!canDeletePanel}
                onClick={() => {
                  onDeletePanel();
                  menu.close();
                }}
                className="text-xs"
                title={!canDeletePanel ? "最后一个面板不可删除" : "删除整个面板（含其全部标签）"}
              >
                删除面板
              </MenuItem>
              {allowSplit && (
                <>
                  <MenuDivider />
                  <MenuItem
                    onClick={() => {
                      onSplit?.("horizontal");
                      menu.close();
                    }}
                    className="text-xs"
                  >
                    左右分割
                  </MenuItem>
                  <MenuItem
                    onClick={() => {
                      onSplit?.("vertical");
                      menu.close();
                    }}
                    className="text-xs"
                  >
                    上下分割
                  </MenuItem>
                </>
              )}
            </>
          )}
        </PopupLayer>
      </div>

      {/* 标签右键菜单（关闭 + 切换标签视图子菜单） */}
      {tabMenu && menuTab && (
        <MenuShell
          x={tabMenu.x}
          y={tabMenu.y}
          onClose={() => setTabMenu(null)}
          widthClass="w-44"
          repositionDeps={[tabMenu.pane, menuTab.id]}
        >
          {tabMenu.pane === "root" ? (
            <>
              <MenuItem
                disabled={menuTab.locked}
                onClick={() => {
                  onCloseTab(menuTab.id);
                  setTabMenu(null);
                }}
                className="text-xs"
                title={menuTab.locked ? "已锁定，需先解锁" : "关闭该标签（最后一个关闭后面板留空）"}
              >
                <X size={13} />
                关闭该标签
              </MenuItem>
              <MenuItem
                disabled={menuTab.locked}
                onClick={() => setTabMenu((m) => (m ? { ...m, pane: "views" } : m))}
                className="text-xs"
                title={menuTab.locked ? "已锁定，需先解锁" : "切换标签视图"}
              >
                <ChevronRight size={13} />
                切换标签视图
              </MenuItem>
            </>
          ) : (
            <>
              <MenuItem
                onClick={() => setTabMenu((m) => (m ? { ...m, pane: "root" } : m))}
                className="text-xs"
              >
                <ChevronLeft size={13} />
                返回
              </MenuItem>
              <MenuDivider />
              {viewKinds.map((v) => {
                const isCurrent = v === menuTab.view;
                const occupiedElsewhere = usedViews.includes(v as ViewKind) && !isCurrent;
                const meta = viewMetaFor(v);
                return (
                  <MenuItem
                    key={v}
                    disabled={isCurrent || occupiedElsewhere}
                    noDisabledCursor
                    onClick={() => {
                      onSetTabView(menuTab.id, v as ViewKind);
                      setTabMenu(null);
                    }}
                    className="text-xs"
                    style={{ color: isCurrent ? "var(--accent)" : undefined }}
                    title={occupiedElsewhere ? "该视图已在其他位置（全局唯一）" : meta.label}
                  >
                    {meta.icon}
                    {meta.label}
                    {isCurrent && <span className="ml-auto text-[10px]">当前</span>}
                    {occupiedElsewhere && <span className="ml-auto text-[10px]">已占用</span>}
                  </MenuItem>
                );
              })}
            </>
          )}
        </MenuShell>
      )}

      {/* 空白右键视图切换菜单（添加视图） */}
      {viewMenu && (
        <ViewPickerMenu
          x={viewMenu.x}
          y={viewMenu.y}
          onClose={() => setViewMenu(null)}
          tabs={tabs}
          usedViews={usedViews}
          kinds={viewKinds}
          onPickView={onPickView}
        />
      )}
    </div>
  );
});

/** 标签排序插入位标记。 */
function InsertionMarker() {
  return (
    <span
      className="w-0.5 self-stretch mx-0.5 flex-shrink-0"
      style={{ background: "var(--accent)" }}
    />
  );
}
