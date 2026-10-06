// @vitest-environment jsdom
/**
 * 单元格两态（显示面 / 编辑框）的文字档位契约。
 *
 * 单元格的显示面与编辑面是**两个元素**（显示 div + 绝对铺满的编辑框），字号与字体族只能各写
 * 一份声明。两处声明叉开时，进入编辑的那一刻文字就跳档——字号与行高成对变化，首行基线还会
 * 位移。故此处锁死「两态各自只声明一个字号档、且两者同档」，字体族同理。
 *
 * 档位类并存时按产物样式表源序决胜（不看 className 书写顺序），故「基元自带档 + 调用点档」
 * 并存的写法即便看起来一致也可能被静默压掉，用例以「恰好一档」把这种写法挡在门外。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent, cleanup } from "@testing-library/react";

vi.mock("@/stores/tableStore", async () => {
  const { create } = await import("zustand");
  const useTableStore = create(() => ({
    rows: [{ id: "r1", values: { f1: "hello", f2: 42 } }],
    selection: undefined,
    undoResetCell: undefined,
    updateCell: vi.fn(),
    beginCellEdit: vi.fn(),
    commitCellEdit: vi.fn(),
    abortCellEdit: vi.fn(),
    selectRow: vi.fn(),
    pushUndo: vi.fn(),
  }));
  return { useTableStore };
});

import { useTableStore } from "@/stores/tableStore";
import { TableCell } from "@/components/table/TableCell";

/** 字阶刻度（`tailwind.config.js` 的 `fontSize`）。 */
const FONT_SIZE_TOKENS = new Set([
  "text-micro",
  "text-caption",
  "text-ui",
  "text-body",
  "text-h2",
  "text-h1",
  "text-display",
  "text-xs",
  "text-sm",
  "text-base",
  "text-lg",
  "text-xl",
  "text-2xl",
]);

/** 元素上声明的字号档（空数组 = 靠继承，与所在表一致）。 */
function fontTokensOf(el: Element): string[] {
  return [...el.classList].filter((c) => FONT_SIZE_TOKENS.has(c));
}

const textField = { id: "f1", name: "名称", type: "text" } as never;
const numberField = { id: "f2", name: "数量", type: "number" } as never;
const row = { id: "r1", values: { f1: "hello", f2: 42 } } as never;

function select(fieldId: string): void {
  useTableStore.setState({ selection: { kind: "cell", rowId: "r1", fieldId } });
}

function editorOf(container: HTMLElement): HTMLElement {
  const editor = container.querySelector<HTMLElement>("[data-cell-editor]");
  expect(editor).not.toBeNull();
  return editor as HTMLElement;
}

beforeEach(() => {
  cleanup();
  useTableStore.setState({ selection: undefined });
});

describe("单元格两态的文字档位", () => {
  it("文本列：显示面与编辑框同字号档，双击进入编辑也不跳档", () => {
    select("f1");
    const { container } = render(<TableCell field={textField} row={row} />);
    const display = container.firstElementChild as HTMLElement;
    const editor = editorOf(container);

    expect(fontTokensOf(display)).toHaveLength(1);
    expect(fontTokensOf(editor)).toEqual(fontTokensOf(display));

    // 双击进入编辑态（编辑框显形、显示面转为占位）后仍是同档
    fireEvent.doubleClick(display);
    expect(editor.dataset.editing).toBeDefined();
    expect(fontTokensOf(editor)).toEqual(fontTokensOf(display));
  });

  it("数字列：显示面与编辑框同字号档，且编辑框同样等宽", () => {
    select("f2");
    const { container } = render(<TableCell field={numberField} row={row} />);
    const wrapper = container.firstElementChild as HTMLElement;
    // 数字列的显示面是外层容器里的显示行（外层只承担定位）
    const display = wrapper.firstElementChild as HTMLElement;
    const editor = editorOf(container);

    expect(fontTokensOf(editor)).toEqual(fontTokensOf(display));
    // 数值一律等宽（按列扫读），编辑态不得退回默认字体
    expect(display.style.fontFamily).toBe("var(--font-mono)");
    expect(editor.style.fontFamily).toBe("var(--font-mono)");

    fireEvent.doubleClick(wrapper);
    expect(editor.dataset.editing).toBeDefined();
    expect(fontTokensOf(editor)).toEqual(fontTokensOf(display));
    expect(editor.style.fontFamily).toBe("var(--font-mono)");
  });
});
