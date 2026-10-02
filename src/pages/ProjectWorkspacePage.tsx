/**
 * 工作区页面（可自定义布局）。
 *
 * 全局 chrome：标题栏（仓库名 + 布局 tabs + 右操作区（设置/全屏/窗口控制））。
 * 面板网格由 `WorkspaceGrid` 按激活布局渲染，
 * 文件打开/关闭/恢复联动在 `useWorkspaceFileEffects`（跨 store 一致性），视图渲染全在面板内部。
 */
import { Maximize, Settings } from "lucide-react";
import { useEffect, useRef } from "react";
import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { SettingsModal, VaultSettingsModal } from "@/components/settings/SettingsModal";
import { TitleBarControls } from "@/components/common/TitleBarControls";
import { LayoutTabs } from "@/components/layout/LayoutTabs";
import { WorkspaceGrid } from "@/components/layout/WorkspaceGrid";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useWorkspaceFileEffects } from "@/hooks/useWorkspaceFileEffects";
import { HOME_LAYOUT_ID } from "@/types";

export function ProjectWorkspacePage() {
  const toggleFullscreen = useAppStore((s) => s.toggleFullscreen);
  const minimizeWindow = useAppStore((s) => s.minimizeWindow);
  const toggleMaximizeWindow = useAppStore((s) => s.toggleMaximizeWindow);
  const closeWindow = useAppStore((s) => s.closeWindow);

  // 设置弹窗（全局：面板内「前往设置」入口经 openSettings 打开，可指定初始 tab）
  const settingsModal = useAppStore((s) => s.settingsModal);
  const openSettings = useAppStore((s) => s.openSettings);
  const closeSettings = useAppStore((s) => s.closeSettings);
  // 仓库设置弹窗（文件面板仓库行/空间行右键打开；目标可为未激活的仓库）
  const vaultSettingsModal = useAppStore((s) => s.vaultSettingsModal);
  const closeVaultSettings = useAppStore((s) => s.closeVaultSettings);

  // 当前激活仓库身份（「进仓库时打开主页」门控）
  const hasVaultIdentity = useAppStore((s) => s.vaultIdentity !== null);

  // 文件生命周期联动（改名跟随/自动恢复/历史署名，桌面与移动端共用）
  useWorkspaceFileEffects();

  // 当前打开的文件状态与激活布局（面板网格渲染入口）
  const activeLayoutId = useUiStateStore((s) => s.activeLayoutId);
  const activeTree = useUiStateStore((s) => {
    const layout =
      s.workspaceLayouts.find((l) => l.id === s.activeLayoutId) ?? s.workspaceLayouts[0];
    return layout.tree;
  });

  // AI 对话面板会话与表格改动落盘：进仓库读盘 + 切仓库时 flush 防 debounce 丢改动，
  // 均已归入领域生命周期注册表分发（builtin.chatpanel 的 onVaultEntered、各域 flush）

  /** 「进仓库时打开主页」开关：仅在本次运行的首次进仓生效（boot 自动进仓，或从空态创建/进入
   *  第一个仓库）；面板内切换仓库不生效——切换保持当前布局，打断位置违背切换的连续性预期。
   *  依赖 uiLoaded：ui-state 从磁盘加载完成前不得激活——否则随后 load 会用磁盘 activeLayoutId 覆盖。
   *  门控按「有激活仓库身份」——协作空间无本地 root，按 vaultRoot 会让开关在空间内不生效。 */
  const uiLoaded = useUiStateStore((s) => s.loaded);
  const defaultHomeLayout = useSettingsStore((s) => s.defaultHomeLayout);
  const homeAppliedRef = useRef(false);
  useEffect(() => {
    if (homeAppliedRef.current) return;
    if (!defaultHomeLayout || !uiLoaded || !hasVaultIdentity) return;
    homeAppliedRef.current = true;
    const ui = useUiStateStore.getState();
    if (ui.workspaceLayouts.some((l) => l.id === HOME_LAYOUT_ID)) {
      ui.activateLayout(HOME_LAYOUT_ID);
    }
  }, [defaultHomeLayout, hasVaultIdentity, uiLoaded]);

  /** 全屏切换（视图控制图标，经 store 转发到 services）。 */
  const handleToggleFullscreen = () => {
    void toggleFullscreen().catch((e) => {
      console.error("全屏切换失败", e);
    });
  };

  return (
    <div
      className="h-full w-full flex flex-col"
      style={{ background: "var(--bg-primary)" }}
    >
      <div className="flex-1 flex flex-col min-h-0">
        {/* 标题栏横条：仓库名 + 布局 tabs → 右操作区（设置/全屏/窗口控制，常驻） */}
        <div
          className="h-9 flex items-center gap-1 pl-1 pr-1 flex-shrink-0 select-none"
          style={{ background: "var(--bg-secondary)", borderBottom: "1px solid var(--border)" }}
          data-tauri-drag-region
        >
          <LayoutTabs />

            {/* 右操作区（常驻）：设置 + 全屏（ml-auto 贴右缘，窗口控制在其后）。
                设置是核心应用入口，恒宿主渲染、不随插件启停消失；外部插件经 titlebar/right 槽并列贡献 */}
            <div className="ml-auto flex-shrink-0 flex items-center" data-tauri-drag-region>
              {/* 插件贡献区：标题栏右操作区（list 槽，priority 降序；容器避让窗口拖拽） */}
              <span data-tauri-drag-region="false" className="flex items-center">
                <SlotListMount slot="titlebar/right" />
              </span>
              <button
                onClick={(e) => { e.stopPropagation(); openSettings(); }}
                className="w-8 h-8 flex items-center justify-center rounded-sm hover:opacity-80"
                style={{ color: "var(--text-secondary)" }}
                title="设置"
                data-tauri-drag-region="false"
              >
                <Settings size={16} />
              </button>
              <button
                onClick={(e) => { e.stopPropagation(); handleToggleFullscreen(); }}
                className="w-8 h-8 flex items-center justify-center rounded-sm hover:opacity-80"
                style={{ color: "var(--text-secondary)" }}
                title="全屏"
                data-tauri-drag-region="false"
              >
                <Maximize size={16} />
              </button>
            </div>
            <TitleBarControls
              onMinimize={() => void minimizeWindow()}
              onMaximize={() => void toggleMaximizeWindow()}
              onClose={() => void closeWindow()}
            />
          </div>

          {/* 面板网格（激活布局；key 保证切布局整树重挂，defaultSize 恢复各面板比例） */}
          <div className="flex-1 min-h-0">
            <WorkspaceGrid key={activeLayoutId ?? "default"} tree={activeTree} />
          </div>
        </div>

      {settingsModal && (
        <SettingsModal initialTab={settingsModal.tab} onClose={closeSettings} />
      )}
      {vaultSettingsModal && (
        <VaultSettingsModal
          target={vaultSettingsModal.target}
          onClose={closeVaultSettings}
        />
      )}
    </div>
  );
}
