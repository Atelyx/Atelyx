/**
 * slots 代数测试：纯核心（utils/cordis/slots）+ 运行时注册表（services/cordis/slots）。
 */
import { describe, expect, it, afterEach } from "vitest";
import type { SlotContribution } from "@/utils/cordis/slots";
import { pickSlotWinner, sortSlotList } from "@/utils/cordis/slots";
import {
  disposePluginSlots,
  listSlot,
  registerSlot,
  registerViewSlot,
  registeredSlots,
  resolveSlot,
  resolveViewKind,
  unregisterSlot,
  viewKinds,
} from "@/services/cordis/slots";

const contrib = (partial: Partial<SlotContribution> & { id: string }): SlotContribution => ({
  pluginId: "p",
  slot: "view/x",
  cardinality: "single",
  scope: "root",
  priority: 0,
  payload: {},
  ...partial,
});

/** 已注册 id 跟踪（afterEach 精确撤销，防跨用例污染注册表）。 */
const registered: string[] = [];
function reg(c: SlotContribution): void {
  registerSlot(c);
  registered.push(c.id);
}
function regView(kind: string, pluginId: string, label: string): () => void {
  const off = registerViewSlot(kind, pluginId, { label });
  registered.push(`${pluginId}:view/${kind}`);
  return off;
}

afterEach(() => {
  for (const id of registered) unregisterSlot(id);
  registered.length = 0;
});

describe("slots 纯核心", () => {
  it("single 胜出：最高 priority 胜，同优先级后注册者胜", () => {
    const a = contrib({ id: "a", priority: 1 });
    const b = contrib({ id: "b", priority: 2 });
    const c = contrib({ id: "c", priority: 2 });
    expect(pickSlotWinner([a, b, c])?.id).toBe("c");
    expect(pickSlotWinner([b, a])?.id).toBe("b");
    expect(pickSlotWinner([])).toBeUndefined();
  });

  it("list 排序：priority 降序，同优先级保持注册序", () => {
    const a = contrib({ id: "a", priority: 0 });
    const b = contrib({ id: "b", priority: 5 });
    const c = contrib({ id: "c", priority: 2 });
    expect(sortSlotList([a, b, c]).map((x) => x.id)).toEqual(["b", "c", "a"]);
  });
});

describe("slots 注册表", () => {
  it("registerViewSlot → resolveViewKind / viewKinds；撤销后消失", () => {
    const off = regView("canvas", "builtin.canvas", "画布");
    expect(resolveViewKind("canvas")?.payload.label).toBe("画布");
    expect(resolveViewKind("canvas")?.pluginId).toBe("builtin.canvas");
    expect(viewKinds()).toContain("canvas");
    off();
    expect(resolveViewKind("canvas")).toBeUndefined();
    expect(viewKinds()).not.toContain("canvas");
  });

  it("重复注册同一 id 拒绝", () => {
    regView("canvas", "builtin.canvas", "画布");
    expect(() => regView("canvas", "builtin.canvas", "画布")).toThrow("已注册");
  });

  it("disposePluginSlots 撤销某插件的全部槽贡献", () => {
    regView("canvas", "builtin.canvas", "画布");
    regView("table", "builtin.canvas", "表格");
    regView("search", "builtin.search", "搜索");
    disposePluginSlots("builtin.canvas");
    expect(resolveViewKind("canvas")).toBeUndefined();
    expect(resolveViewKind("table")).toBeUndefined();
    expect(resolveViewKind("search")?.pluginId).toBe("builtin.search");
  });

  it("registeredSlots / listSlot 按槽聚合", () => {
    reg(contrib({ id: "p1:v1", slot: "view/a" }));
    reg(contrib({ id: "p1:v2", slot: "view/b" }));
    reg(contrib({ id: "p2:v1", slot: "view/a" }));
    expect(registeredSlots().sort()).toEqual(["view/a", "view/b"]);
    expect(listSlot("view/a").map((c) => c.id).sort()).toEqual(["p1:v1", "p2:v1"]);
  });

  it("resolveSlot 按槽取胜出贡献", () => {
    reg(contrib({ id: "a", slot: "view/a", priority: 1 }));
    reg(contrib({ id: "b", slot: "view/b", priority: 2 }));
    expect(resolveSlot("view/a")?.id).toBe("a");
    expect(resolveSlot("view/none")).toBeUndefined();
  });
});
