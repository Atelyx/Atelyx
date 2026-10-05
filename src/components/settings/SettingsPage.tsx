/**
 * 设置页（整页视图，不是浮层弹窗）：
 * - 入口 = 标题栏右上角「设置」按钮，打开即顶掉工作区的面板网格（像切布局一样），再点按钮返回。
 * - 左栏分「应用级（全局共享）」与「仓库级（仅当前仓库/空间）」两组，仓库级分组只在已进入仓库时
 *   出现且固定作用于当前激活仓库；「关于」不属任何分组，作为底部独立入口。
 * - 内容区统一页面头（标题 + 作用域说明 + 作用域徽标）+ tab 内容；各 tab 草稿与状态自持。
 *
 * 左侧标签栏可折叠；窄屏 = 整屏 + 导航抽屉；tab 内容条件分派。
 */
import {
  ArrowLeft,
  Bot,
  ChevronLeft,
  ChevronRight,
  FolderTree,
  Info,
  Keyboard,
  Palette,
  PanelsTopLeft,
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
import { useState, type ComponentType, type ReactNode } from "react";
import { IconButton } from "@/components/common/Button";
import { ProviderSettingsSection } from "@/components/settings/ProviderSettingsSection";
import { AgentSettingsSection } from "@/components/settings/AgentSettingsSection";
import { AboutSection } from "@/components/settings/AboutSection";
import { GeneralSettingsTab } from "@/components/settings/tabs/GeneralSettingsTab";
import { ThemeSettingsTab } from "@/components/settings/tabs/ThemeSettingsTab";
import { CollabSettingsTab } from "@/components/settings/tabs/CollabSettingsTab";
import { ModelServicesSettingsTab } from "@/components/settings/tabs/ModelServicesSettingsTab";
import { FilesSettingsTab } from "@/components/settings/tabs/FilesSettingsTab";
import { WorkspaceSettingsTab } from "@/components/settings/tabs/WorkspaceSettingsTab";
import { EditorSettingsTab } from "@/components/settings/tabs/EditorSettingsTab";
import { EditorPreferencesTab } from "@/components/settings/tabs/EditorPreferencesTab";
import { ShortcutSettingsTab } from "@/components/settings/tabs/ShortcutSettingsTab";
import { SearchSettingsTab } from "@/components/settings/tabs/SearchSettingsTab";
import { PluginsSettingsTab } from "@/components/plugins/PluginsSettingsTab";
import { ErrorBoundary } from "@/components/common/ErrorBoundary";
import { MobileNavDrawer, type MobileNavItem } from "@/components/layout/MobileNavDrawer";
import { useMediaQuery, NARROW_QUERY } from "@/hooks/useMediaQuery";
import { useBackHandler } from "@/hooks/useBackHandler";
import { useAppStore } from "@/stores/appStore";
import { usePluginStore } from "@/stores/pluginStore";

/** 应用级 tab key（`editorPrefs` 不用 `editor`：仓库级同名 tab 与之并存于同一左栏，key 必须唯一）。 */
type AppTab = "general" | "theme" | "collab" | "editorPrefs" | "shortcuts" | "plugins" | "about";
type VaultTab = "providers" | "modelServices" | "agents" | "search" | "files" | "workspace" | "editor";

/** 应用级 tab（跨仓库共享，落 global.json）。 */
const APP_TABS: { key: AppTab; label: string; icon: LucideIcon }[] = [
  { key: "general", label: "通用", icon: Settings },
  { key: "theme", label: "主题", icon: Palette },
  { key: "collab", label: "多人协作", icon: Users },
  { key: "editorPrefs", label: "编辑器", icon: PenLine },
  { key: "shortcuts", label: "快捷键", icon: Keyboard },
  { key: "plugins", label: "插件", icon: Puzzle },
];

/** 不属任何分组的底部入口（关于页不归属设置范围内，固定落在左栏底部）。 */
const BOTTOM_TABS: { key: AppTab; label: string; icon: LucideIcon }[] = [
  { key: "about", label: "关于", icon: Info },
];

/** 仓库级 tab（跟仓库走，落点见 services/metadata 的双源分发）。 */
const VAULT_TABS: { key: VaultTab; label: string; icon: LucideIcon }[] = [
  { key: "providers", label: "模型供应商", icon: Server },
  { key: "modelServices", label: "模型服务", icon: Bot },
  { key: "agents", label: "Agent", icon: Sparkles },
  { key: "search", label: "联网搜索", icon: Search },
  { key: "files", label: "文件与路径", icon: FolderTree },
  { key: "workspace", label: "工作区", icon: PanelsTopLeft },
  { key: "editor", label: "编辑器", icon: PenLine },
];

/** 全部内置 tab（初始 tab 校验与「是否插件 tab」判定共用）。 */
const BUILTIN_TABS = [...APP_TABS, ...VAULT_TABS, ...BOTTOM_TABS];

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

/** 左栏标签项。 */
type SettingsTab = { key: string; label: string; icon: LucideIcon };

/** 左栏分组：标题 + 作用域（决定分组标题配色）+ 组内标签；用于区分「应用级 / 仓库级」。 */
type SettingsNavGroup = {
  title: string;
  /** 标题右侧的等宽附注（如「全局」或目标仓库名）。 */
  hint?: string;
  scope: "app" | "vault";
  tabs: SettingsTab[];
};

/** 设置页外壳：左侧分组标签栏（含底部独立入口）+ 内容区（页面头 + tab 内容）。 */
function SettingsShell({
  title,
  groups,
  bottomTabs = [],
  page,
  tab,
  onTabChange,
  onClose,
  children,
}: {
  title: string;
  groups: SettingsNavGroup[];
  /** 不属任何分组、固定在左栏底部的入口（关于）。 */
  bottomTabs?: SettingsTab[];
  /** 内容区页面头：当前 tab 的标题 + 作用域说明 + 作用域徽标。 */
  page: { title: string; description: string; scope: "app" | "vault"; scopeLabel: string };
  tab: string;
  onTabChange: (key: string) => void;
  onClose: () => void;
  children: ReactNode;
}) {
  /** 左侧标签栏折叠状态（折叠后仅显示图标）。 */
  const [tabsCollapsed, setTabsCollapsed] = useState(false);
  const narrow = useMediaQuery(NARROW_QUERY);
  useBackHandler(true, () => {
    onClose();
    return true;
  });

  /** 窄屏导航抽屉条目（图标尺寸与宽屏 tab 栏一致，由抽屉自行排布）；抽屉不分组，底部入口排最后。 */
  const navItems: MobileNavItem[] = [...groups.flatMap((group) => group.tabs), ...bottomTabs].map(
    (item) => ({
      key: item.key,
      label: item.label,
      icon: <item.icon size={16} />,
    }),
  );

  /** 左栏标签按钮（分组内与底部入口共用同一形态）。 */
  const tabButton = (item: SettingsTab) => (
    <button
      key={item.key}
      onClick={() => onTabChange(item.key)}
      title={item.label}
      className={`w-full flex items-center gap-2 px-2.5 py-1.5 rounded-[var(--radius-sm)] text-xs transition ${
        tabsCollapsed ? "justify-center px-0" : ""
      } ${
        tab === item.key
          ? "bg-[var(--accent-soft)] text-[var(--accent)] font-medium"
          : "text-[var(--text-secondary)] hover:bg-[var(--hover)]"
      }`}
    >
      <item.icon size={14} className="shrink-0" />
      {!tabsCollapsed && <span className="truncate">{item.label}</span>}
    </button>
  );

  /** 内容列（页面头 + tab 内容）：窄屏/宽屏两分支共用。 */
  const content = (
    <div className="flex-1 min-w-0 flex flex-col overflow-hidden">
      <div
        className="px-5 py-3 border-b flex items-start gap-3 shrink-0"
        style={{ borderColor: "var(--border-subtle)" }}
      >
        <div className="min-w-0">
          <h1 className="text-h2 font-semibold truncate" style={{ color: "var(--text-primary)" }}>
            {page.title}
          </h1>
          <p className="text-xs mt-0.5" style={{ color: "var(--text-muted)" }}>
            {page.description}
          </p>
        </div>
        {/* 作用域徽标：与左栏分组标题同一口径，扫一眼即知这一页改的是谁 */}
        <span
          className="ml-auto shrink-0 inline-flex items-center h-5 px-2 rounded-full text-micro"
          style={
            page.scope === "vault"
              ? {
                  color: "var(--accent)",
                  background: "var(--accent-soft)",
                  border: "1px solid color-mix(in srgb, var(--accent) 32%, transparent)",
                }
              : {
                  color: "var(--text-secondary)",
                  background: "var(--bg-tertiary)",
                  border: "1px solid var(--border)",
                }
          }
        >
          {page.scopeLabel}
        </span>
      </div>
      {children}
    </div>
  );

  // 窄屏 = 整屏（带返回头的导航抽屉）；宽屏 = 工作区内容区整页（标题与关闭在左栏顶部）
  return (
    <div
      className={narrow ? "fixed inset-0 z-50 flex flex-col" : "h-full min-h-0 flex"}
      style={{
        background: "var(--bg-primary)",
        paddingBottom: narrow ? "env(safe-area-inset-bottom)" : undefined,
      }}
    >
      {narrow && (
        <div
          className="flex items-center gap-2 px-2 min-h-12 shrink-0 border-b"
          style={{
            borderColor: "var(--border)",
            background: "var(--bg-secondary)",
            paddingTop: "env(safe-area-inset-top)",
          }}
        >
          <IconButton
            icon={<ArrowLeft size={16} />}
            label="返回工作区"
            size="touch"
            className="shrink-0"
            onClick={onClose}
          />
          <span className="text-sm font-semibold truncate" style={{ color: "var(--text-primary)" }}>
            {title}
          </span>
        </div>
      )}

      {narrow ? (
        <div className="flex-1 min-h-0 flex">
          <MobileNavDrawer items={navItems} active={tab} onSelect={onTabChange} />
          {content}
        </div>
      ) : (
        <div className="flex flex-1 min-h-0 overflow-hidden">
          {/* 左栏宽度按内容区比例自适应（窗口越宽它越宽），并夹在可读区间内：
              太窄会把「仓库级」分组标题里的仓库名截断，太宽则挤占设置内容 */}
          <aside
            className={`flex flex-col border-r shrink-0 transition-[width] ${
              tabsCollapsed ? "w-11" : "w-[20%] min-w-[180px] max-w-[280px]"
            }`}
            style={{ borderColor: "var(--border)" }}
          >
            {/* 页面标题与关闭：设置是整页视图（不是浮层），返回入口放在左栏顶部 */}
            <div className={`flex items-center gap-2 shrink-0 ${tabsCollapsed ? "justify-center py-3" : "px-3 py-3"}`}>
              {tabsCollapsed ? (
                <IconButton
                  icon={<ArrowLeft size={15} />}
                  label="返回工作区"
                  size="md"
                  onClick={onClose}
                />
              ) : (
                <>
                  <span className="text-sm font-semibold truncate" style={{ color: "var(--text-primary)" }}>
                    {title}
                  </span>
                  <IconButton
                    icon={<X size={14} />}
                    label="返回工作区"
                    onClick={onClose}
                    className="ml-auto shrink-0"
                  />
                </>
              )}
            </div>
            <div className="flex-1 overflow-auto p-2">
              {groups.map((group, gi) => (
                <div
                  key={group.title}
                  className={gi > 0 ? "mt-2 pt-3 border-t" : ""}
                  style={gi > 0 ? { borderColor: "var(--border-subtle)" } : undefined}
                >
                  {/* 分组标题：区分「应用级 / 仓库级」；仓库级用金色，应用级用中性色 */}
                  {!tabsCollapsed && (
                    <div
                      className="flex items-center gap-2 px-2 pb-2 text-micro font-semibold tracking-wide"
                      style={{ color: group.scope === "vault" ? "var(--accent)" : "var(--text-muted)" }}
                    >
                      <span
                        className="w-[3px] h-[11px] rounded-full shrink-0"
                        style={{
                          background: group.scope === "vault" ? "var(--accent)" : "var(--border-strong)",
                        }
          }
                      />
                      <span className="truncate">{group.title}</span>
                      {group.hint && (
                        <span
                          className="ml-auto truncate font-mono text-micro font-normal tracking-normal"
                          style={{ color: "var(--text-muted)" }}
                        >
                          {group.hint}
                        </span>
                      )}
                    </div>
                  )}
                  <div className="space-y-1">{group.tabs.map(tabButton)}</div>
                </div>
              ))}
            </div>
            {/* 底部独立入口（关于）：不属任何分组，固定在左栏底部不随分组滚动 */}
            {bottomTabs.length > 0 && (
              <div
                className="shrink-0 px-2 pt-2 border-t"
                style={{ borderColor: "var(--border-subtle)" }}
              >
                {bottomTabs.map(tabButton)}
              </div>
            )}
            <button
              onClick={() => setTabsCollapsed((v) => !v)}
              title={tabsCollapsed ? "展开标签栏" : "折叠标签栏"}
              className={`m-2 flex items-center gap-1 rounded-[var(--radius-sm)] px-2 py-1.5 text-xs text-[var(--text-secondary)] hover:bg-[var(--hover)] ${
                tabsCollapsed ? "justify-center" : ""
              }`}
            >
              {tabsCollapsed ? <ChevronRight size={14} /> : <ChevronLeft size={14} />}
              {!tabsCollapsed && "折叠"}
            </button>
          </aside>
          {content}
        </div>
      )}
    </div>
  );
}

/** 设置页（标题栏右上角「设置」入口，整页视图）：应用级 + 当前仓库的仓库级同页。
 * 插件设置 tab 以注册时的键并入应用级组（注册变化经 uiRevision 刷新）。 */
export function SettingsPage({ onClose, initialTab }: { onClose: () => void; initialTab?: string }) {
  usePluginStore((s) => s.uiRevision);
  const pluginTabs = usePluginStore.getState().pluginSettings();
  // 仓库级分组只在已进入仓库时出现；作用域固定为当前激活仓库/空间
  const vaultIdentity = useAppStore((s) => s.vaultIdentity);
  const vaultName = useAppStore((s) => s.vaultName);
  // 安卓无物理键盘、全局热键无 OS 支持：快捷键 tab 不进移动端导航（key 保留在 BUILTIN_TABS 供校验）
  const isAndroid = useAppStore((s) => s.platform.isAndroid);
  const appTabs = isAndroid ? APP_TABS.filter((t) => t.key !== "shortcuts") : APP_TABS;
  const [tab, setTab] = useState<string>(() => {
    const builtinKeys: string[] = BUILTIN_TABS.map((t) => t.key);
    if (initialTab && (builtinKeys.includes(initialTab) || pluginTabs.some((t) => pluginTabId(t) === initialTab))) {
      return initialTab;
    }
    return "general";
  });
  /** 当前生效 tab：停在仓库级 tab 时仓库消失（空间会话失效等）→ 落回应用级「通用」，
   *  否则会渲染出一个没有导航项、也无分组归属的孤立页。 */
  const vaultTab = VAULT_TABS.find((t) => t.key === tab);
  const activeTab = vaultTab && !vaultIdentity ? "general" : tab;
  /** 当前 tab 对应的插件设置项注册（非内置 tab 即插件 tab；注册已撤销时显示占位）。 */
  const pluginTab = pluginTabs.find((t) => pluginTabId(t) === activeTab);
  const isPluginTab = !BUILTIN_TABS.some((t) => t.key === activeTab);
  /** 当前 tab 的作用域（决定页面头说明与徽标；插件 tab 归应用级）。 */
  const activeVaultTab = VAULT_TABS.find((t) => t.key === activeTab);
  const scope: "app" | "vault" = activeVaultTab ? "vault" : "app";
  const pageTitle =
    activeVaultTab?.label ??
    [...APP_TABS, ...BOTTOM_TABS].find((t) => t.key === activeTab)?.label ??
    pluginTab?.label ??
    "设置";
  const pageDescription =
    scope === "vault"
      ? `作用于当前仓库（${vaultName || "未进入仓库"}）· 配置随该仓库走，不随其他仓库生效`
      : "全库共享 · 存于本机应用配置，跨仓库生效";

  return (
    <SettingsShell
      title="设置"
      groups={[
        {
          title: "应用级",
          hint: "全局共享",
          scope: "app",
          tabs: [
            ...appTabs,
            ...pluginTabs.map((t) => ({ key: pluginTabId(t), label: t.label, icon: Puzzle as LucideIcon })),
          ],
        },
        ...(vaultIdentity
          ? [
              {
                title: "仓库级",
                hint: `仅「${vaultName}」`,
                scope: "vault" as const,
                tabs: VAULT_TABS,
              },
            ]
          : []),
      ]}
      bottomTabs={BOTTOM_TABS}
      page={{ title: pageTitle, description: pageDescription, scope, scopeLabel: scope === "vault" ? "仓库级" : "应用级" }}
      tab={activeTab}
      onTabChange={setTab}
      onClose={onClose}
    >
      {activeTab === "general" ? (
        /* ===== 通用：应用级外观（字号/字体/自动恢复/主页/自动检查更新） ===== */
        <GeneralSettingsTab />
      ) : tab === "theme" ? (
        /* ===== 主题：主题插件选择 + 激活主题的设置项 ===== */
        <ThemeSettingsTab />
      ) : tab === "collab" ? (
        /* ===== 多人协作（应用级） ===== */
        <CollabSettingsTab />
      ) : tab === "editorPrefs" ? (
        /* ===== 编辑器：应用级显示偏好（宽松换行/页面内标题/正文行宽） ===== */
        <EditorPreferencesTab />
      ) : tab === "shortcuts" ? (
        /* ===== 快捷键：应用内命令 + 全局热键（OS 级） ===== */
        <ShortcutSettingsTab />
      ) : tab === "about" ? (
        /* ===== 关于面板：Logo + 版本号 + 检查更新 ===== */
        <AboutSection />
      ) : tab === "plugins" ? (
        /* ===== 插件面板：已装插件管理 + 市场浏览 ===== */
        <PluginsSettingsTab />
      ) : tab === "providers" ? (
        /* ===== 模型供应商：当前仓库的供应商管理（多模型 + 测试连通性）+ API key 落盘策略 ===== */
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
      ) : tab === "workspace" ? (
        /* ===== 工作区面板：启动仓库时自动切换场景 ===== */
        <WorkspaceSettingsTab />
      ) : tab === "editor" ? (
        /* ===== 编辑器（仓库级）：一键重建内部链接 ===== */
        <EditorSettingsTab />
      ) : isPluginTab ? (
        /* ===== 插件设置项（主线程平面注册） ===== */
        <PluginSettingMount component={pluginTab?.component} />
      ) : (
        <GeneralSettingsTab />
      )}
    </SettingsShell>
  );
}

