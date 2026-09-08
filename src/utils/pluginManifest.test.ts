/**
 * 插件清单校验与兼容性纯函数测试（utils/pluginManifest）。
 *
 * 覆盖：id 合法性、版本比较、宿主兼容（版本范围/平台）、清单校验的必填/可选/归一化、
 * 前向兼容（未知附加分类跳过）、declares 命名空间披露。
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
  schemaVersion: 1,
  id: "com.example.todo",
  name: "示例插件",
  version: "1.2.3",
  type: "tool",
  main: "plugin.js",
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
  it("接受合法清单并归一化缺省值", () => {
    const result = validatePluginManifest(validManifest());
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.scope).toBe("app");
    expect(result.manifest.types).toEqual(["tool"]);
    expect(result.manifest.declares).toBeUndefined(); // 未声明 declares 时缺省省略
  });
  it("拒绝结构错误", () => {
    expect(validatePluginManifest(null).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), id: "todo" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), name: "" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), type: "watcher" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), main: "" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), schemaVersion: 3 }).ok).toBe(false); // 版本过新
    expect(validatePluginManifest({ ...validManifest(), runtime: "rust" }).ok).toBe(false); // 未知运行时
  });
  it("前向兼容：未知附加分类跳过而不报错；declares 保留全部命名空间", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      type: "tool",
      types: ["tool", "future-kind"],
      declares: ["state", "com.example.db", "future:ns"],
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.types).toEqual(["tool"]);
    expect(result.manifest.declares).toEqual(["state", "com.example.db", "future:ns"]);
  });
  it("declares 畸形（非字符串/空串）拒绝", () => {
    expect(validatePluginManifest({ ...validManifest(), declares: "state" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), declares: [""] }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), declares: [123] }).ok).toBe(false);
  });
  it("保留 declares/permissions/platforms 并校验类型", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      scope: "vault",
      declares: ["state", "shell"],
      permissions: { shell: "执行打包命令" },
      platforms: ["windows-x64"],
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.scope).toBe("vault");
    expect(result.manifest.declares).toEqual(["state", "shell"]);
    expect(result.manifest.permissions).toEqual({ shell: "执行打包命令" });
    expect(result.manifest.platforms).toEqual(["windows-x64"]);
  });
  it("schemaVersion 2：runtime/provides/requires/contributes 归一化", () => {
    const result = validatePluginManifest({
      ...validManifest(),
      schemaVersion: 2,
      runtime: "python",
      provides: ["com.example.db", "com.example.db2"],
      requires: ["vault", "com.example.dep"],
      contributes: {
        commands: [{ id: "hi", label: "你好" }, { label: "缺 id 被跳过" }],
        panels: [{ kind: "com.example.db.panel", label: "面板" }],
        future: [{ id: "x" }],
      },
    });
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.manifest.runtime).toBe("python");
    expect(result.manifest.provides).toEqual(["com.example.db", "com.example.db2"]);
    expect(result.manifest.requires).toEqual(["vault", "com.example.dep"]);
    expect(result.manifest.contributes?.commands).toEqual([{ id: "hi", label: "你好" }]);
    expect(result.manifest.contributes?.panels).toEqual([{ kind: "com.example.db.panel", label: "面板" }]);
    expect(result.manifest.contributes?.settings).toBeUndefined();
    expect(result.manifest.contributes?.commands?.some((c) => c.id === "hi")).toBe(true);
  });
  it("theme 声明式皮肤解析与结构校验", () => {
    const ok = validatePluginManifest({
      ...validManifest(),
      type: "theme",
      theme: { variables: { "--accent": "#7c3aed", bg: "#111" }, dark: { "--bg": "#000" } },
    });
    expect(ok.ok).toBe(true);
    if (!ok.ok) return;
    expect(ok.manifest.theme?.variables).toEqual({ "--accent": "#7c3aed", bg: "#111" });
    expect(ok.manifest.theme?.dark).toEqual({ "--bg": "#000" });

    expect(validatePluginManifest({ ...validManifest(), theme: { variables: "x" } }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), theme: [] }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), theme: { dark: "x" } }).ok).toBe(false);
  });
});

describe("validatePluginManifest 的 replace 字段", () => {
  it("replace 归一化：replace ⊆ requires 时通过并保留", () => {
    const ok = validatePluginManifest({
      ...validManifest(),
      requires: ["com.shared", "shell"],
      replace: ["com.shared"],
    });
    expect(ok.ok).toBe(true);
    if (!ok.ok) return;
    expect(ok.manifest.replace).toEqual(["com.shared"]);
  });

  it("replace 声明了 requires 未声明的命名空间：拒绝（显式替换意图必须以 requires 为前提）", () => {
    const r = validatePluginManifest({
      ...validManifest(),
      requires: ["shell"],
      replace: ["com.shared"],
    });
    expect(r.ok).toBe(false);
    if (r.ok) return;
    expect(r.errors.join(" ")).toContain("须同时声明在 requires 里");
  });

  it("replace 非字符串数组：拒绝", () => {
    expect(validatePluginManifest({ ...validManifest(), replace: "com.shared" }).ok).toBe(false);
    expect(validatePluginManifest({ ...validManifest(), replace: [123] }).ok).toBe(false);
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
      schemaVersion: 1,
      id: "com.example.dark",
      name: "暗色皮肤",
      version: "1.0.0",
      type: "theme",
      theme: { variables: { bg: "#000" } },
    });
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.manifest.main).toBeUndefined();
    expect(r.manifest.theme?.variables).toEqual({ bg: "#000" });
  });
  it("非 theme（tool）插件缺 main 拒绝", () => {
    expect(validatePluginManifest({ ...validManifest(), main: undefined }).ok).toBe(false);
  });
  it("混合类型（含非 theme）插件缺 main 拒绝", () => {
    const r = validatePluginManifest({
      ...validManifest(),
      main: undefined,
      type: "theme",
      types: ["theme", "tool"],
      theme: { variables: { bg: "#000" } },
    });
    expect(r.ok).toBe(false);
  });
});
