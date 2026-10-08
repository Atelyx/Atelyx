/**
 * 设置 → 插件面板：插件列表（默认组合成员 + 已装插件）的启停/重载/更新/卸载 + 市场、本地文件夹、Git 地址安装入口。
 * 槽位替换（改控件）由插件在 apply 里经 ctx.slots 的 priority 声明；行级接管（改「这一行由谁装配」）由清单声明 + 用户层拍板，入口见 CompositionPanel。
 * 只经 pluginStore 触达插件能力，不直连 services。
 */
import { useMemo, useState } from "react";
import { FolderOpen, GitBranch, Info, RefreshCw, RotateCw, Trash2 } from "lucide-react";
import { usePluginStore, type PluginInstallResult } from "@/stores/pluginStore";
import { useAppStore } from "@/stores/appStore";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { Menu, MenuItem } from "@/components/common/Menu";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { PluginDetailsDialog } from "@/components/plugins/PluginDetailsDialog";
import { PluginUninstallDialog } from "@/components/plugins/PluginUninstallDialog";
import { MarketplaceSection } from "@/components/plugins/MarketplaceSection";
import { Input } from "@/components/common/Input";
import { IconButton } from "@/components/common/Button";
import { SlotConflictPanel } from "@/components/plugins/SlotConflictPanel";
import { CompositionPanel } from "@/components/plugins/CompositionPanel";
import { DEFAULT_COMPOSITION } from "@/components/plugins/cordis/builtins";
import { deriveThemeProviders, isThemePluginRow } from "@/utils/pluginTheme";
import { composePlugins, compositionPackages } from "@/utils/cordis/composition";
import { errText } from "@/types";
import {
  PLUGIN_SOURCE_LABELS,
  PLUGIN_TYPE_LABELS,
} from "@/constants/plugins";

type TabMode = "installed" | "market";

export function PluginsSettingsTab() {
  const plugins = usePluginStore((s) => s.plugins);
  // 平台能力（进程执行在移动端不提供）：经 appStore 读取，保持组件不直连 services
  const capabilities = useAppStore((s) => s.platform.capabilities);
  // 订阅 UI 注册修订号：插件 UI 平面命令异步注册/卸载变化触发重渲染（命令列表在服务层非响应式）。
  usePluginStore((s) => s.uiRevision);
  const setEnabled = usePluginStore((s) => s.setEnabled);
  const update = usePluginStore((s) => s.update);
  const rollback = usePluginStore((s) => s.rollback);
  const reload = usePluginStore((s) => s.reload);
  const uninstall = usePluginStore((s) => s.uninstall);
  const installLocal = usePluginStore((s) => s.installLocal);
  const pickLocalPluginDir = usePluginStore((s) => s.pickLocalPluginDir);
  const installGit = usePluginStore((s) => s.installGit);
  const capabilityLabel = usePluginStore((s) => s.capabilityLabel);
  const capabilitySensitive = usePluginStore((s) => s.capabilitySensitive);
  const pluginCommands = usePluginStore((s) => s.pluginCommands);
  const runPluginCommand = usePluginStore((s) => s.runPluginCommand);
  const restoreDefaultComposition = usePluginStore((s) => s.restoreDefaultComposition);
  const pluginAudit = usePluginStore((s) => s.pluginAudit);
  const slotChain = usePluginStore((s) => s.slotChain);
  const stateError = usePluginStore((s) => s.stateError);

  const [mode, setMode] = useState<TabMode>("installed");
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const [confirmUninstall, setConfirmUninstall] = useState<string | null>(null);
  const [confirmRollback, setConfirmRollback] = useState<string | null>(null);
  const [detailsId, setDetailsId] = useState<string | null>(null);
  const [contextMenu, setContextMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const [gitUrl, setGitUrl] = useState("");
  const [installing, setInstalling] = useState(false);
  const [pendingLocalPath, setPendingLocalPath] = useState<string | null>(null);
  const [pendingGitUrl, setPendingGitUrl] = useState<string | null>(null);

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
        // 安装结果的警示（如保留数据未完整恢复）优先于成功文案，不能被成功提示吞掉
        const warning = (done as PluginInstallResult | undefined)?.warning;
        setNotice({ kind: warning ? "error" : "ok", text: warning ?? okText });
        onOk?.();
      }
    } catch (e) {
      setNotice({ kind: "error", text: errText(e) });
    } finally {
      setInstalling(false);
    }
  };

  /** 回退确认的统一执行入口：清确认态 → 回退 → 成败提示（详情弹窗与行右键两条路径共用）。 */
  const confirmRollbackFor = (id: string) => {
    setConfirmRollback(null);
    void rollback(id).then(
      () => setNotice({ kind: "ok", text: "插件已回退，当前插件数据已保留" }),
      (e) => setNotice({ kind: "error", text: errText(e) }),
    );
  };

  /** 从本地文件夹安装：先选目录，再弹安装确认（与市场/Git 安装同一模式）。 */
  const pickLocalDir = async () => {
    if (installing) return;
    try {
      const path = await pickLocalPluginDir();
      if (!path) return;
      setPendingLocalPath(path);
    } catch (e) {
      setNotice({ kind: "error", text: errText(e) });
    }
  };

  /** 从 Git 地址安装：先校验非空，再弹安装确认。 */
  const startGitInstall = () => {
    const url = gitUrl.trim();
    if (!url) {
      setNotice({ kind: "error", text: "请输入 git 仓库地址" });
      return;
    }
    if (installing) return;
    setPendingGitUrl(url);
  };

  return (
    <section className="flex-1 min-h-0 p-5 overflow-y-auto">
      {/* 插件状态文件损坏/不可读的降级提示：行全部以停用态展示，修复文件后重载解除。 */}
      {stateError && (
        <div
          className="text-xs mb-3 break-words" style={{ color: "var(--danger)" }}>
          {stateError}
        </div>
      )}
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
          style={{ color: notice.kind === "ok" ? "var(--text-secondary)" : "var(--danger)" }}
        >
          {notice.text}
        </div>
      )}
      {/* 安装入口：本地文件夹（junction 实时引用）+ git 地址（clone） */}
      <div className="flex items-center gap-2 mb-3 flex-shrink-0">
        <button
          onClick={() => void pickLocalDir()}
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
          <Input
            value={gitUrl}
            onChange={(e) => setGitUrl(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") startGitInstall();
            }}
            placeholder="从 Git 地址安装（如 https://github.com/owner/repo）"
            className="flex-1 min-w-0"
          />
          <button
            onClick={() => startGitInstall()}
            disabled={installing || !gitUrl.trim()}
            className="px-2.5 py-1.5 rounded border text-xs flex-shrink-0 transition-opacity disabled:opacity-50"
            style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
          >
            {installing ? "安装中…" : "安装"}
          </button>
        </div>
      </div>

      {/* 组合接管裁决：行的实现来源（插件声明 / 用户钉住）；无需裁决时不显示 */}
      <CompositionPanel />

      {/* 槽位冲突裁决：single 槽多贡献由用户定胜者；无冲突行时不显示 */}
      <SlotConflictPanel />

      {/* 列表头：随应用分发的默认组合成员默认启用；恢复默认装配补回已卸载成员（不复活停用） */}
      <div className="mb-3 flex items-center justify-between">
        <span className="text-micro font-medium" style={{ color: "var(--text-muted)" }}>
          插件
        </span>
        <button
          onClick={() =>
            void restoreDefaultComposition().catch((e) => setNotice({ kind: "error", text: errText(e) }))
          }
          className="flex items-center gap-1 text-micro px-1.5 py-0.5 rounded hover:bg-[var(--hover)] flex-shrink-0"
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
          const failed = p?.phase === "failed";
          const lastTheme = p ? isLastEnabledTheme(p) : false;
          return (
            <div
              key={row.id}
              className={`rounded border p-3 ${row.installed ? "" : "opacity-60"}`}
              style={{ borderColor: "var(--border-subtle)", background: "var(--bg-tertiary)" }}
              onContextMenu={(event) => {
                if (!row.installed) return;
                event.preventDefault();
                setContextMenu({ id: row.id, x: event.clientX, y: event.clientY });
              }}
            >
              <div className="flex items-center gap-2">
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>
                      {row.name}
                    </span>
                    {p && (
                      <span
                        className="text-micro px-1.5 py-0.5 rounded border flex-shrink-0"
                        style={{ color: "var(--text-secondary)", borderColor: "var(--border)" }}
                      >
                        {PLUGIN_TYPE_LABELS[p.manifest.type]}
                      </span>
                    )}
                    <span
                      className="text-micro px-1.5 py-0.5 rounded flex-shrink-0"
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
                      <span className="text-micro px-1.5 py-0.5 rounded" style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}>
                        已卸载
                      </span>
                    )}
                    {failed && (
                      <span className="text-micro px-1.5 py-0.5 rounded" style={{ color: "var(--danger)", background: "color-mix(in srgb, var(--danger) 10%, transparent)" }}>
                        加载失败
                      </span>
                    )}
                  </div>
                  <div className="text-micro truncate" style={{ color: "var(--text-muted)" }}>
                    {row.id}
                    {row.installed ? ` · v${row.version}` : " · 默认插件（已卸载）"}
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
                    <IconButton
                      icon={<Info size={14} />}
                      label="查看详情"
                      onClick={() => setDetailsId(row.id)}
                      variant="subtle"
                      size="sm"
                    />
                    {row.sourceKind === "local" && (
                      <IconButton
                        icon={<RotateCw size={14} />}
                        label={row.enabled ? "重载：重跑打包并重新挂载，使源码改动生效" : "启用后可重载"}
                        onClick={() =>
                          void reload(row.id).then(
                            () => setNotice({ kind: "ok", text: "插件已重载，源码改动已生效" }),
                            (e) => setNotice({ kind: "error", text: errText(e) }),
                          )
                        }
                        disabled={!row.enabled}
                        variant="subtle"
                        size="sm"
                      />
                    )}
                    {p.installDir !== "" && row.sourceKind !== "local" && (
                      <IconButton
                        icon={<RefreshCw size={14} />}
                        label="更新"
                        onClick={() => void update(row.id).catch((e) => setNotice({ kind: "error", text: errText(e) }))}
                        variant="subtle"
                        size="sm"
                      />
                    )}
                    <IconButton
                      icon={<Trash2 size={14} />}
                      label={lastTheme ? LAST_THEME_HINT : "卸载"}
                      onClick={() => {
                        if (lastTheme) return;
                        setConfirmUninstall(row.id);
                      }}
                      disabled={lastTheme}
                      variant="subtle"
                      size="sm"
                      className={lastTheme ? "" : "hover:!text-[var(--danger)]"}
                    />
                  </>
                ) : (
                  <button
                    onClick={() =>
                      void restoreDefaultComposition().catch((e) => setNotice({ kind: "error", text: errText(e) }))
                    }
                    title="恢复默认装配（补回已卸载的默认插件）"
                    className="flex items-center gap-1 px-2 py-1 rounded border text-micro flex-shrink-0"
                    style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
                  >
                    <RefreshCw size={12} />
                    恢复
                  </button>
                )}
              </div>

            </div>
          );
        })}
      </div>

      {contextMenu && plugins[contextMenu.id] && (() => {
        const target = plugins[contextMenu.id];
        return (
          <Menu x={contextMenu.x} y={contextMenu.y} onClose={() => setContextMenu(null)} widthClass="w-44">
            <MenuItem onClick={() => { setDetailsId(contextMenu.id); setContextMenu(null); }}>
              <Info size={13} /> 查看详情
            </MenuItem>
            {target.previousVersion && (
              <MenuItem onClick={() => { setConfirmRollback(contextMenu.id); setContextMenu(null); }}>
                <RefreshCw size={13} /> 回退到 v{target.previousVersion}
              </MenuItem>
            )}
          </Menu>
        );
      })()}

      {detailsId && plugins[detailsId] && (
        <PluginDetailsDialog
          plugin={plugins[detailsId]}
          audit={auditByRow.get(detailsId)}
          commands={commands}
          capabilityLabel={capabilityLabel}
          capabilitySensitive={capabilitySensitive}
          processAvailable={capabilities.processExecution}
          getSlotChain={slotChain}
          onRunCommand={(globalId) => void runPluginCommand(globalId).then(
            () => setNotice({ kind: "ok", text: "命令已执行" }),
            (e) => setNotice({ kind: "error", text: `命令执行失败：${errText(e)}` }),
          )}
          onRollback={() => setConfirmRollback(detailsId)}
          onClose={() => setDetailsId(null)}
          rollbackConfirm={confirmRollback === detailsId}
          onConfirmRollback={() => confirmRollbackFor(detailsId)}
          onCancelRollback={() => setConfirmRollback(null)}
        />
      )}

      {confirmRollback && !detailsId && plugins[confirmRollback] && (
        <ConfirmDialog
          title={`回退插件「${plugins[confirmRollback].manifest.name}」`}
          description={`将插件代码恢复到 v${plugins[confirmRollback].previousVersion ?? "上一版本"}，并保留当前插件数据。回退成功后将清空保留版本`}
          confirmText="回退"
          danger={false}
          onConfirm={() => confirmRollbackFor(confirmRollback)}
          onCancel={() => setConfirmRollback(null)}
        />
      )}

      {confirmUninstall && (plugins[confirmUninstall]?.installDir !== "" && plugins[confirmUninstall]?.sourceKind !== "local" ? (
        // 有落位目录的仓库/Git 来源：三选一（保留配置卸载为默认；本地链接行数据在源目录内、
        // 随应用分发行无磁盘数据，两者卸载不涉及保留问题，维持确认/取消）
        <PluginUninstallDialog
          title={`卸载插件「${plugins[confirmUninstall]?.manifest.name ?? confirmUninstall}」`}
          description="保留配置卸载：插件数据搬到保留区，重装同 id 插件时自动恢复；彻底卸载：数据一并删除，不可恢复。"
          onKeepData={() => {
            const id = confirmUninstall;
            setConfirmUninstall(null);
            void uninstall(id, true).catch((e) => setNotice({ kind: "error", text: errText(e) }));
          }}
          onDeleteAll={() => {
            const id = confirmUninstall;
            setConfirmUninstall(null);
            void uninstall(id, false).catch((e) => setNotice({ kind: "error", text: errText(e) }));
          }}
          onCancel={() => setConfirmUninstall(null)}
        />
      ) : (
        <ConfirmDialog
          title={`卸载插件「${plugins[confirmUninstall]?.manifest.name ?? confirmUninstall}」`}
          description={
            plugins[confirmUninstall]?.installDir === ""
              ? isThemePluginRow(plugins[confirmUninstall]!)
                ? "将移除该默认主题插件（实现随应用编译；可经「恢复默认装配」装回）。主题随即切回剩余的主题插件。"
                : "将移除该默认插件（实现随应用编译；可经「恢复默认装配」装回）。其视图随即从工作区移除。"
              : "将移除该插件的目录链接，本地源目录本身不受影响。插件贡献的功能随即移除。"
          }
          confirmText="卸载"
          onConfirm={() => {
            const id = confirmUninstall;
            setConfirmUninstall(null);
            void uninstall(id).catch((e) => setNotice({ kind: "error", text: errText(e) }));
          }}
          onCancel={() => setConfirmUninstall(null)}
        />
      ))}

      {(pendingLocalPath || pendingGitUrl) && (
        <ConfirmDialog
          title={pendingLocalPath ? "从本地文件夹安装" : "从 Git 地址安装"}
          description={
            pendingLocalPath
              ? "实时引用所选本地目录（无拷贝，源码改动即时生效）。"
              : "将按地址克隆插件仓库（保留 .git 供更新）。"
          }
          confirmText="安装"
          danger={false}
          onConfirm={() => {
            const path = pendingLocalPath;
            const git = pendingGitUrl;
            setPendingLocalPath(null);
            setPendingGitUrl(null);
            if (path) {
              void runInstall(
                () => installLocal(path),
                "已从本地文件夹安装（替换行沿用原启用状态，全新插件默认停用；源目录改动即时生效）",
              );
            } else if (git) {
              void runInstall(() => installGit(git), "已从 Git 地址安装（替换行沿用原启用状态，全新插件默认停用）", () => setGitUrl(""));
            }
          }}
          onCancel={() => {
            setPendingLocalPath(null);
            setPendingGitUrl(null);
          }}
        />
      )}
        </>
      )}
    </section>
  );
}
