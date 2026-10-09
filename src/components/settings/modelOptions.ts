import type { DropdownOption } from "@/components/common/DropdownSelect";
import type { ProviderConfig } from "@/types";

/** DropdownSelect value 编码 (providerId, model) 对的分隔符（模型/供应商 ID 不含该字符，跨供应商同名模型不合并）。 */
export const MODEL_PAIR_SEP = "\u0000";

/** 编码 (providerId, model) 对为下拉 value。 */
export const modelPairValue = (providerId: string, model: string) =>
  `${providerId}${MODEL_PAIR_SEP}${model}`;

/** 模型下拉选项条目（每 (provider, model) 一条，跨供应商同名不合并）。 */
export interface ModelChoiceEntry {
  providerId: string;
  model: string;
  label: string;
  group: string;
}

/** 构建模型下拉选项：存活项 + 当前值占位（当前值不在存活项中时前置「已失效」项，使当前值可显示、可改选）。 */
export function buildModelChoices(
  entries: ModelChoiceEntry[],
  currentKey: string,
  staleLabel: string,
): DropdownOption[] {
  const opts: DropdownOption[] = entries.map((e) => ({
    value: modelPairValue(e.providerId, e.model),
    label: e.label,
    group: e.group,
  }));
  if (currentKey && !opts.some((o) => o.value === currentKey)) {
    opts.unshift({ value: currentKey, label: staleLabel, group: "已失效" });
  }
  return opts;
}

/** 供应商名可重名（用户自定义）：重名时在分组头补短 id 后缀，避免同名供应商的模型在视觉上合并。 */
const groupLabel = (p: { id: string; name: string }, nameCounts: Map<string, number>) =>
  (nameCounts.get(p.name) ?? 0) > 1 ? `${p.name}（${p.id.slice(0, 6)}）` : p.name;

/** 模型选项：每个供应商的每个模型各一条（同名模型跨供应商**不合并**；仅单供应商内部去重），
 * 供默认模型 / 话题自动命名下拉共用——同一 model ID 不同供应商是不同可选项。
 * 供应商名可重名：内部统计重名数，供分组头补短 id 后缀。 */
export function buildModelEntries(providers: ProviderConfig[]): ModelChoiceEntry[] {
  const nameCounts = new Map<string, number>();
  for (const p of providers) {
    nameCounts.set(p.name, (nameCounts.get(p.name) ?? 0) + 1);
  }
  return providers.flatMap((p) =>
    Array.from(new Map(p.models.map((m) => [m.id, m])).values()).map((m) => ({
      providerId: p.id,
      model: m.id,
      label: m.nickname ?? m.id,
      group: groupLabel(p, nameCounts),
    })),
  );
}

/** 默认模型当前值编码：固定供应商（modelProviderId）+ model 名；无默认 = 空串。
 *  未设 `modelProviderId` 的配置按编码取不到存活项，下拉据 `buildModelChoices` 的「已失效」项展示当前值供改选。 */
export function defaultModelKeyFor(model: string | undefined, pinnedId?: string): string {
  if (!model) return "";
  return modelPairValue(pinnedId ?? "", model);
}
