/**
 * 分隔条拖拽重分配纯函数：平移、双侧下限钳制、其余子项不变。
 */
import { describe, it, expect } from "vitest";
import { applySplitDrag } from "@/components/layout/WorkspaceGrid";

describe("applySplitDrag", () => {
  it("按 delta 平移相邻两项，其余子项不变", () => {
    expect(applySplitDrag([50, 50], 1, 10)).toEqual([60, 40]);
    // 多叉：只动手柄相邻两项
    expect(applySplitDrag([30, 40, 30], 1, 5)).toEqual([35, 35, 30]);
    expect(applySplitDrag([30, 40, 30], 2, -5)).toEqual([30, 35, 35]);
  });

  it("左侧触底钳制到最小值，两侧之和守恒", () => {
    expect(applySplitDrag([20, 80], 1, -50)).toEqual([12, 88]);
  });

  it("右侧触底钳制到最小值，两侧之和守恒", () => {
    expect(applySplitDrag([80, 20], 1, 50)).toEqual([88, 12]);
  });

  it("两侧之和不足以容纳双下限时对半分", () => {
    expect(applySplitDrag([5, 15], 1, 5)).toEqual([10, 10]);
  });

  it("不修改原数组", () => {
    const sizes = [50, 50];
    applySplitDrag(sizes, 1, 10);
    expect(sizes).toEqual([50, 50]);
  });
});
