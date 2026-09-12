/**
 * 设置 → 插件面板：插件列表（默认组合成员 + 已装插件）+ 市场浏览。
 *
 * - 列表（单表，装配顺序 = 默认组合成员在前）：每行可启停/更新/卸载，展示来源徽标/版本/运行状态/
 *   分段失败诊断/声明与实际调用审计/命令入口；未安装的默认组合成员成灰行，经「恢复默认装配」装回。
 * - 替换/增强关系由插件自己在 apply 里经 ctx.slots 的 priority / inject 声明（作者侧决定）：
 *   用户侧只需安装 + 启用，不在管理页暴露行序/实现来源等开发者语义。
 * - 安装入口：市场安装（git clone）之外，支持「从本地文件夹安装」（junction 实时引用）与
 *   「从 Git 地址安装」（git clone）。
 * 分层：本组件只经 pluginStore 触达插件能力（不直连 services）；列表行推导取自 utils/cordis/composition。
 */
import { useMemo, useState } from "react";
import { FolderOpen, GitBranch, RefreshCw, Terminal, Trash2 } from "lucide-react";
import { usePluginStore, type PluginInstallResult } from "@/stores/pluginStore";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { MarketplaceSection } from "@/components/plugins/MarketplaceSection";
import { DEFAULT_COMPOSITION } from "@/components/plugins/cordis/builtins";
import { deriveThemeProviders, isThemePluginRow } from "@/utils/pluginTheme";
import { composePlugins, compositionPackages } from "@/utils/cordis/composition";
import { errText } from "@/types";
import {
  PLUGIN_MOUNT_PHASE_LABELS,
  PLUGIN_MOUNT_PHASE_ORDER,
  PLUGIN_SCOPE_LABELS,
  PLUGIN_SOURCE_LABELS,
  PLUGIN_TYPE_LABELS,
} from "@/constants/plugins";

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
  const restoreDefaultComposition = usePluginStore((s) => s.restoreDefaultComposition);
  const pluginAudit = usePluginStore((s) => s.pluginAudit);

  const [mode, setMode] = useState<TabMode>("installed");
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const [confirmUninstall, setConfirmUninstall] = useState<string | null>(null);
  const [gitUrl, setGitUrl] = useState("");
  const [installing, setInstalling] = useState(false);

  const rows = useMemo(() => composePlugins(DEFAULT_COMPOSITION, compositionPackages(plugins)), [plugins]);
  // 主题插件守恒（与 Rust 命令校验双保险）：主题插件必须至少保留一个启用——
  // 停用/卸载「当前启用且为最后一个」的主题插件时禁用操作并附提示。
  // 判定口径与 Rust plugin_is_theme 一致（isThemePluginRow 排除基础主题重名条目）。
  const enabledThemeCount = deriveThemeProviders(Object.values(plugins)).providers.length;
  const isLastEnabledTheme = (p: (typeof plugins)[string]) =>
    isThemePluginRow(p) && p.enabled && enabledThemeCount <= 1;
  const LAST_THEME_HINT = "至少保留一个主题插件（可先启用/安装其他主题插件）";
  // 命令与审计快照按渲染读取（注册面变化经 uiRevision 订阅触发重渲染；行数固定，开销可忽略）。
  const commands = pluginCommands();
  const auditByRow = new Map(pluginAudit().map((a) => [a.pluginId, a]));

  /** 安装统一入口：action 返回 false（如取消目录选择）不算成功、不提示；onOk 成功回调（如清空输入）。 */
  const runInstall = async (
    action: () => Promise<boolean | void | PluginInstallResult>,
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
            void runInstall(() => installLocalFromPicker(), "已从本地文件夹安装（默认未启用，源目录改动即时生效）")
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
                void runInstall(() => installGit(gitUrl), "已从 Git 地址安装（默认未启用）", () => setGitUrl(""));
              }
            }}
            placeholder="从 Git 地址安装（如 https://github.com/owner/repo）"
            className="flex-1 min-w-0 px-2 py-1.5 rounded border text-xs outline-none"
            style={{ borderColor: "var(--border)", color: "var(--text-primary)", background: "var(--bg-primary)" }}
          />
          <button
            onClick={() => void runInstall(() => installGit(gitUrl), "已从 Git 地址安装（默认未启用）", () => setGitUrl(""))}
            disabled={installing || !gitUrl.trim()}
            className="px-2.5 py-1.5 rounded border text-xs flex-shrink-0 transition-opacity disabled:opacity-50"
            style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
          >
            {installing ? "安装中…" : "安装"}
          </button>
        </div>
      </div>

      {/* 列表头：随应用分发的默认组合成员默认启用；恢复默认装配补回已卸载成员（不复活停用） */}
      <div className="mb-3 flex items-center justify-between">
        <span className="text-[11px] font-medium" style={{ color: "var(--text-muted)" }}>
          插件
        </span>
        <button
          onClick={() =>
            void restoreDefaultComposition().catch((e) => setNotice({ kind: "error", text: errText(e) }))
          }
          className="flex items-center gap-1 text-[11px] px-1.5 py-0.5 rounded hover:bg-[var(--hover)] flex-shrink-0"
          style={{ color: "var(--text-secondary)" }}
          title="补回已卸载的默认插件（不覆盖停用状态）"
        >
          <RefreshCw size={12} />
          恢复默认装配
        </button>
      </div>

      <div className="space-y-2">
        {rows.map((row) => {
          const p = plugins[row.id];
          const declares = p?.manifest.declares ?? [];
          const failed = p?.phase === "failed";
          const failure = p?.failure;
          const cmds = commands.filter((c) => c.pluginId === row.id);
          const audit = auditByRow.get(row.id);
          const lastTheme = p ? isLastEnabledTheme(p) : false;
          return (
            <div
              key={row.id}
              className={`rounded border p-3 ${row.installed ? "" : "opacity-60"}`}
              style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}
            >
              <div className="flex items-center gap-2">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>
                      {row.name}
                    </span>
                    {p && (
                      <span
                        className="text-[10px] px-1.5 py-0.5 rounded border flex-shrink-0"
                        style={{ color: "var(--text-secondary)", borderColor: "var(--border)" }}
                      >
                        {PLUGIN_TYPE_LABELS[p.manifest.type]}
                      </span>
                    )}
                    <span
                      className="text-[10px] px-1.5 py-0.5 rounded flex-shrink-0"
                      title={
                        row.sourceKind === "local"
                          ? "实时引用本地目录，源码改动即时生效"
                          : row.sourceKind === "git"
                            ? "Git 仓库来源，更新 = git pull"
                            : row.sourceKind === "builtin"
                              ? "随应用分发：实现随应用编译，版本随 App 更新"
                              : "从市场安装"
                      }
                      style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}
                    >
                      {row.installed ? PLUGIN_SOURCE_LABELS[row.sourceKind] : "未安装"}
                    </span>
                    {!row.installed && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded" style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}>
                        已卸载
                      </span>
                    )}
                    {failed && (
                      <span className="text-[10px] px-1.5 py-0.5 rounded" style={{ color: "#f87171", background: "rgba(248,113,113,0.1)" }}>
                        加载失败
                      </span>
                    )}
                  </div>
                  <div className="text-[11px] truncate" style={{ color: "var(--text-muted)" }}>
                    {row.id}
                    {row.installed ? ` · ${PLUGIN_SCOPE_LABELS[p.scope]} · v${row.version}` : " · 默认插件（已卸载）"}
                    {row.tagline ? ` · ${row.tagline}` : ""}
                  </div>
                </div>
                {row.installed ? (
                  <>
                    <ToggleSwitch
                      checked={row.enabled}
                      onChange={(on) => void setEnabled(row.id, on).catch((e) => setNotice({ kind: "error", text: errText(e) }))}
                      title={lastTheme ? LAST_THEME_HINT : row.enabled ? "停用" : "启用"}
                      disabled={lastTheme}
                    />
                    {p.installDir !== "" && row.sourceKind !== "local" && (
                      <button
                        onClick={() => void update(row.id).catch((e) => setNotice({ kind: "error", text: errText(e) }))}
                        title="更新"
                        className="p-1.5 rounded hover:bg-[var(--hover)]"
                        style={{ color: "var(--text-muted)" }}
                      >
                        <RefreshCw size={14} />
                      </button>
                    )}
                    <button
                      onClick={() => {
                        if (lastTheme) return;
                        setConfirmUninstall(row.id);
                      }}
                      aria-disabled={lastTheme}
                      title={lastTheme ? LAST_THEME_HINT : "卸载"}
                      className={`p-1.5 rounded ${lastTheme ? "opacity-50" : "hover:bg-[var(--hover)]"}`}
                      style={{ color: "var(--text-muted)" }}
                    >
                      <Trash2 size={14} />
                    </button>
                  </>
                ) : (
                  <button
                    onClick={() =>
                      void restoreDefaultComposition().catch((e) => setNotice({ kind: "error", text: errText(e) }))
                    }
                    title="恢复默认装配（补回已卸载的默认插件）"
                    className="flex items-center gap-1 px-2 py-1 rounded border text-[11px] flex-shrink-0"
                    style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
                  >
                    <RefreshCw size={12} />
                    恢复
                  </button>
                )}
              </div>

              {failed && failure && (
                <div className="mt-1.5 space-y-1">
                  <div className="flex flex-wrap gap-1">
                    {PLUGIN_MOUNT_PHASE_ORDER.map((phase) => (
                      <span
                        key={phase}
                        className="text-[10px] px-1.5 py-0.5 rounded"
                        style={
                          phase === failure.phase
                            ? { color: "#f87171", background: "rgba(248,113,113,0.12)" }
                            : { color: "var(--text-muted)", background: "var(--bg-secondary)" }
                        }
                      >
                        {PLUGIN_MOUNT_PHASE_LABELS[phase]}
                      </span>
                    ))}
                  </div>
                  <div className="text-[11px] break-words" style={{ color: "#f87171" }}>
                    {failure.message}
                  </div>
                  {failure.missing && (
                    <div className="text-[10px] break-words" style={{ color: "var(--text-muted)" }}>
                      缺失依赖：{failure.missing.join("、")}
                    </div>
                  )}
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
                      title={`披露：将访问 ${ns}`}
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
              {audit && (audit.services.length > 0 || audit.events.length > 0 || audit.calls.length > 0) && (
                <div className="mt-1.5 flex flex-wrap gap-1">
                  {audit.calls.map((call) => (
                    <span
                      key={`call:${call.service}.${call.method}:${call.summary}`}
                      className="text-[10px] px-1.5 py-0.5 rounded border"
                      title={`实际调用：${call.service}.${call.method}`}
                      style={{
                        color: "#f59e0b",
                        borderColor: "var(--border)",
                        background: "rgba(245,158,11,0.1)",
                      }}
                    >
                      {capabilityLabel(call.service)} · {call.summary}
                    </span>
                  ))}
                  {audit.services.map((ns) => (
                    <span
                      key={`svc:${ns}`}
                      className="text-[10px] px-1.5 py-0.5 rounded"
                      title={`实际访问服务：${ns}`}
                      style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}
                    >
                      {capabilityLabel(ns)} · 已访问
                    </span>
                  ))}
                  {audit.events.map((ev) => (
                    <span
                      key={`evt:${ev}`}
                      className="text-[10px] px-1.5 py-0.5 rounded"
                      title={`实际订阅事件：${ev}`}
                      style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}
                    >
                      {ev} · 已订阅
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
            plugins[confirmUninstall]?.installDir === ""
              ? isThemePluginRow(plugins[confirmUninstall]!)
                ? "将移除该默认主题插件（实现随应用编译；可经「恢复默认装配」装回）。主题随即切回剩余的主题插件。"
                : "将移除该默认插件（实现随应用编译；可经「恢复默认装配」装回）。其视图随即从工作区移除。"
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
