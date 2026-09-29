/**
 * buildDecorations 回归测试（纯 state 侧 + 快捷新建回填的 jsdom 视图测试）：
 * 覆盖全语法混合文档，验证装饰构建不抛 RangeSetBuilder 排序错误、
 * 各类 widget/行装饰都能产出（防排序/重叠类回归）。
 */
/** @vitest-environment jsdom */
import { describe, expect, it } from "vitest";
import { EditorState, StateEffect, type Extension } from "@codemirror/state";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { syntaxTree } from "@codemirror/language";
import { EditorView, type DecorationSet } from "@codemirror/view";
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
  type LinkClickContext,
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

/** 无视图点击上下文：回填定位对失效视图安全返回 null（仅验证点击回调通路时使用）。 */
const noViewCtx = { view: null, el: null } as unknown as LinkClickContext;

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
function linkInfo(w: LinkWidget): { text: string; url: string; kind: string; click: (ctx: LinkClickContext) => void } {
  const v = w as unknown as {
    text: string;
    url: string;
    kind: string;
    onClick: (ctx: LinkClickContext) => void;
  };
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

/** jsdom 的 Range 未实现 getClientRects（CM 测量需要）：补空实现防未捕获异常。 */
Object.defineProperty(Range.prototype, "getClientRects", {
  value: () => [],
  configurable: true,
});

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
  function buildWithCreate(md: string, onCreateNote: DecorationOptions["onCreateNote"]): DecorationSet {
    const state = EditorState.create({
      doc: md,
      extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
    });
    return buildDecorations(state, { ...opts, onCreateNote }, () => {});
  }

  it("混合语法文档中的 `[新建]()` 产出可点击 create widget，点击回调收到 label", async () => {
    const created: string[] = [];
    const links = linkWidgets(
      buildWithCreate(MIXED_DOC, (l) => {
        created.push(l);
        return Promise.resolve(null);
      }),
      MIXED_DOC.length,
    ).map(linkInfo);
    const create = links.filter((l) => l.kind === "create");
    expect(create).toHaveLength(1);
    expect(create[0]).toMatchObject({ text: "新建", url: "" });
    create[0].click(noViewCtx);
    await new Promise((r) => setTimeout(r, 0));
    expect(created).toEqual(["新建"]);
  });

  it("`[名]( )` 空白路径 trim 后同样走 create", () => {
    const doc = "[新建]( )";
    const links = linkWidgets(buildWithCreate(doc, async () => null), doc.length).map(linkInfo);
    expect(links.filter((l) => l.kind === "create")).toHaveLength(1);
  });

  it("`[]()` 空 label 不产出 create widget（避免建出无名笔记）", () => {
    const doc = "[]()";
    const links = linkWidgets(buildWithCreate(doc, async () => null), doc.length).map(linkInfo);
    expect(links.some((l) => l.kind === "create")).toBe(false);
  });

  it("`[x](<url>)` 尖括号形式 URL 非空，不产出 create widget", () => {
    const doc = "[x](<https://e.com>)";
    const links = linkWidgets(buildWithCreate(doc, async () => null), doc.length).map(linkInfo);
    expect(links.some((l) => l.kind === "create")).toBe(false);
  });

  it("`[点我](https://example.com)` 仍渲染为外链 widget", () => {
    const doc = "[点我](https://example.com)";
    const links = linkWidgets(buildWithCreate(doc, async () => null), doc.length).map(linkInfo);
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

describe("wiki 链接缺失态（未命中笔记 → 点击快捷新建 + 回填路径）", () => {
  function buildWiki(
    md: string,
    extra: Partial<DecorationOptions>,
  ): DecorationSet {
    const state = EditorState.create({
      doc: md,
      extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
    });
    return buildDecorations(state, { ...opts, ...extra }, () => {});
  }

  it("提供 resolveWikiNote 且未命中 + onCreateNote → create widget，点击回调收到目标名", async () => {
    const created: string[] = [];
    const doc = "引用 [[缺失笔记]] 结尾";
    const links = linkWidgets(
      buildWiki(doc, {
        resolveWikiNote: () => false,
        onCreateNote: (n) => {
          created.push(n);
          return Promise.resolve(null);
        },
      }),
      doc.length,
    ).map(linkInfo);
    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({ text: "缺失笔记", url: "", kind: "create" });
    links[0].click(noViewCtx);
    await new Promise((r) => setTimeout(r, 0));
    expect(created).toEqual(["缺失笔记"]);
  });

  it("resolveWikiNote 命中 → 正常 wiki 打开 widget（kind wiki），不产 create", () => {
    const doc = "引用 [[已有笔记]] 结尾";
    const links = linkWidgets(
      buildWiki(doc, { resolveWikiNote: () => true, onOpenNote: () => {} }),
      doc.length,
    ).map(linkInfo);
    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({ text: "已有笔记", kind: "wiki" });
  });

  it("别名语法缺失态：显示别名，点击新建用 `|` 前目标名", async () => {
    const created: string[] = [];
    const doc = "引用 [[目标笔记|别名]] 结尾";
    const links = linkWidgets(
      buildWiki(doc, {
        resolveWikiNote: () => false,
        onCreateNote: (n) => {
          created.push(n);
          return Promise.resolve(null);
        },
      }),
      doc.length,
    ).map(linkInfo);
    expect(links[0]).toMatchObject({ text: "别名", url: "", kind: "create" });
    links[0].click(noViewCtx);
    await new Promise((r) => setTimeout(r, 0));
    expect(created).toEqual(["目标笔记"]);
  });

  it("未提供 resolveWikiNote → 维持原行为（onOpenNote 打开 widget，不判缺失）", () => {
    const doc = "引用 [[任意目标]] 结尾";
    const links = linkWidgets(
      buildWiki(doc, { onOpenNote: () => {}, onCreateNote: async () => null }),
      doc.length,
    ).map(linkInfo);
    expect(links).toHaveLength(1);
    expect(links[0].kind).toBe("wiki");
  });

  function mountView(md: string, anchor = 0): EditorView {
    const parent = document.createElement("div");
    document.body.appendChild(parent);
    const state = EditorState.create({
      doc: md,
      selection: { anchor },
      extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
    });
    return new EditorView({ state, parent });
  }

  it("空链接点击新建后，源文回填为路径链接并打开新笔记", async () => {
    const view = mountView("[新建]()");
    const opened: [string, string][] = [];
    const decs = buildDecorations(
      view.state,
      {
        ...opts,
        readOnly: false,
        onCreateNote: (n) => Promise.resolve(`dir/${n}.md`),
        onOpenCreatedNote: (file, name) => opened.push([file, name]),
      },
      () => {},
    );
    const create = linkWidgets(decs, 6).map(linkInfo)[0];
    create.click({ view, el: document.createElement("span") });
    await new Promise((r) => setTimeout(r, 0));
    expect(view.state.doc.toString()).toBe("[新建](dir/新建.md)");
    expect(opened).toEqual([["dir/新建.md", "新建"]]);
    view.destroy();
  });

  it("空链接后随正文时回填同样生效", async () => {
    const doc = "[新建]() 后面还有文字";
    const view = mountView(doc, 0);
    const decs = buildDecorations(
      view.state,
      {
        ...opts,
        readOnly: false,
        onCreateNote: (n) => Promise.resolve(`dir/${n}.md`),
        onOpenCreatedNote: () => {},
      },
      () => {},
    );
    const create = linkWidgets(decs, doc.length).map(linkInfo)[0];
    expect(create).toBeDefined();
    create.click({ view, el: document.createElement("span") });
    await new Promise((r) => setTimeout(r, 0));
    expect(view.state.doc.toString()).toBe("[新建](dir/新建.md) 后面还有文字");
    view.destroy();
  });

  it("未命中 wiki 点击新建后回填，链接随即指向新笔记", async () => {
    const doc = "引用 [[缺失笔记]] 结尾\n";
    // 光标落在第二行：可编辑态光标行显示原文（wiki 装饰在光标行不渲染），须移出才能取到 widget
    const view = mountView(doc, doc.length);
    const decs = buildDecorations(
      view.state,
      {
        ...opts,
        readOnly: false,
        resolveWikiNote: () => false,
        onCreateNote: (n) => Promise.resolve(`${n}.md`),
        onOpenCreatedNote: () => {},
      },
      () => {},
    );
    const w = linkWidgets(decs, doc.length).map(linkInfo)[0];
    w.click({ view, el: document.createElement("span") });
    await new Promise((r) => setTimeout(r, 0));
    expect(view.state.doc.toString()).toBe("引用 [缺失笔记](缺失笔记.md) 结尾\n");
    view.destroy();
  });

  it("创建期间区间原文被改动 → 跳过回填不盲改，但仍创建并打开", async () => {
    const view = mountView("[新建]()");
    const opened: string[] = [];
    const decs = buildDecorations(
      view.state,
      {
        ...opts,
        readOnly: false,
        onOpenCreatedNote: (file) => opened.push(file),
        onCreateNote: (n) => {
          // 模拟创建期间用户在前方插入文本，构建期区间随之失效
          view.dispatch({ changes: { from: 0, insert: "前缀" } });
          return Promise.resolve(`${n}.md`);
        },
      },
      () => {},
    );
    const create = linkWidgets(decs, 6).map(linkInfo)[0];
    create.click({ view, el: document.createElement("span") });
    await new Promise((r) => setTimeout(r, 0));
    expect(view.state.doc.toString()).toBe("前缀[新建]()");
    expect(opened).toEqual(["新建.md"]);
    view.destroy();
  });

  it("只读面（对话气泡/预览）跳过回填：仅创建并打开，不改写展示文本", async () => {
    const view = mountView("[新建]()");
    const opened: string[] = [];
    const decs = buildDecorations(
      view.state,
      {
        ...opts,
        readOnly: true,
        onOpenCreatedNote: (file) => opened.push(file),
        onCreateNote: (n) => Promise.resolve(`${n}.md`),
      },
      () => {},
    );
    const create = linkWidgets(decs, 6).map(linkInfo)[0];
    create.click({ view, el: document.createElement("span") });
    await new Promise((r) => setTimeout(r, 0));
    expect(view.state.doc.toString()).toBe("[新建]()");
    expect(opened).toEqual(["新建.md"]);
    view.destroy();
  });

  it("未命中路径链接 → create widget（url 保留原路径），点击按路径创建且不回填", async () => {
    const doc = "见 [双向链接](笔记/双向链接.md) 一节";
    const created: string[] = [];
    const opened: [string, string][] = [];
    const view = mountView(doc);
    const decs = buildDecorations(
      view.state,
      {
        ...opts,
        onCreateNote: (n) => {
          created.push(n);
          return Promise.resolve(n);
        },
        onOpenCreatedNote: (file, name) => opened.push([file, name]),
      },
      () => {},
    );
    const links = linkWidgets(decs, doc.length).map(linkInfo);
    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({ text: "双向链接", url: "笔记/双向链接.md", kind: "create" });
    links[0].click({ view, el: document.createElement("span") });
    await new Promise((r) => setTimeout(r, 0));
    expect(created).toEqual(["笔记/双向链接.md"]);
    expect(view.state.doc.toString()).toBe(doc);
    // 打开回调：file = 实际落盘路径，name = 笔记显示名（非原始路径串）
    expect(opened).toEqual([["笔记/双向链接.md", "双向链接"]]);
    view.destroy();
  });

  it("路径链接含 `..` → 保持原文不产出 create widget；无后缀路径可创建", () => {
    const doc = "[逃逸](../x.md) 与 [无后缀](notes/x)";
    const links = linkWidgets(buildWiki(doc, { onCreateNote: async () => null }), doc.length).map(linkInfo);
    expect(links.filter((l) => l.url === "../x.md")).toHaveLength(0);
    const noExt = links.filter((l) => l.url === "notes/x");
    expect(noExt).toHaveLength(1);
    expect(noExt[0].kind).toBe("create");
  });

  it("锚点与非 .md 后缀的目标保持原文，不产出 create widget", () => {
    const doc = "[目录](#section) 与 [手册](docs/manual.pdf)";
    const links = linkWidgets(buildWiki(doc, { onCreateNote: async () => null }), doc.length).map(linkInfo);
    expect(links).toHaveLength(0);
  });

  it("`[[文字]]` 片段不按引用链接处理（交 wiki 分支）", () => {
    const doc = "引用 [[文字]] 结尾";
    const links = linkWidgets(
      buildWiki(doc, { resolveWikiNote: () => false, onCreateNote: async () => null }),
      doc.length,
    ).map(linkInfo);
    expect(links).toHaveLength(1);
    expect(links[0].kind).toBe("create");
  });

  it("单层 [文字]（引用形态）不产出任何链接 widget", () => {
    const doc = "引用 [文字] 结尾";
    const links = linkWidgets(
      buildWiki(doc, { resolveWikiNote: () => false, onCreateNote: async () => null }),
      doc.length,
    ).map(linkInfo);
    expect(links).toHaveLength(0);
  });

  it("单层 [文字] 的括号加内联样式中和次要色（覆盖 LinkMark 高亮）", () => {
    const doc = "引用 [文字] 结尾";
    const decs = build(doc);
    const neutralized: string[] = [];
    decs.between(0, doc.length, (from, to, dec) => {
      const style = (dec as unknown as { spec?: { attributes?: { style?: string } } }).spec?.attributes?.style;
      if (style?.includes("var(--text-primary)")) neutralized.push(`${from}-${to}`);
    });
    // `[` 与 `]` 两个 LinkMark 区间都被中和（Link 节点 [3,7] 的两端）
    expect(neutralized).toEqual(["3-4", "6-7"]);
  });

  it("无后缀路径链接按「路径 + .md」解析命中 → 打开 widget", () => {
    const doc = "见 [链接](链接) 一节";
    const opened: string[] = [];
    const links = linkWidgets(
      buildWiki(doc, {
        isVaultPathNote: (href) => href === "链接.md",
        onOpenVaultPathNote: (href) => opened.push(href),
      }),
      doc.length,
    ).map(linkInfo);
    expect(links).toHaveLength(1);
    expect(links[0]).toMatchObject({ text: "链接", url: "链接.md", kind: "path" });
    links[0].click(noViewCtx);
    expect(opened).toEqual(["链接.md"]);
  });

  it("无后缀路径链接未命中 → 点击创建 路径.md 并回填补全后缀", async () => {
    const doc = "见 [文字](后面有文字的话) 一节\n";
    const created: string[] = [];
    const view = mountView(doc, doc.length);
    const decs = buildDecorations(
      view.state,
      {
        ...opts,
        readOnly: false,
        onCreateNote: (n) => {
          created.push(n);
          return Promise.resolve(n);
        },
        onOpenCreatedNote: () => {},
      },
      () => {},
    );
    const w = linkWidgets(decs, doc.length).map(linkInfo)[0];
    expect(w).toMatchObject({ text: "文字", kind: "create" });
    w.click({ view, el: document.createElement("span") });
    await new Promise((r) => setTimeout(r, 0));
    expect(created).toEqual(["后面有文字的话.md"]);
    expect(view.state.doc.toString()).toBe("见 [文字](后面有文字的话.md) 一节\n");
    view.destroy();
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

  it("输入法组合期（input.type.compose）→ 重建（文档在组合期持续变化，跳过会让装饰错位于新文档）", () => {
    const tr = stateWithLanguage(langA).update({
      changes: { from: 0, insert: "中" },
      userEvent: "input.type.compose",
    });
    expect(tr.docChanged).toBe(true);
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
