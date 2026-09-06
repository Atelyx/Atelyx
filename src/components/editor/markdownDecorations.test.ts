/**
 * buildDecorations 回归测试（纯 state 侧，无 DOM）：
 * 覆盖全语法混合文档，验证装饰构建不抛 RangeSetBuilder 排序错误、
 * 各类 widget/行装饰都能产出（防排序/重叠类回归）。
 */
import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import type { DecorationSet } from "@codemirror/view";
import { buildDecorations } from "./markdownDecorations";
import {
  TableWidget,
  MathWidget,
  HtmlWidget,
  FootnoteDefWidget,
  type DecorationOptions,
} from "./markdownWidgets";

const opts: DecorationOptions = {
  vaultRoot: null,
  readOnly: true,
  interactiveCheckbox: true,
  onOpenUrl: () => {},
  onOpenPath: () => {},
  readImage: async () => null,
  mentions: [{ key: "n1", label: "文件A" }],
  onMentionClick: () => {},
};

function build(md: string, readOnly = true): DecorationSet {
  const state = EditorState.create({
    doc: md,
    extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
  });
  return buildDecorations(
    state,
    { ...opts, readOnly },
    () => {
      throw new Error("widget dispatch 不应在纯构建路径触发");
    },
  );
}

/** 收集区间内所有 widget 的类名（按文档位置）。 */
function widgetNames(decs: DecorationSet, len: number): string[] {
  const names: string[] = [];
  decs.between(0, len, (_from, _to, dec) => {
    const w = (dec as { spec?: { widget?: object } }).spec?.widget;
    if (w) names.push((w as { constructor?: { name?: string } }).constructor?.name ?? "?");
  });
  return names;
}

const MIXED_DOC = [
  "# 标题一",
  "",
  "**粗体** 与 *斜体* 与 `行内代码` 与 ~~删除线~~ 与 ==高亮== 与 %%注释%%",
  "",
  "| 列A | 列B |",
  "| --- | :---: |",
  "| 1 | 2 |",
  "",
  "> [!note] 提示",
  "> 引用内容",
  "",
  "- [ ] 任务一",
  "- [x] 任务二",
  "  - 嵌套列表",
  "",
  "1. 有序一",
  "2. 有序二",
  "",
  "$$x^2 + y^2 = z^2$$",
  "",
  "行内公式 $E=mc^2$，标签 #tag1，wiki [[笔记A|别名]]，脚注[^1]",
  "",
  "[^1]: 脚注定义",
  "",
  "<kbd>Ctrl</kbd> + <kbd>C</kbd>",
  "",
  "<details><summary>展开</summary>隐藏内容</details>",
  "",
  "```ts",
  "const a: number = 1;",
  "```",
  "",
  "---",
  "",
  "外部链接 [点我](https://example.com)，空链接 [新建]()",
  "",
  "用户消息 @文件A 结尾",
].join("\n");

describe("buildDecorations", () => {
  it("混合语法文档构建不抛错（只读）", () => {
    expect(() => build(MIXED_DOC)).not.toThrow();
  });

  it("可编辑态（光标行显示原文）构建不抛错", () => {
    expect(() => build(MIXED_DOC, false)).not.toThrow();
  });

  it("空文档与纯文本不抛错", () => {
    expect(() => build("")).not.toThrow();
    expect(() => build("只有一行纯文本")).not.toThrow();
  });

  it("行装饰与 widget 同 from 时不抛错（标题 + 任务列表首行）", () => {
    const doc = ["- [ ] 任务", "  - [x] 子任务", "# 标题", "正文"].join("\n");
    expect(() => build(doc)).not.toThrow();
  });
});

describe("图片渲染", () => {
  it("`![alt](url)` 渲染 ImageWidget（lezer Image 节点含前导 !，解析前须剥除）", () => {
    const doc = "![图](img.png)";
    expect(widgetNames(build(doc), doc.length)).toContain("ImageWidget");
  });

  it("行内代码内的图片语法不装饰（opaque）", () => {
    const doc = "`![x](y.png)` 后文";
    expect(widgetNames(build(doc), doc.length)).not.toContain("ImageWidget");
  });
});

describe("行内代码内语法保持源码（opaque 一致性）", () => {
  it("行内代码内 `[[wiki]]` 不装饰成链接", () => {
    const doc = "`[[w]]` 与后文";
    expect(widgetNames(build(doc), doc.length)).not.toContain("LinkWidget");
  });

  it("行内代码内 `#tag` 不装饰成胶囊", () => {
    const doc = "`#tag` 与后文";
    expect(widgetNames(build(doc), doc.length)).not.toContain("TagWidget");
  });
});

describe("表格点击编辑（光标起点边界）", () => {
  const TABLE = "| 列A | 列B |\n| --- | --- |\n| 1 | 2 |\n";

  function buildAt(md: string, anchor: number): DecorationSet {
    const state = EditorState.create({
      doc: md,
      selection: { anchor },
      extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
    });
    return buildDecorations(state, { ...opts, readOnly: false }, () => {});
  }

  it("光标落在表格块首（from）→ 表格 widget 撕掉、源码可见", () => {
    const decs = buildAt(TABLE, 0);
    expect(widgetNames(decs, TABLE.length)).not.toContain("TableWidget");
  });

  it("光标在表格块外 → 表格 widget 渲染", () => {
    const md = "前置\n\n" + TABLE;
    const decs = buildAt(md, 0);
    expect(widgetNames(decs, md.length)).toContain("TableWidget");
  });

  it("多字符选区与表格重叠 → 表格 widget 撕掉", () => {
    const md = "前置\n\n" + TABLE;
    const state = EditorState.create({
      doc: md,
      selection: { anchor: 5, head: 10 },
      extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
    });
    const decs = buildDecorations(state, { ...opts, readOnly: false }, () => {});
    expect(widgetNames(decs, md.length)).not.toContain("TableWidget");
  });
});

describe("widget eq（只读↔编辑翻转须强制重画）", () => {
  // CM 按 eq 决定是否复用旧 DOM；eq 漏比 onEdit 会让翻转后保留无监听器的旧 DOM，
  // 点击编辑（表格/公式/HTML/脚注块）失效。这里验证 onEdit 有无翻转 → eq false。
  it("TableWidget：onEdit 有无翻转 → eq false；状态相同 → eq true", () => {
    const src = "| a |\n| - |\n| 1 |\n";
    expect(new TableWidget(src, null).eq(new TableWidget(src, () => {}))).toBe(false);
    expect(new TableWidget(src, () => {}).eq(new TableWidget(src, () => {}))).toBe(true);
  });

  it("MathWidget：onEdit 有无翻转 → eq false", () => {
    expect(new MathWidget("x", false, null).eq(new MathWidget("x", false, () => {}))).toBe(false);
    expect(new MathWidget("x", false, () => {}).eq(new MathWidget("x", false, () => {}))).toBe(true);
  });

  it("HtmlWidget：onEdit 有无翻转 → eq false", () => {
    const html = "<b>x</b>";
    expect(
      new HtmlWidget(html, false, opts, null).eq(new HtmlWidget(html, false, opts, () => {})),
    ).toBe(false);
    expect(
      new HtmlWidget(html, false, opts, () => {}).eq(new HtmlWidget(html, false, opts, () => {})),
    ).toBe(true);
  });

  it("FootnoteDefWidget：onEdit 有无翻转 → eq false", () => {
    expect(new FootnoteDefWidget("1", "text", null).eq(new FootnoteDefWidget("1", "text", () => {}))).toBe(
      false,
    );
  });
});
