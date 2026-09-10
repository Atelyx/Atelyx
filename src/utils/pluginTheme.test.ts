/**
 * 主题内核纯函数测试（utils/pluginTheme）。
 *
 * 覆盖：派生（启停过滤/重名丢弃/默认主题插件同路径）、激活插件解析（命中/回退）、
 * 激活条目解析（默认主题插件三态含 system、其余插件 variant/缺省）、变量键归一化。 */
import { describe, it, expect } from "vitest";
import {
  ACCENT_COLOR_KEY,
  BUILTIN_THEME_PLUGIN_ID,
  COLOR_MODE_KEY,
  VARIANT_KEY,
  deriveThemeProviders,
  isThemePluginRow,
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
/** 用户主题条目（避免与基础主题条目 id 重名，否则条目会被丢弃）。 */
const thirdLight: ThemeDefinition = { id: "com.a.light", name: "A 浅色", colorScheme: "light", variables: {} };

/** 默认主题插件行（清单形态：浅/深两基底 + accent 声明）。 */
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

  it("默认主题插件与其他插件同路径进入（builtin 标记 + accent 声明）", () => {
    const p = provider(builtinRow());
    expect(p.builtin).toBe(true);
    expect(p.accent).toBe(true);
    expect(p.themes.map((t) => t.id)).toEqual(["light", "dark"]);
  });

  it("用户条目与基础主题条目 id 重名：丢弃该条目；全重名则插件不作为提供者", () => {
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

  it("默认主题插件的 light/dark 基底不触发重名丢弃", () => {
    const d = deriveThemeProviders([builtinRow()]);
    expect(d.rejected).toEqual([]);
  });
});

describe("resolveActiveThemePlugin", () => {
  const providers = deriveThemeProviders([builtinRow(), row("com.a.theme", [thirdLight])]).providers;

  it("命中持久化的激活插件 id", () => {
    expect(resolveActiveThemePlugin("com.a.theme", providers)?.pluginId).toBe("com.a.theme");
  });

  it("缺省（未持久化）回退默认主题插件", () => {
    expect(resolveActiveThemePlugin(undefined, providers)?.pluginId).toBe(BUILTIN_THEME_PLUGIN_ID);
  });

  it("未知 id（插件被停用/卸载遗留）回退默认主题插件", () => {
    expect(resolveActiveThemePlugin("com.gone.theme", providers)?.pluginId).toBe(BUILTIN_THEME_PLUGIN_ID);
  });

  it("默认主题插件不在时回退第一个提供者；无提供者返回 undefined", () => {
    const only = deriveThemeProviders([row("com.a.theme", [thirdLight])]).providers;
    expect(resolveActiveThemePlugin("com.gone.theme", only)?.pluginId).toBe("com.a.theme");
    expect(resolveActiveThemePlugin("com.a.theme", [])).toBeUndefined();
  });
});

describe("resolveActiveThemeEntry", () => {
  const builtin = provider(builtinRow());

  it("默认主题插件深浅模式：light/dark 直取对应基底", () => {
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "light" }, false)?.id).toBe("light");
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "dark" }, true)?.id).toBe("dark");
  });

  it("默认主题插件深浅模式 system：按 systemDark 解析", () => {
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "system" }, true)?.id).toBe("dark");
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "system" }, false)?.id).toBe("light");
  });

  it("默认主题插件缺省/非法深浅模式：按跟随系统处理", () => {
    expect(resolveActiveThemeEntry(builtin, {}, true)?.id).toBe("dark");
    expect(resolveActiveThemeEntry(builtin, { [COLOR_MODE_KEY]: "blue" }, false)?.id).toBe("light");
  });

  it("其余插件：variant 命中条目，否则第一个条目", () => {
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

describe("normalizeThemeVarKeys", () => {
  it("变量键补 -- 前缀", () => {
    expect(normalizeThemeVarKeys({ accent: "#123", "--bg": "#fff" })).toEqual({ "--accent": "#123", "--bg": "#fff" });
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
  it("合法 themes 判定为主题；仅基础主题条目重名判定为非主题（与 derive 同规则）", () => {
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
