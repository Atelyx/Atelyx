/**
 * 应用外观应用（主题/字号/字体 + 系统主题跟随 + 主题插件应用）。
 * 主窗口（App）与撕裂窗口（PanelWindowRoot）共用：撕裂窗口是独立 webview，
 * 需要自行应用同一套外观（settingsStore 应用级配置，两窗口各自读盘）。
 *
 * 应用顺序（分层：基础方案 < 主题插件变量 < 用户设置项）：
 * 1. `.dark` class + color-scheme（由 CSS 内 :root/:root.dark 承担）；
 * 2. 激活主题条目的 variables（inline style 于 :root，变更前回撤上一次写入的键）；
 * 3. 该主题插件的强调色设置项（用户偏好，压过主题 variables 的 --accent）。
 */
import { useEffect, useMemo, useRef, useState } from "react";
import { useSettingsStore } from "@/stores/settingsStore";
import { usePluginStore } from "@/stores/pluginStore";
import { darkenHex, foregroundFor } from "@/utils/color";
import {
  ACCENT_COLOR_KEY,
  deriveThemeProviders,
  normalizeThemeVarKeys,
  resolveActiveThemeEntry,
  resolveActiveThemePlugin,
} from "@/utils/pluginTheme";

/** 主题设置值兜底（条目缺失时复用常量，避免每渲染新引用导致无谓重渲染）。 */
const EMPTY_SETTINGS: Record<string, unknown> = {};

export function useAppearance(): void {
  const theme = useSettingsStore((s) => s.theme);
  const themeSettings = useSettingsStore((s) => s.themeSettings);
  const fontSize = useSettingsStore((s) => s.fontSize);
  const fontFamily = useSettingsStore((s) => s.fontFamily);

  // 跟随系统：监听 prefers-color-scheme 变化（默认主题插件深浅模式 = 跟随系统时实时生效）
  const [systemDark, setSystemDark] = useState(() =>
    window.matchMedia("(prefers-color-scheme: dark)").matches,
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (e: MediaQueryListEvent) => setSystemDark(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  // 解析激活主题：已启用主题插件（含默认主题插件，同一清单路径）→ 激活插件 → 激活条目
  const plugins = usePluginStore((s) => s.plugins);
  const providers = useMemo(
    () => deriveThemeProviders(Object.values(plugins)).providers,
    [plugins],
  );
  const active = useMemo(() => {
    const provider = resolveActiveThemePlugin(theme, providers);
    if (!provider) return undefined;
    return {
      provider,
      entry: resolveActiveThemeEntry(provider, themeSettings[provider.pluginId] ?? EMPTY_SETTINGS, systemDark),
    };
  }, [theme, providers, themeSettings, systemDark]);

  /** 上一次写入 :root 的变量键（主题 + 强调色统一回撤集合）。 */
  const prevVarKeysRef = useRef<string[]>([]);

  // 主题 class 应用（分层：store 只存状态，DOM 副作用归 hook）
  useEffect(() => {
    document.documentElement.classList.toggle("dark", active?.entry?.colorScheme === "dark");
  }, [active]);

  // 主题变量 + 强调色应用（单一真相源，避免变量 effect 与强调色 effect 争抢 --accent）：
  // final = 激活条目的 variables（归一化）+ 该主题插件的强调色设置项覆盖（用户设置最后应用，
  // 压过主题 variables 里的 --accent；无合法强调色时回退主题变量/默认金）。变更前统一回撤上一次
  // 写入的键，主题插件停用/卸载后不留残留变量。
  const accentColor = active
    ? (themeSettings[active.provider.pluginId]?.[ACCENT_COLOR_KEY] as string | undefined)
    : undefined;
  useEffect(() => {
    const root = document.documentElement;
    for (const key of prevVarKeysRef.current) root.style.removeProperty(key);
    const vars: Record<string, string> = {};
    if (active?.entry) {
      Object.assign(vars, normalizeThemeVarKeys(active.entry.variables));
    }
    if (accentColor && /^#[0-9a-fA-F]{6}$/.test(accentColor)) {
      vars["--accent"] = accentColor;
      vars["--accent-hover"] = darkenHex(accentColor);
      vars["--accent-fg"] = foregroundFor(accentColor);
    }
    const keys: string[] = [];
    for (const [key, value] of Object.entries(vars)) {
      root.style.setProperty(key, value);
      keys.push(key);
    }
    prevVarKeysRef.current = keys;
  }, [active, accentColor]);

  // 字体（应用级）：覆盖 :root font-size / font-family，空值回默认（CSS 默认）
  useEffect(() => {
    const root = document.documentElement;
    root.style.fontSize = fontSize ? `${fontSize}px` : "";
    root.style.fontFamily = fontFamily ?? "";
  }, [fontSize, fontFamily]);
}
