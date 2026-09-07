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
import { FolderOpen, GitBranch, RefreshCw, Trash2 } from "lucide-react";
import { usePluginStore } from "@/stores/pluginStore";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { MarketplaceSection } from "@/components/plugins/MarketplaceSection";
import { errText } from "@/types";
import { PLUGIN_SCOPE_LABELS, PLUGIN_SOURCE_LABELS, PLUGIN_TYPE_LABELS } from "@/constants/plugins";

type TabMode = "installed" | "market";

export function PluginsSettingsTab() {
  const plugins = usePluginStore((s) => s.plugins);
  const setEnabled = usePluginStore((s) => s.setEnabled);
  const update = usePluginStore((s) => s.update);
  const uninstall = usePluginStore((s) => s.uninstall);
  const installLocalFromPicker = usePluginStore((s) => s.installLocalFromPicker);
  const installGit = usePluginStore((s) => s.installGit);
  const capabilityLabel = usePluginStore((s) => s.capabilityLabel);
  const capabilitySensitive = usePluginStore((s) => s.capabilitySensitive);

  const [mode, setMode] = useState<TabMode>("installed");
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const [confirmUninstall, setConfirmUninstall] = useState<string | null>(null);
  const [gitUrl, setGitUrl] = useState("");
  const [installing, setInstalling] = useState(false);

  const rows = Object.values(plugins).sort((a, b) => (a.id < b.id ? -1 : 1));

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
    <section className="flex-1 min-h-0 p-5 flex flex-col overflow-hidden">
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
      {/* 已装插件列表 */}
      <div className="flex-1 min-h-0 overflow-auto space-y-2">
        {rows.length === 0 && (
          <div className="text-sm py-8 text-center" style={{ color: "var(--text-muted)" }}>
            尚未安装插件。可从上方的「本地文件夹 / Git 地址」安装，或前往「市场」tab 浏览安装。
          </div>
        )}
        {rows.map((p) => {
          const declares = p.manifest.declares ?? [];
          const failed = p.phase === "failed";
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
            plugins[confirmUninstall]?.sourceKind === "local"
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
