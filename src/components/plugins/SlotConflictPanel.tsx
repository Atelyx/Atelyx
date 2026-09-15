/**
 * 槽位冲突裁决面板（设置 → 插件）：single 槽多个插件贡献时，由用户决定显示哪个贡献。
 *
 * 数据全部经 pluginStore 中转（组件不直连 services）：冲突行 = `pluginStore.slotConflictRows()`
 * （钉住信息来自应用级 ui-state），切换胜者写 `uiStateStore.setSlotWinner`（null = 跟随 priority）。
 * 订阅 pluginStore.uiRevision（注册变化）与 uiStateStore.slotWinnerOverrides（裁决变化）。
 * 无冲突行时不渲染——治理面板只在需要裁决时出现，避免噪音。
 */
import { usePluginStore } from "@/stores/pluginStore";
import { useUiStateStore } from "@/stores/uiStateStore";

export function SlotConflictPanel() {
  usePluginStore((s) => s.uiRevision);
  useUiStateStore((s) => s.slotWinnerOverrides);
  const setSlotWinner = useUiStateStore((s) => s.setSlotWinner);
  const rows = usePluginStore.getState().slotConflictRows();
  if (rows.length === 0) return null;

  return (
    <div className="mb-4 rounded border p-3" style={{ borderColor: "var(--border)", background: "var(--bg-primary)" }}>
      <div className="text-[11px] font-medium mb-2" style={{ color: "var(--text-muted)" }}>
        槽位冲突（多个插件贡献同一位置，选择显示哪个）
      </div>
      <div className="space-y-2">
        {rows.map((row) => {
          const pinnedStale = row.pinnedId !== null && !row.contributors.some((c) => c.id === row.pinnedId);
          const winner = row.contributors.find((c) => c.id === row.winnerId);
          return (
            <div key={row.slot} className="rounded border p-2.5" style={{ borderColor: "var(--border)" }}>
              <div className="flex items-center gap-2 mb-1.5">
                <span className="text-xs font-medium truncate" style={{ color: "var(--text-primary)" }}>
                  {row.slot}
                </span>
                <span className="text-[10px] px-1.5 py-0.5 rounded flex-shrink-0" style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}>
                  声明方：{row.declarer}
                </span>
              </div>
              <div className="flex items-center gap-2">
                <select
                  value={row.pinnedId ?? ""}
                  onChange={(e) => setSlotWinner(row.slot, e.target.value || null)}
                  className="flex-1 min-w-0 px-2 py-1 rounded border text-xs outline-none"
                  style={{ borderColor: "var(--border)", color: "var(--text-primary)", background: "var(--bg-primary)" }}
                >
                  <option value="">跟随优先级（默认）</option>
                  {pinnedStale && (
                    <option value={row.pinnedId ?? undefined}>已失效的钉住（贡献已卸载）</option>
                  )}
                  {row.contributors.map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.label ? `${c.label} · ` : ""}
                      {c.pluginId}（priority {c.priority}
                      {row.winnerId === c.id && row.pinnedId === null ? "，当前胜出" : ""}）
                    </option>
                  ))}
                </select>
              </div>
              {pinnedStale && (
                <div className="text-[10px] mt-1" style={{ color: "#f59e0b" }}>
                  钉住的贡献已卸载，当前按 priority 胜出：{winner?.label ? `${winner.label} · ` : ""}{winner?.pluginId ?? "无"}
                </div>
              )}
              {row.pinnedId === null && winner && (
                <div className="text-[10px] mt-1" style={{ color: "var(--text-muted)" }}>
                  当前胜出：{winner.label ? `${winner.label} · ` : ""}{winner.pluginId}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
