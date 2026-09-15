/**
 * 槽位治理（O3-4）注册表直测：single 槽胜者用户覆盖、冲突清单、修改链、审计槽位面。
 *
 * 不依赖内核挂载——直接调 services/cordis/slots 的注册函数与聚合函数（注册表是模块级单例，
 * 测试按既有 pattern 用返回的撤销函数清理；覆盖源 afterEach 置空复位纯 priority 决胜）。
 */
import { describe, expect, it, beforeEach, afterEach } from "vitest";
import { createKernel, type Kernel } from "./kernel";
import {
  declareSlot,
  listAllContributions,
  listAllDecorators,
  registerSlotContrib,
  registerSlotDecoratorFor,
  resolveSlot,
  setSlotWinnerOverrideSource,
  slotChain,
  slotConflictRows,
  slotDeclarer,
} from "./slots";
import { auditSnapshot } from "./audit";

let kernel: Kernel | null = null;
const unregs: (() => void)[] = [];
const declUnregs: (() => void)[] = [];

beforeEach(() => {
  unregs.length = 0;
  declUnregs.length = 0;
  setSlotWinnerOverrideSource(null);
});

afterEach(() => {
  setSlotWinnerOverrideSource(null);
  for (const fn of unregs.splice(0)) fn();
  for (const fn of declUnregs.splice(0)) fn();
  if (kernel) {
    kernel.dispose();
    kernel = null;
  }
});

/** 注册视图槽贡献（view/ 前缀 single，payload 只需 label），记撤销。 */
function addView(slot: string, pluginId: string, priority = 0): void {
  unregs.push(registerSlotContrib(slot, pluginId, { label: pluginId }, { priority }));
}

describe("single 槽胜者用户覆盖（resolveSlot + 注入源）", () => {
  it("钉住低 priority 贡献恒胜出；失效钉住回退 priority", () => {
    addView("view/com.test.panel", "com.a", 1);
    addView("view/com.test.panel", "com.b", 10);
    expect(resolveSlot("view/com.test.panel")?.pluginId).toBe("com.b");

    setSlotWinnerOverrideSource(() => "com.a:view/com.test.panel");
    expect(resolveSlot("view/com.test.panel")?.pluginId).toBe("com.a");

    // 被钉者卸载：钉住失效 → 回退 priority 胜者。
    unregs.splice(0, 1)[0]();
    expect(resolveSlot("view/com.test.panel")?.pluginId).toBe("com.b");
  });

  it("钉住 id 未注册或指向其他槽 = 不生效（回退 priority）", () => {
    addView("view/com.test.panel", "com.a", 1);
    addView("view/com.test.panel", "com.b", 10);
    // 未注册：id 不在注册表。
    setSlotWinnerOverrideSource(() => "com.nope:view/com.test.panel");
    expect(resolveSlot("view/com.test.panel")?.pluginId).toBe("com.b");

    // 指向其他槽：id 已注册但 slot 不一致——命中 pinnedWinner 的跨槽守卫。
    addView("view/other", "com.a", 5);
    setSlotWinnerOverrideSource((slot) => (slot === "view/com.test.panel" ? "com.a:view/other" : null));
    expect(resolveSlot("view/com.test.panel")?.pluginId).toBe("com.b");
  });

  it("覆盖源未注入 = 纯 priority 决胜（既有行为不变）", () => {
    addView("view/com.test.panel", "com.a", 1);
    addView("view/com.test.panel", "com.b", 10);
    expect(resolveSlot("view/com.test.panel")?.pluginId).toBe("com.b");
  });
});

describe("slotConflictRows（冲突清单纯函数）", () => {
  it("single 槽 ≥2 贡献出冲突行；winner 按 priority；钉住命中替换胜者", () => {
    addView("view/com.test.panel", "com.a", 1);
    addView("view/com.test.panel", "com.b", 10);
    const rows = slotConflictRows({});
    expect(rows).toHaveLength(1);
    expect(rows[0].slot).toBe("view/com.test.panel");
    expect(rows[0].declarer).toBe("宿主");
    expect(rows[0].contributors.map((c) => c.pluginId)).toEqual(["com.b", "com.a"]);
    expect(rows[0].winnerId).toBe("com.b:view/com.test.panel");
    expect(rows[0].pinnedId).toBeNull();

    const pinned = slotConflictRows({ "view/com.test.panel": "com.a:view/com.test.panel" });
    expect(pinned[0].winnerId).toBe("com.a:view/com.test.panel");
    expect(pinned[0].pinnedId).toBe("com.a:view/com.test.panel");
  });

  it("失效钉住保留键但回退 priority；list 槽与单贡献槽不出行", () => {
    addView("view/com.test.panel", "com.a", 1);
    addView("view/com.test.panel", "com.b", 10);
    const stale = slotConflictRows({ "view/com.test.panel": "com.gone:view/com.test.panel" });
    expect(stale[0].pinnedId).toBe("com.gone:view/com.test.panel");
    expect(stale[0].winnerId).toBe("com.b:view/com.test.panel");

    // 单贡献 single 槽：无冲突可裁决。
    addView("view/com.test.single", "com.a");
    // list 槽（inspector/ 前缀）：不参与 single 冲突。
    unregs.push(
      registerSlotContrib("inspector/com.test.node", "com.a", { component: () => null }, { cardinality: "list" }),
    );
    unregs.push(
      registerSlotContrib("inspector/com.test.node", "com.b", { component: () => null }, { cardinality: "list" }),
    );
    const rows = slotConflictRows({});
    expect(rows.map((r) => r.slot)).toEqual(["view/com.test.panel"]);
  });

  it("读钉住防原型链：无钉住时名为原型键的槽不误报钉住", () => {
    // 槽名是插件任意字符串，"constructor" 是合法 key；pins 空表时裸索引会命中原型链。
    declUnregs.push(declareSlot({ key: "constructor", cardinality: "single", required: ["component"] }, "com.declarer"));
    unregs.push(registerSlotContrib("constructor", "com.a", { component: () => null }, { priority: 1 }));
    unregs.push(registerSlotContrib("constructor", "com.b", { component: () => null }, { priority: 2 }));
    const rows = slotConflictRows({});
    expect(rows).toHaveLength(1);
    expect(rows[0].pinnedId).toBeNull();
    expect(rows[0].winnerId).toBe("com.b:constructor");
  });
});

describe("slotChain（修改链）", () => {
  it("静态宿主槽：声明方 = 宿主，贡献按 priority 排序", () => {
    addView("view/com.test.panel", "com.a", 1);
    addView("view/com.test.panel", "com.b", 5);
    const chain = slotChain("view/com.test.panel");
    expect(chain.declarer).toBe("宿主");
    expect(chain.contributors.map((c) => c.pluginId)).toEqual(["com.b", "com.a"]);
    expect(chain.decorators).toEqual([]);
  });

  it("装饰者入链（可装饰槽）：按 priority 排序", () => {
    unregs.push(registerSlotContrib("toolbar/note/right", "com.a", { component: () => null }, { cardinality: "list", priority: 1 }));
    unregs.push(registerSlotContrib("toolbar/note/right", "com.b", { component: () => null }, { cardinality: "list", priority: 5 }));
    unregs.push(registerSlotDecoratorFor("toolbar/note/right", "com.c", () => null, { priority: 3 }));
    const chain = slotChain("toolbar/note/right");
    expect(chain.declarer).toBe("宿主");
    expect(chain.contributors.map((c) => c.pluginId)).toEqual(["com.b", "com.a"]);
    expect(chain.decorators.map((d) => d.pluginId)).toEqual(["com.c"]);
  });

  it("插件自声明槽：声明方 = 声明插件 id（精确 key 与前缀声明）", () => {
    declUnregs.push(declareSlot({ key: "toolbar/com.test.conf", cardinality: "single", required: ["component"] }, "com.declarer"));
    unregs.push(registerSlotContrib("toolbar/com.test.conf", "com.a", { component: () => null }, { priority: 1 }));
    expect(slotChain("toolbar/com.test.conf").declarer).toBe("com.declarer");
    expect(slotDeclarer("toolbar/com.test.conf")).toBe("com.declarer");

    declUnregs.push(declareSlot({ key: "toolbar/com.test.pfx", prefix: true, cardinality: "single", required: ["component"] }, "com.declarer"));
    unregs.push(registerSlotContrib("toolbar/com.test.pfx/play", "com.a", { component: () => null }));
    expect(slotChain("toolbar/com.test.pfx/play").declarer).toBe("com.declarer");
  });

  it("声明随 fiber 撤销后回退宿主兜底", () => {
    const un = declareSlot({ key: "toolbar/com.test.conf", cardinality: "single", required: ["component"] }, "com.declarer");
    un();
    expect(slotDeclarer("toolbar/com.test.conf")).toBe("宿主");
  });
});

describe("审计快照含槽位贡献/装饰（归属可见数据面）", () => {
  it("注册贡献 + 装饰 → snapshot 按插件归集；卸载后消失", () => {
    kernel = createKernel();
    addView("view/com.test.audit", "com.test.audit", 2);
    unregs.push(registerSlotDecoratorFor("toolbar/note/right", "com.test.audit", () => null, { priority: 1 }));

    let snap = auditSnapshot(kernel.ctx);
    const entry = snap.find((e) => e.pluginId === "com.test.audit");
    expect(entry).toBeDefined();
    expect(entry?.slotContributions).toEqual([
      { slot: "view/com.test.audit", id: "com.test.audit:view/com.test.audit", cardinality: "single", priority: 2, label: "com.test.audit" },
    ]);
    expect(entry?.slotDecorators).toEqual([
      { slot: "toolbar/note/right", id: "com.test.audit:decorate:toolbar/note/right", priority: 1 },
    ]);

    for (const fn of unregs.splice(0)) fn();
    snap = auditSnapshot(kernel.ctx);
    expect(snap.some((e) => e.pluginId === "com.test.audit")).toBe(false);
  });

  it("listAllContributions/listAllDecorators 覆盖注册表快照", () => {
    addView("view/com.test.panel", "com.a");
    unregs.push(registerSlotDecoratorFor("toolbar/note/right", "com.b", () => null));
    expect(listAllContributions().map((c) => c.pluginId)).toContain("com.a");
    expect(listAllDecorators().map((d) => d.pluginId)).toContain("com.b");
  });
});
