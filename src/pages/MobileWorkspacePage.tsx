/**
 * 移动端单栏工作区：顶栏（仓库名 + 设置入口）+ 单视图 + 底部导航。
 *
 * 视图与桌面同一分派来源（统一视图贡献注册表）：组合行启用才有标签，用户停用行 → 标签消失；
 * 零插件可启动（全部停用时显示空态提示，壳本身可用）。
 * 文件生命周期联动与桌面共用（useWorkspaceFileEffects）；打开文件切到对应视图（启动恢复
 * 发生在挂载前，不劫持「最近打开」启动页）。
 */
import { Settings } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { usePluginStore } from "@/stores/pluginStore";
import { SettingsModal, VaultSettingsModal } from "@/components/settings/SettingsModal";
import { MobileTabBar, type MobileTab } from "@/components/layout/MobileTabBar";
import { ViewHost, viewMetaFor } from "@/components/layout/ViewHost";
import { useWorkspaceFileEffects } from "@/hooks/useWorkspaceFileEffects";

/** 移动端单栏壳的宿主面板 id：表格/画布的聚焦门控按 focusedPanelId === hostId 判定，
 * 单栏壳恒聚焦。 */
const MOBILE_HOST_ID = "mobile";

/** 底部导航固定常用序（首发视图；其余视图含插件面板按 id 追加其后）。 */
const PREFERRED_TAB_ORDER = ["recent", "note", "files", "aichat", "table"];

/** 当前可用视图标签：固定常用序在前，其余（含插件面板）按 id 追加。 */
function buildTabs(): MobileTab[] {
  const kinds = usePluginStore.getState().pluginViewKinds();
  const ordered = [
    ...PREFERRED_TAB_ORDER.filter((k) => kinds.includes(k)),
    ...kinds.filter((k) => !PREFERRED_TAB_ORDER.includes(k)).sort(),
  ];
  return ordered.map((kind) => {
    const meta = viewMetaFor(kind);
    return { kind, label: meta.label, icon: meta.icon };
  });
}

export function MobileWorkspacePage() {
  // 文件生命周期联动（改名跟随/自动恢复/历史署名，桌面与移动端共用）
  useWorkspaceFileEffects();

  // 设置弹窗（与桌面同状态面；顶栏设置入口打开）
  const settingsModal = useAppStore((s) => s.settingsModal);
  const openSettings = useAppStore((s) => s.openSettings);
  const closeSettings = useAppStore((s) => s.closeSettings);
  const vaultSettingsModal = useAppStore((s) => s.vaultSettingsModal);
  const closeVaultSettings = useAppStore((s) => s.closeVaultSettings);

  // 顶栏仓库名（个人仓库 = 目录名；协作空间 = 服务端承载，不落本地路径）
  const vaultIdentity = useAppStore((s) => s.vaultIdentity);
  const vaultName = vaultIdentity === null
    ? "Atelyx"
    : vaultIdentity.kind === "local"
      ? vaultIdentity.root.split(/[\\/]/).filter(Boolean).pop() ?? vaultIdentity.root
      : "协作空间";

  // 可用视图标签：订阅 uiRevision（视图槽增删 = 插件启停/挂载后）触发重渲染，buildTabs 重取注册表
  usePluginStore((s) => s.uiRevision);
  const tabs = buildTabs();

  const [activeView, setActiveView] = useState<string | null>("recent");

  // 激活视图的贡献消失（插件被停用/卸载）→ 落回首个可用标签
  useEffect(() => {
    if (activeView && tabs.some((t) => t.kind === activeView)) return;
    setActiveView(tabs[0]?.kind ?? null);
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

  return (
    <div
      className="h-full w-full flex flex-col"
      style={{ background: "var(--bg-primary)" }}
    >
      {/* 顶栏：仓库名 + 设置入口（设置是核心应用入口，恒宿主渲染） */}
      <div
        className="h-11 flex-shrink-0 flex items-center gap-2 pl-3 pr-1 select-none"
        style={{ background: "var(--bg-secondary)", borderBottom: "1px solid var(--border)" }}
      >
        <span className="text-sm truncate flex-1" style={{ color: "var(--text-primary)" }}>
          {vaultName}
        </span>
        <button
          onClick={() => openSettings()}
          className="w-10 h-10 flex items-center justify-center rounded-md"
          style={{ color: "var(--text-secondary)" }}
          aria-label="设置"
        >
          <Settings size={18} />
        </button>
      </div>

      {/* 单视图承载（统一视图槽分派；缺贡献 = 降级占位，与桌面同语义） */}
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

      <MobileTabBar tabs={tabs} active={activeView} onSelect={setActiveView} />

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
