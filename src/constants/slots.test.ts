/**
 * 槽位声明表与注册校验测试：固定具名槽未声明即失败（附近似槽名提示），
 * 基数与载荷字段不符即失败，开放 kind 槽按前缀放行，声明表与宿主渲染点双向一致。
 */
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it } from "vitest";
import { SLOT_DECLARATIONS, findSlotDeclaration, slotPayloadShape, suggestSlotNames } from "@/constants/slots";
import {
  listSlot,
  registeredSlots,
  registerSlotContrib,
  registerViewSlot,
  resolveViewKind,
  unregisterSlot,
} from "@/services/cordis/slots";

afterEach(() => {
  // 槽注册表为模块级：按 id 兜底清空，防单个用例失败污染后续。
  for (const slot of registeredSlots()) {
    for (const contrib of listSlot(slot)) unregisterSlot(contrib.id);
  }
});

/** 递归收集 src 下的非测试源码文件（声明表 ↔ 渲染点守卫用）。 */
function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) sourceFiles(full, out);
    else if (/\.tsx?$/.test(entry) && !/\.test\.tsx?$/.test(entry)) out.push(full);
  }
  return out;
}

describe("槽位声明表", () => {
  it("固定具名槽与菜单目标精确命中，开放 kind 槽按前缀命中", () => {
    expect(findSlotDeclaration("toolbar/note/right")?.cardinality).toBe("list");
    expect(findSlotDeclaration("settings/theme")?.key).toBe("settings/theme");
    expect(findSlotDeclaration("contextmenu/canvas")?.cardinality).toBe("list");
    expect(findSlotDeclaration("view/canvas")?.key).toBe("view");
    expect(findSlotDeclaration("node/anything")?.key).toBe("node");
  });

  it("未声明的位置无匹配：裸前缀、空 kind、无宿主的菜单目标、未知家族", () => {
    expect(findSlotDeclaration("view")).toBeUndefined();
    expect(findSlotDeclaration("view/")).toBeUndefined();
    expect(findSlotDeclaration("contextmenu/file")).toBeUndefined();
    expect(findSlotDeclaration("toolbar/nope")).toBeUndefined();
    expect(findSlotDeclaration("nosuchfamily/x")).toBeUndefined();
  });

  it("声明表 key 唯一", () => {
    const keys = SLOT_DECLARATIONS.map((d) => d.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it("载荷形状由字段契约派生", () => {
    expect(slotPayloadShape(findSlotDeclaration("view/canvas")!)).toBe("{ label, component?, render? }");
    expect(slotPayloadShape(findSlotDeclaration("toolbar/note/right")!)).toBe("{ component }");
  });

  it("深冻结：list() 暴露的引用不可被插件改写", () => {
    expect(Object.isFrozen(SLOT_DECLARATIONS)).toBe(true);
    for (const decl of SLOT_DECLARATIONS) {
      expect(Object.isFrozen(decl), decl.key).toBe(true);
      expect(Object.isFrozen(decl.required), `${decl.key}.required`).toBe(true);
      if (decl.optional) expect(Object.isFrozen(decl.optional), `${decl.key}.optional`).toBe(true);
    }
    const target = SLOT_DECLARATIONS.find((d) => d.key === "toolbar/note/right")!;
    expect(() => {
      (target as unknown as { scope: string }).scope = "篡改";
    }).toThrow();
  });

  it("近似槽名提示：同族优先、按公共前缀排序、limit 截断", () => {
    expect(suggestSlotNames("toolbar/notes/right")[0]).toBe("toolbar/note/right");
    expect(suggestSlotNames("settings/unknown")).toHaveLength(3);
    expect(suggestSlotNames("settings/unknown", 1)).toHaveLength(1);
    expect(suggestSlotNames("toolbar/files", 0)).toEqual([]);
    expect(suggestSlotNames("nosuchfamily/x")).toEqual([]);
  });
});

describe("声明表 ↔ 宿主渲染点守卫", () => {
  // 覆盖范围：宿主用「字面量槽名」的 <SlotListMount> / <MenuSlotList>。
  // 不覆盖：view/node/edge/tableview 的宿主分派（ViewHost/CanvasView 按运行时 kind 解析，无法静态枚举），
  // 故下方显式拒绝动态槽名与未登记的 single 宿主，防它们绕过声明表。
  it("固定具名槽与 SlotListMount 渲染点双向一一对应", () => {
    const root = fileURLToPath(new URL("../..", import.meta.url));
    const rendered = new Set<string>();
    const menuTargets = new Set<string>();
    for (const file of sourceFiles(join(root, "src"))) {
      const text = readFileSync(file, "utf8");
      for (const m of text.matchAll(/<SlotListMount[^>]*\bslot="([^"]+)"/g)) if (m[1]) rendered.add(m[1]);
      for (const m of text.matchAll(/<MenuSlotList[^>]*\btarget="([^"]+)"/g)) if (m[1]) menuTargets.add(m[1]);
      expect(text, `${file}：槽位宿主须写字面量槽名，动态槽名会绕过声明表守卫`).not.toMatch(
        /<SlotListMount[^>]*\bslot=\{/,
      );
      expect(text, `${file}：菜单宿主须写字面量 target`).not.toMatch(/<MenuSlotList[^>]*\btarget=\{/);
      expect(text, `${file}：新增 single 槽宿主须先在声明表登记为 single 并更新本守卫`).not.toMatch(
        /<SlotMount[^>]*\bslot=/,
      );
    }
    // 双向：宿主渲染点都有声明（缺声明 = 注册静默丢失），声明都有渲染点（多声明 = 注册成功却不显示）。
    const declared = SLOT_DECLARATIONS.filter((d) => !d.prefix && !d.key.startsWith("contextmenu/")).map((d) => d.key);
    expect([...rendered].sort()).toEqual([...declared].sort());
    // 菜单目标与声明的 contextmenu 槽同样双向一致（无宿主的 target 不得留在声明里，反之亦然）。
    const declaredMenus = SLOT_DECLARATIONS.filter((d) => d.key.startsWith("contextmenu/")).map((d) =>
      d.key.slice("contextmenu/".length),
    );
    expect([...menuTargets].sort()).toEqual([...declaredMenus].sort());
  });
});

describe("注册校验", () => {
  it("未声明的固定槽 → 抛错并附近似槽名", () => {
    expect(() => registerSlotContrib("toolbar/notes/right", "com.test.p", {})).toThrow(/未声明的槽位/);
    expect(() => registerSlotContrib("toolbar/notes/right", "com.test.p", {})).toThrow(/toolbar\/note\/right/);
  });

  it("无同族候选 → 提示宿主未渲染该位置", () => {
    expect(() => registerSlotContrib("nosuchfamily/x", "com.test.p", {})).toThrow(/宿主未渲染该位置/);
  });

  it("注册到无宿主的菜单目标 → 抛错并提示已声明的目标", () => {
    expect(() =>
      registerSlotContrib(
        "contextmenu/file",
        "com.test.p",
        { label: "打开", onClick: () => undefined },
        { cardinality: "list" },
      ),
    ).toThrow(/contextmenu\/canvas/);
  });

  it("基数不符 → 抛错", () => {
    expect(() =>
      registerSlotContrib("toolbar/note/right", "com.test.p", {}, { cardinality: "single" }),
    ).toThrow(/基数/);
  });

  it("载荷缺必需字段 → 抛错", () => {
    expect(() =>
      registerSlotContrib("toolbar/note/right", "com.test.p", {}, { cardinality: "list" }),
    ).toThrow(/缺少字段：component/);
    expect(() => registerSlotContrib("view/x", "com.test.p", { component: () => null })).toThrow(
      /缺少字段：label/,
    );
  });

  it("缺字段只看自有属性（原型链同名成员不算提供）", () => {
    const payload = Object.create({ component: () => null }) as Record<string, unknown>;
    expect(() =>
      registerSlotContrib("toolbar/note/right", "com.test.p", payload, { cardinality: "list" }),
    ).toThrow(/缺少字段：component/);
  });

  it("载荷带未知字段 → 抛错（字段名拼错不再静默渲染空白）", () => {
    // 缺必需字段优先报「缺少字段」，二者都指出错处（不静默）。
    expect(() =>
      registerSlotContrib("toolbar/note/right", "com.test.p", { compnent: () => null }, { cardinality: "list" }),
    ).toThrow(/缺少字段：component/);
    // 必需字段齐备但有拼错的额外字段 → 报未知字段。
    expect(() =>
      registerSlotContrib(
        "toolbar/note/right",
        "com.test.p",
        { component: () => null, compnent: () => null },
        { cardinality: "list" },
      ),
    ).toThrow(/未知字段：compnent/);
  });

  it("载荷非对象 → 抛错", () => {
    expect(() =>
      registerSlotContrib("toolbar/note/right", "com.test.p", null, { cardinality: "list" }),
    ).toThrow(/须为对象/);
  });

  it("已声明的固定槽注册成功且可撤销", () => {
    const off = registerSlotContrib(
      "toolbar/note/right",
      "com.test.p",
      { component: () => null },
      { cardinality: "list" },
    );
    expect(listSlot("toolbar/note/right")).toHaveLength(1);
    off();
    expect(listSlot("toolbar/note/right")).toEqual([]);
  });

  it("开放 kind 槽：可自定 kind，component 与 render 可并存", () => {
    const off = registerViewSlot("my-custom-view", "com.test.p", {
      label: "自定义",
      component: () => null,
      render: () => null,
    });
    expect(resolveViewKind("my-custom-view")?.payload.label).toBe("自定义");
    off();
    expect(resolveViewKind("my-custom-view")).toBeUndefined();
  });

  it("开放 kind 槽同样拒绝未知字段", () => {
    expect(() => registerViewSlot("my-custom-view", "com.test.p", { label: "视图", nope: 1 } as never)).toThrow(
      /未知字段：nope/,
    );
  });
});
