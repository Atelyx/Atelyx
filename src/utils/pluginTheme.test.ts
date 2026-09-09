/**
 * 主题内核纯函数测试（utils/pluginTheme）。
 *
 * 覆盖：派生（启停过滤/重名拒绝/内置同路径）、激活插件解析（命中/回退）、
 * 激活条目解析（内置三态含 system、第三方 variant/缺省）、变量键归一化。
 */
import { describe, it, expect } from "vitest";
import {
  ACCENT_COLOR_KEY,
  BUILTIN_THEME_PLUGIN_ID,
  COLOR_MODE_KEY,
  VARIANT_KEY,
  deriveThemeProviders,
  isThemeColorMode,
  isThemePluginRow,
  normalizeThemeConfig,
  normalizeThemeVarKeys,
  resolveActiveThemeEntry,
  resolveActiveThemePlugin,
  type ThemePluginRow,
  type ThemeProvider,
} from "./pluginTheme";
import type { ThemeDefinition } from "@/types";

function row(id: string, themes: ThemeDefinition[], opts?: { enabled?: boolean; name?: string; accent?: boolean }): ThemePluginRow {
  return {
    id,
    enabled: opts?.enabled ?? true,
    manifest: {
      name: opts?.name ?? id,
      themes,
      ...(opts?.accent ? { themeOptions: { accent: true } } : {}),
    },
  };
}

const lightDef: ThemeDefinition = { id: "light", name: "浅色", colorScheme: "light", variables: {} };
const darkDef: ThemeDefinition = { id: "dark", name: "深色", colorScheme: "dark", variables: {} };
/** 第三方主题条目（避免与内置基底 id 重名，否则会被 derive 拒绝）。 */
const thirdLight: ThemeDefinition = { id: "com.a.light", name: "A 浅色", colorScheme: "light", variables: {} };

/** 内置主题插件行（合成清单形态：浅/深两基底 + accent 声明）。 */
function builtinRow(): ThemePluginRow {
  return {
    id: BUILTIN_THEME_PLUGIN_ID,
    enabled: true,
    manifest: {
      name: "默认主题",
      themes: [lightDef, darkDef],
      themeOptions: { accent: true },
    },
  };
}

function provider(rowInput: ThemePluginRow): ThemeProvider {
  const d = deriveThemeProviders([rowInput]);
  expect(d.providers).toHaveLength(1);
  return d.providers[0];
}

describe("deriveThemeProviders", () => {
  it("过滤停用插件与无 themes 插件", () => {
    const d = deriveThemeProviders([
      row("com.a.theme", [thirdLight]),
      row("com.b.disabled", [darkDef], { enabled: false }),
      row("com.c.tool", [{ id: "x", name: "X", colorScheme: "light", variables: {} }], {}),
    ]);
    expect(d.providers.map((p) => p.pluginId)).toEqual(["com.a.theme", "com.c.tool"]);
  });

  it("内置主题插件经合成清单同路径进入（builtin 标记 + accent 声明）", () => {
    const p = provider(builtinRow());
    expect(p.builtin).toBe(true);
    expect(p.accent).toBe(true);
    expect(p.themes.map((t) => t.id)).toEqual(["light", "dark"]);
  });

  it("第三方条目与内置基底 id 重名：拒绝该条目；全重名则插件不作为提供者", () => {
    const d = deriveThemeProviders([
      row("com.a.theme", [
        { id: "light", name: "占用", colorScheme: "light", variables: {} },
        { id: "nord", name: "Nord", colorScheme: "dark", variables: {} },
      ]),
      row("com.b.theme", [{ id: "dark", name: "占用", colorScheme: "dark", variables: {} }]),
    ]);
    expect(d.rejected).toEqual([
      { pluginId: "com.a.theme", themeId: "light" },
      { pluginId: "com.b.theme", themeId: "dark" },
    ]);
    expect(d.providers.map((p) => p.pluginId)).toEqual(["com.a.theme"]);
    expect(d.providers[0].themes.map((t) => t.id)).toEqual(["nord"]);
  });

  it("内置插件的 light/dark 基底不触发重名拒绝", () => {
    const d = deriveThemeProviders([builtinRow()]);
    expect(d.rejected).toEqual([]);
  });
});

describe("resolveActiveThemePlugin", () => {
  const providers = deriveThemeProviders([builtinRow(), row("com.a.theme", [thirdLight])]).providers;

  it("命中持久化的激活插件 id", () => {
    expect(resolveActiveThemePlugin("com.a.theme", providers)?.pluginId).toBe("com.a.theme");
  });

  it("缺省（未持久化）回退内置主题插件", () => {
    expect(resolveActiveThemePlugin(undefined, providers)?.pluginId).toBe(BUILTIN_THEME_PLUGIN_ID);
  });

  it("未知 id（插件被停用/卸载遗留）回退内置主题插件", () => {
    expect(resolveActiveThemePlugin("com.gone.theme", providers)?.pluginId).toBe(BUILTIN_THEME_PLUGIN_ID);
  });

  it("内置不在时回退第一个提供者；无提供者返回 undefined", () => {
    const only = deriveThemeProviders([row("com.a.theme", [thirdLight])]).providers;
    expect(resolveActiveThemePlugin("com.gone.theme", only)?.pluginId).toBe("com.a.theme");
    expect(resolveActiveThemePlugin("com.a.theme", [])).toBeUndefined();
  });
});

describe("resolveActiveThemeEntry", () => {
  const builtin = provider(builtinRow());

  it("内置深浅模式：light/dark 直取对应基底", () => {
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "light" }, false)?.id).toBe("light");
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "dark" }, true)?.id).toBe("dark");
  });

  it("内置深浅模式 system：按 systemDark 解析", () => {
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "system" }, true)?.id).toBe("dark");
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "system" }, false)?.id).toBe("light");
  });

  it("内置缺省/非法深浅模式：按跟随系统处理", () => {
    expect(resolveActiveThemeEntry(builtin, {}, true)?.id).toBe("dark");
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "blue" }, false)?.id).toBe("light");
  });

  it("第三方：variant 命中条目，否则第一个条目", () => {
    const p = provider(
      row("com.a.theme", [
        { id: "nord-light", name: "Nord 浅", colorScheme: "light", variables: {} },
        { id: "nord-dark", name: "Nord 深", colorScheme: "dark", variables: {} },
      ]),
    );
    expect(resolveActiveThemeEntry(p, { [VARIANT_KEY]: "nord-dark" }, true)?.id).toBe("nord-dark");
    expect(resolveActiveThemeEntry(p, { [VARIANT_KEY]: "nope" }, true)?.id).toBe("nord-light");
    expect(resolveActiveThemeEntry(p, {}, true)?.id).toBe("nord-light");
  });
});

describe("normalizeThemeVarKeys / isThemeColorMode", () => {
  it("变量键补 -- 前缀", () => {
    expect(normalizeThemeVarKeys({ accent: "#123", "--bg": "#fff" })).toEqual({ "--accent": "#123", "--bg": "#fff" });
  });
  it("深浅模式值域校验", () => {
    expect(isThemeColorMode("system")).toBe(true);
    expect(isThemeColorMode("light")).toBe(true);
    expect(isThemeColorMode("dark")).toBe(true);
    expect(isThemeColorMode("blue")).toBe(false);
    expect(isThemeColorMode(undefined)).toBe(false);
  });
});

describe("normalizeThemeConfig（读入归一化/迁移）", () => {
  it("无配置：默认内置主题插件 + 深浅模式跟随系统", () => {
    const out = normalizeThemeConfig({});
    expect(out.theme).toBe(BUILTIN_THEME_PLUGIN_ID);
    expect(out.themeSettings[BUILTIN_THEME_PLUGIN_ID][COLOR_MODE_KEY]).toBe("system");
  });

  it("旧 theme 三态 → 内置主题插件 + 深浅模式设置", () => {
    for (const mode of ["light", "dark", "system"] as const) {
      const out = normalizeThemeConfig({ theme: mode });
      expect(out.theme).toBe(BUILTIN_THEME_PLUGIN_ID);
      expect(out.themeSettings[BUILTIN_THEME_PLUGIN_ID][COLOR_MODE_KEY]).toBe(mode);
    }
  });

  it("旧全局 accentColor → 内置条目强调色（仅当磁盘未设置过）", () => {
    const out = normalizeThemeConfig({ accentColor: "#123456" });
    expect(out.themeSettings[BUILTIN_THEME_PLUGIN_ID][ACCENT_COLOR_KEY]).toBe("#123456");
    // 磁盘已设置强调色时不被旧值覆盖
    const kept = normalizeThemeConfig({
      accentColor: "#000000",
      themeSettings: { [BUILTIN_THEME_PLUGIN_ID]: { [ACCENT_COLOR_KEY]: "#abcdef" } },
    });
    expect(kept.themeSettings[BUILTIN_THEME_PLUGIN_ID][ACCENT_COLOR_KEY]).toBe("#abcdef");
  });

  it("磁盘 themeSettings 优先（含第三方条目透传），内置条目补默认深浅模式", () => {
    const out = normalizeThemeConfig({
      theme: "com.a.theme",
      themeSettings: { "com.a.theme": { variant: "nord-dark" } },
    });
    expect(out.theme).toBe("com.a.theme");
    expect(out.themeSettings["com.a.theme"].variant).toBe("nord-dark");
    expect(out.themeSettings[BUILTIN_THEME_PLUGIN_ID][COLOR_MODE_KEY]).toBe("system");
  });

  it("磁盘 colorMode 已存在时旧 theme 三态不覆盖（磁盘优先）", () => {
    const out = normalizeThemeConfig({
      theme: "dark",
      themeSettings: { [BUILTIN_THEME_PLUGIN_ID]: { [COLOR_MODE_KEY]: "light" } },
    });
    expect(out.themeSettings[BUILTIN_THEME_PLUGIN_ID][COLOR_MODE_KEY]).toBe("light");
  });
});

describe("deriveThemeProviders 形状防御（畸形清单 raw-cast 兜底）", () => {
  it("themes 非数组/畸形条目不崩溃且不作为提供者", () => {
    const d = deriveThemeProviders([
      row("com.a.theme", [{ id: "ok", name: "OK", colorScheme: "light", variables: {} }]),
      { id: "com.b.bad", enabled: true, manifest: { name: "坏", themes: "not-array" as unknown as ThemeDefinition[] } },
      {
        id: "com.c.bad",
        enabled: true,
        manifest: {
          name: "坏条目",
          themes: [
            { id: "x", name: "X", colorScheme: "light", variables: null as unknown as Record<string, string> },
          ],
        },
      },
    ]);
    expect(d.providers.map((p) => p.pluginId)).toEqual(["com.a.theme"]);
  });
});

describe("isThemePluginRow（守恒判定口径）", () => {
  it("合法 themes 判定为主题；仅内置基底重名条目判定为非主题（与 derive 同规则）", () => {
    expect(isThemePluginRow(row("com.a.theme", [thirdLight]))).toBe(true);
    expect(isThemePluginRow(builtinRow())).toBe(true);
    expect(isThemePluginRow(row("com.b.theme", [{ id: "light", name: "占用", colorScheme: "light", variables: {} }]))).toBe(false);
    expect(isThemePluginRow(row("com.c.tool", []))).toBe(false);
  });
});

describe("ACCENT_COLOR_KEY 常量", () => {
  it("强调色键名为 accentColor", () => {
    expect(ACCENT_COLOR_KEY).toBe("accentColor");
  });
});
