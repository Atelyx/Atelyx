/**
 * 第一方插件注册表完整性测试（components/plugins/cordis/builtins）。
 *
 * 与 Rust 侧内置清单（commands/plugin.rs BUILTIN_PLUGINS）以 pluginId 对应；本测试防 TS 侧
 * 手误（id 唯一/kind 唯一/kind 属封闭 ViewKind/前缀约定），Rust 侧有对应校验。
 */
import { describe, it, expect } from "vitest";
import { CORDIS_BUILTIN_DEFS } from "./builtins";
import { VIEW_KINDS } from "@/types";

describe("第一方插件注册表", () => {
  it("pluginId 唯一且使用 builtin. 前缀", () => {
    const ids = CORDIS_BUILTIN_DEFS.map((p) => p.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) expect(id.startsWith("builtin.")).toBe(true);
  });

  it("全部视图 kind 唯一且属 VIEW_KINDS（内置提供封闭枚举的内置 kind）", () => {
    const kinds = CORDIS_BUILTIN_DEFS.flatMap((p) => p.views.map((v) => v.kind));
    expect(new Set(kinds).size).toBe(kinds.length);
    for (const k of kinds) expect(VIEW_KINDS).toContain(k as (typeof VIEW_KINDS)[number]);
  });
});
