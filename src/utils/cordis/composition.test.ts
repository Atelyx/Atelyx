/**
 * 组合层纯函数测试（utils/cordis/composition）。
 *
 * 覆盖：列表行推导（默认组合层在前、已装插件追加在后、已卸载默认成员成灰行）、装配顺序过滤。
 */
import { describe, expect, it } from "vitest";
import {
  composePlugins,
  mountOrder,
  type CompositionDefault,
  type CompositionPackage,
} from "./composition";

const DEFAULTS: CompositionDefault[] = [
  { id: "builtin.search", name: "搜索" },
  { id: "builtin.canvas", name: "画布" },
  { id: "builtin.note", name: "笔记" },
];

function pkg(id: string, enabled = true): CompositionPackage {
  return { id, name: id, version: "1.0.0", sourceKind: "market", enabled };
}

describe("composePlugins", () => {
  it("默认组合层在前（定义顺序），已装插件按 id 追加在后", () => {
    const rows = composePlugins(DEFAULTS, [pkg("com.b.tool"), pkg("com.a.tool"), pkg("builtin.canvas")]);
    expect(rows.map((r) => r.id)).toEqual([
      "builtin.search",
      "builtin.canvas",
      "builtin.note",
      "com.a.tool",
      "com.b.tool",
    ]);
  });

  it("已卸载的默认成员成灰行（installed=false，展示字段取默认组合层定义）", () => {
    const rows = composePlugins(DEFAULTS, [pkg("builtin.search")]);
    const note = rows.find((r) => r.id === "builtin.note");
    expect(note).toMatchObject({ installed: false, enabled: false, name: "笔记", version: "" });
  });

  it("已装行取插件列表的展示字段（含停用状态）", () => {
    const rows = composePlugins(DEFAULTS, [{ ...pkg("builtin.canvas", false), name: "画布插件", version: "2.0.0" }]);
    expect(rows.find((r) => r.id === "builtin.canvas")).toMatchObject({
      installed: true,
      enabled: false,
      name: "画布插件",
      version: "2.0.0",
    });
  });
});

describe("mountOrder", () => {
  it("只取已安装且启用的行，保持列表顺序", () => {
    const rows = composePlugins(DEFAULTS, [pkg("builtin.search"), pkg("builtin.canvas", false), pkg("com.a.tool")]);
    expect(mountOrder(rows)).toEqual(["builtin.search", "com.a.tool"]);
  });

  it("未安装的默认成员不参与装配", () => {
    expect(mountOrder(composePlugins(DEFAULTS, []))).toEqual([]);
  });
});
