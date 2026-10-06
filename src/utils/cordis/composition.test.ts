/**
 * 组合层纯函数测试（utils/cordis/composition）。
 *
 * 覆盖：列表行推导（默认组合层在前、已装插件追加在后、已卸载默认成员成灰行）、装配顺序过滤、
 * 接管裁决（插件声明 / 用户层峰板 / 冲突 / 实现不可用回退 / 被引用行不独立装配 / 成环保留）。
 */
import { describe, expect, it } from "vitest";
import {
  composePlugins,
  mountOrder,
  orderViewKindsByRow,
  resolveComposition,
  sanitizeCompositionPatches,
  type ActiveCompositionDeclaration,
  type CompositionDefault,
  type CompositionPackage,
  type CompositionRow,
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

/** 三行默认成员（已装启用）+ 两个可用的接管候选（com.provider / com.rival）。 */
function fixture(): { rows: CompositionRow[]; usable: Set<string> } {
  const rows = composePlugins(DEFAULTS, [
    pkg("builtin.search"),
    pkg("builtin.canvas"),
    pkg("builtin.note"),
    pkg("com.provider"),
    pkg("com.rival"),
  ]);
  const usable = new Set(["builtin.search", "builtin.canvas", "builtin.note", "com.provider", "com.rival"]);
  return { rows, usable };
}

function resolve(
  rows: CompositionRow[],
  usable: Set<string>,
  declarations: ActiveCompositionDeclaration[] = [],
  userPatches: Record<string, string> = {},
) {
  return resolveComposition({ rows, declarations, userPatches, implUsable: (id) => usable.has(id) });
}

describe("resolveComposition", () => {
  it("无声明无用户层：装配计划与 mountOrder 逐项一致（回归锚点）", () => {
    const { rows, usable } = fixture();
    const res = resolve(rows, usable);
    expect(res.mounts).toEqual(mountOrder(rows).map((id) => ({ rowId: id, implId: id })));
    expect(res.bindings["builtin.search"]).toMatchObject({
      implId: "builtin.search",
      source: "default",
      declarers: [],
      userImpl: null,
      selfAvailable: true,
    });
    expect(res.unmatched).toEqual([]);
  });

  it("插件声明接管：行的实现换成声明方，且声明方自身行不独立装配", () => {
    const { rows, usable } = fixture();
    const res = resolve(rows, usable, [{ pluginId: "com.provider", target: "builtin.note", priority: 5 }]);
    expect(res.mounts).toEqual([
      { rowId: "builtin.search", implId: "builtin.search" },
      { rowId: "builtin.canvas", implId: "builtin.canvas" },
      { rowId: "builtin.note", implId: "com.provider" },
      { rowId: "com.rival", implId: "com.rival" },
    ]);
    expect(res.bindings["builtin.note"]).toMatchObject({ implId: "com.provider", source: "plugin" });
    // com.provider 的自身行被引用为实现 → 不独立装配（否则同一份 apply 挂两次）
    expect(res.mounts.some((m) => m.rowId === "com.provider")).toBe(false);
  });

  it("多声明方按 priority 降序生效，同 priority 按插件 id 升序", () => {
    const { rows, usable } = fixture();
    const res = resolve(rows, usable, [
      { pluginId: "com.rival", target: "builtin.note", priority: 1 },
      { pluginId: "com.provider", target: "builtin.note", priority: 9 },
    ]);
    expect(res.bindings["builtin.note"].declarers.map((d) => d.pluginId)).toEqual(["com.provider", "com.rival"]);
    expect(res.bindings["builtin.note"].implId).toBe("com.provider");
  });

  it("用户层恒胜声明；`default` = 回本行默认实现且声明不参与", () => {
    const { rows, usable } = fixture();
    const pinned = resolve(rows, usable, [{ pluginId: "com.provider", target: "builtin.note" }], {
      "builtin.note": "com.rival",
    });
    expect(pinned.bindings["builtin.note"]).toMatchObject({ implId: "com.rival", source: "user" });

    const back = resolve(rows, usable, [{ pluginId: "com.provider", target: "builtin.note" }], {
      "builtin.note": "default",
    });
    expect(back.bindings["builtin.note"]).toMatchObject({ implId: "builtin.note", source: "user" });
    // 回默认后声明方恢复自身装配（无行引用它了）
    expect(back.mounts.some((m) => m.rowId === "com.provider")).toBe(true);
  });

  it("声明的实现不可用：回退本行默认实现并带可读原因（不静默）", () => {
    const { rows, usable } = fixture();
    usable.delete("com.rival");
    const res = resolve(rows, usable, [], { "builtin.note": "com.rival" });
    expect(res.bindings["builtin.note"].implId).toBe("builtin.note");
    expect(res.bindings["builtin.note"].problem).toContain("com.rival");
    expect(res.mounts.find((m) => m.rowId === "builtin.note")).toEqual({
      rowId: "builtin.note",
      implId: "builtin.note",
    });
  });

  it("停用的提供者不可用 ⇒ 接管失效（用户钉住也不生效）", () => {
    const rows = composePlugins(DEFAULTS, [pkg("com.provider", false)]);
    const res = resolveComposition({
      rows,
      declarations: [],
      userPatches: { "builtin.note": "com.provider" },
      implUsable: (id) => id !== "com.provider",
    });
    expect(res.bindings["builtin.note"]).toMatchObject({ implId: "builtin.note", source: "user" });
    expect(res.bindings["builtin.note"].problem).toBeTruthy();
  });

  it("相互引用（成环）时两行都保留自身装配，不一起消失", () => {
    const { rows, usable } = fixture();
    const res = resolve(rows, usable, [], {
      "com.provider": "com.rival",
      "com.rival": "com.provider",
    });
    expect(res.mounts.find((m) => m.rowId === "com.provider")).toEqual({
      rowId: "com.provider",
      implId: "com.rival",
    });
    expect(res.mounts.find((m) => m.rowId === "com.rival")).toEqual({
      rowId: "com.rival",
      implId: "com.provider",
    });
  });

  it("目标行不存在的声明进 unmatched（不静默丢弃）", () => {
    const { rows, usable } = fixture();
    const res = resolve(rows, usable, [{ pluginId: "com.provider", target: "com.absent" }]);
    expect(res.unmatched).toEqual([{ pluginId: "com.provider", target: "com.absent" }]);
    // 未命中目标行 ⇒ 没有别的行被它接管（只剩它自己那行）
    expect(res.mounts.every((m) => m.rowId === m.implId)).toBe(true);
  });

  it("行 id 与 Object.prototype 属性重名时不当真命中用户层（hasOwn 判定）", () => {
    const rows = composePlugins([{ id: "constructor", name: "占位" }], [pkg("constructor")]);
    const res = resolve(rows, new Set(["constructor"]));
    expect(res.bindings["constructor"].userImpl).toBeNull();
    expect(res.bindings["constructor"]).toMatchObject({ implId: "constructor", source: "default" });
  });
});

describe("orderViewKindsByRow", () => {
  it("按行位置排序，同键（同一行内）保持入参顺序", () => {
    const kinds = ["n2", "c1", "n1", "c2"];
    const row = (k: string) => (k.startsWith("n") ? 1 : 0);
    expect(orderViewKindsByRow(kinds, row)).toEqual(["c1", "c2", "n2", "n1"]);
  });

  it("无归属的 kind 排在有归属的之后（同键保持入参顺序）", () => {
    const kinds = ["z", "a"];
    expect(orderViewKindsByRow(kinds, () => 7)).toEqual(["z", "a"]);
    expect(orderViewKindsByRow(["x", "y"], (k) => (k === "y" ? 0 : 7))).toEqual(["y", "x"]);
  });

  it("不改入参数组", () => {
    const kinds = ["b", "a"];
    orderViewKindsByRow(kinds, (k) => (k === "a" ? 0 : 1));
    expect(kinds).toEqual(["b", "a"]);
  });
});

describe("sanitizeCompositionPatches", () => {
  it("只保留非空字符串项，其余（脏值/原型链/非法容器）丢弃", () => {
    expect(sanitizeCompositionPatches({ "builtin.note": "com.a", "builtin.canvas": 3, "": "x" })).toEqual({
      "builtin.note": "com.a",
    });
    expect(sanitizeCompositionPatches(undefined)).toEqual({});
    expect(sanitizeCompositionPatches([1, 2])).toEqual({});
    expect(sanitizeCompositionPatches("x")).toEqual({});
  });
});

describe("用户层多行指向同一实现", () => {
  it("用户层是整表：两行都可钉到同一插件（各行各装一份，锁行为）", () => {
    const { rows, usable } = fixture();
    const res = resolve(rows, usable, [], {
      "builtin.search": "com.provider",
      "builtin.canvas": "com.provider",
    });
    expect(res.mounts.find((m) => m.rowId === "builtin.search")).toEqual({
      rowId: "builtin.search",
      implId: "com.provider",
    });
    expect(res.mounts.find((m) => m.rowId === "builtin.canvas")).toEqual({
      rowId: "builtin.canvas",
      implId: "com.provider",
    });
    // 提供者的自身行仍不独立装配
    expect(res.mounts.some((m) => m.rowId === "com.provider")).toBe(false);
  });
});
