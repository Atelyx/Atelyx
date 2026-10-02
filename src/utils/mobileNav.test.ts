/**
 * 移动端导航顺序单测：保存序（可被用户手改，含脏值）与当前可用视图集合的收敛规则。
 */
import { describe, expect, it } from "vitest";
import { MOBILE_NAV_DEFAULT_ORDER, orderMobileViews, swapInOrder } from "./mobileNav";

describe("orderMobileViews", () => {
  it("无保存序时按内建常用序在前、其余按 id 追加", () => {
    const kinds = ["table", "canvas", "note", "recent", "files"];
    expect(orderMobileViews(kinds, [])).toEqual(["recent", "note", "files", "table", "canvas"]);
  });

  it("保存序生效且只作用于当前可用视图（已停用的插件视图自动剔除）", () => {
    const kinds = ["note", "table", "files"];
    expect(orderMobileViews(kinds, ["table", "gone", "note"])).toEqual(["table", "note", "files"]);
  });

  it("保存序里的重复项只保留一次（手改 global.json 的脏值不得产出重复 key）", () => {
    const kinds = ["note", "table", "files"];
    expect(orderMobileViews(kinds, ["note", "note", "table"])).toEqual(["note", "table", "files"]);
  });

  it("结果恒等于可用视图集合的排列（不丢不增）", () => {
    const kinds = ["a", "b", "c", "d"];
    expect([...orderMobileViews(kinds, ["c", "c", "x"])].sort()).toEqual([...kinds].sort());
  });

  it("缺省顺序里的视图全部不可用时，退化为可用集合本身", () => {
    expect(orderMobileViews(["plugin.b", "plugin.a"], [])).toEqual(["plugin.a", "plugin.b"]);
    // 缺省序常量与实现同源，避免两处各写一份
    expect(MOBILE_NAV_DEFAULT_ORDER.length).toBeGreaterThan(0);
  });
});

describe("swapInOrder", () => {
  it("相邻互换", () => {
    expect(swapInOrder(["a", "b", "c"], 0, 1)).toEqual(["b", "a", "c"]);
    expect(swapInOrder(["a", "b", "c"], 2, 1)).toEqual(["a", "c", "b"]);
  });

  it("越界或原地不动时原样返回（且不改原数组）", () => {
    const order = ["a", "b"];
    expect(swapInOrder(order, 1, 1)).toEqual(order);
    expect(swapInOrder(order, -1, 0)).toEqual(order);
    expect(swapInOrder(order, 0, 2)).toEqual(order);
    expect(order).toEqual(["a", "b"]);
  });
});
