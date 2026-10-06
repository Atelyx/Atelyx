/**
 * 插件包清单校验与兼容性纯函数测试（utils/pluginManifest）。
 *
 * 覆盖：name 合法性、版本比较、宿主兼容（版本范围/平台）、插件包（package.json + atelyx 块）
 * 校验的必填/可选/归一化、前向兼容（未知字段与未知附加分类跳过）、
 * 运行时依赖（dependencies）与显式打包开关（bundle）。
 */
import { describe, it, expect } from "vitest";
import {
  compareVersions,
  packageCompatibleWithHost,
  pluginCompatibleWithHost,
  pluginIdValid,
  pluginTypeList,
  validatePluginManifest,
} from "./pluginManifest";
import { PLUGIN_HOST_API_VERSION } from "@/constants/plugins";

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
  it("安卓平台串命中清单 platforms", () => {
    expect(pluginCompatibleWithHost({ ...base, platforms: ["android"] }, "0.4.0", "android").ok).toBe(true);
    expect(
      pluginCompatibleWithHost({ ...base, platforms: ["windows-x64", "linux-x64"] }, "0.4.0", "android").ok,
    ).toBe(false);
  });
  it("安卓：带依赖或要求宿主打包的插件不兼容，桌面不受影响", () => {
    const deps = { dependencies: { "some-pkg": "^1.0.0" } };
    expect(pluginCompatibleWithHost(base, "0.4.0", "android").ok).toBe(true);
    const blocked = pluginCompatibleWithHost({ ...base, ...deps }, "0.4.0", "android");
    expect(blocked.ok).toBe(false);
    if (!blocked.ok) expect(blocked.reason).toContain("依赖");
    expect(pluginCompatibleWithHost({ ...base, bundle: true }, "0.4.0", "android").ok).toBe(false);
    expect(pluginCompatibleWithHost({ ...base, ...deps }, "0.4.0", "windows-x64").ok).toBe(true);
  });
  it("契约版本：缺省视为当前版本，显式不匹配则拒绝并给出所需版本", () => {
    expect(pluginCompatibleWithHost(base, "0.4.0", "windows-x64").ok).toBe(true);
    expect(pluginCompatibleWithHost({ ...base, hostApiVersion: PLUGIN_HOST_API_VERSION }, "0.4.0", "windows-x64").ok).toBe(true);
    const mismatch = pluginCompatibleWithHost({ ...base, hostApiVersion: PLUGIN_HOST_API_VERSION + 1 }, "0.4.0", "windows-x64");
    expect(mismatch.ok).toBe(false);
    if (!mismatch.ok) expect(mismatch.reason).toContain(String(PLUGIN_HOST_API_VERSION + 1));
  });
  it("宿主版本未知（null）时跳过版本范围判断，契约版本仍校验", () => {
    expect(pluginCompatibleWithHost({ ...base, atelyxVersionMin: "9.9.9" }, null, "windows-x64").ok).toBe(true);
    expect(pluginCompatibleWithHost({ ...base, hostApiVersion: PLUGIN_HOST_API_VERSION + 1 }, null, "windows-x64").ok).toBe(false);
  });
  it("hostApiVersion 非数字（字符串/对象）在清单校验即拒绝", () => {
    for (const bad of ["1", null, { major: 1 }, [1]]) {
      const raw = validManifest();
      (raw.atelyx as Record<string, unknown>).hostApiVersion = bad;
      const result = validatePluginManifest(raw);
      expect(result.ok, JSON.stringify(bad)).toBe(false);
      if (!result.ok) expect(result.errors.join()).toContain("hostApiVersion");
    }
  });
});

describe("packageCompatibleWithHost", () => {
  it("atelyx 块里的约束被正确读取（顶层同名伪装字段不参与判定）", () => {
    // 顶层伪装的 atelyxVersionMin 不参与判定：归一化只认 atelyx 块
    const disguised = validManifest();
    disguised.atelyxVersionMin = "9.9.9";
    expect(packageCompatibleWithHost(disguised, "0.4.0", "windows-x64").ok).toBe(true);
    // 约束写在 atelyx 块内：宿主低于下限 → 拒绝
    const constrained = validManifest();
    (constrained.atelyx as Record<string, unknown>).atelyxVersionMin = "9.9.9";
    const blocked = packageCompatibleWithHost(constrained, "0.4.0", "windows-x64");
    expect(blocked.ok).toBe(false);
    if (!blocked.ok) expect(blocked.reason).toContain("9.9.9");
    // 宿主版本满足下限 → 通过，并返回归一化清单
    const ok = packageCompatibleWithHost(constrained, "9.9.9", "windows-x64");
    expect(ok.ok).toBe(true);
    if (ok.ok) expect(ok.manifest.id).toBe("com.example.todo");
  });
  it("清单非法（缺 atelyx 块/主分类未知）拒绝", () => {
    expect(packageCompatibleWithHost({ name: "com.a.b", version: "1.0.0" }, "1.0.0", "windows-x64").ok).toBe(false);
    const badType = validManifest();
    (badType.atelyx as Record<string, unknown>).type = "widget";
    expect(packageCompatibleWithHost(badType, "1.0.0", "windows-x64").ok).toBe(false);
  });
  it("宿主版本未知（null）拒绝（fail-closed）", () => {
    const result = packageCompatibleWithHost(validManifest(), null, "windows-x64");
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toContain("宿主版本");
  });
  it("平台约束按归一化清单判定", () => {
    const raw = validManifest();
    (raw.atelyx as Record<string, unknown>).platforms = ["linux-x64"];
    expect(packageCompatibleWithHost(raw, "1.0.0", "windows-x64").ok).toBe(false);
    expect(packageCompatibleWithHost(raw, "1.0.0", "linux-x64").ok).toBe(true);
  });
});

describe("validatePluginManifest", () => {
  it("接受合法插件包并归一化缺省值", () => {
    const result = validatePluginManifest(validManifest());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.id).toBe("com.example.todo");
    expect(result.manifest.name).toBe("示例插件");
    expect(result.manifest.types).toEqual(["tool"]);
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
  it("前向兼容：未知附加分类与未知元数据字段（含旧版 declares/permissions）跳过而不报错", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      atelyx: {
        ...(validManifest().atelyx as Record<string, unknown>),
        types: ["tool", "future-kind"],
        declares: ["table", "com.example.db", "future:ns"],
        permissions: { table: "读取表格" },
      },
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.types).toEqual(["tool"]);
    expect("declares" in result.manifest).toBe(false);
    expect("permissions" in result.manifest).toBe(false);
  });
  it("dependencies 归一化：保留包名 → 版本；缺省与空表都不带该字段", () => {
    const declared = validatePluginManifest({
      ...validManifest(),
      dependencies: { nanoid: "^5.0.0", "ms": "2.1.3" },
    });
    expect(declared.ok).toBe(true);
    if (!declared.ok) return;
    expect(declared.manifest.dependencies).toEqual({ nanoid: "^5.0.0", ms: "2.1.3" });

    const absent = validatePluginManifest(validManifest());
    expect(absent.ok).toBe(true);
    if (!absent.ok) return;
    expect(absent.manifest.dependencies).toBeUndefined();

    const empty = validatePluginManifest({ ...validManifest(), dependencies: {} });
    expect(empty.ok).toBe(true);
    if (!empty.ok) return;
    expect(empty.manifest.dependencies).toBeUndefined();
  });
  it("dependencies 畸形（非对象/非字符串值/空版本）拒绝", () => {
    expect(validatePluginManifest({ ...validManifest(), dependencies: ["nanoid"] }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), dependencies: { nanoid: 5 } }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), dependencies: { nanoid: "  " } }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), dependencies: { "": "^1.0.0" } }).ok).toBe(false);
  });
  it("bundle 归一化：true 保留、缺省不出现；非布尔拒绝", () => {
    const flagged = validatePluginManifest({
      ...validManifest(),
      atelyx: { ...(validManifest().atelyx as Record<string, unknown>), bundle: true },
    });
    expect(flagged.ok).toBe(true);
    if (!flagged.ok) return;
    expect(flagged.manifest.bundle).toBe(true);

    const absent = validatePluginManifest(validManifest());
    expect(absent.ok).toBe(true);
    if (!absent.ok) return;
    expect(absent.manifest.bundle).toBeUndefined();

    expect(
      validatePluginManifest({
        ...validManifest(),
        atelyx: { ...(validManifest().atelyx as Record<string, unknown>), bundle: "yes" },
      }).ok,
    ).toBe(false);
  });
  it("keepMountedOnVaultSwitch 归一化：true 保留、缺省不出现；非布尔拒绝", () => {
    const flagged = validatePluginManifest({
      ...validManifest(),
      atelyx: { ...(validManifest().atelyx as Record<string, unknown>), keepMountedOnVaultSwitch: true },
    });
    expect(flagged.ok).toBe(true);
    if (!flagged.ok) return;
    expect(flagged.manifest.keepMountedOnVaultSwitch).toBe(true);

    const absent = validatePluginManifest(validManifest());
    expect(absent.ok).toBe(true);
    if (!absent.ok) return;
    expect(absent.manifest.keepMountedOnVaultSwitch).toBeUndefined();

    expect(
      validatePluginManifest({
        ...validManifest(),
        atelyx: { ...(validManifest().atelyx as Record<string, unknown>), keepMountedOnVaultSwitch: 1 },
      }).ok,
    ).toBe(false);
  });
  it("compositionPatch 归一化：合法声明保留（含 priority），缺省不出现", () => {
    const declared = validatePluginManifest({
      ...validManifest(),
      atelyx: {
        ...(validManifest().atelyx as Record<string, unknown>),
        compositionPatch: [{ target: "builtin.chatcore", priority: 10 }],
      },
    });
    expect(declared.ok).toBe(true);
    if (!declared.ok) return;
    expect(declared.manifest.compositionPatch).toEqual([{ target: "builtin.chatcore", priority: 10 }]);

    const absent = validatePluginManifest(validManifest());
    expect(absent.ok).toBe(true);
    if (!absent.ok) return;
    expect(absent.manifest.compositionPatch).toBeUndefined();
  });

  it("compositionPatch 畸形拒绝：非数组 / 项非对象 / target 非法 / 重复目标 / 接管自身行 / priority 非有限数", () => {
    const withPatch = (compositionPatch: unknown) => ({
      ...validManifest(),
      atelyx: { ...(validManifest().atelyx as Record<string, unknown>), compositionPatch },
    });
    expect(validatePluginManifest(withPatch({ target: "builtin.note" })).ok).toBe(false);
    expect(validatePluginManifest(withPatch(["builtin.note"])).ok).toBe(false);
    expect(validatePluginManifest(withPatch([{ target: "builtin" }])).ok).toBe(false);
    expect(
      validatePluginManifest(withPatch([{ target: "builtin.note" }, { target: "builtin.note" }])).ok,
    ).toBe(false);
    // 接管自身行是空操作（作者多半写错了目标），响亮拒绝而不是静默生效
    expect(validatePluginManifest(withPatch([{ target: "com.example.todo" }])).ok).toBe(false);
    // 一个插件接管多行会让同一份代码按行各装一份（槽位与能力重复注册），拒绝
    expect(
      validatePluginManifest(
        withPatch([{ target: "builtin.chatcore" }, { target: "builtin.note" }]),
      ).ok,
    ).toBe(false);
    expect(validatePluginManifest(withPatch([{ target: "builtin.note", priority: "高" }])).ok).toBe(false);
  });

  it("保留 platforms/hostApiVersion", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      atelyx: {
        ...(validManifest().atelyx as Record<string, unknown>),
        platforms: ["windows-x64"],
        hostApiVersion: 1,
      },
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
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
