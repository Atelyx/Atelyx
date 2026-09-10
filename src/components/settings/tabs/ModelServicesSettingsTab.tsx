import { DropdownSelect, type DropdownOption } from "@/components/common/DropdownSelect";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import {
  MODEL_PAIR_SEP,
  buildModelChoices,
  buildModelEntries,
  defaultModelKeyFor,
  modelPairValue,
  type ModelChoiceEntry,
} from "@/components/settings/modelOptions";
import { useSettingsStore } from "@/stores/settingsStore";

/** 话题自动命名下拉「跟随默认模型」哨兵值（与任何模型 id 区分；空串 = 不启用）。 */
const AUTO_NAMING_DEFAULT = "__default__";

/** 模型服务：默认模型与话题自动命名模型的仓库级设置。 */
export function ModelServicesSettingsTab() {
  const vaultConfig = useSettingsStore((s) => s.vaultConfig);
  const config = useSettingsStore((s) => s.config);
  const setVaultModel = useSettingsStore((s) => s.setVaultModel);
  const setAutoNamingEnabled = useSettingsStore((s) => s.setAutoNamingEnabled);
  const setAutoNamingModel = useSettingsStore((s) => s.setAutoNamingModel);

  const modelEntries: ModelChoiceEntry[] = buildModelEntries(config.providers);
  /** 默认模型当前值编码：优先固定供应商（modelProviderId），旧配置按 model 名反查首个命中；无默认 = 空串。 */
  const defaultModelKey = defaultModelKeyFor(
    modelEntries,
    vaultConfig?.model,
    vaultConfig?.modelProviderId,
  );
  /** 默认模型下拉选项：先「不指定」，再接存活模型项（含存量值兼容的「已失效」前置项）。 */
  const defaultModelChoices: DropdownOption[] = [
    { value: "", label: "不指定" },
    ...buildModelChoices(modelEntries, defaultModelKey, vaultConfig?.model ?? defaultModelKey),
  ];
  /** 话题自动命名：缺省不启用（下拉「不启用」项）。 */
  const autoNamingEnabled = vaultConfig?.autoNamingEnabled ?? false;
  /** 话题自动命名模型（缺省 = 跟随默认模型）；编码为 (providerId, model) 对，供下拉 value 匹配。 */
  const autoNamingModelValue = vaultConfig?.autoNamingModel
    ? modelPairValue(vaultConfig.autoNamingModel.providerId, vaultConfig.autoNamingModel.model)
    : "";
  /** 话题自动命名模型下拉选项：与默认模型同源（每供应商每模型一条 + 存量值兼容展示）。 */
  const autoNamingChoices = buildModelChoices(
    modelEntries,
    autoNamingModelValue,
    vaultConfig?.autoNamingModel?.model ?? autoNamingModelValue,
  );

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* 默认模型（已实现）：仓库级默认模型，存 .atelyx/config.json */}
      <SettingCard
        title="默认模型"
        description="未指定时的默认模型；留空 = 未指定（对话需手动选择模型）"
      >
        <DropdownSelect
          value={defaultModelKey}
          onChange={(v) => {
            if (!v) {
              void setVaultModel(null);
              return;
            }
            const [providerId, model] = v.split(MODEL_PAIR_SEP);
            void setVaultModel({ providerId, model });
          }}
          options={defaultModelChoices}
          className="text-sm rounded px-2 py-1 w-[200px] flex-shrink-0"
          style={{
            color: "var(--text-secondary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>

      {/* 话题自动命名：下拉选择（不启用 / 跟随默认模型 / 指定模型；话题命名一般用小模型） */}
      <SettingCard
        title="话题自动命名"
        description="首轮对话后自动生成简短标题；「不启用」= 关闭自动命名"
      >
        <DropdownSelect
          value={autoNamingEnabled ? autoNamingModelValue || AUTO_NAMING_DEFAULT : ""}
          onChange={(v) => {
            if (!v) {
              void setAutoNamingEnabled(false);
              return;
            }
            if (v === AUTO_NAMING_DEFAULT) {
              void setAutoNamingEnabled(true).then(() =>
                setAutoNamingModel(null),
              );
              return;
            }
            const [providerId, model] = v.split(MODEL_PAIR_SEP);
            void setAutoNamingEnabled(true).then(() =>
              setAutoNamingModel(
                providerId ? { providerId, model } : null,
              ),
            );
          }}
          options={[
            { value: "", label: "不启用" },
            { value: AUTO_NAMING_DEFAULT, label: "跟随默认模型" },
            ...autoNamingChoices,
          ]}
          className="text-sm rounded px-2 py-1 w-[200px] flex-shrink-0"
          style={{
            color: "var(--text-secondary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/modelServices） */}
      <SlotListMount slot="settings/modelServices" />
    </section>
  );
}
