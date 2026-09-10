/**
 * 随应用分发插件注册表与默认组合完整性测试（components/plugins/cordis/builtins）。
 *
 * 防 TS 侧手误：id 唯一（builtin. 前缀）、视图 kind 唯一且属内置 VIEW_KINDS、
 * 默认组合层与定义一一对应、清单满足插件包契约（宿主据此播种与展示，Rust 侧不再持有清单）。
 */
import { describe, it, expect } from "vitest";
import { CORDIS_BUILTIN_DEFS, DEFAULT_COMPOSITION, builtinManifest } from "./builtins";
import { validatePluginManifest } from "@/utils/pluginManifest";
import { VIEW_KINDS } from "@/types";

describe("随应用分发插件注册表", () => {
  it("pluginId 唯一且使用 builtin. 前缀", () => {
    const ids = CORDIS_BUILTIN_DEFS.map((p) => p.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) expect(id.startsWith("builtin.")).toBe(true);
  });

  it("全部视图 kind 唯一且属内置 VIEW_KINDS（插件可注册开放 kind，内置 kind 应在内置清单内）", () => {
    const kinds = CORDIS_BUILTIN_DEFS.flatMap((p) => p.views.map((v) => v.kind));
    expect(new Set(kinds).size).toBe(kinds.length);
    for (const k of kinds) expect(VIEW_KINDS).toContain(k as (typeof VIEW_KINDS)[number]);
  });

  it("默认组合层与定义一一对应（id 同集、顺序取定义顺序）", () => {
    expect(DEFAULT_COMPOSITION.map((d) => d.id)).toEqual(CORDIS_BUILTIN_DEFS.map((d) => d.id));
    expect(DEFAULT_COMPOSITION.map((d) => d.name)).toEqual(CORDIS_BUILTIN_DEFS.map((d) => d.name));
    const ids = DEFAULT_COMPOSITION.map((d) => d.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("清单为插件包原始形状（跨 Rust 边界的播种契约：name = id + atelyx 块）", () => {
    for (const def of CORDIS_BUILTIN_DEFS) {
      const manifest = builtinManifest(def, "1.2.3");
      expect(manifest.name).toBe(def.id);
      expect(manifest.version).toBe("1.2.3");
      expect(manifest.main).toBeTruthy();
      const atelyx = manifest.atelyx as Record<string, unknown>;
      expect(atelyx.name).toBe(def.name);
      expect(atelyx.type).toBe(def.type);
      if (def.type === "theme") {
        const themes = atelyx.themes as Array<{ id: string }>;
        expect(themes.map((t) => t.id)).toEqual(["light", "dark"]);
        expect((atelyx.themeOptions as { accent?: boolean }).accent).toBe(true);
      }
    }
  });

  it("清单可经前端校验归一化（行对象消费路径）", () => {
    for (const def of CORDIS_BUILTIN_DEFS) {
      const validated = validatePluginManifest(builtinManifest(def, "1.2.3"));
      expect(validated.ok).toBe(true);
      if (validated.ok) {
        expect(validated.manifest.id).toBe(def.id);
        expect(validated.manifest.name).toBe(def.name);
        expect(validated.manifest.type).toBe(def.type);
      }
    }
  });
});
