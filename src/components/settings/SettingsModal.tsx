/**
 * 设置弹窗（两个作用域共用同一个壳）：
 * - 应用级 `SettingsModal`：标题栏「设置」入口，只含跨仓库共享的 tab（通用/主题/多人协作/插件/关于）
 *   + 插件注册的设置 tab；
 * - 仓库级 `VaultSettingsModal`：文件面板仓库行/空间行右键「仓库设置」入口，只含仓库级 tab
 *   （模型供应商/模型服务/Agent/联网搜索/文件与路径/编辑器），作用于被打开的那个仓库
 *   （读取中的会话态、只读与失败提示见 settingsStore 的设置会话）。
 *
 * 左侧标签栏可折叠；tab 内容条件分派；各 tab 草稿与状态自持（直接订阅 store，取值经编辑目标选择器）。
 */
import {
  AlertTriangle,
  Bot,
  ChevronLeft,
  ChevronRight,
  FolderTree,
  Info,
  Loader2,
  Palette,
  PenLine,
  Puzzle,
  RotateCcw,
  Search,
  Server,
  Settings,
  Sparkles,
  Users,
  X,
  type LucideIcon,
} from "lucide-react";
import { useState, type ComponentType, type ReactNode } from "react";
import { ProviderSettingsSection } from "@/components/settings/ProviderSettingsSection";
import { AgentSettingsSection } from "@/components/settings/AgentSettingsSection";
import { AboutSection } from "@/components/settings/AboutSection";
import { GeneralSettingsTab } from "@/components/settings/tabs/GeneralSettingsTab";
import { ThemeSettingsTab } from "@/components/settings/tabs/ThemeSettingsTab";
import { CollabSettingsTab } from "@/components/settings/tabs/CollabSettingsTab";
import { ModelServicesSettingsTab } from "@/components/settings/tabs/ModelServicesSettingsTab";
import { FilesSettingsTab } from "@/components/settings/tabs/FilesSettingsTab";
import { EditorSettingsTab } from "@/components/settings/tabs/EditorSettingsTab";
import { SearchSettingsTab } from "@/components/settings/tabs/SearchSettingsTab";
import { PluginsSettingsTab } from "@/components/plugins/PluginsSettingsTab";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";
import { usePluginStore } from "@/stores/pluginStore";
import { useSettingsStore, selectVaultSettingsSession } from "@/stores/settingsStore";
import type { VaultSettingsTarget } from "@/types";

type AppTab = "general" | "theme" | "collab" | "plugins" | "about";
type VaultTab = "providers" | "modelServices" | "agents" | "search" | "files" | "editor";

/** 应用级 tab（跨仓库共享，落 global.json）。 */
const APP_TABS: { key: AppTab; label: string; icon: LucideIcon }[] = [
  { key: "general", label: "通用", icon: Settings },
  { key: "theme", label: "主题", icon: Palette },
  { key: "collab", label: "多人协作", icon: Users },
  { key: "plugins", label: "插件", icon: Puzzle },
  { key: "about", label: "关于", icon: Info },
];

/** 仓库级 tab（跟仓库走，落点见 services/metadata 的双源分发）。 */
const VAULT_TABS: { key: VaultTab; label: string; icon: LucideIcon }[] = [
  { key: "providers", label: "模型供应商", icon: Server },
  { key: "modelServices", label: "模型服务", icon: Bot },
  { key: "agents", label: "Agent", icon: Sparkles },
  { key: "search", label: "联网搜索", icon: Search },
  { key: "files", label: "文件与路径", icon: FolderTree },
  { key: "editor", label: "编辑器", icon: PenLine },
];

/** 插件设置项 tab 标识：`pluginId` + `key`（不同插件同名 key 互不冲突，与内置 tab 也不可能相撞）。 */
function pluginTabId(t: { pluginId: string; key: string }): string {
  return `${t.pluginId}:${t.key}`;
}

/** 插件设置项承载（直接渲染注册的组件；插件停用/卸载后显示占位）。 */
function PluginSettingMount({ component: Comp }: { component: ComponentType | undefined }) {
  if (!Comp) {
    return (
      <div className="p-5 text-sm" style={{ color: "var(--text-muted)" }}>
        该插件设置已停用或卸载
      </div>
    );
  }
  return (
    <ErrorBoundary>
      <Comp />
    </ErrorBoundary>
  );
}

/** 设置弹窗外壳：标题栏（标题 + 可选副标题）+ 可选横幅 + 左侧可折叠标签栏 + 内容区。 */
function SettingsShell({
  title,
  subtitle,
  banner,
  tabs,
  tab,
  onTabChange,
  onClose,
  children,
}: {
  title: string;
  subtitle?: string;
  banner?: ReactNode;
  tabs: { key: string; label: string; icon: LucideIcon }[];
  tab: string;
  onTabChange: (key: string) => void;
  onClose: () => void;
  children: ReactNode;
}) {
  /** 左侧标签栏折叠状态（折叠后仅显示图标）。 */
  const [tabsCollapsed, setTabsCollapsed] = useState(false);

  return (
    <div
      className="fixed inset-0 bg-black/60 flex items-center justify-center z-50"
      onClick={onClose}
    >
      <div
        className="rounded-lg w-[840px] h-[80vh] flex flex-col border shadow-2xl"
        style={{
          background: "var(--bg-secondary)",
          borderColor: "var(--border)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <header
          className="px-5 py-3 border-b flex items-center justify-between"
          style={{ borderColor: "var(--border)" }}
        >
          <div className="min-w-0">
            <h2 className="font-semibold truncate" style={{ color: "var(--text-primary)" }}>
              {title}
            </h2>
            {subtitle && (
              <p className="text-xs mt-0.5 truncate" style={{ color: "var(--text-muted)" }}>
                {subtitle}
              </p>
            )}
          </div>
          <button
            onClick={onClose}
            style={{ color: "var(--text-muted)" }}
            className="hover:opacity-80"
          >
            <X size={14} />
          </button>
        </header>

        {banner && (
          <div className="px-5 py-2 border-b text-xs" style={{ borderColor: "var(--border)" }}>
            {banner}
          </div>
        )}

        {/* 左侧 tab 栏（可折叠）+ 右侧内容区 */}
        <div className="flex flex-1 overflow-hidden">
          <aside
            className={`flex flex-col border-r shrink-0 transition-[width] ${tabsCollapsed ? "w-11" : "w-40"}`}
            style={{ borderColor: "var(--border)" }}
          >
            <div className="flex-1 overflow-auto p-2 space-y-1">
              {tabs.map((item) => (
                <button
                  key={item.key}
                  onClick={() => onTabChange(item.key)}
                  title={item.label}
                  className={`w-full flex items-center gap-2 px-2.5 py-2 rounded text-sm transition ${
                    tabsCollapsed ? "justify-center px-0" : ""
                  } ${
                    tab === item.key
                      ? "bg-[var(--accent)] text-[var(--accent-fg)]"
                      : "text-[var(--text-secondary)] hover:bg-[var(--hover)]"
                  }`}
                >
                  <item.icon size={14} className="shrink-0" />
                  {!tabsCollapsed && <span className="truncate">{item.label}</span>}
                </button>
              ))}
            </div>
            <button
              onClick={() => setTabsCollapsed((v) => !v)}
              title={tabsCollapsed ? "展开标签栏" : "折叠标签栏"}
              className={`m-2 flex items-center gap-1 rounded px-2 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--hover)] ${
                tabsCollapsed ? "justify-center" : ""
              }`}
            >
              {tabsCollapsed ? <ChevronRight size={14} /> : <ChevronLeft size={14} />}
              {!tabsCollapsed && "折叠"}
            </button>
          </aside>
          <div className="flex-1 min-w-0 flex flex-col overflow-hidden">{children}</div>
        </div>
      </div>
    </div>
  );
}

/** 应用级设置弹窗（标题栏「设置」入口）。
 * 插件设置 tab 以注册时的键并入左侧栏（注册变化经 uiRevision 刷新）。 */
export function SettingsModal({ onClose, initialTab }: { onClose: () => void; initialTab?: string }) {
  usePluginStore((s) => s.uiRevision);
  const pluginTabs = usePluginStore.getState().pluginSettings();
  const [tab, setTab] = useState<string>(() => {
    const builtinKeys: string[] = APP_TABS.map((t) => t.key);
    if (initialTab && (builtinKeys.includes(initialTab) || pluginTabs.some((t) => pluginTabId(t) === initialTab))) {
      return initialTab;
    }
    return "general";
  });
  /** 当前 tab 对应的插件设置项注册（非内置 tab 即插件 tab；注册已撤销时显示占位）。 */
  const pluginTab = pluginTabs.find((t) => pluginTabId(t) === tab);
  const isPluginTab = !APP_TABS.some((t) => t.key === tab);

  return (
    <SettingsShell
      title="设置"
      tabs={[
        ...APP_TABS,
        ...pluginTabs.map((t) => ({ key: pluginTabId(t), label: t.label, icon: Puzzle as LucideIcon })),
      ]}
      tab={tab}
      onTabChange={setTab}
      onClose={onClose}
    >
      {tab === "general" ? (
        /* ===== 通用：应用级外观（字号/字体/自动恢复/主页/自动更新） ===== */
        <GeneralSettingsTab />
      ) : tab === "theme" ? (
        /* ===== 主题：主题插件选择 + 激活主题的设置项 ===== */
        <ThemeSettingsTab />
      ) : tab === "collab" ? (
        /* ===== 多人协作（应用级） ===== */
        <CollabSettingsTab />
      ) : tab === "about" ? (
        /* ===== 关于面板：Logo + 版本号 + 检查更新 ===== */
        <AboutSection />
      ) : tab === "plugins" ? (
        /* ===== 插件面板：已装插件管理 + 市场浏览 ===== */
        <PluginsSettingsTab />
      ) : isPluginTab ? (
        /* ===== 插件设置项（主线程平面注册） ===== */
        <PluginSettingMount component={pluginTab?.component} />
      ) : (
        <GeneralSettingsTab />
      )}
    </SettingsShell>
  );
}

/** 目标身份的可读描述（本地仓库 = 完整路径；协作空间 = 服务器地址 + 空间 id）。 */
function targetIdentityLabel(target: VaultSettingsTarget): string {
  return target.kind === "local" ? target.root : `${target.serverUrl} · ${target.spaceId}`;
}

/** 仓库设置弹窗：作用于文件面板里被右键的那个仓库（可为未激活的仓库）。
 * 会话为 null = 编辑的就是当前激活仓库（读写的都是激活态，与运行时同一份数据）；
 * 否则读取中/读取失败/只读三态由横幅与内容区明示，写入由 store 侧拒绝并提示。 */
export function VaultSettingsModal({
  target,
  onClose,
}: {
  target: VaultSettingsTarget;
  onClose: () => void;
}) {
  const session = useSettingsStore(selectVaultSettingsSession);
  const reloadSession = useSettingsStore((s) => s.reloadVaultSettingsSession);
  const [tab, setTab] = useState<VaultTab>("providers");

  /** 编辑目标是当前激活仓库（无会话）。 */
  const editingActive = session === null;
  const loading = !editingActive && !session.loaded && !session.error;
  const failed = !editingActive && !session.loaded && !!session.error;

  /** 顶部横幅：非激活目标必显，其余按需（只读 / 配置损坏）。 */
  const banner: ReactNode = (
    <div className="space-y-1">
      {!editingActive && (
        <div className="flex items-center gap-1.5" style={{ color: "var(--text-secondary)" }}>
          <AlertTriangle size={13} className="shrink-0" />
          <span className="min-w-0">
            正在编辑非当前仓库：<span style={{ color: "var(--text-primary)" }}>{target.name}</span>
            （{targetIdentityLabel(target)}）；改动只影响这个仓库，当前工作区不受影响。
          </span>
        </div>
      )}
      {session?.readOnly && (
        <div className="flex items-center gap-1.5" style={{ color: "#f59e0b" }}>
          <AlertTriangle size={13} className="shrink-0" />
          <span className="min-w-0">
            你在该空间内是查看者：可以查看设置，改动不会被保存。
          </span>
        </div>
      )}
      {session?.corruptBackup && (
        <div className="flex items-center gap-1.5" style={{ color: "#f87171" }}>
          <AlertTriangle size={13} className="shrink-0" />
          <span className="min-w-0">
            该仓库配置已损坏，原文备份为 .atelyx/{session.corruptBackup}：供应商、默认模型与 API key
            已重置，需重新配置。
          </span>
        </div>
      )}
    </div>
  );

  return (
    <SettingsShell
      title="仓库设置"
      subtitle={
        editingActive
          ? `当前仓库：${target.name}（${targetIdentityLabel(target)}）`
          : `${target.name}（${targetIdentityLabel(target)}）`
      }
      banner={banner}
      tabs={VAULT_TABS}
      tab={tab}
      onTabChange={(k) => setTab(k as VaultTab)}
      onClose={onClose}
    >
      {loading ? (
        <div className="flex-1 flex flex-col items-center justify-center gap-2">
          <Loader2 size={18} className="animate-spin" style={{ color: "var(--accent)" }} />
          <p className="text-sm" style={{ color: "var(--text-muted)" }}>
            正在读取该仓库的配置…
          </p>
        </div>
      ) : failed ? (
        <div className="flex-1 flex flex-col items-center justify-center gap-3 px-8 text-center">
          <AlertTriangle size={18} style={{ color: "#f87171" }} />
          <p className="text-sm" style={{ color: "var(--text-secondary)" }}>
            该仓库的配置未能读取：{session?.error}
          </p>
          <p className="text-xs" style={{ color: "var(--text-muted)" }}>
            读取失败时不做任何写入（写入会覆盖磁盘上的真实配置）。
          </p>
          <button
            onClick={() => void reloadSession()}
            className="flex items-center gap-1.5 px-3 py-1.5 rounded border text-xs hover:opacity-80"
            style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
          >
            <RotateCcw size={13} />
            重试
          </button>
        </div>
      ) : tab === "providers" ? (
        /* ===== 模型供应商：仓库级供应商管理（多模型 + 测试连通性）+ API key 落盘策略 ===== */
        <ProviderSettingsSection />
      ) : tab === "modelServices" ? (
        /* ===== 模型服务：默认模型与话题自动命名模型 ===== */
        <ModelServicesSettingsTab />
      ) : tab === "agents" ? (
        /* ===== Agent 面板：对话预设（名称 + 系统提示词 + 工具）配置 ===== */
        <AgentSettingsSection />
      ) : tab === "search" ? (
        /* ===== 联网搜索面板 ===== */
        <SearchSettingsTab />
      ) : tab === "files" ? (
        /* ===== 文件与路径面板 ===== */
        <FilesSettingsTab />
      ) : (
        /* ===== 编辑器面板 ===== */
        <EditorSettingsTab />
      )}
    </SettingsShell>
  );
}
