import { SettingCard } from "@/components/settings/SettingCard";
import { DropdownSelect, type DropdownOption } from "@/components/common/DropdownSelect";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { resolveEntryScene } from "@/utils/workspaceLayout";

/** 工作区面板（仓库级）：启动仓库时自动切换场景。直接订阅 store（配置与场景镜像）。 */
export function WorkspaceSettingsTab() {
  const vaultConfig = useSettingsStore((s) => s.vaultConfig);
  const setEntryScene = useSettingsStore((s) => s.setEntryScene);
  const scenes = useUiStateStore((s) => s.scenes);

  // 选项 = 「不切换」+ 全部场景；悬挂引用（场景已删）归位到不切换项
  const options: DropdownOption[] = [
    { value: "", label: "不切换" },
    ...scenes.map((s) => ({ value: s.id, label: s.name })),
  ];
  const value = resolveEntryScene(vaultConfig?.entrySceneId, scenes) ?? "";

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* 启动仓库时自动切换场景：每次进入仓库生效；切换即恢复该场景记忆的激活布局 */}
      <SettingCard
        title="启动时切换场景"
        description="进入仓库时切到所选场景；「不切换」= 保持上次界面"
      >
        <DropdownSelect
          value={value}
          onChange={(v) => void setEntryScene(v || undefined)}
          options={options}
          emptyText="暂无场景"
          className="text-sm rounded px-2 py-1 max-w-[220px]"
          style={{
            color: "var(--text-secondary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/workspace） */}
      <SlotListMount slot="settings/workspace" />
    </section>
  );
}
