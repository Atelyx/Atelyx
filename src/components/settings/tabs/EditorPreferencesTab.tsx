import { useEffect, useRef, useState } from "react";
import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { Input } from "@/components/common/Input";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useSettingsStore } from "@/stores/settingsStore";
import {
  NOTE_LINE_WIDTH_DEFAULT,
  NOTE_LINE_WIDTH_MAX,
  NOTE_LINE_WIDTH_MIN,
} from "@/constants/notes";

/** 编辑器面板（应用级显示偏好）：宽松换行 / 页面内标题 / 正文行宽。
 *  三者是「本机怎么读正文」的偏好，与仓库无关，落 global.json、跨仓库共享。 */
export function EditorPreferencesTab() {
  const softLineBreak = useSettingsStore((s) => s.softLineBreak);
  const setSoftLineBreak = useSettingsStore((s) => s.setSoftLineBreak);
  const inlineTitle = useSettingsStore((s) => s.inlineTitle);
  const setInlineTitle = useSettingsStore((s) => s.setInlineTitle);
  const noteLineWidth = useSettingsStore((s) => s.noteLineWidth);
  const setNoteLineWidth = useSettingsStore((s) => s.setNoteLineWidth);

  /** 行宽为 0 = 不限制（随面板铺满）。 */
  const noLimit = noteLineWidth === 0;
  /** 输入草稿：数字框允许中途态，失焦/回车才提交（边打边提交会把「78」钳成 320 打断输入）。 */
  const [widthDraft, setWidthDraft] = useState(noteLineWidth > 0 ? String(noteLineWidth) : "");
  /** 最近一次非零行宽：取消「不限制」时回填，不丢用户设过的宽度。 */
  const lastWidthRef = useRef(noteLineWidth > 0 ? noteLineWidth : NOTE_LINE_WIDTH_DEFAULT);
  useEffect(() => {
    if (noteLineWidth > 0) lastWidthRef.current = noteLineWidth;
    setWidthDraft(noteLineWidth > 0 ? String(noteLineWidth) : "");
  }, [noteLineWidth]);

  /** 提交草稿：空 = 不限制，其余按 px 交 store 归一化钳制（越界/非数经此收口）。 */
  const commitWidth = () => {
    const raw = widthDraft.trim();
    void setNoteLineWidth(raw === "" ? 0 : Number(raw));
  };

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

      {/* 正文行宽：标题/属性/正文/反链同列共用一档，超宽居中留白；不限制 = 随面板铺满 */}
      <SettingCard
        title="正文行宽"
        description={`笔记正文的可读性上限，${NOTE_LINE_WIDTH_MIN}–${NOTE_LINE_WIDTH_MAX}px；开启「不限制」后随面板宽度铺满，只保留左右内边距`}
      >
        <div className="flex items-center gap-2">
          <Input
            type="number"
            min={NOTE_LINE_WIDTH_MIN}
            max={NOTE_LINE_WIDTH_MAX}
            step={20}
            value={widthDraft}
            disabled={noLimit}
            onChange={(e) => setWidthDraft(e.target.value)}
            onBlur={commitWidth}
            onKeyDown={(e) => {
              if (e.key === "Enter") e.currentTarget.blur();
            }}
            placeholder="—"
            aria-label="正文行宽（px）"
            className="!w-20 text-right"
          />
          <span className="text-xs" style={{ color: "var(--text-muted)" }}>
            px
          </span>
          <ToggleSwitch
            checked={noLimit}
            onChange={(v) => void setNoteLineWidth(v ? 0 : lastWidthRef.current)}
            title="不限制正文行宽（随面板宽度铺满）"
          />
          <span className="text-xs" style={{ color: "var(--text-secondary)" }}>
            不限制
          </span>
        </div>
      </SettingCard>

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/editorPrefs） */}
      <SlotListMount slot="settings/editorPrefs" />
    </section>
  );
}
