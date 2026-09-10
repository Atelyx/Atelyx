/**
 * 「主题」设置 tab（内核设置页）：下拉选择主题插件（默认主题 + 已启用的用户主题插件），
 * 下方渲染激活插件的设置项——
 * - 默认主题插件：深浅模式（跟随系统/浅色/深色，内核预置）+ 强调色（内核预置，值自动应用 --accent）；
 * - 用户主题插件：声明 themeOptions.accent 时渲染预置强调色卡 + registerThemeSetting 注册的自定义设置区块。
 *
 * 主题系统是内核原语（切换/解析/应用/存储框架）；「提供什么主题与设置项」由主题插件声明。
 */
import { Check, RotateCcw } from "lucide-react";
import { useMemo } from "react";
import { DropdownSelect, type DropdownOption } from "@/components/common/DropdownSelect";
import { SettingCard } from "@/components/settings/SettingCard";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { useDebouncedDraft } from "@/hooks/useDraftSync";
import { usePluginStore } from "@/stores/pluginStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { DEFAULT_ACCENT, foregroundFor } from "@/utils/color";
import {
  ACCENT_COLOR_KEY,
  COLOR_MODE_KEY,
  deriveThemeProviders,
  resolveActiveThemePlugin,
  type ThemeColorMode,
} from "@/utils/pluginTheme";

/** 默认主题插件的深浅模式选项（值 = 主题条目/解析目标；跟随系统 = 按 prefers-color-scheme 实时解析）。 */
const COLOR_MODE_OPTIONS: { label: string; value: ThemeColorMode }[] = [
  { label: "跟随系统", value: "system" },
  { label: "浅色", value: "light" },
  { label: "深色", value: "dark" },
];

/** 强调色预设色板（600/700 阶深色系：白字对比 ≥ 5:1，默认金由「恢复默认」按钮回归；取色器可自由选色）。 */
const ACCENT_PRESETS = ["#2563eb", "#0d9488", "#7c3aed", "#dc2626", "#15803d"];

/** 主题设置值兜底（条目缺失时复用常量，避免每渲染新引用导致无谓重渲染）。 */
const EMPTY_SETTINGS: Record<string, unknown> = {};

/** 强调色设置卡（内核预置设置项）：预设色板 + 取色器 + 恢复默认；值存 themeSettings[pluginId].accentColor。 */
function AccentSettingCard({ pluginId }: { pluginId: string }) {
  const accentColor = useSettingsStore(
    (s) => s.themeSettings[pluginId]?.[ACCENT_COLOR_KEY] as string | undefined,
  );
  const setThemeSetting = useSettingsStore((s) => s.setThemeSetting);
  const [accentDraft, commitAccentDraft] = useDebouncedDraft(
    accentColor ?? DEFAULT_ACCENT,
    (v) => void setThemeSetting(pluginId, ACCENT_COLOR_KEY, v),
  );
  return (
    <SettingCard title="强调色" description="界面强调色（按钮 / 选中高亮 / 画布箭头）">
      <div className="flex items-center gap-2">
        <div className="flex items-center gap-1.5">
          {ACCENT_PRESETS.map((c) => {
            const active = accentColor?.toLowerCase() === c;
            return (
              <button
                key={c}
                onClick={() => commitAccentDraft(c)}
                title={`强调色 ${c}`}
                className="w-5 h-5 rounded-full flex items-center justify-center transition hover:scale-110 flex-shrink-0"
                style={{ background: c }}
              >
                {active && <Check size={11} style={{ color: foregroundFor(c) }} />}
              </button>
            );
          })}
        </div>
        <input
          type="color"
          value={accentDraft}
          onChange={(e) => commitAccentDraft(e.target.value)}
          title="自定义颜色"
          className="w-6 h-6 rounded cursor-pointer bg-transparent p-0 border-0"
        />
        <button
          onClick={() => commitAccentDraft(DEFAULT_ACCENT)}
          title="恢复默认金色"
          className="flex items-center gap-1 text-xs rounded px-1.5 py-1 hover:bg-[var(--hover)] flex-shrink-0"
          style={{ color: "var(--text-secondary)" }}
        >
          <RotateCcw size={12} />
          恢复默认
        </button>
      </div>
    </SettingCard>
  );
}

/** 默认主题插件的设置区：深浅模式 + 强调色。pluginId = 解析后的激活插件 id（可能回退，非原始持久化值）。 */
function BuiltinThemeSettings({
  pluginId,
  settings,
}: {
  pluginId: string;
  settings: Record<string, unknown>;
}) {
  const setThemeSetting = useSettingsStore((s) => s.setThemeSetting);
  const colorMode: ThemeColorMode =
    settings[COLOR_MODE_KEY] === "light" || settings[COLOR_MODE_KEY] === "dark"
      ? (settings[COLOR_MODE_KEY] as ThemeColorMode)
      : "system";
  return (
    <>
      <SettingCard
        title="深浅模式"
        description="跟随系统 = 按系统外观自动切换浅色/深色"
      >
        <div className="flex items-center gap-1">
          {COLOR_MODE_OPTIONS.map((o) => (
            <button
              key={o.value}
              onClick={() => void setThemeSetting(pluginId, COLOR_MODE_KEY, o.value)}
              className={`text-xs rounded px-2.5 py-1.5 transition ${
                colorMode === o.value
                  ? "bg-[var(--accent)] text-[var(--accent-fg)]"
                  : "text-[var(--text-secondary)] hover:bg-[var(--hover)]"
              }`}
            >
              {o.label}
            </button>
          ))}
        </div>
      </SettingCard>
      {/* key={pluginId}：切插件时强制重挂载，取色器草稿/高亮与真实生效的 --accent 对齐 */}
      <AccentSettingCard key={pluginId} pluginId={pluginId} />
    </>
  );
}

/** 用户主题插件的设置区：声明的强调色 + registerThemeSetting 注册的自定义设置区块。 */
function PluginThemeSettings({
  pluginId,
  settings,
}: {
  pluginId: string;
  settings: Record<string, unknown>;
}) {
  // uiRevision 订阅 + 渲染时 getState() 读取（与 SettingsModal 的 pluginSettings 同模式）：
  // registerThemeSetting 注册/撤销经 notify → uiRevision++ 触发重渲染，注册表重读
  usePluginStore((s) => s.uiRevision);
  const themeOptions = usePluginStore((s) => s.plugins[pluginId]?.manifest.themeOptions);
  const setThemeSetting = useSettingsStore((s) => s.setThemeSetting);
  const regs = usePluginStore.getState().pluginThemeSettings(pluginId);
  const onChange = useMemo(
    () => (key: string, value: unknown) => void setThemeSetting(pluginId, key, value),
    [pluginId, setThemeSetting],
  );
  return (
    <>
      {themeOptions?.accent && <AccentSettingCard key={pluginId} pluginId={pluginId} />}
      {regs.map((r) => {
        const Comp = r.component;
        return (
          <SettingCard key={r.key} title={r.label} description="">
            <Comp value={settings} onChange={onChange} />
          </SettingCard>
        );
      })}
      {!themeOptions?.accent && regs.length === 0 && (
        <div className="text-sm" style={{ color: "var(--text-muted)" }}>
          该主题没有可设置的选项；配色切换等设置由主题插件提供
        </div>
      )}
    </>
  );
}

/** 「主题」设置页：下拉选主题插件 + 激活插件设置区。 */
export function ThemeSettingsTab() {
  const plugins = usePluginStore((s) => s.plugins);
  const theme = useSettingsStore((s) => s.theme);
  const themeSettings = useSettingsStore((s) => s.themeSettings);
  const setThemePlugin = useSettingsStore((s) => s.setThemePlugin);

  const providers = useMemo(
    () => deriveThemeProviders(Object.values(plugins)).providers,
    [plugins],
  );
  const active = useMemo(() => resolveActiveThemePlugin(theme, providers), [theme, providers]);
  const settings = active ? themeSettings[active.pluginId] ?? EMPTY_SETTINGS : EMPTY_SETTINGS;

  const options = useMemo<DropdownOption[]>(() => {
    const opts: DropdownOption[] = [];
    for (const p of providers) {
      if (p.builtin) {
        opts.push({ value: p.pluginId, label: p.name, group: "默认主题" });
      } else {
        // 不显示条目数（配色切换等设置由主题插件自备，见 registerThemeSetting）
        opts.push({ value: p.pluginId, label: p.name, group: "用户主题" });
      }
    }
    return opts;
  }, [providers]);

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      <SettingCard title="主题" description="选择主题插件应用主题；设置项由所选主题提供">
        <DropdownSelect
          value={active?.pluginId ?? ""}
          onChange={(v) => void setThemePlugin(v)}
          options={options}
          placeholder="未选择主题"
          emptyText="暂无可用主题插件"
          className="text-sm rounded px-2 py-1 max-w-[260px]"
          style={{
            color: "var(--text-secondary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>
      {active?.builtin ? (
        <BuiltinThemeSettings pluginId={active.pluginId} settings={settings} />
      ) : active ? (
        <PluginThemeSettings pluginId={active.pluginId} settings={settings} />
      ) : (
        <div className="text-sm" style={{ color: "var(--text-muted)" }}>
          暂无可用主题插件
        </div>
      )}

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/theme） */}
      <SlotListMount slot="settings/theme" />
    </section>
  );
}
