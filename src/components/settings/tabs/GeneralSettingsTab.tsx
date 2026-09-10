import { ToggleSwitch } from "@/components/common/ToggleSwitch";
import { DropdownSelect } from "@/components/common/DropdownSelect";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useSettingsStore } from "@/stores/settingsStore";
import { useAppStore } from "@/stores/appStore";
import { useDraftSync } from "@/hooks/useDraftSync";

/** 界面字体选项（value = CSS font-family；空串 = 跟随系统默认）。 */
const FONT_OPTIONS: { label: string; value: string }[] = [
  { label: "跟随系统", value: "" },
  {
    label: "无衬线",
    value: "system-ui, -apple-system, 'Segoe UI', sans-serif",
  },
  { label: "衬线", value: "Georgia, 'Times New Roman', serif" },
  { label: "等宽", value: "Consolas, 'Courier New', monospace" },
];

/** 通用面板（应用级外观 + 仓库级 key 同步开关）：草稿与状态自持，直接订阅 store。
 * 主题模式与强调色已迁至「主题」tab（主题插件 + 设置项），此处只保留字号/字体等。 */
export function GeneralSettingsTab() {
  // 应用级外观（跨仓库共享，global.json）：字号 / 字体 / 自动恢复 / 主页布局 / 自动更新
  const fontSize = useSettingsStore((s) => s.fontSize);
  const setFontSize = useSettingsStore((s) => s.setFontSize);
  const fontFamily = useSettingsStore((s) => s.fontFamily);
  const setFontFamily = useSettingsStore((s) => s.setFontFamily);
  const autoRestoreFiles = useSettingsStore((s) => s.autoRestoreFiles);
  const setAutoRestoreFiles = useSettingsStore((s) => s.setAutoRestoreFiles);
  const defaultHomeLayout = useSettingsStore((s) => s.defaultHomeLayout);
  const setDefaultHomeLayout = useSettingsStore((s) => s.setDefaultHomeLayout);
  const autoUpdate = useAppStore((s) => s.autoUpdate);
  const setAutoUpdate = useAppStore((s) => s.setAutoUpdate);
  // API key 随仓库保存（仓库级）
  const vaultConfig = useSettingsStore((s) => s.vaultConfig);
  const setSyncKeys = useSettingsStore((s) => s.setSyncKeys);
  const syncKeys = !!vaultConfig?.syncKeys;

  // 字号用本地草稿 + blur 提交：受控 + 范围校验会拒绝输入中间态（如敲 "1" 准备输 15）导致无法输入
  const [fontSizeDraft, setFontSizeDraft] = useDraftSync(
    fontSize !== undefined ? String(fontSize) : "",
  );

  /** blur/Enter 提交字号；非法值回滚为当前配置值。 */
  const commitFontSize = () => {
    const v = fontSizeDraft.trim();
    if (v === "") {
      void setFontSize(undefined);
      return;
    }
    const n = Number(v);
    if (n >= 12 && n <= 20) {
      void setFontSize(n);
    } else {
      setFontSizeDraft(fontSize !== undefined ? String(fontSize) : ""); // 非法值回滚
    }
  };

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      {/* 字体大小（应用级） */}
      <SettingCard title="字体大小" description="界面字号；留空 = 18">
        <input
          type="number"
          min={12}
          max={20}
          step={1}
          value={fontSizeDraft}
          onChange={(e) => setFontSizeDraft(e.target.value)}
          onBlur={commitFontSize}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
          }}
          placeholder="18"
          className="text-sm rounded px-2 py-1 outline-none max-w-[90px]"
          style={{
            color: "var(--text-primary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>

      {/* 字体（应用级） */}
      <SettingCard title="字体" description="界面字体">
        <DropdownSelect
          value={fontFamily ?? ""}
          onChange={(v) => void setFontFamily(v || undefined)}
          options={FONT_OPTIONS}
          className="text-sm rounded px-2 py-1 max-w-[220px]"
          style={{
            color: "var(--text-secondary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>

      {/* 自动恢复上次打开的文件（应用级）：进入仓库时恢复上次打开的画布/笔记窗口 */}
      <SettingCard
        title="自动恢复上次打开的文件"
        description="进入仓库时恢复上次打开的文件"
      >
        <ToggleSwitch
          checked={autoRestoreFiles}
          onChange={(v) => void setAutoRestoreFiles(v)}
          title="自动恢复上次打开的文件"
        />
      </SettingCard>

      {/* 进仓库时打开主页（应用级）：开启后进入仓库自动切到主页布局；关闭 = 保持恢复上次界面 */}
      <SettingCard
        title="进仓库时打开主页"
        description="进入仓库自动切到主页布局；关闭则恢复上次界面"
      >
        <ToggleSwitch
          checked={defaultHomeLayout}
          onChange={(v) => void setDefaultHomeLayout(v)}
          title="进仓库时打开主页"
        />
      </SettingCard>

      {/* 自动更新（应用级，global.json）：开启后启动时静默检查新版本并自动安装 */}
      <SettingCard
        title="自动更新"
        description="启动时自动检查新版本并安装；关闭 = 不联网检查"
      >
        <ToggleSwitch
          checked={autoUpdate}
          onChange={(v) => void setAutoUpdate(v)}
          title="自动更新"
        />
      </SettingCard>

      {/* API key 随仓库保存（仓库级）：开 = key 明文随 config.json 同步多设备；关 = 仅存本机钥匙串 */}
      <SettingCard
        title="API key 随仓库保存"
        description="key 随仓库同步共用；仓库公开/共享时可能泄露"
      >
        <ToggleSwitch
          checked={syncKeys}
          onChange={(v) => void setSyncKeys(v)}
          title="API key 随仓库保存"
        />
      </SettingCard>

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/general） */}
      <SlotListMount slot="settings/general" />
    </section>
  );
}
