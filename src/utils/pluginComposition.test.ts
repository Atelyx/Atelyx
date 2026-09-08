/**
 * 组合配置视图推导纯函数测试（utils/pluginComposition.ts）。
 * 覆盖默认成员四种状态 + 第三方启停 + 畸形默认条目守卫 + 空集退化。
 */
import { describe, it, expect } from "vitest";
import type { PluginManifest } from "@/types";
import { deriveComposition, type CompositionRow } from "./pluginComposition";

const def = (id: string, name: string): PluginManifest => ({
  schemaVersion: 2,
  id,
  name,
  version: "1.0.0",
  type: "panel",
  main: "builtin",
});

const row = (r: CompositionRow): string => `${r.id}:${r.status}`;

describe("deriveComposition", () => {
  it("默认成员：启用/停用/已卸载三态", () => {
    const defaults = [def("builtin.search", "搜索"), def("builtin.recent", "最近打开"), def("builtin.calendar", "日历")];
    const installed = {
      "builtin.search": { name: "搜索", enabled: true },
      "builtin.recent": { name: "最近打开", enabled: false },
    };
    const rows = deriveComposition(defaults, installed);
    expect(row(rows[0])).toBe("builtin.search:default-on");
    expect(row(rows[1])).toBe("builtin.recent:default-off");
    expect(row(rows[2])).toBe("builtin.calendar:uninstalled-default");
    // 默认成员名字以默认集为准（含已卸载成员——plugin_list 不含它，只能来自默认集）
    expect(rows[2].name).toBe("日历");
    expect(rows[2].installed).toBe(false);
    expect(rows[2].enabled).toBe(false);
  });

  it("第三方：启用/停用", () => {
    const defaults = [def("builtin.search", "搜索")];
    const installed = {
      "builtin.search": { name: "搜索", enabled: true },
      "com.acme.tools": { name: "工具", enabled: true },
      "com.acme.off": { name: "停用插件", enabled: false },
    };
    const rows = deriveComposition(defaults, installed);
    expect(row(rows[0])).toBe("builtin.search:default-on");
    expect(row(rows[1])).toBe("com.acme.tools:user-on");
    expect(row(rows[2])).toBe("com.acme.off:user-off");
    expect(rows[1].role).toBe("third-party");
  });

  it("畸形默认条目（缺 id/空 id）跳过", () => {
    const defaults = [
      def("builtin.search", "搜索"),
      { ...def("bad.empty", "空id"), id: "" },
      { schemaVersion: 2, name: "无id", version: "1.0.0", type: "panel" } as unknown as PluginManifest,
    ];
    const rows = deriveComposition(defaults, { "builtin.search": { name: "搜索", enabled: true } });
    expect(rows.map((r) => r.id)).toEqual(["builtin.search"]);
  });

  it("空默认集：只有第三方行", () => {
    const rows = deriveComposition([], { "com.acme.tools": { name: "工具", enabled: true } });
    expect(rows).toHaveLength(1);
    expect(row(rows[0])).toBe("com.acme.tools:user-on");
  });

  it("空已装集：全部默认成员为已卸载灰行", () => {
    const rows = deriveComposition([def("builtin.search", "搜索"), def("builtin.recent", "最近打开")], {});
    expect(rows.map((r) => r.status)).toEqual(["uninstalled-default", "uninstalled-default"]);
  });
});
