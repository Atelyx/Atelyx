/**
 * 组合接管面板（设置 → 插件）：显示每一行的实现来源，并提供用户层裁决（钉住/改回默认）。
 * 数据全部经 pluginStore 中转（组件不直连 services）；无可治理行时不渲染——治理面板只在需要裁决时出现。
 */
import { useState } from "react";
import { usePluginStore } from "@/stores/pluginStore";
import { DEFAULT_COMPOSITION } from "@/components/plugins/cordis/builtins";
import { composePlugins, compositionPackages } from "@/utils/cordis/composition";
import { COMPOSITION_IMPL_DEFAULT } from "@/constants/plugins";
import { type CompositionBinding } from "@/types";
import { errText } from "@/utils/errors";

/** 当前生效实现的展示文案。 */
function describeImplementation(b: CompositionBinding): string {
  // 回退后 implId 已是本行自身，失效的原始指定由 problem 文案给出（下方单独展示）
  if (b.problem) return "本行默认实现（指定实现不可用，见下方说明）";
  if (b.source === "plugin") return `由「${b.declarers[0].pluginId}」接管`;
  if (b.source === "user") {
    return b.implId === b.rowId ? "本行默认实现（用户指定）" : `由「${b.implId}」装配（用户指定）`;
  }
  return "本行默认实现";
}

/** 用户层条目指向的实现 id（`"default"` 归一为行自身）。 */
function pinnedImplId(rowId: string, userImpl: string): string {
  return userImpl === COMPOSITION_IMPL_DEFAULT ? rowId : userImpl;
}

export function CompositionPanel() {
  const composition = usePluginStore((s) => s.composition);
  const plugins = usePluginStore((s) => s.plugins);
  // 订阅 UI 注册修订号：装载后裁决快照随之更新，触发重渲染。
  usePluginStore((s) => s.uiRevision);
  const setCompositionImpl = usePluginStore((s) => s.setCompositionImpl);
  const [notice, setNotice] = useState<string | null>(null);
  if (!composition) return null;

  const rows = composePlugins(DEFAULT_COMPOSITION, compositionPackages(plugins));
  const mountedRowIds = new Set(composition.mounts.map((m) => m.rowId));
  // 被接管的行（其位置跑别人的实现）→ 提供者 id → 目标行 id 列表
  const takenOverBy = new Map<string, string[]>();
  for (const m of composition.mounts) {
    if (m.implId === m.rowId) continue;
    takenOverBy.set(m.implId, [...(takenOverBy.get(m.implId) ?? []), m.rowId]);
  }
  const rowName = new Map(rows.map((r) => [r.id, r.name]));
  const governed = rows.filter((row) => {
    const b = composition.bindings[row.id];
    if (!b) return false;
    return b.declarers.length > 0 || b.userImpl !== null || Boolean(b.problem) || takenOverBy.has(row.id);
  });
  if (governed.length === 0 && composition.unmatched.length === 0) return null;

  // null = 删键回落插件声明层；"default" = 钉住本行默认实现
  const apply = (rowId: string, value: string) => {
    void setCompositionImpl(rowId, value || null).then(
      () => setNotice(null),
      (e) => setNotice(`接管设置保存失败：${errText(e)}`),
    );
  };

  return (
    <div className="mb-4 rounded border p-3" style={{ borderColor: "var(--border-subtle)", background: "var(--bg-tertiary)" }}>
      <div className="text-micro font-medium mb-2" style={{ color: "var(--text-muted)" }}>
        组合接管（行的实现来源可被插件声明接管，可在此钉住或改回默认）
      </div>
      {notice && (
        <div className="text-micro mb-2 break-words" style={{ color: "var(--danger)" }}>
          {notice}
        </div>
      )}
      {/* 声明了不存在的行：静默忽略会让人以为「声明了却没生效」 */}
      {composition.unmatched.length > 0 && (
        <div className="text-micro mb-2 break-words" style={{ color: "var(--warning)" }}>
          {composition.unmatched.map((u) => `「${u.pluginId}」声明的目标行「${u.target}」不存在`).join("；")}
        </div>
      )}
      <div className="space-y-2">
        {governed.map((row) => {
          const b = composition.bindings[row.id];
          const targets = takenOverBy.get(row.id);
          // 提供者的自身行不独立装配：代码在目标行的位置运行
          const suppressed = targets !== undefined && !mountedRowIds.has(row.id);
          const userValue = b.userImpl ?? "";
          // 钉住是否真的生效：裁决采纳它（implId 与归一后的钉住值一致）；被采纳但该插件未声明本行
          // 也是生效的（用户层恒胜，不要求声明方存在），只有不可用而回退才算失效。
          const userEffective = userValue !== "" && b.implId === pinnedImplId(row.id, userValue);
          const userUnlisted =
            userValue !== "" &&
            userValue !== COMPOSITION_IMPL_DEFAULT &&
            !b.declarers.some((d) => d.pluginId === userValue);
          return (
            <div key={row.id} className="rounded border p-2.5" style={{ borderColor: "var(--border)" }}>
              <div className="flex items-center gap-2 mb-1.5">
                <span className="text-xs font-medium truncate" style={{ color: "var(--text-primary)" }}>
                  {row.name}
                </span>
                <span
                  className="text-micro px-1.5 py-0.5 rounded flex-shrink-0"
                  style={{ color: "var(--text-muted)", background: "var(--bg-secondary)" }}
                >
                  {row.id}
                </span>
                {suppressed && (
                  <span
                    className="text-micro px-1.5 py-0.5 rounded flex-shrink-0"
                    style={{ color: "var(--text-secondary)", background: "var(--bg-secondary)" }}
                  >
                    本行不独立装配
                  </span>
                )}
              </div>
              <div className="text-micro mb-1.5" style={{ color: "var(--text-secondary)" }}>
                当前：{describeImplementation(b)}
              </div>
              <select
                value={userValue}
                onChange={(e) => apply(row.id, e.target.value)}
                className="w-full min-w-0 px-2 py-1 rounded border text-xs outline-none"
                style={{ borderColor: "var(--input-border)", color: "var(--text-primary)", background: "var(--input-bg)" }}
              >
                <option value="">跟随插件声明（默认）</option>
                <option value={COMPOSITION_IMPL_DEFAULT}>用本行默认实现（钉住）</option>
                {b.declarers.map((d) => (
                  <option key={d.pluginId} value={d.pluginId}>
                    钉住：「{d.pluginId}」接管（priority {d.priority}
                    {!b.problem && b.source === "plugin" && b.declarers[0].pluginId === d.pluginId
                      ? "，当前生效"
                      : ""}
                    ）
                  </option>
                ))}
                {userUnlisted && (
                  <option value={userValue}>
                    {userEffective
                      ? `当前钉住：「${userValue}」`
                      : `当前钉住：「${userValue}」（不可用，已回退默认实现）`}
                  </option>
                )}
              </select>
              {suppressed && (
                <div className="text-micro mt-1" style={{ color: "var(--text-muted)" }}>
                  代码在{targets!.map((t) => `「${rowName.get(t) ?? t}」`).join("、")}的位置运行
                </div>
              )}
              {b.declarers.length > 1 && b.source !== "user" && (
                <div className="text-micro mt-1" style={{ color: "var(--text-muted)" }}>
                  多个插件声明接管本行，按 priority 生效：
                  {b.declarers.map((d) => `${d.pluginId}（${d.priority}）`).join("、")}
                </div>
              )}
              {b.problem && (
                <div className="text-micro mt-1 break-words" style={{ color: "var(--warning)" }}>
                  {b.problem}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
