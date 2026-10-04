/**
 * Markdown 内核解析单测（纯函数，node 环境即可）。
 * 覆盖 GFM 块切分、行内 span、私有预扫（wiki/标签/高亮/注释/数学/脚注/mention）与跳过区。
 */
import { describe, expect, it } from "vitest";
import { parseMarkdown } from "./parse";
import type { InlineSpan, MarkdownBlock } from "@/types/markdown";

function inlineSpans(blocks: MarkdownBlock[]): InlineSpan[] {
  const out: InlineSpan[] = [];
  const visitInline = (spans: InlineSpan[]) => {
    for (const s of spans) {
      out.push(s);
      if ("children" in s) visitInline(s.children);
    }
  };
  const visit = (bs: MarkdownBlock[]) => {
    for (const b of bs) {
      if ("inline" in b) visitInline(b.inline);
      if (b.kind === "blockquote") visit(b.children);
    }
  };
  visit(blocks);
  return out;
}

const blockDoc = [
  "# Title",
  "",
  "para **b** here",
  "",
  "> [!note] quoted",
  "> more",
  "",
  "- [x] done",
  "- plain",
  "",
  "1. one",
  "2. two",
  "",
  "```js",
  "const x = 1;",
  "```",
  "",
  "---",
  "",
  "<div>hi</div>",
  "",
  "$$",
  "x^2",
  "$$",
  "",
  "[^a]: footnote text",
  "",
  "| h1 | h2 |",
  "| --- | :--: |",
  "| a | b |",
].join("\n");

describe("parseMarkdown 块级切分", () => {
  const doc = parseMarkdown(blockDoc);
  const kinds = doc.blocks.map((b) => b.kind);

  it("顶层块类型序列正确", () => {
    expect(kinds).toEqual([
      "heading",
      "paragraph",
      "blockquote",
      "list",
      "list",
      "fencedCode",
      "hr",
      "htmlBlock",
      "mathBlock",
      "footnoteDef",
      "table",
    ]);
  });

  it("ATX 标题级别与内容", () => {
    const h = doc.blocks[0];
    expect(h.kind).toBe("heading");
    if (h.kind !== "heading") return;
    expect(h.level).toBe(1);
    expect(inlineSpans([h]).map((s) => (s.kind === "text" ? s.text : s.kind)).join("")).toBe("Title");
  });

  it("callout 提取类型并剥除 [!note] 标记", () => {
    const bq = doc.blocks[2];
    if (bq.kind !== "blockquote") throw new Error("expect blockquote");
    expect(bq.callout).toBe("note");
    const text = inlineSpans(bq.children)
      .map((s) => (s.kind === "text" ? s.text : ""))
      .join("");
    expect(text).not.toContain("[!note]");
    expect(text).toContain("quoted");
  });

  it("无序任务列表 / 有序列表", () => {
    const ul = doc.blocks[3];
    if (ul.kind !== "list") throw new Error("expect list");
    expect(ul.ordered).toBe(false);
    expect(ul.items).toHaveLength(2);
    expect(ul.items[0]).toMatchObject({ task: true, checked: true });
    expect(ul.items[1]).toMatchObject({ task: false, checked: false });

    const ol = doc.blocks[4];
    if (ol.kind !== "list") throw new Error("expect list");
    expect(ol.ordered).toBe(true);
    expect(ol.items).toHaveLength(2);
  });

  it("围栏代码语言与内容", () => {
    const code = doc.blocks[5];
    if (code.kind !== "fencedCode") throw new Error("expect fencedCode");
    expect(code.lang).toBe("js");
    expect(code.code).toBe("const x = 1;");
    expect(code.contentTo).toBeGreaterThan(code.contentFrom);
  });

  it("块数学 tex", () => {
    const math = doc.blocks[8];
    if (math.kind !== "mathBlock") throw new Error("expect mathBlock");
    expect(math.tex).toBe("x^2");
  });

  it("脚注定义 label + inline", () => {
    const fn = doc.blocks[9];
    if (fn.kind !== "footnoteDef") throw new Error("expect footnoteDef");
    expect(fn.label).toBe("a");
    expect(inlineSpans([fn]).some((s) => s.kind === "text" && s.text.includes("footnote text"))).toBe(true);
  });

  it("表格表头 / 对齐 / 数据行", () => {
    const table = doc.blocks[10];
    if (table.kind !== "table") throw new Error("expect table");
    const text = (cells: Array<{ from: number; to: number }>) => cells.map((c) => doc.source.slice(c.from, c.to));
    expect(text(table.header)).toEqual(["h1", "h2"]);
    expect(table.aligns).toEqual(["", "center"]);
    expect(table.rows.map(text)).toEqual([["a", "b"]]);
  });

  it("Setext 标题", () => {
    const d = parseMarkdown("Title\n=====\n\nsub\n---\n");
    expect(d.blocks[0]).toMatchObject({ kind: "heading", level: 1 });
    expect(d.blocks[1]).toMatchObject({ kind: "heading", level: 2 });
  });

  it("HTML 注释块按 htmlBlock 处理", () => {
    const d = parseMarkdown("<!-- c -->\n");
    expect(d.blocks[0]).toMatchObject({ kind: "htmlBlock" });
  });

  it("单行脚注定义（LinkReference 形态）也识别为 footnoteDef", () => {
    const d = parseMarkdown("[^b]: singleword\n");
    expect(d.blocks[0]).toMatchObject({ kind: "footnoteDef", label: "b" });
  });
});

describe("parseMarkdown 行内 span", () => {
  const src =
    "**bold** _em_ ~~del~~ `code` [ext](https://x.com) [path](notes/a.md) [new]() [plain](ftp://x) " +
    "[[wiki|W]] #tag ==hl== %%note%% $e=mc^2$ [^fn] @Alice ![img|100x50](p.png)";
  const options = {
    mentions: [{ key: "u1", label: "Alice" }],
    resolveLink: (href: string): "path" | "plain" => (href === "notes/a.md" ? "path" : "plain"),
  };
  const spans = inlineSpans(parseMarkdown(src, options).blocks);
  const find = <T extends InlineSpan["kind"]>(kind: T) => spans.find((s) => s.kind === kind);

  it("强调 / 删除线 / 行内代码", () => {
    const strong = find("strong");
    expect(strong && strong.kind === "strong" && strong.children.some((c) => c.kind === "text" && c.text === "bold")).toBe(true);
    expect(find("em")).toBeTruthy();
    expect(find("strike")).toBeTruthy();
    expect(find("code")).toMatchObject({ kind: "code", text: "code" });
  });

  it("链接四种形态", () => {
    const links = spans.filter((s) => s.kind === "link");
    const byForm = (form: string) => links.find((l) => l.kind === "link" && l.form === form);
    expect(byForm("external")).toMatchObject({ href: "https://x.com" });
    expect(byForm("path")).toMatchObject({ href: "notes/a.md" });
    expect(byForm("create")).toMatchObject({ href: "" });
    expect(byForm("plain")).toMatchObject({ href: "ftp://x" });
  });

  it("wiki / 标签 / 高亮 / 注释", () => {
    expect(find("wiki")).toMatchObject({ target: "wiki", label: "W" });
    expect(find("tag")).toMatchObject({ tag: "tag" });
    const hl = find("highlight");
    expect(hl && "children" in hl && hl.children.some((c) => c.kind === "text" && c.text === "hl")).toBe(true);
    expect(find("comment")).toBeTruthy();
  });

  it("行内数学 / 脚注引用 / mention / 图片尺寸", () => {
    expect(find("mathInline")).toMatchObject({ tex: "e=mc^2" });
    expect(find("footnoteRef")).toMatchObject({ label: "fn" });
    expect(find("mention")).toMatchObject({ key: "u1", label: "Alice", char: "@" });
    expect(find("image")).toMatchObject({ src: "p.png", alt: "img", width: "100", height: "50" });
  });

  it("# 触发的已知 mention 优先于标签语法，未知候选仍为标签", () => {
    const spans2 = inlineSpans(parseMarkdown("#Alice #unknown", options).blocks);
    expect(spans2.find((s) => s.kind === "mention")).toMatchObject({
      key: "u1",
      label: "Alice",
      char: "#",
    });
    expect(spans2.find((s) => s.kind === "tag")).toMatchObject({ tag: "unknown" });
  });

  it("自动链接 <https://x> 剥角括号", () => {
    const d = parseMarkdown("see <https://x.example> now");
    const s = inlineSpans(d.blocks).find((x) => x.kind === "autolink");
    expect(s).toMatchObject({ href: "https://x.example", label: "https://x.example" });
  });

  it("裸 URL 自动链接", () => {
    const d = parseMarkdown("see https://bare.example now");
    const s = inlineSpans(d.blocks).find((x) => x.kind === "autolink");
    expect(s).toMatchObject({ href: "https://bare.example" });
  });
});

describe("parseMarkdown 跳过区", () => {
  it("围栏代码与行内代码内的 #标签 / $$ 不被识别", () => {
    const src = ["```", "#tag $$", "```", "", "inline `#tag` here", "", "real #tag here"].join("\n");
    const doc = parseMarkdown(src);
    const tags = inlineSpans(doc.blocks).filter((s) => s.kind === "tag");
    expect(tags).toHaveLength(1);
    expect(tags[0]).toMatchObject({ tag: "tag" });
    const code = doc.blocks[0];
    expect(code.kind === "fencedCode" && code.code).toContain("#tag $$");
  });

  it("链接标签内的标签不被识别", () => {
    const doc = parseMarkdown("[see #tag](https://x.com)");
    expect(inlineSpans(doc.blocks).some((s) => s.kind === "tag")).toBe(false);
  });

  it("围栏代码内的 $$ 不被识别为块级数学", () => {
    const doc = parseMarkdown(["```", "$$", "x", "$$", "```"].join("\n"));
    expect(doc.blocks.some((b) => b.kind === "mathBlock")).toBe(false);
    expect(doc.blocks[0]).toMatchObject({ kind: "fencedCode" });
  });
});

describe("parseMarkdown 嵌套与边界回归", () => {
  const textOf = (doc: ReturnType<typeof parseMarkdown>): string =>
    inlineSpans(doc.blocks)
      .map((s) => (s.kind === "text" ? s.text : ""))
      .join("");

  it("嵌套列表递归成子块，内层项不混入外层行内文本", () => {
    const doc = parseMarkdown("- outer\n  - inner\n    - deep\n");
    const outer = doc.blocks[0];
    if (outer.kind !== "list") throw new Error("expect list");
    expect(outer.items).toHaveLength(1);
    const inner = outer.items[0]!.children[0]!;
    expect(inner).toMatchObject({ kind: "list", ordered: false });
    if (inner.kind !== "list") return;
    const deep = inner.items[0]!.children[0]!;
    expect(deep).toMatchObject({ kind: "list" });
    expect(textOf(doc)).not.toContain("- inner");
  });

  it("数学区间与段落部分交叠：区间外尾随文本保留为段落", () => {
    const doc = parseMarkdown("$$x\n$$ tail text\n");
    expect(doc.blocks.map((b) => b.kind)).toEqual(["mathBlock", "paragraph"]);
    expect(textOf(doc)).toContain("tail text");
  });

  it("与代码围栏交叠的数学区间整段作废，后文段落完整保留", () => {
    const doc = parseMarkdown("```text\n$$\n```\nhello $$ world\n");
    expect(doc.blocks.map((b) => b.kind)).toEqual(["fencedCode", "paragraph"]);
    expect(textOf(doc)).toContain("hello $$ world");
  });

  it("空数学体降级为原文段落，源文本不从渲染面消失", () => {
    const doc = parseMarkdown("$$\n$$\n");
    expect(doc.blocks.map((b) => b.kind)).toEqual(["paragraph"]);
    expect(textOf(doc)).toContain("$$");
  });

  it("CRLF 源文本的脚注定义仍识别，定义行后的正文保持为段落", () => {
    const doc = parseMarkdown("[^a]: note text\r\nbody\r\n");
    expect(doc.blocks.map((b) => b.kind)).toEqual(["footnoteDef", "paragraph"]);
    expect(doc.blocks[0]).toMatchObject({ kind: "footnoteDef", label: "a" });
    expect(textOf(doc)).toContain("note text");
    expect(textOf(doc)).toContain("body");
  });

  it("表格单元格内的 \\| 转义不被切列", () => {
    const doc = parseMarkdown("| x \\| y | z |\n| --- | --- |\n| a | b |\n");
    const table = doc.blocks[0];
    if (table.kind !== "table") throw new Error("expect table");
    expect(table.header.map((c) => doc.source.slice(c.from, c.to))).toEqual(["x \\| y", "z"]);
    expect(table.rows.map((r) => r.map((c) => doc.source.slice(c.from, c.to)))).toEqual([["a", "b"]]);
  });

  it("非相邻 HTMLTag 不越界合并，标签间文本与 #标签 语法保留", () => {
    const doc = parseMarkdown("<b>x</b> plain #tag <i>y</i>\n");
    const spans = inlineSpans(doc.blocks);
    expect(spans.some((s) => s.kind === "text" && s.text.includes(" plain "))).toBe(true);
    expect(spans.some((s) => s.kind === "tag")).toBe(true);
  });

  it("单段落内多个数学区间依次消费，区间残余文本保留", () => {
    const doc = parseMarkdown("$$a$$\ntext\n$$b$$\n");
    expect(doc.blocks.map((b) => b.kind)).toEqual(["mathBlock", "paragraph", "mathBlock"]);
    expect(textOf(doc)).toContain("text");
  });

  it("跨段数学区间只跳过覆盖部分，区间外的残余文本不丢", () => {
    // 区间 [0,7) 由首段消费，第二段仅开头的 `$$` 落在区间内，尾部 y 必须保留为段落
    const doc = parseMarkdown("$$x\n\n$$y\n");
    expect(doc.blocks.map((b) => b.kind)).toEqual(["mathBlock", "paragraph"]);
    expect(textOf(doc)).toContain("y");

    // 完全落在区间内的段落仍被跳过（其内容属于数学块，不得重复渲染）
    const nested = parseMarkdown("$$x\n\nmiddle\n\n$$y\n");
    expect(nested.blocks.map((b) => b.kind)).toEqual(["mathBlock", "paragraph"]);
    expect(textOf(nested)).not.toContain("middle");
    expect(textOf(nested)).toContain("y");
  });

  it("表格行以 \\| 结尾且无闭合定界管道时转义仍还原为字面竖线", () => {
    const doc = parseMarkdown("| a \\|\n| --- |\n");
    const table = doc.blocks[0];
    if (table.kind !== "table") throw new Error("expect table");
    expect(table.header.map((c) => doc.source.slice(c.from, c.to))).toEqual(["a \\|"]);
  });

  it("配对的开闭 HTMLTag 合并为单个不透明区", () => {
    const doc = parseMarkdown("text <b>x</b> end\n");
    const htmlSpans = inlineSpans(doc.blocks).filter((s) => s.kind === "html");
    expect(htmlSpans).toHaveLength(1);
    expect(htmlSpans[0] && htmlSpans[0].kind === "html" && htmlSpans[0].html).toBe("<b>x</b>");
  });
});