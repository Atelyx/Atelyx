/**
 * 安装作用域选择卡片：确认弹窗内嵌的两选项（本机 / 随仓库共享）。
 *
 * 安装三入口（本地文件夹 / Git 地址 / 市场）共用：确认弹窗内先选作用域再落位。
 * 可选 `lockedScope`：某项被锁定为唯一可选项时另一项置灰，`lockedNote` 显示置灰原因
 * （如同 id 行被随应用分发的实现占用时只能装到本机，vault 安装必被 Rust 拒绝）。
 */
import { CheckCircle2, Circle } from "lucide-react";
import { PLUGIN_SCOPE_LABELS } from "@/constants/plugins";
import type { PluginScope } from "@/types";

const SCOPE_OPTIONS: PluginScope[] = ["app", "vault"];
const SCOPE_MEANINGS: Record<PluginScope, string> = {
  app: "仅在这台电脑可用",
  vault: "存入当前仓库，代码随仓库扩散",
};

export function InstallScopeSelector({
  value,
  onChange,
  lockedScope,
  lockedNote,
}: {
  value: PluginScope;
  onChange: (scope: PluginScope) => void;
  /** 锁定的作用域（另一项置灰不可选）；如 id 被随应用分发实现占用时锁定「本机」。 */
  lockedScope?: PluginScope;
  /** 置灰项的说明文案。 */
  lockedNote?: string;
}) {
  return (
    <div className="space-y-1.5" role="radiogroup" aria-label="安装作用域">
      {SCOPE_OPTIONS.map((scope) => {
        const locked = lockedScope !== undefined && lockedScope !== scope;
        const active = value === scope;
        return (
          <button
            key={scope}
            type="button"
            role="radio"
            aria-checked={active}
            disabled={locked}
            onClick={() => onChange(scope)}
            className="w-full text-left rounded border px-2.5 py-1.5 flex items-center gap-2 disabled:opacity-50"
            style={{
              borderColor: active ? "var(--accent)" : "var(--border)",
              background: active ? "rgba(212,175,55,0.08)" : "var(--bg-primary)",
              color: "var(--text-primary)",
            }}
          >
            {active ? (
              <CheckCircle2 size={15} className="flex-shrink-0" style={{ color: "var(--accent)" }} />
            ) : (
              <Circle size={15} className="flex-shrink-0" style={{ color: "var(--text-muted)" }} />
            )}
            <span className="flex-1 min-w-0">
              <span className="text-xs font-medium block">{PLUGIN_SCOPE_LABELS[scope]}</span>
              <span className="text-[11px] block" style={{ color: "var(--text-muted)" }}>
                {locked && lockedNote ? lockedNote : SCOPE_MEANINGS[scope]}
              </span>
            </span>
          </button>
        );
      })}
    </div>
  );
}
