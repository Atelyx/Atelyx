/**
 * 主题内核纯函数：从已启用插件派生主题提供者、解析激活主题插件与激活主题条目。
 * 主题系统是内核原语（切换/解析/应用），「提供什么主题」由主题插件（含内置条目）声明——
 * 本模块只做机制，不含任何主题内容；DOM 副作用归 useAppearance（分层：utils 纯函数）。
 */
import type { ThemeDefinition } from "@/types";

/** 内置主题插件 id（与 Rust BUILTIN_PLUGINS 合成清单 id 一致，两侧需同步）。 */
export const BUILTIN_THEME_PLUGIN_ID = "builtin.theme";

/** 内置基底主题条目 id（与合成清单 themes 条目一致；第三方主题条目禁止占用，防冒名）。 */
export const BUILTIN_THEME_LIGHT_ID = "light";
export const BUILTIN_THEME_DARK_ID = "dark";

/** 内置主题插件深浅模式设置项键（值域 ThemeColorMode）。 */
export const COLOR_MODE_KEY = "colorMode";
/** 预置强调色设置项键（任何声明 themeOptions.accent 的主题插件共用；值自动应用 --accent 系列）。 */
export const ACCENT_COLOR_KEY = "accentColor";
/** 第三方主题条目选择键（自定义设置项写该键选中 themes 条目；缺省用第一个条目）。 */
export const VARIANT_KEY = "variant";

/** 内置主题插件深浅模式值域：跟随系统 / 浅色 / 深色。 */
export type ThemeColorMode = "system" | "light" | "dark";

/** 派生输入（宽松切片：pluginStore 行即可，测试友好）。 */
export interface ThemePluginRow {
  id: string;
  enabled: boolean;
  manifest: {
    name?: string;
    themes?: ThemeDefinition[];
    themeOptions?: { accent?: boolean };
  };
}

/** 一个主题提供者（已启用且含 themes 的插件；内置主题插件经合成清单与第三方同路径进入）。 */
export interface ThemeProvider {
  pluginId: string;
  /** 插件显示名（下拉列表展示）。 */
  name: string;
  /** 主题条目（内置 = 浅/深基底；第三方 = 声明条目）。 */
  themes: ThemeDefinition[];
  /** 是否声明预置强调色设置项（themeOptions.accent）。 */
  accent: boolean;
  /** 是否内置主题插件（其设置项含内核预置深浅模式）。 */
  builtin: boolean;
}

/** 主题派生结果（providers + 被拒绝的条目，供测试与展示）。 */
export interface ThemeDerivation {
  providers: ThemeProvider[];
  /** 与内置基底 id 重名被拒绝的第三方条目。 */
  rejected: Array<{ pluginId: string; themeId: string }>;
}

/** 内置基底 id 集合（第三方主题条目禁止占用）。 */
const BUILTIN_THEME_IDS: ReadonlySet<string> = new Set([BUILTIN_THEME_LIGHT_ID, BUILTIN_THEME_DARK_ID]);

/**
 * 派生主题提供者列表：已启用且 manifest.themes 非空（校验后恒非空）的插件。
 * 与内置基底 id 重名的第三方条目拒绝（防冒名，与 ViewKind 防劫持同纪律）。
 * 形状防御：畸形 themes（非数组/条目缺 id/name/colorScheme/variables）来自校验失败后 raw-cast
 * 兜底清单（Rust 与前端校验已从源头拦截，此处为双保险，防漏网清单击穿整窗）。
 */
export function deriveThemeProviders(plugins: Array<ThemePluginRow>): ThemeDerivation {
  const providers: ThemeProvider[] = [];
  const rejected: Array<{ pluginId: string; themeId: string }> = [];
  for (const p of plugins) {
    if (!p.enabled) continue;
    const rawThemes = p.manifest.themes;
    if (!Array.isArray(rawThemes) || rawThemes.length === 0) continue;
    const themes = rawThemes.filter(
      (t): t is ThemeDefinition =>
        typeof t === "object" &&
        t !== null &&
        typeof (t as ThemeDefinition).id === "string" &&
        typeof (t as ThemeDefinition).name === "string" &&
        ((t as ThemeDefinition).colorScheme === "light" || (t as ThemeDefinition).colorScheme === "dark") &&
        typeof (t as ThemeDefinition).variables === "object" &&
        (t as ThemeDefinition).variables !== null &&
        !Array.isArray((t as ThemeDefinition).variables),
    );
    if (themes.length === 0) continue;
    const builtin = p.id === BUILTIN_THEME_PLUGIN_ID;
    let items = themes;
    if (!builtin) {
      const clash = themes.filter((t) => BUILTIN_THEME_IDS.has(t.id));
      for (const t of clash) rejected.push({ pluginId: p.id, themeId: t.id });
      if (clash.length > 0) items = themes.filter((t) => !BUILTIN_THEME_IDS.has(t.id));
      if (items.length === 0) continue;
    }
    providers.push({
      pluginId: p.id,
      name: p.manifest.name ?? p.id,
      themes: items,
      accent: p.manifest.themeOptions?.accent === true,
      builtin,
    });
  }
  return { providers, rejected };
}

/** 是否主题提供者（与 deriveThemeProviders 同一口径：合法 themes 条目去内置基底重名后非空）。
 *  供管理页守恒禁用等场景对单个插件行判定，与 Rust 侧 plugin_is_theme 规则一致。 */
export function isThemePluginRow(p: Pick<ThemePluginRow, "id" | "manifest">): boolean {
  return deriveThemeProviders([{ ...p, enabled: true }]).providers.length > 0;
}

/**
 * 解析激活主题插件：themeId（持久化的激活插件 id）→ 提供者。
 * 未知/失效 id（插件被停用/卸载后遗留）→ 回退内置主题插件（若存在）否则第一个提供者；
 * 无任何提供者（理论不可达，守恒规则保证 ≥1）→ undefined。
 */
export function resolveActiveThemePlugin(
  themeId: string | undefined,
  providers: ThemeProvider[],
): ThemeProvider | undefined {
  if (providers.length === 0) return undefined;
  if (themeId !== undefined) {
    const hit = providers.find((p) => p.pluginId === themeId);
    if (hit) return hit;
  }
  return providers.find((p) => p.pluginId === BUILTIN_THEME_PLUGIN_ID) ?? providers[0];
}

/**
 * 解析激活主题条目：提供者 + 其设置项值 → 主题条目。
 * 内置：深浅模式值（system 按 systemDark 解析）→ 对应基底的条目（缺省 = 跟随系统）；
 * 第三方：variant 键命中 themes 条目，否则第一个条目（缺省条目）。
 */
export function resolveActiveThemeEntry(
  provider: ThemeProvider,
  settings: Record<string, unknown>,
  systemDark: boolean,
): ThemeDefinition | undefined {
  if (provider.themes.length === 0) return undefined;
  if (provider.builtin) {
    const mode = settings[COLOR_MODE_KEY];
    const target: "light" | "dark" =
      mode === "light" ? "light"
      : mode === "dark" ? "dark"
      : systemDark ? "dark"
      : "light";
    return provider.themes.find((t) => t.colorScheme === target) ?? provider.themes[0];
  }
  const variant = settings[VARIANT_KEY];
  const hit = typeof variant === "string" ? provider.themes.find((t) => t.id === variant) : undefined;
  return hit ?? provider.themes[0];
}

/** 变量键归一化：补 `--` 前缀（插件可省略；应用时统一写 :root inline style）。 */
export function normalizeThemeVarKeys(variables: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(variables)) {
    out[key.startsWith("--") ? key : `--${key}`] = value;
  }
  return out;
}

/** 深浅模式是否为合法值（旧 theme 三态判定 + 读取迁移用）。 */
export function isThemeColorMode(value: unknown): value is ThemeColorMode {
  return value === "system" || value === "light" || value === "dark";
}

/** 内置主题插件默认设置（深浅模式跟随系统；强调色缺省 = 默认金，键缺失即默认）。 */
export const DEFAULT_BUILTIN_THEME_SETTINGS: Record<string, unknown> = { [COLOR_MODE_KEY]: "system" };

/** 应用级主题配置读入归一化（纯函数可直测）：磁盘 `themeSettings` 优先，旧 `theme` 三态 /
 *  旧全局 `accentColor` 迁移进内置主题插件条目。供 settingsStore.load 消费。 */
export interface LegacyThemeConfig {
  /** 旧三态 light/dark/system 或新值 = 激活主题插件 id。 */
  theme?: string;
  /** 旧版全局强调色（hex；磁盘未设置过时才迁移）。 */
  accentColor?: string;
  themeSettings?: Record<string, Record<string, unknown>>;
}

export function normalizeThemeConfig(
  cfg: LegacyThemeConfig,
): { theme: string; themeSettings: Record<string, Record<string, unknown>> } {
  const diskSettings = cfg.themeSettings ?? {};
  const diskBuiltin = diskSettings[BUILTIN_THEME_PLUGIN_ID] ?? {};
  // 顺序：默认 → 旧值迁移（仅当磁盘未显式设置）→ 磁盘条目覆盖（磁盘权威）
  const builtinSettings: Record<string, unknown> = { ...DEFAULT_BUILTIN_THEME_SETTINGS };
  let theme: string = BUILTIN_THEME_PLUGIN_ID;
  // 旧 theme 三态 → 内置主题插件 + 深浅模式设置；新值 = 激活主题插件 id
  if (isThemeColorMode(cfg.theme)) {
    theme = BUILTIN_THEME_PLUGIN_ID;
    if (diskBuiltin[COLOR_MODE_KEY] === undefined) builtinSettings[COLOR_MODE_KEY] = cfg.theme;
  } else if (typeof cfg.theme === "string") {
    theme = cfg.theme;
  }
  // 旧全局强调色 → 内置条目强调色（仅当磁盘未设置过）
  if (cfg.accentColor && diskBuiltin[ACCENT_COLOR_KEY] === undefined) {
    builtinSettings[ACCENT_COLOR_KEY] = cfg.accentColor;
  }
  Object.assign(builtinSettings, diskBuiltin);
  return { theme, themeSettings: { ...diskSettings, [BUILTIN_THEME_PLUGIN_ID]: builtinSettings } };
}
