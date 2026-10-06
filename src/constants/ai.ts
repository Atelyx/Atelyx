/**
 * AI 配置默认值。供应商列表存于仓库级配置（.atelyx/config.json 的 `ai` 段，见 VaultConfig）；
 * API key 不随配置落盘，默认仅存 OS keychain（services/keychain，按仓库隔离），仅 `syncKeys`
 * 显式开启时随仓库配置落盘。推理等级为会话/节点级覆盖（ModelSelect 子面板选择），缺省 = 不指定。
 */
import type { AiConfig, ReasoningEffort } from "@/types";

export const DEFAULT_AI_CONFIG: AiConfig = {
  providers: [],
};

export interface ReasoningEffortOption {
  /** "" = 不指定/跟随默认（发送时不带 reasoning_effort）；否则为档位 id。 */
  value: ReasoningEffort | "";
  label: string;
}

/** 推理等级选项（ModelSelect「推理等级」子面板 + 触发器显示共用）：默认（不指定）+ off/low/medium/high。 */
export const REASONING_EFFORT_OPTIONS: ReasoningEffortOption[] = [
  { value: "", label: "默认" },
  { value: "off", label: "关闭" },
  { value: "low", label: "低" },
  { value: "medium", label: "中" },
  { value: "high", label: "高" },
];

/** 档位显示名（触发器/菜单行用；undefined = 默认；未知值回退值本身）。 */
export function reasoningEffortLabel(effort: ReasoningEffort | undefined): string {
  if (effort === undefined) return "默认";
  return REASONING_EFFORT_OPTIONS.find((o) => o.value === effort)?.label ?? effort;
}
