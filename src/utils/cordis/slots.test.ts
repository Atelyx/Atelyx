/**
 * slots 代数测试：纯核心（utils/cordis/slots）+ 运行时注册表（services/cordis/slots）。
 */
import { describe, expect, it, afterEach } from "vitest";
import type { SlotContribution } from "@/utils/cordis/slots";
import { pickSlotWinner, sortSlotList } from "@/utils/cordis/slots";
import {
  disposePluginSlots,
  listSlot,
  registerNodeSlot,
  registerSlot,
  registerTableViewSlot,
  registerUiSlot,
  registerViewSlot,
  registeredSlots,
  resolveNodeSlot,
  resolveSlot,
  resolveTableViewSlot,
  resolveViewKind,
  tableViewKinds,
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

  it("resolveViewKind 返回稳定引用（selector 订阅依赖；贡献注册期内多次调用同一对象）", () => {
    regView("canvas", "builtin.canvas", "画布");
    // 稳定引用：若每次返回新对象，zustand selector 会触发无限重渲染。
    expect(resolveViewKind("canvas")).toBe(resolveViewKind("canvas"));
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

  it("node/edge/tableview 槽：single 胜出，可被高 priority 替换", () => {
    const off1 = registerNodeSlot("conversation", "builtin.canvas", (() => null) as never, { priority: 0 });
    registered.push("builtin.canvas:node/conversation");
    expect(resolveNodeSlot("conversation")?.pluginId).toBe("builtin.canvas");
    // 用户插件以更高 priority 注册同 type → 胜出（替换默认实现）。
    const off2 = registerNodeSlot("conversation", "com.test.conv", (() => null) as never, { priority: 10 });
    registered.push("com.test.conv:node/conversation");
    expect(resolveNodeSlot("conversation")?.pluginId).toBe("com.test.conv");
    expect(resolveNodeSlot("missing")).toBeUndefined();
    off1();
    off2();
    expect(resolveNodeSlot("conversation")).toBeUndefined();
  });

  it("tableview 槽：single 胜出 + tableViewKinds 枚举", () => {
    const off = registerTableViewSlot("timeline", "com.test.tl", { label: "时间线", component: (() => null) as never }, { priority: 0 });
    registered.push("com.test.tl:tableview/timeline");
    expect(resolveTableViewSlot("timeline")?.payload.label).toBe("时间线");
    expect(tableViewKinds()).toContain("timeline");
    off();
    expect(resolveTableViewSlot("timeline")).toBeUndefined();
  });

  it("UI 区域槽默认 list（多贡献有序，priority 降序）", () => {
    const off = registerUiSlot("toolbar/note/right", "com.test.tb", { component: (() => null) as never }, { priority: 1 });
    registered.push("com.test.tb:toolbar/note/right");
    expect(listSlot("toolbar/note/right").map((c) => (c.payload as { component: unknown }).component)).toHaveLength(1);
    expect((listSlot("toolbar/note/right")[0]?.payload as { component: unknown }).component).toBeDefined();
    off();
    expect(listSlot("toolbar/note/right")).toEqual([]);
  });

  it("list 槽同插件多贡献 id 自动去重（不拒绝）", () => {
    const a = registerUiSlot("toolbar/note/right", "com.test.tb", { component: (() => null) as never }, { priority: 0 });
    const b = registerUiSlot("toolbar/note/right", "com.test.tb", { component: (() => null) as never }, { priority: 0 });
    registered.push("com.test.tb:toolbar/note/right");
    registered.push("com.test.tb:toolbar/note/right:1");
    expect(listSlot("toolbar/note/right")).toHaveLength(2);
    a();
    b();
  });
});
