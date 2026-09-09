/**
 * 设置 → 插件面板：已装插件管理 + 市场浏览。
 *
 * - 已装列表（app + 当前仓库 vault 级）：启停/更新/卸载，展示类型/作用域/来源/版本/运行状态/
 *   披露的能力命名空间（宿主能力显示注册表标签 + 敏感高亮，插件命名空间原样）+ 实际调用审计。
 * - 安装入口：市场安装（git clone）之外，支持「从本地文件夹安装」（junction 实时引用）与
 *   「从 Git 地址安装」（git clone）。
 * - 市场：搜索/筛选/安装（见 MarketplaceSection）。
 * 分层：本组件只经 pluginStore 触达插件能力（不直连 services）。
 */
import { useState } from "react";
import { FolderOpen, GitBranch, RefreshCw, Terminal, Trash2 } from "lucide-react";
import { usePluginStore } from "@/stores/pluginStore";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { MarketplaceSection } from "@/components/plugins/MarketplaceSection";
import { deriveComposition } from "@/utils/pluginComposition";
import { errText } from "@/types";
import { PLUGIN_SCOPE_LABELS, PLUGIN_SOURCE_LABELS, PLUGIN_TYPE_LABELS } from "@/constants/plugins";

type TabMode = "installed" | "market";

export function PluginsSettingsTab() {
  const plugins = usePluginStore((s) => s.plugins);
  // 订阅 UI 注册修订号：插件 UI 平面命令异步注册/卸载变化触发重渲染（命令列表在服务层非响应式）。
  usePluginStore((s) => s.uiRevision);
  const setEnabled = usePluginStore((s) => s.setEnabled);
  const update = usePluginStore((s) => s.update);
  const uninstall = usePluginStore((s) => s.uninstall);
  const installLocalFromPicker = usePluginStore((s) => s.installLocalFromPicker);
  const installGit = usePluginStore((s) => s.installGit);
  const capabilityLabel = usePluginStore((s) => s.capabilityLabel);
  const capabilitySensitive = usePluginStore((s) => s.capabilitySensitive);
  const pluginCommands = usePluginStore((s) => s.pluginCommands);
  const runPluginCommand = usePluginStore((s) => s.runPluginCommand);
  const restoreBuiltin = usePluginStore((s) => s.restoreBuiltin);
  const compositionDefaults = usePluginStore((s) => s.compositionDefaults);

  const [mode, setMode] = useState<TabMode>("installed");
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const [confirmUninstall, setConfirmUninstall] = useState<string | null>(null);
  const [gitUrl, setGitUrl] = useState("");
  const [installing, setInstalling] = useState(false);

  const allRows = Object.values(plugins).sort((a, b) => (a.id < b.id ? -1 : 1));
  const builtinRows = allRows.filter((p) => p.sourceKind === "builtin");
  const installedRows = allRows.filter((p) => p.sourceKind !== "builtin");
  // 装配视图推导：默认集（官方内置插件） × 已装/启用 → 已卸载的默认成员（灰行 + 恢复入口）。
  const installedForComposition: Record<string, { name: string; enabled: boolean }> = {};
  for (const p of allRows) installedForComposition[p.id] = { name: p.manifest.name, enabled: p.enabled };
  const uninstalledDefaults = deriveComposition(compositionDefaults, installedForComposition).filter(
    (r) => r.role === "default" && !r.installed,
  );
  // 命令合并全量一次（UI 平面异步注册经 uiRevision 订阅刷新）。
  const commands = pluginCommands();

  /** 安装统一入口：action 返回 false（如取消目录选择）不算成功、不提示；onOk 成功回调（如清空输入）。 */
  const runInstall = async (
    action: () => Promise<boolean | void>,
    okText: string,
    onOk?: () => void,
  ) => {
    if (installing) return;
    setInstalling(true);
    try {
      const done = await action();
      if (done !== false) {
        setNotice({ kind: "ok", text: okText });
        onOk?.();
      }
    } catch (e) {
      setNotice({ kind: "error", text: errText(e) });
    } finally {
      setInstalling(false);
    }
  };

  return (
    <section className="flex-1 min-h-0 p-5 overflow-y-auto">
      {/* 模式切换：已安装 / 市场 */}
      <div className="flex gap-1 mb-4">
        {(
          [
            { key: "installed", label: "已安装" },
            { key: "market", label: "市场" },
          ] as { key: TabMode; label: string }[]
        ).map((m) => (
          <button
            key={m.key}
            onClick={() => setMode(m.key)}
            className="px-3 py-1.5 rounded text-xs"
            style={
              mode === m.key
                ? { background: "var(--accent)", color: "var(--accent-fg)" }
                : { color: "var(--text-secondary)", background: "transparent" }
            }
          >
            {m.label}
          </button>
        ))}
      </div>

      {mode === "market" ? (
        <MarketplaceSection />
      ) : (
        <>
      {/* 操作反馈（安装/更新/卸载错误等） */}
      {notice && (
        <div
          className="text-xs mb-3 break-words"
          style={{ color: notice.kind === "ok" ? "var(--text-secondary)" : "#f87171" }}
        >
          {notice.text}
        </div>
      )}
      {/* 安装入口：本地文件夹（junction 实时引用）+ git 地址（clone） */}
      <div className="flex items-center gap-2 mb-3 flex-shrink-0">
        <button
          onClick={() =>
            void runInstall(() => installLocalFromPicker(), "已从本地文件夹安装（源目录改动即时生效）")
          }
          disabled={installing}
          title="选择本地插件源码目录，实时引用安装（无拷贝，改源码即时生效）"
          className="flex items-center gap-1.5 px-2.5 py-1.5 rounded border text-xs flex-shrink-0 transition-opacity disabled:opacity-50"
          style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
        >
          <FolderOpen size={13} />
          从本地文件夹安装
        </button>
        <div className="flex items-center gap-1.5 flex-1 min-w-0">
          <GitBranch size={13} className="flex-shrink-0" style={{ color: "var(--text-muted)" }} />
          <input
            value={gitUrl}
            onChange={(e) => setGitUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                void runInstall(() => installGit(gitUrl), "已从 Git 地址安装", () => setGitUrl(""));
              }
            }}
            placeholder="从 Git 地址安装（如 https://github.com/owner/repo）"
            className="flex-1 min-w-0 px-2 py-1.5 rounded border text-xs outline-none"
            style={{ borderColor: "var(--border)", color: "var(--text-primary)", background: "var(--bg-primary)" }}
          />
          <button
            onClick={() => void runInstall(() => installGit(gitUrl), "已从 Git 地址安装", () => setGitUrl(""))}
            disabled={installing || !gitUrl.trim()}
            className="px-2.5 py-1.5 rounded border text-xs flex-shrink-0 transition-opacity disabled:opacity-50"
            style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
          >
            {installing ? "安装中…" : "安装"}
          </button>
        </div>
      </div>
      {/* App 组成（默认装配）：官方默认插件集，默认启用；可停用/卸载调整。
          装配是「默认值层」而非约束——启停只改运行时状态，恢复默认装配只补回已卸载成员、不复活停用。 */}
      <div className="mb-3 flex-shrink-0">
        <div className="flex items-center justify-between mb-1.5">
          <div className="min-w-0">
            <span className="text-[11px] font-medium" style={{ color: "var(--text-muted)" }}>
              内置插件
            </span>
          </div>
          <button
            onClick={() => void restoreBuiltin().catch((e) => setNotice({ kind: "error", text: errText(e) }))}
            className="flex items-center gap-1 text-[11px] px-1.5 py-0.5 rounded hover:bg-[var(--hover)] flex-shrink-0"
            style={{ color: "var(--text-secondary)" }}
            title="补回已卸载的默认插件（不覆盖停用状态）"
          >
            <RefreshCw size={12} />
            恢复默认装配
          </button>
        </div>
        <div
          className="rounded border p-3 space-y-2"
          style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}
        >
          {builtinRows.length === 0 && uninstalledDefaults.length === 0 && (
            <div className="text-[11px]" style={{ color: "var(--text-muted)" }}>
              默认插件已全部卸载；可点上方「恢复默认装配」重新装回。
            </div>
          )}
          {builtinRows.map((p) => (
            <div key={p.id} className="flex items-center gap-2">
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>
                    {p.manifest.name}
                  </span>
                  <span
                    className="text-[10px] px-1.5 py-0.5 rounded border flex-shrink-0"
                    style={{ color: "var(--text-secondary)", borderColor: "var(--border)" }}
                  >
                    {PLUGIN_TYPE_LABELS[p.manifest.type]}
                  </span>
                  <span
                    className="text-[10px] px-1.5 py-0.5 rounded flex-shrink-0"
                    style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}
                  >
                    {PLUGIN_SOURCE_LABELS.builtin}
                  </span>
                </div>
                <div className="text-[11px] truncate" style={{ color: "var(--text-muted)" }}>
                  {p.id} · {p.manifest.tagline ?? ""}
                </div>
              </div>
              <ToggleSwitch
                checked={p.enabled}
                onChange={(on) => void setEnabled(p.id, on).catch((e) => setNotice({ kind: "error", text: errText(e) }))}
                title={p.enabled ? "停用" : "启用"}
              />
              <button
                onClick={() => setConfirmUninstall(p.id)}
                title="卸载"
                className="p-1.5 rounded hover:bg-[var(--hover)]"
                style={{ color: "var(--text-muted)" }}
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
          {uninstalledDefaults.map((r) => (
            <div key={r.id} className="flex items-center gap-2 opacity-60">
              <div className="flex-1 min-w-0">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>
                    {r.name}
                  </span>
                  <span
                    className="text-[10px] px-1.5 py-0.5 rounded flex-shrink-0"
                    style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}
                  >
                    已卸载
                  </span>
                </div>
                <div className="text-[11px] truncate" style={{ color: "var(--text-muted)" }}>
                  {r.id} · 默认插件（已卸载，恢复即装回）
                </div>
              </div>
              <button
                onClick={() => void restoreBuiltin().catch((e) => setNotice({ kind: "error", text: errText(e) }))}
                title="恢复默认装配（补回全部已卸载的默认插件）"
                className="flex items-center gap-1 px-2 py-1 rounded border text-[11px] flex-shrink-0"
                style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
              >
                <RefreshCw size={12} />
                恢复默认装配
              </button>
            </div>
          ))}
        </div>
      </div>
      {/* 已装插件列表（第三方；默认装配分区在上方） */}
      <div className="space-y-2">
        {installedRows.length === 0 && (
          <div className="text-sm py-8 text-center" style={{ color: "var(--text-muted)" }}>
            尚未安装第三方插件。可从上方的「本地文件夹 / Git 地址」安装，或前往「市场」tab 浏览安装。
          </div>
        )}
        {installedRows.map((p) => {
          const declares = p.manifest.declares ?? [];
          const failed = p.phase === "failed";
          const cmds = commands.filter((c) => c.pluginId === p.id);
          return (
            <div
              key={p.id}
              className="rounded border p-3"
              style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}
            >
              <div className="flex items-center gap-2">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>
                      {p.manifest.name}
                    </span>
                    <span
                      className="text-[10px] px-1.5 py-0.5 rounded border flex-shrink-0"
                      title={
                        p.sourceKind === "local"
                          ? "实时引用本地目录，源码改动即时生效"
                          : p.sourceKind === "git"
                            ? "Git 仓库来源，更新 = git pull"
                            : "从市场安装"
                      }
                      style={{
                        color:
                          p.sourceKind === "local" ? "#4ade80" : p.sourceKind === "git" ? "#60a5fa" : "var(--text-secondary)",
                        borderColor: "var(--border)",
                        background:
                          p.sourceKind === "local"
                            ? "rgba(74,222,128,0.08)"
                            : p.sourceKind === "git"
                              ? "rgba(96,165,250,0.08)"
                              : "transparent",
                      }}
                    >
                      {PLUGIN_SOURCE_LABELS[p.sourceKind]}
                    </span>
                    {failed && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded" style={{ color: "#f87171", background: "rgba(248,113,113,0.1)" }}>
                        加载失败
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] truncate" style={{ color: "var(--text-muted)" }}>
                    {p.id} · {PLUGIN_TYPE_LABELS[p.manifest.type]} · {PLUGIN_SCOPE_LABELS[p.scope]} · v{p.manifest.version}
                  </div>
                </div>
                <ToggleSwitch
                  checked={p.enabled}
                  onChange={(on) => void setEnabled(p.id, on)}
                  title={p.enabled ? "停用" : "启用"}
                />
                {p.sourceKind !== "local" && (
                  <button
                    onClick={() => void update(p.id).catch((e) => setNotice({ kind: "error", text: errText(e) }))}
                    title="更新"
                    className="p-1.5 rounded hover:bg-[var(--hover)]"
                    style={{ color: "var(--text-muted)" }}
                  >
                    <RefreshCw size={14} />
                  </button>
                )}
                <button
                  onClick={() => setConfirmUninstall(p.id)}
                  title="卸载"
                  className="p-1.5 rounded hover:bg-[var(--hover)]"
                  style={{ color: "var(--text-muted)" }}
                >
                  <Trash2 size={14} />
                </button>
              </div>
              {failed && p.error && (
                <div className="mt-1.5 text-[11px] break-words" style={{ color: "#f87171" }}>
                  {p.error}
                </div>
              )}
              {cmds.length > 0 && (
                <div className="mt-2 flex flex-wrap items-center gap-1">
                  <Terminal size={12} style={{ color: "var(--text-muted)" }} />
                  {cmds.map((c) => (
                    <button
                      key={c.globalId}
                      onClick={() =>
                        void runPluginCommand(c.globalId).then(
                          () => setNotice({ kind: "ok", text: `命令「${c.label}」已执行` }),
                          (e) => setNotice({ kind: "error", text: `命令执行失败：${errText(e)}` }),
                        )
                      }
                      title="运行此插件命令"
                      className="px-1.5 py-0.5 rounded border text-[10px]"
                      style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
                    >
                      {c.label}
                    </button>
                  ))}
                </div>
              )}
              {declares.length > 0 && (
                <div className="mt-2 flex flex-wrap gap-1">
                  {declares.map((ns) => (
                    <span
                      key={ns}
                      className="text-[10px] px-1.5 py-0.5 rounded border"
                      title={`披露：将调用 ${ns}`}
                      style={{
                        color: capabilitySensitive(ns) ? "#f59e0b" : "var(--text-secondary)",
                        borderColor: "var(--border)",
                        background: capabilitySensitive(ns) ? "rgba(245,158,11,0.1)" : "transparent",
                      }}
                    >
                      {capabilitySensitive(ns) ? `${capabilityLabel(ns)}（敏感）` : capabilityLabel(ns)}
                    </span>
                  ))}
                </div>
              )}
              {p.usedCapabilities.length > 0 && (
                <div className="mt-1.5 flex flex-wrap gap-1">
                  {p.usedCapabilities.map((ns) => (
                    <span
                      key={ns}
                      className="text-[10px] px-1.5 py-0.5 rounded"
                      title={`实际调用：${ns}`}
                      style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}
                    >
                      {capabilityLabel(ns)} · 已调用
                    </span>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {confirmUninstall && (
        <ConfirmDialog
          title={`卸载插件「${plugins[confirmUninstall]?.manifest.name ?? confirmUninstall}」`}
          description={
            plugins[confirmUninstall]?.sourceKind === "builtin"
              ? "将移除该默认插件（随 App 分发；可经「恢复默认装配」重新装回）。其视图随即从工作区移除。"
              : plugins[confirmUninstall]?.sourceKind === "local"
                ? "将移除该插件的目录链接，本地源目录本身不受影响。插件贡献的功能随即移除。"
                : "将删除插件目录与本地状态，插件贡献的功能随即移除。此操作不可撤销。"
          }
          confirmText="卸载"
          onConfirm={() => {
            const id = confirmUninstall;
            setConfirmUninstall(null);
            void uninstall(id).catch((e) => setNotice({ kind: "error", text: errText(e) }));
          }}
          onCancel={() => setConfirmUninstall(null)}
        />
      )}
        </>
      )}
    </section>
  );
}
