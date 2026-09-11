/**
 * buildDecorations 回归测试（纯 state 侧，无 DOM）：
 * 覆盖全语法混合文档，验证装饰构建不抛 RangeSetBuilder 排序错误、
 * 各类 widget/行装饰都能产出（防排序/重叠类回归）。
 */
import { describe, expect, it } from "vitest";
import { EditorState, StateEffect, type Extension } from "@codemirror/state";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { syntaxTree } from "@codemirror/language";
import type { DecorationSet } from "@codemirror/view";
import {
  buildDecorations,
  blockLineAtEdge,
  livePreviewNeedsRebuild,
  sourceLineAtFraction,
  taskMarkerRange,
} from "./markdownDecorations";
import {
  TableWidget,
  MathWidget,
  HtmlWidget,
  FootnoteDefWidget,
  LinkWidget,
  parseBracketLink,
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

/** 收集区间内所有 LinkWidget（按文档位置）。 */
function linkWidgets(decs: DecorationSet, len: number): LinkWidget[] {
  const out: LinkWidget[] = [];
  decs.between(0, len, (_from, _to, dec) => {
    const w = (dec as { spec?: { widget?: object } }).spec?.widget;
    if (w instanceof LinkWidget) out.push(w);
  });
  return out;
}

/** LinkWidget 私有字段的只读视口（测试内窥视模式与点击回调，不改生产可见性）。 */
function linkInfo(w: LinkWidget): { text: string; url: string; kind: string; click: () => void } {
  const v = w as unknown as { text: string; url: string; kind: string; onClick: () => void };
  return { text: v.text, url: v.url, kind: v.kind, click: v.onClick };
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

describe("括号链接与 `[名]()` 快捷新建（URL 可空）", () => {
  function buildWithCreate(md: string, onCreateNote: (label: string) => void): DecorationSet {
    const state = EditorState.create({
      doc: md,
      extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
    });
    return buildDecorations(state, { ...opts, onCreateNote }, () => {});
  }

  it("混合语法文档中的 `[新建]()` 产出可点击 create widget，点击回调收到 label", () => {
    const created: string[] = [];
    const links = linkWidgets(buildWithCreate(MIXED_DOC, (l) => created.push(l)), MIXED_DOC.length).map(linkInfo);
    const create = links.filter((l) => l.kind === "create");
    expect(create).toHaveLength(1);
    expect(create[0]).toMatchObject({ text: "新建", url: "" });
    create[0].click();
    expect(created).toEqual(["新建"]);
  });

  it("`[名]( )` 空白路径 trim 后同样走 create", () => {
    const doc = "[新建]( )";
    const links = linkWidgets(buildWithCreate(doc, () => {}), doc.length).map(linkInfo);
    expect(links.filter((l) => l.kind === "create")).toHaveLength(1);
  });

  it("`[]()` 空 label 不产出 create widget（避免建出无名笔记）", () => {
    const doc = "[]()";
    const links = linkWidgets(buildWithCreate(doc, () => {}), doc.length).map(linkInfo);
    expect(links.some((l) => l.kind === "create")).toBe(false);
  });

  it("`[x](<url>)` 尖括号形式 URL 非空，不产出 create widget", () => {
    const doc = "[x](<https://e.com>)";
    const links = linkWidgets(buildWithCreate(doc, () => {}), doc.length).map(linkInfo);
    expect(links.some((l) => l.kind === "create")).toBe(false);
  });

  it("`[点我](https://example.com)` 仍渲染为外链 widget", () => {
    const doc = "[点我](https://example.com)";
    const links = linkWidgets(buildWithCreate(doc, () => {}), doc.length).map(linkInfo);
    expect(links.map((l) => l.kind)).toEqual(["external"]);
    expect(links[0].text).toBe("点我");
  });

  it("parseBracketLink：URL 可空但 label 保留，title 形式不误解析", () => {
    expect(parseBracketLink("[新建]()")).toEqual({ label: "新建", url: "" });
    expect(parseBracketLink("[新建]( )")).toEqual({ label: "新建", url: "" });
    expect(parseBracketLink("[]()")).toEqual({ label: "", url: "" });
    expect(parseBracketLink("[a](b.md)")).toEqual({ label: "a", url: "b.md" });
    expect(parseBracketLink('[a](b.md "标题")')).toEqual({ label: "a", url: "b.md" });
    // 尖括号形式原样保留 `<...>`（非空 url，不判成空路径；也因此不会被当作外链前缀）
    expect(parseBracketLink("[a](<https://e.com>)")).toEqual({ label: "a", url: "<https://e.com>" });
    expect(parseBracketLink("不是链接")).toBeNull();
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

  it("光标落在块整行区间末端（末行行尾，即点最后一行源码末尾）→ 保持源码、不坍缩回 widget", () => {
    // 修复前：半开区间 head < to 在 head == 末行行尾（点末行源码末尾）时误判移出 → widget 回包
    const decs = buildAt(TABLE, TABLE.length - 1);
    expect(widgetNames(decs, TABLE.length)).not.toContain("TableWidget");
  });

  it("光标移到块后下一行（空行分隔）内容 → 恢复渲染 widget", () => {
    const md = TABLE + "\n尾行";
    const decs = buildAt(md, md.length - 1);
    expect(widgetNames(decs, md.length)).toContain("TableWidget");
  });
});

describe("sourceLineAtFraction（点击纵向比例 → 源码对应行，F2）", () => {
  const md = ["前置", "", "| 列A | 列B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
  const state = EditorState.create({
    doc: md,
    extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
  });
  const doc = state.doc;
  // 表格源码从行 3 到行 5
  const from = doc.line(3).from;
  const to = doc.line(5).to;

  it("frac 0 → 首行起点", () => {
    expect(sourceLineAtFraction(state, from, to, 0)).toBe(doc.line(3).from);
  });

  it("frac 0.5 → 中间行起点（点击表格中部落中间行源码）", () => {
    expect(sourceLineAtFraction(state, from, to, 0.5)).toBe(doc.line(4).from);
  });

  it("frac 0.99 → 末行起点（点击块底缘落末行源码，不再恒落块首）", () => {
    expect(sourceLineAtFraction(state, from, to, 0.99)).toBe(doc.line(5).from);
  });

  it("超出比例钳制（frac<0 / >1 不越界）", () => {
    expect(sourceLineAtFraction(state, from, to, -1)).toBe(doc.line(3).from);
    expect(sourceLineAtFraction(state, from, to, 5)).toBe(doc.line(5).from);
  });
});

describe("blockLineAtEdge（以块当前区间边缘反推落点，修复构建期闭包陈旧）", () => {
  const OLD_MD = ["前置", "", "| 列A | 列B |", "| --- | --- |", "| 1 | 2 |"].join("\n");
  const NEW_MD = ["前置", "", "", "| 列A | 列B |", "| --- | --- |", "| 1 | 2 |"].join("\n"); // 上方多插一行
  const newDoc = EditorState.create({ doc: NEW_MD, extensions: [markdown({ addKeymap: false, base: markdownLanguage })] }).doc;
  const oldDoc = EditorState.create({ doc: OLD_MD, extensions: [markdown({ addKeymap: false, base: markdownLanguage })] }).doc;
  // 构建期捕获的行数：旧文档中表格 3 行（源码未变 → 行数稳定）
  const lineCount = oldDoc.line(5).number - oldDoc.line(3).number + 1;

  it("上半点击（edgePos = 块当前 from）→ 落新文档首行，不随过期坐标漂移", () => {
    const currentFrom = newDoc.line(4).from; // 新文档中表格首行（posAtCoords 上半返回 from 边缘）
    expect(blockLineAtEdge(newDoc, 0.3, currentFrom, lineCount)).toBe(newDoc.line(4).from);
  });

  it("中部点击（edgePos = 块当前 to）→ 落新文档中间行（分隔行）", () => {
    const currentTo = newDoc.line(6).to; // posAtCoords 下半返回 to 边缘
    expect(blockLineAtEdge(newDoc, 0.5, currentTo, lineCount)).toBe(newDoc.line(5).from);
  });

  it("下半点击（edgePos = 块当前 to）→ 落新文档末行", () => {
    const currentTo = newDoc.line(6).to;
    expect(blockLineAtEdge(newDoc, 0.9, currentTo, lineCount)).toBe(newDoc.line(6).from);
  });
});

describe("taskMarkerRange（勾选框以当前行重扫定位）", () => {
  it("无序任务标记", () => {
    expect(taskMarkerRange("- [ ] 待办", 0)).toEqual({ from: 0, to: 5 });
    expect(taskMarkerRange("  - [x] 完成", 2)).toEqual({ from: 4, to: 9 });
  });

  it("有序任务标记", () => {
    expect(taskMarkerRange("1. [ ] 项", 0)).toEqual({ from: 0, to: 6 });
    expect(taskMarkerRange("10) [x] 项", 0)).toEqual({ from: 0, to: 7 });
  });

  it("无标记 → null", () => {
    expect(taskMarkerRange("- 普通列表", 0)).toBeNull();
    expect(taskMarkerRange("正文 [1]", 0)).toBeNull();
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

describe("livePreviewNeedsRebuild（装饰重建判定）", () => {
  const roEffect = StateEffect.define<boolean>();
  const marker = StateEffect.define<null>();
  // 两个独立的 markdown() 实例：配置不同 → 重新配置语言 facet 会换掉语法树引用
  const langA = markdown({ addKeymap: false, base: markdownLanguage });
  const langB = markdown({ addKeymap: false, base: markdownLanguage });
  const DOC = "# 标题\n\n正文 [点我](https://example.com)\n\n| a | b |\n| - | - |\n| 1 | 2 |\n";

  function stateWithLanguage(ext: Extension): EditorState {
    return EditorState.create({ doc: DOC, extensions: [ext] });
  }

  it("文档变化 → 重建", () => {
    const tr = stateWithLanguage(langA).update({ changes: { from: 0, insert: "x" } });
    expect(livePreviewNeedsRebuild(tr, roEffect)).toBe(true);
  });

  it("显式选区变化 → 重建", () => {
    const tr = stateWithLanguage(langA).update({ selection: { anchor: 1 } });
    expect(livePreviewNeedsRebuild(tr, roEffect)).toBe(true);
  });

  it("只读切换效果 → 重建", () => {
    const tr = stateWithLanguage(langA).update({ effects: roEffect.of(false) });
    expect(livePreviewNeedsRebuild(tr, roEffect)).toBe(true);
  });

  it("effects-only 且语法树推进 → 重建（后台解析补完的唯一信号）", () => {
    const tr = stateWithLanguage(langA).update({ effects: StateEffect.reconfigure.of([langB]) });
    expect(tr.docChanged).toBe(false);
    expect(tr.selection).toBeUndefined();
    expect(syntaxTree(tr.state)).not.toBe(syntaxTree(tr.startState));
    expect(livePreviewNeedsRebuild(tr, roEffect)).toBe(true);
  });

  it("effects-only 且语法树未变 → 不重建（无关 effect 不触发全量重建）", () => {
    const tr = stateWithLanguage(langA).update({ effects: marker.of(null) });
    expect(syntaxTree(tr.state)).toBe(syntaxTree(tr.startState));
    expect(livePreviewNeedsRebuild(tr, roEffect)).toBe(false);
  });
});
