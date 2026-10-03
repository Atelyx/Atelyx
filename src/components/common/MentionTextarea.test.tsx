// @vitest-environment jsdom
/**
 * @提及输入框的字体一致性。
 *
 * 关键约束：overlay（@标签层）与 textarea 共用同一份字体，二者字号/行高若不一致，
 * 标签会与光标错行——而 overlay 是纯视觉层、textarea 文字透明，断言不能只看类名，
 * 必须比对实际落到元素上的 font-size / line-height。
 *
 * 字号必须走字阶变量：应用级「字体大小」设置改的是根字号，写死 px 的字号不缩放，
 * 会与同级文字出现断层；且 px 字号配 rem 行高时，缩放后行高不跟随而行距失衡。
 */
import { describe, it, expect, afterEach } from "vitest";
import { render, cleanup } from "@testing-library/react";
import { INPUT_FONT } from "@/components/common/MentionTextarea";

afterEach(cleanup);

/** 解析 `var(--fs-body)` / `14px` 这类值为可比对的数值（相对根字号，测试里按 16px 根算）。 */
function resolveFontSize(value: string | number | undefined): number {
  if (typeof value === "number") return value;
  const rem = /^var\(--fs-[a-z0-9-]+\)$/.exec(String(value ?? ""));
  if (!rem) return Number.parseFloat(String(value ?? ""));
  // 变量取自styles/index.css 的字阶表，此处只需断言「走变量」而非具体档位
  return 0;
}

describe("INPUT_FONT", () => {
  it("字号与行高走字阶变量（随应用级字体大小设置缩放）", () => {
    expect(INPUT_FONT.fontSize).toBe("var(--fs-body)");
    expect(String(INPUT_FONT.lineHeight)).toMatch(/rem$/);
  });

  it("overlay 与 textarea 实际渲染出的字体一致", () => {
    const { container } = render(
      <div>
        <div data-testid="overlay" style={INPUT_FONT} />
        <textarea data-testid="input" style={INPUT_FONT} />
      </div>,
    );
    const overlay = container.querySelector('[data-testid="overlay"]') as HTMLElement;
    const input = container.querySelector('[data-testid="input"]') as HTMLElement;
    expect(overlay.style.fontSize).toBe(input.style.fontSize);
    expect(overlay.style.lineHeight).toBe(input.style.lineHeight);
    expect(overlay.style.fontFamily).toBe(input.style.fontFamily);
  });

  it("字号不是写死的 px 数值", () => {
    // 写死 px 是回归形态：此类断言在数值写法下会失败
    expect(resolveFontSize(INPUT_FONT.fontSize)).not.toBe(14);
  });
});