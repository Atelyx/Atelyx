/**
 * 插件详情弹窗：失败诊断 / 声明能力与实际调用对照 / 命令入口 / 回退入口。
 * 纯 UI 组件：props 与回调通信；回退确认弹窗的状态机由父组件（PluginsSettingsTab）持有。
 */
import { useEffect, useRef, useState } from "react";
import { Check, ChevronRight, Circle, RefreshCw, Terminal, X } from "lucide-react";
import type { InstalledPlugin, PluginAuditEntry, PluginCommandContribution, PluginSlotChain } from "@/types";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { PLUGIN_MOUNT_PHASE_LABELS, PLUGIN_MOUNT_PHASE_ORDER } from "@/constants/plugins";

interface PluginDetailsDialogProps {
  plugin: InstalledPlugin;
  audit?: PluginAuditEntry;
  commands: PluginCommandContribution[];
  capabilityLabel: (name: string) => string;
  capabilitySensitive: (name: string) => boolean;
  /** 槽位修改链查询（归属可见：展开某槽看声明方 + 全部贡献/装饰者）。 */
  getSlotChain: (slot: string) => PluginSlotChain;
  onRunCommand: (globalId: string) => void;
  onRollback: () => void;
  onClose: () => void;
  rollbackConfirm: boolean;
  onConfirmRollback: () => void;
  onCancelRollback: () => void;
}

/** 槽位修改链明细（声明方 + 贡献/装饰者列表；挂在展开的槽位行下）。 */
function SlotChainDetail({ chain }: { chain: PluginSlotChain }) {
  return (
    <div className="mt-1.5 ml-3.5 space-y-0.5 border-l pl-2" style={{ borderColor: "var(--border)", color: "var(--text-muted)" }}>
      <div>声明方：{chain.declarer}</div>
      {chain.contributors.length > 0 && (
        <div className="break-words">
          贡献：{chain.contributors.map((c) => `${c.pluginId}（priority ${c.priority}${c.label ? ` · ${c.label}` : ""}）`).join("、")}
        </div>
      )}
      {chain.decorators.length > 0 && (
        <div className="break-words">
          装饰：{chain.decorators.map((d) => `${d.pluginId}（priority ${d.priority}）`).join("、")}
        </div>
      )}
    </div>
  );
}

export function PluginDetailsDialog({
  plugin,
  audit,
  commands,
  capabilityLabel,
  capabilitySensitive,
  getSlotChain,
  onRunCommand,
  onRollback,
  onClose,
  rollbackConfirm,
  onConfirmRollback,
  onCancelRollback,
}: PluginDetailsDialogProps) {
  const closeRef = useRef<HTMLButtonElement>(null);
  const onCloseRef = useRef(onClose);
  const rollbackConfirmRef = useRef(rollbackConfirm);
  onCloseRef.current = onClose;
  rollbackConfirmRef.current = rollbackConfirm;
  const [expandedSlot, setExpandedSlot] = useState<string | null>(null);
  const pluginCommands = commands.filter((command) => command.pluginId === plugin.id);
  const declares = plugin.manifest.declares ?? [];
  // 挂载失败阶段在六阶段顺序中的下标（progress 条：其前 = 已通过，其后 = 未到达）
  const failIndex = plugin.failure ? PLUGIN_MOUNT_PHASE_ORDER.indexOf(plugin.failure.phase) : -1;

  useEffect(() => {
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !rollbackConfirmRef.current) onCloseRef.current();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      previousFocus?.focus();
    };
  }, []);

  // 层级 180：高于既有弹层段（历史弹窗 90 / 灯箱 100 / 右键菜单 110）——弹窗可能从右键菜单
  // 路径打开；内部叠加的回退确认弹窗自身为 z-[200]，自然落在详情弹窗之上
  return (
    <div
      className="fixed inset-0 z-[180] flex items-center justify-center"
      style={{ background: "rgba(0,0,0,0.5)" }}
      onClick={rollbackConfirm ? undefined : onClose}
      role="dialog"
      aria-modal="true"
      aria-label={`${plugin.manifest.name}插件详情`}
    >
      <div
        className="w-[min(38rem,calc(100vw-2rem))] max-h-[calc(100vh-2rem)] overflow-y-auto rounded-lg border shadow-xl p-4"
        style={{ background: "var(--bg-secondary)", borderColor: "var(--border)" }}
        onClick={(event) => event.stopPropagation()}
      >
        <div className="flex items-start gap-3 mb-4">
          <div className="flex-1 min-w-0">
            <h3 className="text-sm font-medium truncate" style={{ color: "var(--text-primary)" }}>{plugin.manifest.name}</h3>
            <div className="text-[11px] break-all mt-1" style={{ color: "var(--text-muted)" }}>
              {plugin.id} · v{plugin.manifest.version} · {plugin.phase === "active" ? "运行中" : plugin.phase === "failed" ? "加载失败" : "已停用"}
            </div>
          </div>
          <button ref={closeRef} onClick={onClose} className="p-1 rounded hover:bg-[var(--hover)]" style={{ color: "var(--text-muted)" }} title="关闭详情">
            <X size={15} />
          </button>
        </div>

        {plugin.previousVersion && (
          <div className="mb-4 rounded border p-3" style={{ borderColor: "var(--border)" }}>
            <div className="text-xs" style={{ color: "var(--text-secondary)" }}>可回退版本：v{plugin.previousVersion}</div>
            <div className="text-[11px] mt-1" style={{ color: "var(--text-muted)" }}>只替换代码，插件数据保持当前内容；回退成功后将清空保留版本。</div>
            <button onClick={onRollback} className="mt-2 flex items-center gap-1.5 px-2.5 py-1.5 rounded text-xs" style={{ background: "var(--accent)", color: "var(--accent-fg)" }}>
              <RefreshCw size={13} /> 回退到 v{plugin.previousVersion}
            </button>
          </div>
        )}

        {plugin.failure && (
          <div className="mb-4 rounded border p-3" style={{ borderColor: "rgba(248,113,113,0.45)" }}>
            <div className="flex items-center gap-1.5 text-xs" style={{ color: "#f87171" }}>
              <X size={13} /> 加载失败
            </div>
            {/* 六阶段挂载进度：失败阶段之前 = 已通过，失败阶段红底高亮，其后 = 未到达 */}
            <div className="mt-2 flex flex-wrap items-center gap-1.5">
              {PLUGIN_MOUNT_PHASE_ORDER.map((phase, i) => {
                const label = PLUGIN_MOUNT_PHASE_LABELS[phase];
                if (i === failIndex) {
                  return (
                    <span key={phase} className="flex items-center gap-1 px-1.5 py-0.5 rounded text-[11px]" style={{ background: "rgba(248,113,113,0.12)", color: "#f87171" }}>
                      <X size={12} /> {label}
                    </span>
                  );
                }
                if (i < failIndex) {
                  return (
                    <span key={phase} className="flex items-center gap-1 text-[11px]" style={{ color: "var(--text-secondary)" }}>
                      <Check size={12} /> {label}
                    </span>
                  );
                }
                return (
                  <span key={phase} className="flex items-center gap-1 text-[11px]" style={{ color: "var(--text-muted)" }}>
                    <Circle size={10} /> {label}
                  </span>
                );
              })}
            </div>
            <div className="text-xs mt-1 break-words" style={{ color: "#f87171" }}>{plugin.failure.message}</div>
            {plugin.failure.missing && <div className="text-[11px] mt-1" style={{ color: "var(--text-muted)" }}>缺失依赖：{plugin.failure.missing.join("、")}</div>}
          </div>
        )}

        {declares.length > 0 && (
          <section className="mb-4">
            <h4 className="text-[11px] font-medium mb-2" style={{ color: "var(--text-muted)" }}>声明能力</h4>
            <div className="flex flex-wrap gap-1">{declares.map((name) => <span key={name} className="text-[10px] px-1.5 py-0.5 rounded border" style={{ color: capabilitySensitive(name) ? "#f59e0b" : "var(--text-secondary)", borderColor: "var(--border)" }}>{capabilityLabel(name)}{capabilitySensitive(name) ? "（敏感）" : ""}</span>)}</div>
          </section>
        )}

        {audit && (audit.services.length > 0 || audit.events.length > 0 || audit.calls.length > 0) && (
          <section className="mb-4">
            <h4 className="text-[11px] font-medium mb-2" style={{ color: "var(--text-muted)" }}>实际访问与调用</h4>
            <div className="space-y-1 text-[10px]" style={{ color: "var(--text-secondary)" }}>
              {audit.services.map((name) => <div key={`service:${name}`}>{capabilityLabel(name)} · 已访问</div>)}
              {audit.events.map((name) => <div key={`event:${name}`}>{name} · 已订阅</div>)}
              {audit.calls.map((call) => <div key={`${call.service}.${call.method}:${call.summary}`}>{capabilityLabel(call.service)} · {call.summary}</div>)}
            </div>
          </section>
        )}

        {audit && (audit.slotContributions.length > 0 || audit.slotDecorators.length > 0) && (
          <section className="mb-4">
            <h4 className="text-[11px] font-medium mb-2" style={{ color: "var(--text-muted)" }}>槽位贡献与装饰</h4>
            <div className="space-y-1 text-[10px]" style={{ color: "var(--text-secondary)" }}>
              {audit.slotContributions.map((c) => (
                <div key={c.id}>
                  <button
                    onClick={() => setExpandedSlot(expandedSlot === c.slot ? null : c.slot)}
                    className="flex items-center gap-1 hover:opacity-80 text-left"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <ChevronRight size={12} className={expandedSlot === c.slot ? "rotate-90" : ""} style={{ transition: "transform 0.12s" }} />
                    {c.slot} · 贡献（{c.cardinality} · priority {c.priority}{c.label ? ` · ${c.label}` : ""}）
                  </button>
                  {expandedSlot === c.slot && <SlotChainDetail chain={getSlotChain(c.slot)} />}
                </div>
              ))}
              {audit.slotDecorators.map((d) => (
                <div key={d.id}>
                  <button
                    onClick={() => setExpandedSlot(expandedSlot === d.slot ? null : d.slot)}
                    className="flex items-center gap-1 hover:opacity-80 text-left"
                    style={{ color: "var(--text-secondary)" }}
                  >
                    <ChevronRight size={12} className={expandedSlot === d.slot ? "rotate-90" : ""} style={{ transition: "transform 0.12s" }} />
                    {d.slot} · 装饰（priority {d.priority}）
                  </button>
                  {expandedSlot === d.slot && <SlotChainDetail chain={getSlotChain(d.slot)} />}
                </div>
              ))}
            </div>
          </section>
        )}

        {pluginCommands.length > 0 && (
          <section>
            <h4 className="text-[11px] font-medium mb-2 flex items-center gap-1" style={{ color: "var(--text-muted)" }}><Terminal size={12} /> 命令</h4>
            <div className="flex flex-wrap gap-1">{pluginCommands.map((command) => <button key={command.globalId} onClick={() => onRunCommand(command.globalId)} className="px-1.5 py-0.5 rounded border text-[10px]" style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}>{command.label}</button>)}</div>
          </section>
        )}
      </div>
      {rollbackConfirm && (
        <ConfirmDialog
          title={`回退插件「${plugin.manifest.name}」`}
          description={`将插件代码恢复到 v${plugin.previousVersion ?? "上一版本"}，并保留当前插件数据。回退成功后将清空保留版本，是否继续？`}
          confirmText="回退"
          danger={false}
          onConfirm={onConfirmRollback}
          onCancel={onCancelRollback}
        />
      )}
    </div>
  );
}
