/**
 * 工作区页面（可自定义布局）。
 *
 * 全局 chrome：标题栏（仓库名 + 布局 tabs + 右操作区（设置/全屏/窗口控制））。
 * 面板网格由 `WorkspaceGrid` 按激活布局渲染，
 * 文件打开/关闭/恢复联动在 `useWorkspaceFileEffects`（跨 store 一致性），视图渲染全在面板内部。
 */
import { Maximize, Settings } from "lucide-react";
import { useEffect, useRef } from "react";
import { useAppStore, selectVaultIdentityKey } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { SettingsPage } from "@/components/settings/SettingsPage";
import { TitleBarControls } from "@/components/common/TitleBarControls";
import { IconButton } from "@/components/common/Button";
import { LayoutTabs } from "@/components/layout/LayoutTabs";
import { SceneSwitcher } from "@/components/layout/SceneSwitcher";
import { WorkspaceGrid } from "@/components/layout/WorkspaceGrid";
import { WorkspaceStatusBar } from "@/components/layout/WorkspaceStatusBar";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useWorkspaceFileEffects } from "@/hooks/useWorkspaceFileEffects";
import { resolveEntryScene } from "@/utils/workspaceLayout";

export function ProjectWorkspacePage() {
  const toggleFullscreen = useAppStore((s) => s.toggleFullscreen);
  const minimizeWindow = useAppStore((s) => s.minimizeWindow);
  const toggleMaximizeWindow = useAppStore((s) => s.toggleMaximizeWindow);
  const closeWindow = useAppStore((s) => s.closeWindow);

  // 设置页（应用级 + 当前仓库的仓库级同页；整页视图顶掉面板网格，面板内「前往设置」可指定初始 tab）
  const settingsView = useAppStore((s) => s.settingsView);
  const openSettings = useAppStore((s) => s.openSettings);
  const closeSettings = useAppStore((s) => s.closeSettings);

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

  /** 「启动仓库时自动切换场景」（仓库级，vaultConfig.entrySceneId）：每次进入仓库生效（含面板内切换）。
   *  切场景 = 整组替换面板网格并恢复该场景记忆的激活布局；
   *  指定场景已删除（场景结构与仓库配置独立，悬挂引用常态存在）→ 不切换，保持当前界面。
   *  门控 switchingVaultRoot：切换在途时 vaultConfig 可能还是旧仓库的，收尾后配置才可信；
   *  按仓库身份键（selectVaultIdentityKey）去重，身份离开激活态（键 = "none"）即重置，
   *  A→空态→A 重新生效。依赖 uiLoaded：ui-state 从磁盘加载完成前不得激活——
   *  否则随后 load 会用磁盘状态覆盖。 */
  const uiLoaded = useUiStateStore((s) => s.loaded);
  const switchingVaultRoot = useAppStore((s) => s.switchingVaultRoot);
  const vaultIdentityKey = useAppStore(selectVaultIdentityKey);
  const entrySceneId = useSettingsStore((s) => s.vaultConfig?.entrySceneId ?? null);
  const entryAppliedRef = useRef<string | null>(null);
  useEffect(() => {
    if (vaultIdentityKey === "none") {
      entryAppliedRef.current = null;
      return;
    }
    if (switchingVaultRoot !== null || !uiLoaded) return;
    if (entryAppliedRef.current === vaultIdentityKey) return;
    entryAppliedRef.current = vaultIdentityKey;
    const ui = useUiStateStore.getState();
    const targetSceneId = resolveEntryScene(entrySceneId, ui.scenes);
    if (!targetSceneId || targetSceneId === ui.activeSceneId) return;
    ui.activateScene(targetSceneId);
  }, [vaultIdentityKey, switchingVaultRoot, uiLoaded, entrySceneId]);

  /** 全屏切换（视图控制图标，经 store 转发到 services）。 */
  const handleToggleFullscreen = () => {
    void toggleFullscreen().catch((e) => {
      console.error("全屏切换失败", e);
    });
  };

  return (
    // 不画底色：与 html/body 的底色同值，重画只会让半透明皮肤（极光）多叠一层、氛围底透不上来
    <div className="h-full w-full flex flex-col">
      <div className="flex-1 flex flex-col min-h-0">
        {/* 标题栏横条：仓库名 + 布局 tabs → 右操作区（设置/全屏/窗口控制，常驻） */}
        <div
          className="h-9 flex items-center gap-1 pl-1 pr-1 flex-shrink-0 select-none"
          style={{ background: "var(--bg-secondary)", borderBottom: "1px solid var(--border)" }}
          data-tauri-drag-region
        >
          <LayoutTabs />

            {/* 右操作区（常驻）：场景切换 + 设置 + 全屏（ml-auto 贴右缘，窗口控制在其后）。
                设置是核心应用入口，恒宿主渲染、不随插件启停消失；外部插件经 titlebar/right 槽并列贡献 */}
            <div className="ml-auto flex-shrink-0 flex items-center" data-tauri-drag-region>
              {/* 插件贡献区：标题栏右操作区（list 槽，priority 降序；容器避让窗口拖拽） */}
              <span data-tauri-drag-region="false" className="flex items-center">
                <SlotListMount slot="titlebar/right" />
              </span>
              <SceneSwitcher />
              <IconButton
                onClick={(e) => {
                  e.stopPropagation();
                  if (settingsView) closeSettings();
                  else openSettings();
                }}
                // 设置页打开时按钮保持激活态（与左栏布局 tab 同一口径），再点一次回工作区
                style={{
                  color: settingsView ? "var(--accent)" : "var(--text-secondary)",
                  background: settingsView ? "var(--accent-soft)" : undefined,
                }}
                variant="ghost"
                size="lg"
                icon={<Settings size={16} />}
                label="设置"
                aria-pressed={!!settingsView}
                data-tauri-drag-region="false"
              />
              <IconButton
                onClick={(e) => { e.stopPropagation(); handleToggleFullscreen(); }}
                variant="ghost"
                size="lg"
                icon={<Maximize size={16} />}
                label="全屏"
                data-tauri-drag-region="false"
              />
            </div>
            <TitleBarControls
              onMinimize={() => void minimizeWindow()}
              onMaximize={() => void toggleMaximizeWindow()}
              onClose={() => void closeWindow()}
            />
          </div>

          {/* 内容区：设置页打开时整页顶掉面板网格（像切布局一样），否则渲染激活布局 */}
          <div className="flex-1 min-h-0">
            {settingsView ? (
              <SettingsPage initialTab={settingsView.tab} onClose={closeSettings} />
            ) : (
              <WorkspaceGrid key={activeLayoutId ?? "default"} tree={activeTree} />
            )}
          </div>

          {/* 工作区状态栏：全局/环境信息（只此一处，内容区切换不影响） */}
          <WorkspaceStatusBar />
      </div>
    </div>
  );
}
