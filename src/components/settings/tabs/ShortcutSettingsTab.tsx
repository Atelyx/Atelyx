/**
 * 快捷键设置（应用级）：应用内命令快捷键（按插件分组；生效键 = 用户覆盖 → 命令声明默认键，录制覆盖即时生效）+ 全局热键（OS 级，按 manifest 声明）两分区。
 * 全局改键经 OS 层「注册新键 → 注销旧键」，新键被占用即失败并保留原键；运行时注册但未声明的条目只读展示。
 * 冲突口径：允许共存、命中按注册顺序先到先得；同一插件内两条命令撞键给警示标识（跨插件不提示）。
 */
import { useEffect, useState } from "react";
import { AlertTriangle, RotateCcw } from "lucide-react";
import { Button } from "@/components/common/Button";
import { usePluginStore } from "@/stores/pluginStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useNotificationStore } from "@/stores/notificationStore";
import { BUILTIN_COMMAND_SHORTCUTS, commandBindingError } from "@/constants/commandShortcuts";
import {
  acceleratorConflictKey,
  commandShortcutConflictKey,
  formatAccelerator,
  formatCommandShortcut,
  serializeCommandEvent,
} from "@/utils/shortcutKeys";
import type { PluginCommandContribution } from "@/types";

/** 录制目标：应用内命令（按 globalId）或全局热键（按声明键）。 */
type RecordingTarget = { kind: "command"; globalId: string } | { kind: "global"; key: string } | null;

/** 命令行：label + 生效键展示 + 录制/恢复默认 + 同插件撞键警示。 */
function CommandRow({
  command,
  effective,
  overridden,
  conflict,
  recording,
  onRecord,
  onCancelRecord,
}: {
  command: PluginCommandContribution;
  effective: string | undefined;
  overridden: boolean;
  conflict: boolean;
  recording: boolean;
  onRecord: () => void;
  onCancelRecord: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 py-1.5">
      <div className="min-w-0 flex items-center gap-1.5">
        <span className="text-xs truncate" style={{ color: "var(--text-secondary)" }}>
          {command.label}
        </span>
        {conflict && (
          <span
            className="flex items-center gap-0.5 text-[10px] shrink-0"
            style={{ color: "var(--warning)" }}
            title="同一插件内有其他命令共用此快捷键，按注册顺序先注册的生效"
          >
            <AlertTriangle size={11} /> 撞键
          </span>
        )}
      </div>
      <div className="flex items-center gap-2 shrink-0">
        {recording ? (
          <span
            className="text-xs px-2 py-1 rounded-[var(--radius-sm)] animate-pulse"
            style={{ color: "var(--accent)", background: "var(--accent-soft)" }}
          >
            按下新组合键（Esc 取消）
          </span>
        ) : (
          <button
            onClick={onRecord}
            title="点击后按下新组合键"
            className="text-xs px-2 py-1 rounded-[var(--radius-sm)] font-mono transition hover:bg-[var(--hover)]"
            style={{ color: effective ? "var(--text-primary)" : "var(--text-muted)" }}
          >
            {effective ? formatCommandShortcut(effective) : "未绑定"}
          </button>
        )}
        {overridden && !recording && (
          <Button variant="ghost" onClick={onCancelRecord} title="清除自定义，恢复默认键">
            <RotateCcw size={12} />
          </Button>
        )}
      </div>
    </div>
  );
}

export function ShortcutSettingsTab() {
  usePluginStore((s) => s.uiRevision);
  const plugins = usePluginStore.getState().plugins;
  const commands = usePluginStore.getState().pluginCommands();
  const commandShortcuts = useSettingsStore((s) => s.commandShortcuts);
  const globalShortcuts = useSettingsStore((s) => s.globalShortcuts);
  const setCommandShortcut = useSettingsStore((s) => s.setCommandShortcut);
  const setGlobalShortcutOverride = useSettingsStore((s) => s.setGlobalShortcutOverride);
  const [recording, setRecording] = useState<RecordingTarget>(null);
  const globalShortcutRegistrations = useSettingsStore((s) => s.globalShortcutRegistrations);
  const globalLoadError = useSettingsStore((s) => s.globalShortcutRegistrationsError);
  const refreshGlobalShortcutRegistrations = useSettingsStore(
    (s) => s.refreshGlobalShortcutRegistrations,
  );

  /** 刷新 OS 层登记清单（进入页面与改键后重取，保证「已注册/未注册」状态如实）。 */
  useEffect(() => {
    void refreshGlobalShortcutRegistrations();
  }, [recording, plugins, refreshGlobalShortcutRegistrations]);

  /** 录制：window 捕获阶段接管下一次按键（先于其他快捷键监听），Esc 取消、纯修饰键忽略。 */
  useEffect(() => {
    if (!recording) return;
    const format = recording.kind === "command" ? "command" : "accelerator";
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        setRecording(null);
        return;
      }
      const serialized = serializeCommandEvent(e, format);
      if (!serialized) return;
      const target = recording;
      setRecording(null);
      if (target.kind === "command") {
        // 作用域命令的监听点有修饰键门控：不满足约束的组合永不生效，拒绝保存并说明
        const def = BUILTIN_COMMAND_SHORTCUTS.find((d) => `${d.pluginId}:${d.id}` === target.globalId);
        const violation = def ? commandBindingError(def, serialized) : null;
        if (violation) {
          useNotificationStore.getState().notify({ level: "warning", message: violation });
          return;
        }
        void setCommandShortcut(target.globalId, serialized);
      } else {
        void setGlobalShortcutOverride(target.key, serialized);
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  }, [recording, setCommandShortcut, setGlobalShortcutOverride]);

  /** 命令去重：注册时声明 globalShortcutId 且同插件 manifest 确有该声明的，动作由全局
   *  快捷键区单行承载，不再重复列命令行；声明缺失（清单更新未生效等）时命令行照常展示。
   *  其余命令（含未绑定）一律展示、可设置。 */
  const bindableCommands = commands.filter((c) => {
    if (!c.globalShortcutId) return true;
    const declared = plugins[c.pluginId]?.manifest.shortcuts?.some((d) => d.id === c.globalShortcutId);
    return !declared;
  });

  /** 应用内命令按插件分组（注册表顺序；同名插件行取显示名）。 */
  const commandGroups = new Map<string, PluginCommandContribution[]>();
  for (const c of bindableCommands) {
    const group = commandGroups.get(c.pluginId) ?? [];
    group.push(c);
    commandGroups.set(c.pluginId, group);
  }

  /** 命令生效键（覆盖 → 默认）。 */
  const commandEffective = (c: PluginCommandContribution) =>
    commandShortcuts[c.globalId] ?? c.shortcut;

  /** 同插件撞键检测（冲突判定键归一；>1 条命中同一键即警示）。 */
  const conflictKeysByPlugin = new Map<string, Set<string>>();
  for (const [pluginId, group] of commandGroups) {
    const counts = new Map<string, number>();
    for (const c of group) {
      const key = commandShortcutConflictKey(commandEffective(c) ?? "");
      if (!key) continue;
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    const dup = new Set([...counts.entries()].filter(([, n]) => n > 1).map(([k]) => k));
    if (dup.size > 0) conflictKeysByPlugin.set(pluginId, dup);
  }

  /** 全局热键声明行（已装插件行序；声明 = 可自定义的数据源，与是否已注册无关）。 */
  const globalRows = Object.entries(plugins).flatMap(([, row]) =>
    (row.manifest.shortcuts ?? []).map((decl) => ({
      pluginId: row.id,
      pluginName: row.manifest.name,
      enabled: row.enabled,
      declarationKey: `${row.id}:${decl.id}`,
      label: decl.label,
      defaultKey: decl.key,
    })),
  );
  /** 全局热键生效键（覆盖 → 声明默认）。 */
  const globalEffective = (declarationKey: string, defaultKey: string) =>
    globalShortcuts[declarationKey] || defaultKey;

  /** 运行时注册但未声明的条目（只读）：OS 登记表 - 已声明条目的生效键。 */
  const declaredAccelerators = new Set(
    globalRows.map((r) => acceleratorConflictKey(globalEffective(r.declarationKey, r.defaultKey))),
  );
  const undeclared = globalShortcutRegistrations.filter(
    (r) => !declaredAccelerators.has(acceleratorConflictKey(r.accelerator)),
  );
  const pluginNameOf = (pluginId: string) => plugins[pluginId]?.manifest.name ?? pluginId;

  /** 全局行是否已注册（OS 登记表命中其生效键）。 */
  const registeredOf = (declarationKey: string, defaultKey: string) =>
    globalShortcutRegistrations.some(
      (r) => acceleratorConflictKey(r.accelerator) === acceleratorConflictKey(globalEffective(declarationKey, defaultKey)),
    );

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* ===== 应用内命令快捷键 ===== */}
      <div>
        <div className="text-sm font-medium" style={{ color: "var(--text-primary)" }}>
          命令快捷键
        </div>
        <div className="text-xs mt-0.5" style={{ color: "var(--text-muted)" }}>
          应用内生效；点击键位后按下新组合键即覆盖，Esc 取消。多条命令共用同一键时按注册顺序先到先得。
        </div>
      </div>
      {bindableCommands.length === 0 && (
        <div className="text-xs" style={{ color: "var(--text-muted)" }}>
          当前没有可自定义的命令快捷键（随插件启停变化）
        </div>
      )}      {[...commandGroups.entries()].map(([pluginId, group]) => (
        <div
          key={pluginId}
          className="p-3 rounded-[var(--radius-md)] border"
          style={{ background: "var(--bg-tertiary)", borderColor: "var(--border)" }}
        >
          <div className="text-xs font-medium mb-1" style={{ color: "var(--text-muted)" }}>
            {pluginNameOf(pluginId)}
          </div>
          <div className="divide-y" style={{ borderColor: "var(--border)" }}>
            {group.map((c) => {
              const effective = commandEffective(c);
              return (
                <CommandRow
                  key={c.globalId}
                  command={c}
                  effective={effective}
                  overridden={commandShortcuts[c.globalId] !== undefined}
                  conflict={conflictKeysByPlugin.get(pluginId)?.has(commandShortcutConflictKey(effective ?? "")) ?? false}
                  recording={recording?.kind === "command" && recording.globalId === c.globalId}
                  onRecord={() => setRecording({ kind: "command", globalId: c.globalId })}
                  onCancelRecord={() => void setCommandShortcut(c.globalId, undefined)}
                />
              );
            })}
          </div>
        </div>
      ))}

      {/* ===== 全局热键（OS 级） ===== */}
      <div className="pt-2">
        <div className="text-sm font-medium" style={{ color: "var(--text-primary)" }}>
          全局快捷键
        </div>
        <div className="text-xs mt-0.5" style={{ color: "var(--text-muted)" }}>
          系统级注册，应用不在前台也触发；由插件在清单中声明。改键后即注销旧键、注册新键——新键被其他插件占用时保留原键并提示。
        </div>
      </div>
      {globalLoadError && (
        <div className="text-xs" style={{ color: "var(--text-muted)" }}>
          全局快捷键状态读取失败（当前平台可能不支持）
        </div>
      )}
      {globalRows.length === 0 && (
        <div className="text-xs" style={{ color: "var(--text-muted)" }}>
          当前没有插件声明全局快捷键
        </div>
      )}
      {globalRows.map((r) => {
        const effective = globalEffective(r.declarationKey, r.defaultKey);
        const isRegistered = registeredOf(r.declarationKey, r.defaultKey);
        return (
          <div
            key={r.declarationKey}
            className="flex items-center justify-between gap-3 p-3 rounded-[var(--radius-md)] border"
            style={{ background: "var(--bg-tertiary)", borderColor: "var(--border)" }}
          >
            <div className="min-w-0">
              <div className="text-sm font-medium flex items-center gap-1.5" style={{ color: "var(--text-primary)" }}>
                {r.label}
                {!r.enabled && (
                  <span className="text-[10px] px-1 rounded" style={{ color: "var(--text-muted)", background: "var(--hover)" }}>
                    插件已停用
                  </span>
                )}
                {!isRegistered && r.enabled && (
                  <span className="text-[10px] px-1 rounded" style={{ color: "var(--text-muted)", background: "var(--hover)" }}>
                    未注册
                  </span>
                )}
              </div>
              <div className="text-xs mt-0.5" style={{ color: "var(--text-muted)" }}>
                {r.pluginName}
              </div>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              {recording?.kind === "global" && recording.key === r.declarationKey ? (
                <span
                  className="text-xs px-2 py-1 rounded-[var(--radius-sm)] animate-pulse"
                  style={{ color: "var(--accent)", background: "var(--accent-soft)" }}
                >
                  按下新组合键（Esc 取消）
                </span>
              ) : (
                <button
                  onClick={() => setRecording({ kind: "global", key: r.declarationKey })}
                  title="点击后按下新组合键"
                  className="text-xs px-2 py-1 rounded-[var(--radius-sm)] font-mono transition hover:bg-[var(--hover)]"
                  style={{ color: "var(--text-primary)" }}
                >
                  {formatAccelerator(effective)}
                </button>
              )}
              {globalShortcuts[r.declarationKey] !== undefined && (
                <Button variant="ghost" onClick={() => void setGlobalShortcutOverride(r.declarationKey, undefined)} title="清除自定义，恢复声明默认键">
                  <RotateCcw size={12} />
                </Button>
              )}
            </div>
          </div>
        );
      })}
      {undeclared.length > 0 && (
        <div
          className="p-3 rounded-[var(--radius-md)] border"
          style={{ background: "var(--bg-tertiary)", borderColor: "var(--border)" }}
        >
          <div className="text-xs font-medium mb-1" style={{ color: "var(--text-muted)" }}>
            运行时注册（未声明，只读）
          </div>
          {undeclared.map((r) => (
            <div key={`${r.pluginId}:${r.accelerator}`} className="flex items-center justify-between gap-3 py-1">
              <span className="text-xs truncate" style={{ color: "var(--text-secondary)" }}>
                {pluginNameOf(r.pluginId)}
              </span>
              <span className="text-xs font-mono shrink-0" style={{ color: "var(--text-muted)" }}>
                {formatAccelerator(r.accelerator)}
                {r.windowToggle ? "（窗口切换）" : ""}
              </span>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
