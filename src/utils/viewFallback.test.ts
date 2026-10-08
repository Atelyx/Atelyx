/**
 * 视图贡献缺失时的回退判定：首次装配在途 → 骨架，行停用/卸载 → 处置提示，其余 → 空白。
 */
import { describe, it, expect } from "vitest";
import { viewFallbackOf } from "@/utils/viewFallback";

describe("viewFallbackOf", () => {
  it("首次装配在途：无论提供者状态如何都是骨架（不闪假的「插件已卸载」）", () => {
    expect(viewFallbackOf({ assemblyLoading: true, initialized: false })).toBe("skeleton");
    expect(
      viewFallbackOf({ assemblyLoading: true, initialized: false, provider: { enabled: false } }),
    ).toBe("skeleton");
  });

  it("已装配完成后的重载空档不算首次：按提供者状态渲染，不闪现骨架", () => {
    // 行仍启用（重挂装填中）= 空白占位
    expect(
      viewFallbackOf({ assemblyLoading: true, initialized: true, provider: { enabled: true } }),
    ).toBe("blank");
    expect(viewFallbackOf({ assemblyLoading: false, initialized: true })).toBe("blank");
  });

  it("行停用/卸载 → 处置提示（在途标志已落回）", () => {
    expect(
      viewFallbackOf({ assemblyLoading: false, initialized: true, provider: { enabled: false } }),
    ).toBe("provider");
  });

  it("装配失败（从未完成过装配且不在途）走提供者状态，不永久停在骨架", () => {
    expect(
      viewFallbackOf({ assemblyLoading: false, initialized: false, provider: { enabled: false } }),
    ).toBe("provider");
    expect(viewFallbackOf({ assemblyLoading: false, initialized: false })).toBe("blank");
  });
});
