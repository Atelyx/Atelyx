// @vitest-environment jsdom
/**
 * 图片单元格的文件选择输入与两个添加入口。
 *
 * 输入必须在空态与已有图片两个分支都在场——追加按钮与空态按钮共用同一个 ref，输入只在
 * 其中一支渲染时另一支的 click 会落到空 ref 上（静默无反应）。选择支持多选，一次选择整批
 * 交给 store。store 与图片解析以替身注入，避免拉起内容面 / 服务依赖。
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, fireEvent, cleanup } from "@testing-library/react";

const h = vi.hoisted(() => ({
  addImagesToCell: vi.fn(async () => {}),
}));

vi.mock("@/stores/tableStore", async () => {
  const { create } = await import("zustand");
  const useTableStore = create(() => ({
    title: "表",
    addImagesToCell: h.addImagesToCell,
    removeImageAt: vi.fn(),
    toggleImageDisplay: vi.fn(),
    reorderImages: vi.fn(),
    copyImageToClipboard: vi.fn(async () => true),
    downloadImageToDownloads: vi.fn(async () => true),
  }));
  return { useTableStore };
});

vi.mock("@/hooks/useTableImageSrc", () => ({
  useTableImageSrc: () => null,
  resolveTableImageEntry: async () => "",
  resolveTableImageEntries: async (entries: string[]) => entries.map(() => ""),
}));

import { ImageCell } from "@/components/table/ImageCell";

const field = { id: "f1", name: "图", type: "image" } as never;
const rowWith = (images: string[]) => ({ id: "r1", values: { f1: { images } } }) as never;

function fileInput(container: HTMLElement): HTMLInputElement {
  const input = container.querySelector('input[type="file"]');
  expect(input).not.toBeNull();
  return input as HTMLInputElement;
}

beforeEach(() => {
  cleanup();
  h.addImagesToCell.mockClear();
});

describe("图片单元格文件选择输入", () => {
  it("空单元格：「添加图片」按钮点击触发文件选择", () => {
    const { container, getByTitle } = render(<ImageCell field={field} row={rowWith([])} />);
    const input = fileInput(container);
    const click = vi.spyOn(input, "click").mockImplementation(() => {});
    fireEvent.click(getByTitle("添加图片"));
    expect(click).toHaveBeenCalledTimes(1);
  });

  it("已有图片：「追加图片」按钮点击触发同一个文件选择（输入必须在两支都在场）", () => {
    const { container, getByTitle } = render(
      <ImageCell field={field} row={rowWith(["attachments/a.png"])} />,
    );
    const input = fileInput(container);
    expect(input.multiple).toBe(true);
    const click = vi.spyOn(input, "click").mockImplementation(() => {});
    fireEvent.click(getByTitle("追加图片"));
    expect(click).toHaveBeenCalledTimes(1);
  });

  it("多选：一次选择多张整批交给 store（整批一步撤销）", () => {
    const { container } = render(
      <ImageCell field={field} row={rowWith(["attachments/a.png"])} />,
    );
    const input = fileInput(container);
    const files = [
      new File([new Uint8Array([1])], "a.png", { type: "image/png" }),
      new File([new Uint8Array([2])], "b.png", { type: "image/png" }),
    ];
    Object.defineProperty(input, "files", { value: files, configurable: true });

    fireEvent.change(input);

    expect(h.addImagesToCell).toHaveBeenCalledTimes(1);
    expect(h.addImagesToCell).toHaveBeenCalledWith("r1", "f1", files);
  });
});
