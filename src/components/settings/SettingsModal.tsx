import {
  Bot,
  ChevronLeft,
  ChevronRight,
  FolderTree,
  Info,
  Palette,
  PenLine,
  Puzzle,
  Search,
  Server,
  Settings,
  Sparkles,
  Users,
  X,
  type LucideIcon,
} from "lucide-react";
import { useState, type ComponentType } from "react";
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

type Tab =
  | "general"
  | "theme"
  | "collab"
  | "providers"
  | "modelServices"
  | "agents"
  | "search"
  | "files"
  | "editor"
  | "plugins"
  | "about";

/** 左侧 tab 栏配置（图标 + 标签；折叠后仅显示图标）。 */
const TAB_ITEMS: { key: Tab; label: string; icon: LucideIcon }[] = [
  { key: "general", label: "通用", icon: Settings },
  { key: "theme", label: "主题", icon: Palette },
  { key: "collab", label: "多人协作", icon: Users },
  { key: "providers", label: "模型供应商", icon: Server },
  { key: "modelServices", label: "模型服务", icon: Bot },
  { key: "agents", label: "Agent", icon: Sparkles },
  { key: "search", label: "联网搜索", icon: Search },
  { key: "files", label: "文件与路径", icon: FolderTree },
  { key: "editor", label: "编辑器", icon: PenLine },
  { key: "plugins", label: "插件", icon: Puzzle },
  { key: "about", label: "关于", icon: Info },
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

/** 设置页壳：左侧可折叠标签栏 + tab 条件分派 + 全局弹窗；各 tab 草稿与状态自持（直接订阅 store）。
 * 插件设置项的 tab 以注册时的键并入左侧栏（注册变化经 uiRevision 刷新）。 */
export function SettingsModal({ onClose, initialTab }: { onClose: () => void; initialTab?: string }) {
  // 设置内容：应用级（通用 / 多人协作 / 关于）+ 仓库级（模型供应商 / 模型服务 / Agent / 联网搜索 / 文件与路径 / 编辑器）+ 插件设置项
  usePluginStore((s) => s.uiRevision);
  const pluginTabs = usePluginStore.getState().pluginSettings();
  const [tab, setTab] = useState<string>(() => {
    const builtinKeys: string[] = TAB_ITEMS.map((t) => t.key);
    if (initialTab && (builtinKeys.includes(initialTab) || pluginTabs.some((t) => pluginTabId(t) === initialTab))) {
      return initialTab;
    }
    return "general";
  });
  /** 当前 tab 对应的插件设置项注册（非内置 tab 即插件 tab；注册已撤销时显示占位）。 */
  const pluginTab = pluginTabs.find((t) => pluginTabId(t) === tab);
  const isPluginTab = !TAB_ITEMS.some((t) => t.key === tab);
  /** 左侧 tab 栏折叠状态（折叠后仅显示图标）。 */
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
          <h2
            className="font-semibold"
            style={{ color: "var(--text-primary)" }}
          >
            设置
          </h2>
          <button
            onClick={onClose}
            style={{ color: "var(--text-muted)" }}
            className="hover:opacity-80"
          >
            <X size={14} />
          </button>
        </header>

        {/* 左侧 tab 栏（可折叠）+ 右侧内容区 */}
        <div className="flex flex-1 overflow-hidden">
          <aside
            className={`flex flex-col border-r shrink-0 transition-[width] ${tabsCollapsed ? "w-11" : "w-40"}`}
            style={{ borderColor: "var(--border)" }}
          >
            <div className="flex-1 overflow-auto p-2 space-y-1">
              {[
                ...TAB_ITEMS,
                ...pluginTabs.map((t) => ({ key: pluginTabId(t), label: t.label, icon: Puzzle as LucideIcon })),
              ].map((item) => (
                <button
                  key={item.key}
                  onClick={() => setTab(item.key)}
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
                  {!tabsCollapsed && (
                    <span className="truncate">{item.label}</span>
                  )}
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
              {tabsCollapsed ? (
                <ChevronRight size={14} />
              ) : (
                <ChevronLeft size={14} />
              )}
              {!tabsCollapsed && "折叠"}
            </button>
          </aside>
          <div className="flex-1 min-w-0 flex flex-col overflow-hidden">
            {tab === "general" ? (
              /* ===== 通用：应用级外观（字号/字体等）+ 仓库级 key 同步 ===== */
              <GeneralSettingsTab />
            ) : tab === "theme" ? (
              /* ===== 主题：主题插件选择 + 激活主题的设置项 ===== */
              <ThemeSettingsTab />
            ) : tab === "collab" ? (
              /* ===== 多人协作（应用级） ===== */
              <CollabSettingsTab />
            ) : tab === "providers" ? (
              /* ===== 模型供应商：仓库级供应商管理（多模型 + 测试连通性） ===== */
              <ProviderSettingsSection />
            ) : tab === "modelServices" ? (
              /* ===== 模型服务：各功能使用的模型默认设置（目前仅对话已实现，其余占位） ===== */
              <ModelServicesSettingsTab />
            ) : tab === "agents" ? (
              /* ===== Agent 面板（仓库级）：对话预设（名称 + 系统提示词 + 工具）配置 ===== */
              <AgentSettingsSection />
            ) : tab === "search" ? (
              /* ===== 联网搜索面板（仓库级） ===== */
              <SearchSettingsTab />
            ) : tab === "files" ? (
              /* ===== 文件与路径面板（仓库级） ===== */
              <FilesSettingsTab />
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
              /* ===== 编辑器面板（仓库级） ===== */
              <EditorSettingsTab />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
