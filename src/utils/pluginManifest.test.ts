/**
 * 插件包清单校验与兼容性纯函数测试（utils/pluginManifest）。
 *
 * 覆盖：name 合法性、版本比较、宿主兼容（版本范围/平台）、插件包（package.json + atelyx 块）
 * 校验的必填/可选/归一化、前向兼容（未知附加分类跳过）、declares 服务披露。
 */
import { describe, it, expect } from "vitest";
import {
  compareVersions,
  pluginCompatibleWithHost,
  pluginIdValid,
  pluginTypeList,
  validatePluginManifest,
} from "./pluginManifest";

const validManifest = (): Record<string, unknown> => ({
  name: "com.example.todo",
  version: "1.2.3",
  main: "plugin.ts",
  description: "示例",
  atelyx: { name: "示例插件", type: "tool" },
});

describe("pluginIdValid", () => {
  it("接受反向域名式 id", () => {
    expect(pluginIdValid("com.example.todo")).toBe(true);
    expect(pluginIdValid("a.b")).toBe(true);
    expect(pluginIdValid("io.github.user.plugin-1")).toBe(true);
  });
  it("拒绝非法 id", () => {
    expect(pluginIdValid("todo")).toBe(false); // 单段
    expect(pluginIdValid("com..example")).toBe(false); // 连续点
    expect(pluginIdValid(".com.example")).toBe(false); // 点开头
    expect(pluginIdValid("com.example.")).toBe(false); // 点结尾
    expect(pluginIdValid("com.example-")).toBe(false); // 段以中划线结尾
    expect(pluginIdValid("COM.Example")).toBe(false); // 大写
  });
});

describe("compareVersions", () => {
  it("按段比较", () => {
    expect(compareVersions("1.2.3", "1.2.3")).toBe(0);
    expect(compareVersions("1.2.3", "1.2.4")).toBe(-1);
    expect(compareVersions("1.10.0", "1.9.9")).toBe(1);
  });
  it("容忍缺段与非数字段", () => {
    expect(compareVersions("1.2", "1.2.0")).toBe(0);
    expect(compareVersions("1.2", "1.2.1")).toBe(-1);
    expect(compareVersions("0.3.7-beta", "0.3.7")).toBe(0); // 非数字段按 0
  });
});

describe("pluginCompatibleWithHost", () => {
  const base = { atelyxVersionMin: undefined, atelyxVersionMax: undefined, platforms: undefined };
  it("无约束时兼容", () => {
    expect(pluginCompatibleWithHost(base, "0.4.0", "windows-x64").ok).toBe(true);
  });
  it("版本下限/上限", () => {
    expect(pluginCompatibleWithHost({ ...base, atelyxVersionMin: "0.4.0" }, "0.3.7", "windows-x64").ok).toBe(false);
    expect(pluginCompatibleWithHost({ ...base, atelyxVersionMin: "0.4.0" }, "0.4.0", "windows-x64").ok).toBe(true);
    expect(pluginCompatibleWithHost({ ...base, atelyxVersionMax: "0.5.0" }, "0.5.0", "windows-x64").ok).toBe(false);
    expect(pluginCompatibleWithHost({ ...base, atelyxVersionMax: "0.5.0" }, "0.4.9", "windows-x64").ok).toBe(true);
  });
  it("平台过滤", () => {
    expect(pluginCompatibleWithHost({ ...base, platforms: ["linux-x64"] }, "0.4.0", "windows-x64").ok).toBe(false);
    expect(pluginCompatibleWithHost({ ...base, platforms: ["windows-x64", "linux-x64"] }, "0.4.0", "windows-x64").ok).toBe(true);
  });
});

describe("validatePluginManifest", () => {
  it("接受合法插件包并归一化缺省值", () => {
    const result = validatePluginManifest(validManifest());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.id).toBe("com.example.todo");
    expect(result.manifest.name).toBe("示例插件");
    expect(result.manifest.scope).toBe("app");
    expect(result.manifest.types).toEqual(["tool"]);
    expect(result.manifest.declares).toBeUndefined();
  });
  it("atelyx.name 缺省 = package name；author/license/description/tags 回退顶层字段", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      atelyx: { type: "tool" },
      author: "作者甲",
      license: "MIT",
      keywords: ["效率", "表格"],
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.name).toBe("com.example.todo");
    expect(result.manifest.author).toBe("作者甲");
    expect(result.manifest.license).toBe("MIT");
    expect(result.manifest.tags).toEqual(["效率", "表格"]);
    expect(result.manifest.description).toBe("示例");
  });
  it("拒绝结构错误", () => {
    expect(validatePluginManifest(null).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), name: "todo" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), version: "" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), atelyx: undefined }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), atelyx: { type: "watcher" } }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), main: "" }).ok).toBe(false);
    // 缺 name/atelyx 块的清单拒绝。
    expect(validatePluginManifest({ schemaVersion: 2, id: "com.x", name: "x", version: "1", type: "tool", main: "a.js" }).ok).toBe(false);
  });
  it("前向兼容：未知附加分类跳过而不报错；declares 保留全部服务名", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      atelyx: {
        ...(validManifest().atelyx as Record<string, unknown>),
        types: ["tool", "future-kind"],
        declares: ["table", "com.example.db", "future:ns"],
      },
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.types).toEqual(["tool"]);
    expect(result.manifest.declares).toEqual(["table", "com.example.db", "future:ns"]);
  });
  it("declares 畸形（非字符串/空串）拒绝", () => {
    expect(validatePluginManifest({ ...validManifest(), atelyx: { ...(validManifest().atelyx as Record<string, unknown>), declares: "table" } }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), atelyx: { ...(validManifest().atelyx as Record<string, unknown>), declares: [""] } }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), atelyx: { ...(validManifest().atelyx as Record<string, unknown>), declares: [123] } }).ok).toBe(false);
  });
  it("保留 scope/declares/permissions/platforms/hostApiVersion", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      atelyx: {
        ...(validManifest().atelyx as Record<string, unknown>),
        scope: "vault",
        declares: ["table", "shell"],
        permissions: { shell: "执行打包命令" },
        platforms: ["windows-x64"],
        hostApiVersion: 1,
      },
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.scope).toBe("vault");
    expect(result.manifest.declares).toEqual(["table", "shell"]);
    expect(result.manifest.permissions).toEqual({ shell: "执行打包命令" });
    expect(result.manifest.platforms).toEqual(["windows-x64"]);
    expect(result.manifest.hostApiVersion).toBe(1);
  });
  it("themes 声明式主题条目解析与结构校验（atelyx 块内）", () => {
    const ok = validatePluginManifest({
      ...validManifest(),
      atelyx: {
        type: "theme",
        themes: [
          { id: "nord-light", name: "Nord 浅色", colorScheme: "light", variables: { "--accent": "#7c3aed", bg: "#111" } },
          { id: "nord-dark", name: "Nord 深色", colorScheme: "dark", variables: {} },
        ],
      },
    });
    expect(ok.ok).toBe(true);
    if (!ok.ok) return;
    expect(ok.manifest.themes).toEqual([
      { id: "nord-light", name: "Nord 浅色", colorScheme: "light", variables: { "--accent": "#7c3aed", bg: "#111" } },
      { id: "nord-dark", name: "Nord 深色", colorScheme: "dark", variables: {} },
    ]);

    expect(validatePluginManifest({ ...validManifest(), atelyx: { ...(validManifest().atelyx as Record<string, unknown>), themes: "x" } }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), atelyx: { ...(validManifest().atelyx as Record<string, unknown>), themes: [] } }).ok).toBe(false);
    expect(
      validatePluginManifest({
        ...validManifest(),
        atelyx: { ...(validManifest().atelyx as Record<string, unknown>), themes: [{ id: "a", name: "A", colorScheme: "blue", variables: {} }] },
      }).ok,
    ).toBe(false);
    expect(
      validatePluginManifest({
        ...validManifest(),
        atelyx: {
          ...(validManifest().atelyx as Record<string, unknown>),
          themes: [
            { id: "a", name: "A", colorScheme: "light", variables: {} },
            { id: "a", name: "B", colorScheme: "dark", variables: {} },
          ],
        },
      }).ok,
    ).toBe(false);
  });
  it("themeOptions 预置设置项声明解析", () => {
    const ok = validatePluginManifest({
      ...validManifest(),
      atelyx: {
        type: "theme",
        themes: [{ id: "a", name: "A", colorScheme: "light", variables: {} }],
        themeOptions: { accent: true },
      },
    });
    expect(ok.ok).toBe(true);
    if (!ok.ok) return;
    expect(ok.manifest.themeOptions).toEqual({ accent: true });
    expect(
      validatePluginManifest({
        ...validManifest(),
        atelyx: { ...(validManifest().atelyx as Record<string, unknown>), themeOptions: { accent: "yes" } },
      }).ok,
    ).toBe(false);
  });
});

describe("pluginTypeList", () => {
  it("含主分类去重", () => {
    expect(pluginTypeList({ type: "tool", types: ["tool", "panel"] })).toEqual(["tool", "panel"]);
    expect(pluginTypeList({ type: "theme", types: undefined })).toEqual(["theme"]);
  });
});

describe("theme 免 main（纯主题插件无需入口）", () => {
  it("纯 theme 插件可省略 main", () => {
    const r = validatePluginManifest({
      name: "com.example.dark",
      version: "1.0.0",
      atelyx: { type: "theme", themes: [{ id: "dark", name: "深色", colorScheme: "dark", variables: { bg: "#000" } }] },
    });
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.manifest.main).toBeUndefined();
    expect(r.manifest.themes?.[0].variables).toEqual({ bg: "#000" });
  });
  it("非 theme（tool）插件缺 main 拒绝", () => {
    expect(validatePluginManifest({ ...validManifest(), main: undefined }).ok).toBe(false);
  });
  it("混合类型（含非 theme）插件缺 main 拒绝", () => {
    const r = validatePluginManifest({
      ...validManifest(),
      main: undefined,
      atelyx: {
        type: "theme",
        types: ["theme", "tool"],
        themes: [{ id: "dark", name: "深色", colorScheme: "dark", variables: { bg: "#000" } }],
      },
    });
    expect(r.ok).toBe(false);
  });
});
