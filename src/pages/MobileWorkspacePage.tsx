/**
 * 移动端单栏工作区：顶栏（仓库/空间切换 + 设置入口）+ 底部导航栏（视图切换）+ 单视图。
 * 视图与桌面同一分派来源（统一视图贡献注册表）：组合行启用才有标签，用户停用行 → 标签消失；
 * 零插件可启动（全部停用时显示空态提示）。底栏视图顺序是应用级偏好；文件生命周期联动与桌面
 * 共用（useWorkspaceFileEffects），打开文件切到对应视图（启动恢复不劫持「最近打开」启动页）。
 * 返回键逐层返回在此接线（安卓壳层经 window.__atelyxAndroidBack 调用）；
 * 安卓首次启动渲染存储授权引导（本地仓库要选设备文件夹需先手动开启「所有文件访问权限」）。
 */
import { Settings } from "lucide-react";
import { cloneElement, useEffect, useRef, useState } from "react";
import type { ReactElement } from "react";
import { IconButton } from "@/components/common/Button";
import { useAppStore } from "@/stores/appStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { usePluginStore } from "@/stores/pluginStore";
import { useNotificationStore } from "@/stores/notificationStore";
import { SettingsPage } from "@/components/settings/SettingsPage";
import { MobileBottomBar } from "@/components/layout/MobileBottomBar";
import type { MobileNavItem } from "@/components/layout/MobileNavDrawer";
import { useSettingsStore } from "@/stores/settingsStore";
import { orderMobileViews } from "@/utils/mobileNav";
import { MobileVaultSwitcher } from "@/components/layout/MobileVaultSwitcher";
import { MobileLocalVaultDialog } from "@/components/layout/MobileLocalVaultDialog";
import { ViewHost, viewMetaFor } from "@/components/layout/ViewHost";
import { useWorkspaceFileEffects } from "@/hooks/useWorkspaceFileEffects";
import { runBackHandlers } from "@/utils/mobileBack";
import { installLongPressContextMenu } from "@/utils/longPress";

declare global {
  interface Window {
    /** 安卓返回键入口：壳层注入，返回是否消费本次返回（false = 允许退出应用）。 */
    __atelyxAndroidBack?: () => boolean;
  }
}

/** 移动端单栏壳的宿主面板 id：表格/画布的聚焦门控按 focusedPanelId === hostId 判定，
 * 单栏壳恒聚焦。 */
const MOBILE_HOST_ID = "mobile";

/** 退出确认窗口：顶层第一次按返回提示，窗口内第二次才真正退出。 */
const EXIT_CONFIRM_MS = 2000;

/** 当前可用视图标签：顺序取应用级偏好（设置 → 通用可调；缺省 = 内建常用序），其余视图保持入参顺序（= 组合行顺序，见 pluginViewKinds）。 */
function buildTabs(): MobileNavItem[] {
  const kinds = usePluginStore.getState().pluginViewKinds();
  const ordered = orderMobileViews(kinds, useSettingsStore.getState().mobileNavOrder);
  return ordered.map((kind) => {
    const meta = viewMetaFor(kind);
    // 底栏/抽屉图标按移动端档 16px（VIEW_META 默认 13 是桌面面板标签尺寸）
    const icon = cloneElement(meta.icon as ReactElement<{ size?: number }>, { size: 16 });
    return { key: kind, label: meta.label, icon };
  });
}

export function MobileWorkspacePage() {
  // 文件生命周期联动（改名跟随/自动恢复/历史署名，桌面与移动端共用）
  useWorkspaceFileEffects();

  // 设置页（与桌面同状态面；顶栏设置入口打开，窄屏为整屏视图）
  const settingsView = useAppStore((s) => s.settingsView);
  const openSettings = useAppStore((s) => s.openSettings);
  const closeSettings = useAppStore((s) => s.closeSettings);

  // 顶栏仓库名（个人仓库 = 目录名；协作空间 = 服务端承载，不落本地路径）
  const vaultIdentity = useAppStore((s) => s.vaultIdentity);
  const vaultName = vaultIdentity === null
    ? "Atelyx"
    : vaultIdentity.kind === "local"
      ? vaultIdentity.root.split(/[\\/]/).filter(Boolean).pop() ?? vaultIdentity.root
      : "协作空间";

  // 可用视图标签：订阅 uiRevision（视图槽增删 = 插件启停/挂载后）+ 导航顺序偏好触发重渲染，
  // buildTabs 重取注册表与顺序
  usePluginStore((s) => s.uiRevision);
  useSettingsStore((s) => s.mobileNavOrder);
  const tabs = buildTabs();

  const [activeView, setActiveView] = useState<string | null>("recent");

  // 激活视图的贡献消失（插件被停用/卸载）→ 落回首个可用标签
  useEffect(() => {
    if (activeView && tabs.some((t) => t.key === activeView)) return;
    setActiveView(tabs[0]?.key ?? null);
  }, [tabs, activeView]);

  // 单栏壳常驻聚焦：表格/画布编辑门控按 focusedPanelId === hostId 判定
  const uiLoaded = useUiStateStore((s) => s.loaded);
  useEffect(() => {
    if (!uiLoaded) return;
    useUiStateStore.getState().setFocusedPanel(MOBILE_HOST_ID);
  }, [uiLoaded]);

  // 打开文件 → 切到对应视图（该视图可用才切）。
  // 交互门控：启动时的自动恢复发生在挂载后（异步），不得劫持「最近打开」启动页——
  // 只有用户触碰过界面后的文件打开才跟随切换。
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const currentTableFile = useAppStore((s) => s.currentTableFile);
  const currentCanvasFile = useAppStore((s) => s.currentCanvasFile);
  const interactiveRef = useRef(false);
  useEffect(() => {
    const mark = (): void => {
      interactiveRef.current = true;
    };
    window.addEventListener("pointerdown", mark, { once: true, capture: true });
    return () => window.removeEventListener("pointerdown", mark, { capture: true });
  }, []);
  useEffect(() => {
    if (!interactiveRef.current) return;
    const target = currentNoteFile
      ? "note"
      : currentTableFile
        ? "table"
        : currentCanvasFile
          ? "canvas"
          : null;
    if (target && usePluginStore.getState().pluginViewKinds().includes(target)) {
      setActiveView(target);
    }
  }, [currentNoteFile, currentTableFile, currentCanvasFile]);

  // 全局返回处理器只注册一次：经 ref 读最新视图与主页可用性
  const activeViewRef = useRef(activeView);
  activeViewRef.current = activeView;
  const homeAvailableRef = useRef(false);
  homeAvailableRef.current = tabs.some((t) => t.key === "recent");

  // 返回键逐层返回：浮层/弹窗/侧边栏展开先消费 → 视图内上级（关闭当前文件）→ 回主页 →
  // 顶层二次确认退出（窗口内第二次返回交给壳层结束应用）。
  useEffect(() => {
    let lastExitAt = 0;
    window.__atelyxAndroidBack = () => {
      if (runBackHandlers()) return true;
      const app = useAppStore.getState();
      const view = activeViewRef.current;
      if (view === "note" && app.currentNoteFile) {
        app.closeNote();
        return true;
      }
      if (view === "table" && app.currentTableFile) {
        app.closeTable();
        return true;
      }
      if (view === "canvas" && app.currentCanvasFile) {
        app.closeCanvas();
        return true;
      }
      if (view !== "recent" && homeAvailableRef.current) {
        setActiveView("recent");
        return true;
      }
      const now = Date.now();
      if (now - lastExitAt < EXIT_CONFIRM_MS) return false;
      lastExitAt = now;
      useNotificationStore.getState().notify({ level: "info", message: "再按一次返回键退出" });
      return true;
    };
    return () => {
      delete window.__atelyxAndroidBack;
    };
  }, []);

  // 进后台强制落盘：安卓无「关窗确认」且系统可无通知杀进程，文档不可见即冲刷全部 pending 改动
  useEffect(() => {
    const onVisibility = () => {
      if (document.hidden) void useAppStore.getState().flushAllPending();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => document.removeEventListener("visibilitychange", onVisibility);
  }, []);

  // 触屏长按 → 右键菜单（复用既有 onContextMenu 处理器）
  useEffect(() => installLongPressContextMenu(), []);

  // 首次启动的存储授权引导（安卓：本地仓库需要「所有文件访问权限」，只能引导用户手动开启）
  const isAndroid = useAppStore((s) => s.platform.isAndroid);
  const androidStorageOnboarded = useAppStore((s) => s.androidStorageOnboarded);
  const [showStorageGuide, setShowStorageGuide] = useState(false);
  useEffect(() => {
    if (isAndroid && !androidStorageOnboarded) setShowStorageGuide(true);
  }, [isAndroid, androidStorageOnboarded]);

  return (
    // 不画底色：与 html/body 的底色同值，重画只会让半透明皮肤（极光）多叠一层、氛围底透不上来
    <div className="h-full w-full flex flex-col">
      <div className="flex-1 min-h-0 flex flex-col">
        {/* 顶栏：仓库/空间切换 + 设置入口（设置是核心应用入口，恒宿主渲染） */}
        <div
          className="flex-shrink-0 flex items-center gap-2 px-2 min-h-12 select-none"
          style={{
            background: "var(--bg-secondary)",
            borderBottom: "1px solid var(--border-subtle)",
            paddingTop: "env(safe-area-inset-top)",
          }}
        >
          <MobileVaultSwitcher label={vaultName} />
          <IconButton
            icon={<Settings size={16} />}
            label="设置"
            size="touch"
            onClick={() => openSettings()}
          />
        </div>

        {/* 单视图承载（统一视图槽分派；缺贡献 = 降级占位，与桌面同语义）。
            底部安全区由导航栏承担（不再在此重复叠加） */}
        <div className="flex-1 min-h-0">
          {tabs.length === 0 ? (
            <div
              className="h-full w-full flex items-center justify-center px-8 text-center text-xs leading-relaxed"
              style={{ color: "var(--text-muted)" }}
            >
              暂无启用的视图
              <br />
              可在设置 → 插件中启用或恢复默认组合
            </div>
          ) : activeView ? (
            <ViewHost view={activeView} hostId={MOBILE_HOST_ID} />
          ) : null}
        </div>
      </div>

      {/* 底部导航栏：视图切换（顺序由应用级偏好决定）；零可用视图时不显示 */}
      {tabs.length > 0 && (
        <MobileBottomBar items={tabs} active={activeView} onSelect={setActiveView} />
      )}

      {settingsView && (
        <SettingsPage initialTab={settingsView.tab} onClose={closeSettings} />
      )}
      {showStorageGuide && (
        <MobileLocalVaultDialog
          firstRun
          onClose={() => {
            setShowStorageGuide(false);
            void useAppStore.getState().setAndroidStorageOnboarded(true);
          }}
        />
      )}
    </div>
  );
}
