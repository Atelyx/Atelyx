/**
 * 「主题」设置 tab（内核设置页）：下拉列出全部可选主题——默认主题插件声明的一组主题（「默认」=
 * 浅/深基底按跟随系统解析，其余条目各算一套皮肤，如内置「极光」）与用户主题插件（按插件各一条）；
 * 下方渲染所选主题的设置项：「默认」额外带深浅模式（跟随系统/浅色/深色），两者都吃预置强调色卡
 * 与 registerThemeSetting 注册的自定义设置区块。
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
import type { ThemeDefinition } from "@/types";
import {
  ACCENT_COLOR_KEY,
  BUILTIN_THEME_DARK_ID,
  BUILTIN_THEME_LIGHT_ID,
  COLOR_MODE_KEY,
  VARIANT_KEY,
  deriveThemeProviders,
  resolveActiveThemePlugin,
  type ThemeColorMode,
  type ThemeProvider,
} from "@/utils/pluginTheme";

/** 默认主题行的「默认」项取值（浅/深基底，深浅模式在其设置项里选）。 */
const BASE_SKIN = "base";

/** 一个下拉候选项的落点：默认主题行的皮肤写 variant（缺省 = 默认基底），用户主题插件只切插件。 */
interface ThemeChoice {
  pluginId: string;
  /** 默认主题行才填：null = 默认基底（清掉 variant 回到浅/深），字符串 = 该皮肤条目 id。 */
  skin?: string | null;
}

/** 默认主题行的基底条目（其深浅由 colorMode 决定，不作为独立的下拉项）。 */
function isBaseThemeEntry(t: ThemeDefinition): boolean {
  return t.id === BUILTIN_THEME_LIGHT_ID || t.id === BUILTIN_THEME_DARK_ID;
}

/** 展开主题候选：默认主题行列「默认」+ 各皮肤条目，其余插件各一条（其内部配色切换由插件自备设置项承担）。 */
function themeChoices(providers: ThemeProvider[]): {
  value: string;
  label: string;
  group: string;
  choice: ThemeChoice;
}[] {
  const list: { value: string; label: string; group: string; choice: ThemeChoice }[] = [];
  for (const p of providers) {
    if (p.builtin) {
      list.push({
        value: `${p.pluginId}::${BASE_SKIN}`,
        label: "默认",
        group: p.name,
        choice: { pluginId: p.pluginId, skin: null },
      });
      for (const t of p.themes.filter((t) => !isBaseThemeEntry(t))) {
        list.push({
          value: `${p.pluginId}::${t.id}`,
          label: t.name,
          group: p.name,
          choice: { pluginId: p.pluginId, skin: t.id },
        });
      }
    } else {
      list.push({ value: p.pluginId, label: p.name, group: "用户主题", choice: { pluginId: p.pluginId } });
    }
  }
  return list;
}

/** 强调色预设色板（600/700 阶深色系：白字对比 ≥ 5:1，默认金由「恢复默认」按钮回归；取色器可自由选色）。 */
const ACCENT_PRESETS = ["#2563eb", "#0d9488", "#7c3aed", "#dc2626", "#15803d"];

/** 主题设置值兜底（条目缺失时复用常量，避免每渲染新引用导致无谓重渲染）。 */
const EMPTY_SETTINGS: Record<string, unknown> = {};

/** 默认主题行「默认」项的深浅模式选项（值 = 解析目标；跟随系统 = 按 prefers-color-scheme 实时解析）。 */
const COLOR_MODE_OPTIONS: { label: string; value: ThemeColorMode }[] = [
  { label: "跟随系统", value: "system" },
  { label: "浅色", value: "light" },
  { label: "深色", value: "dark" },
];

/** 默认主题行「默认」项的设置卡：深浅模式（皮肤项没有这档，其明暗固定）。 */
function BuiltinModeCard({
  pluginId,
  settings,
}: {
  pluginId: string;
  settings: Record<string, unknown>;
}) {
  const setThemeSetting = useSettingsStore((s) => s.setThemeSetting);
  // 兜底保持 "system"：与 utils/pluginTheme 的 resolveActiveThemeEntry 同口径
  // （未知/缺失值按跟随系统解析），否则设置页高亮与实际外观会相反
  const colorMode: ThemeColorMode =
    settings[COLOR_MODE_KEY] === "light" || settings[COLOR_MODE_KEY] === "dark"
      ? (settings[COLOR_MODE_KEY] as ThemeColorMode)
      : "system";
  return (
    <SettingCard title="深浅模式" description="跟随系统 = 按系统外观自动切换浅色/深色">
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
  );
}

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

/** 「主题」设置页：下拉列出各主题 + 所选主题的设置区。 */
export function ThemeSettingsTab() {
  const plugins = usePluginStore((s) => s.plugins);
  const theme = useSettingsStore((s) => s.theme);
  const themeSettings = useSettingsStore((s) => s.themeSettings);
  const setThemePlugin = useSettingsStore((s) => s.setThemePlugin);
  const setThemeSetting = useSettingsStore((s) => s.setThemeSetting);

  const providers = useMemo(
    () => deriveThemeProviders(Object.values(plugins)).providers,
    [plugins],
  );
  const active = useMemo(() => resolveActiveThemePlugin(theme, providers), [theme, providers]);
  const settings = active ? themeSettings[active.pluginId] ?? EMPTY_SETTINGS : EMPTY_SETTINGS;

  const choices = useMemo(() => themeChoices(providers), [providers]);
  const options = useMemo<DropdownOption[]>(
    () => choices.map((c) => ({ value: c.value, label: c.label, group: c.group })),
    [choices],
  );
  // 当前选中项：默认主题行按皮肤回显（variant 命中某个皮肤条目 = 该皮肤，否则 = 「默认」基底），
  // 其余插件回显插件 id；与 resolveActiveThemeEntry 同口径，否则下拉显示会与实际生效的外观相反
  const selected = useMemo(() => {
    if (!active) return "";
    if (!active.builtin) return active.pluginId;
    const skin = themeSettings[active.pluginId]?.[VARIANT_KEY];
    const known =
      typeof skin === "string" && choices.some((c) => c.value === `${active.pluginId}::${skin}`);
    return `${active.pluginId}::${known ? skin : BASE_SKIN}`;
  }, [active, themeSettings, choices]);
  /** 「默认」基底是否在用（用它决定是否显示深浅模式卡）。 */
  const baseSkinActive = !!active?.builtin && selected.endsWith(`::${BASE_SKIN}`);

  /** 选主题：默认主题行的皮肤写 variant（选「默认」= 清掉键回到浅/深基底，其余设置项都不动）。 */
  const pickTheme = (value: string) => {
    const hit = choices.find((c) => c.value === value);
    if (!hit) return;
    void setThemePlugin(hit.choice.pluginId);
    if (hit.choice.skin !== undefined) {
      void setThemeSetting(hit.choice.pluginId, VARIANT_KEY, hit.choice.skin ?? undefined);
    }
  };

  return (
    <section className="flex-1 p-5 overflow-auto space-y-4">
      <SettingCard title="主题" description="选择主题；默认主题内含多个主题与皮肤，设置项由所选主题给出">
        <DropdownSelect
          value={selected}
          onChange={pickTheme}
          options={options}
          placeholder="未选择主题"
          emptyText="暂无可用主题"
          className="text-sm rounded px-2 py-1 max-w-[260px]"
          style={{
            color: "var(--text-secondary)",
            background: "var(--input-bg)",
            border: "1px solid var(--input-border)",
          }}
        />
      </SettingCard>
      {active ? (
        <>
          {baseSkinActive && <BuiltinModeCard pluginId={active.pluginId} settings={settings} />}
          <PluginThemeSettings pluginId={active.pluginId} settings={settings} />
        </>
      ) : (
        <div className="text-sm" style={{ color: "var(--text-muted)" }}>
          暂无可用主题
        </div>
      )}

      {/* 插件贡献的设置区块（ctx.slots.registerUi 槽名 settings/theme） */}
      <SlotListMount slot="settings/theme" />
    </section>
  );
}
