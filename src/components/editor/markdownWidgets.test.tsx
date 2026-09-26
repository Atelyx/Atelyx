/**
 * markdownWidgets DOM 回归测试（jsdom）：只读面代码块与表格 widget 的结构契约——
 * 代码块标题栏（语言名/复制按钮）与表格专属横向滚动包裹层。
 * 视觉样式归 index.css，这里只断言 DOM 结构与文案来源。
 * toDOM 的 view 参数仅 onEdit 路径使用，本文件不挂点击回调，传 null 即可。
 */
/** @vitest-environment jsdom */
import { describe, expect, it } from "vitest";
import type { EditorView } from "@codemirror/view";
import { CodeBlockWidget, TableWidget } from "./markdownWidgets";
import { fencedCodeLang } from "./markdownDecorations";

const view = null as unknown as EditorView;

describe("fencedCodeLang", () => {
  it("取 info string 首个词为语言名", () => {
    expect(fencedCodeLang("```python\nprint(1)\n```")).toBe("python");
    expect(fencedCodeLang('```ts title="示例"\ncode\n```')).toBe("ts");
    expect(fencedCodeLang("~~~py\nx\n~~~")).toBe("py");
  });

  it("无语言 = 空串", () => {
    expect(fencedCodeLang("```\ncode\n```")).toBe("");
  });
});

describe("CodeBlockWidget", () => {
  it("标题栏含语言名与图标复制按钮，代码进 pre>code", () => {
    const dom = new CodeBlockWidget("const a = 1;", "ts").toDOM();
    expect(dom.className).toBe("md-editor-code-block");
    const head = dom.querySelector(".md-editor-code-head");
    expect(head).not.toBeNull();
    expect(dom.querySelector(".md-editor-code-lang")?.textContent).toBe("ts");
    const btn = dom.querySelector<HTMLButtonElement>(".md-editor-code-copy");
    expect(btn?.querySelector("svg")).not.toBeNull();
    expect(btn?.getAttribute("aria-label")).toBe("复制代码");
    expect(dom.querySelector("pre > code")?.textContent).toBe("const a = 1;");
  });

  it("无语言时标题栏显示「代码」", () => {
    const dom = new CodeBlockWidget("x = 1", "").toDOM();
    expect(dom.querySelector(".md-editor-code-lang")?.textContent).toBe("代码");
  });

  it("eq 同时比较代码与语言名", () => {
    const w = new CodeBlockWidget("a", "ts");
    expect(w.eq(new CodeBlockWidget("a", "ts"))).toBe(true);
    expect(w.eq(new CodeBlockWidget("a", "js"))).toBe(false);
    expect(w.eq(new CodeBlockWidget("b", "ts"))).toBe(false);
  });
});

describe("TableWidget", () => {
  it("表格包进专属横向滚动容器，单元格取原文", () => {
    const dom = new TableWidget("| a | b |\n| --- | ---: |\n| 1 | 2 |", null).toDOM(view);
    expect(dom.className).toBe("md-editor-table-wrap");
    const table = dom.querySelector("table.md-editor-table");
    expect(table).not.toBeNull();
    const ths = dom.querySelectorAll("th");
    expect(ths.length).toBe(2);
    expect(ths[0]?.textContent).toBe("a");
    expect(dom.querySelectorAll("tbody tr").length).toBe(1);
  });

  it("退化源码（不足两行）也包进滚动容器", () => {
    const dom = new TableWidget("| a |", null).toDOM(view);
    expect(dom.className).toBe("md-editor-table-wrap");
    expect(dom.querySelector("table")?.textContent).toBe("| a |");
  });
});
