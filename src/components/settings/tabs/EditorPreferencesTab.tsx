import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useSettingsStore } from "@/stores/settingsStore";

/** 编辑器面板（应用级显示偏好）：宽松换行 / 页面内标题。
 *  两者是「本机怎么读正文」的偏好，与仓库无关，落 global.json、跨仓库共享。 */
export function EditorPreferencesTab() {
  const softLineBreak = useSettingsStore((s) => s.softLineBreak);
  const setSoftLineBreak = useSettingsStore((s) => s.setSoftLineBreak);
  const inlineTitle = useSettingsStore((s) => s.inlineTitle);
  const setInlineTitle = useSettingsStore((s) => s.setInlineTitle);

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* 宽松换行 */}
      <SettingCard
        title="宽松换行"
        description="单个换行显示为换行；关闭 = 按 Markdown 标准需空行换行"
      >
        <ToggleSwitch
          checked={softLineBreak}
          onChange={(v) => void setSoftLineBreak(v)}
          title="宽松换行"
        />
      </SettingCard>

      {/* 页面内标题 */}
      <SettingCard
        title="页面内标题"
        description="将文件名作为标题：笔记正文顶部显示文件名（不含扩展名），点击标题可直接重命名笔记"
      >
        <ToggleSwitch
          checked={inlineTitle}
          onChange={(v) => void setInlineTitle(v)}
          title="页面内标题（将文件名作为标题）"
        />
      </SettingCard>

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/editorPrefs） */}
      <SlotListMount slot="settings/editorPrefs" />
    </section>
  );
}
