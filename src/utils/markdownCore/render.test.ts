// @vitest-environment jsdom
/**
 * Markdown 内核渲染单测（需 DOM：raw HTML 清洗走 DOMPurify）。
 * 覆盖类名契约、文本转义、raw HTML 清洗（script/on* 剥除）。
 */
import { describe, expect, it } from "vitest";
import { renderMarkdownToHtml } from "./render";

const options = {
  mentions: [{ key: "u1", label: "Alice" }],
  resolveLink: (href: string): "path" | "plain" => (href === "notes/a.md" ? "path" : "plain"),
};

const doc = [
  "# Heading",
  "",
  "para **bold** [ext](https://x.com) [path](notes/a.md) [new]() [[wiki|W]] #tag ==hl== %%c%% $x$ [^fn] @Alice ![alt](p.png)",
  "",
  "> [!note] quoted",
  "",
  "> plain quote",
  "",
  "- item",
  "- [x] done",
  "",
  "1. one",
  "",
  "```js",
  "const x = 1;",
  "```",
  "",
  "---",
  "",
  "[^fn]: footnote text",
  "",
  "| h | v |",
  "| :- | -: |",
  "| 1 | 2 |",
  "",
  "<div>html</div>",
].join("\n");

const html = renderMarkdownToHtml(doc, options);

describe("renderMarkdownToHtml 类名契约", () => {
  it("标题 / 强调 / 高亮 / 标签", () => {
    expect(html).toContain("<h1>Heading</h1>");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain('<mark class="md-highlight">hl</mark>');
    expect(html).toContain('<span class="md-editor-tag">#tag</span>');
  });

  it("链接四种形态的类名", () => {
    expect(html).toContain('<span class="md-editor-link" data-md-href="https://x.com"');
    expect(html).toContain('<span class="md-editor-internal-link" data-md-href="notes/a.md"');
    expect(html).toContain('class="md-editor-internal-link md-editor-internal-link-missing" data-md-href=""');
    expect(html).toContain('data-md-wiki="wiki"');
  });

  it("图片 / 任务框 / 列表标记", () => {
    expect(html).toContain('class="md-editor-image" data-md-src="p.png"');
    expect(html).toContain('<input type="checkbox" class="md-editor-checkbox" checked disabled>');
    expect(html).toContain('<span class="md-editor-list-marker">•</span>');
    expect(html).toContain('<span class="md-editor-list-marker">1.</span>');
  });

  it("数学 / 围栏代码 / 横隔条", () => {
    expect(html).toContain('<span class="md-editor-math">');
    expect(html).toContain('class="md-editor-code-block"');
    expect(html).toContain('class="md-editor-code-head"');
    expect(html).toContain('class="md-editor-code-lang">js</span>');
    expect(html).toContain('class="md-editor-code-copy"');
    expect(html).toContain('<code>const x = 1;</code>');
    expect(html).toContain('<div class="md-editor-divider"></div>');
  });

  it("引用 / callout / 表格 / 脚注 / mention / html", () => {
    expect(html).toContain('<blockquote class="md-quote">');
    expect(html).toContain('<blockquote class="md-callout callout-note">');
    expect(html).toContain('<span class="md-editor-callout-badge">note</span>');
    expect(html).toContain('class="md-editor-table-wrap"');
    expect(html).toContain('class="md-editor-table"');
    expect(html).toContain('<sup class="md-editor-footnote-ref">fn</sup>');
    expect(html).toContain('class="md-editor-footnote-def"');
    expect(html).toContain('<span class="mention-capsule" data-md-mention-key="u1">@Alice</span>');
    expect(html).toContain('<div class="md-editor-html">');
  });

  it("块级数学", () => {
    const out = renderMarkdownToHtml("$$\ny^2\n$$");
    expect(out).toContain("md-editor-math md-editor-math-block");
  });

  it("关闭 KaTeX 时回显源码并加错误类", () => {
    const out = renderMarkdownToHtml("$x$", { katex: false });
    expect(out).toContain("md-editor-math-error");
    expect(out).toContain("$x$");
  });
});

describe("renderMarkdownToHtml 转义与清洗", () => {
  it("普通文本转义（不注入 HTML）", () => {
    const out = renderMarkdownToHtml("a < b & c");
    expect(out).toContain("a &lt; b &amp; c");
  });

  it("表格单元格转义", () => {
    const out = renderMarkdownToHtml("| a |\n| - |\n| <x> |");
    expect(out).toContain("&lt;x&gt;");
    expect(out).not.toContain("<x>");
  });

  it("行内 raw HTML 经白名单清洗（on* 事件剥除、白名单标签保留）", () => {
    const out = renderMarkdownToHtml('text <b onclick="x()">b</b> end');
    expect(out).toContain("<b>b</b>");
    expect(out).not.toContain("onclick");
  });

  it("HTML 块内 <script> 被剥离", () => {
    const out = renderMarkdownToHtml("<script>alert(1)</script>");
    expect(out).not.toContain("<script");
    expect(out).not.toContain("alert(1)");
  });
});

describe("renderMarkdownToHtml 编辑面偏移模式", () => {
  const out = renderMarkdownToHtml("para **bold**", { offsets: true });

  it("块容器带块标记与源区间", () => {
    expect(out).toMatch(/<p data-md-block data-md-kind="paragraph" data-md-from="0" data-md-to="\d+">/);
  });

  it("行内元素带源区间，纯文本被包裹以便逐字符定位", () => {
    expect(out).toMatch(/<strong data-md-from="5" data-md-to="13">/);
    expect(out).toMatch(/<span data-md-from="\d+" data-md-to="\d+">bold<\/span>/);
  });

  it("围栏代码正文带内容区间（供编辑面把偏移映射进 <code>）", () => {
    const code = renderMarkdownToHtml("```js\nconst x = 1;\n```", { offsets: true });
    expect(code).toMatch(/<code data-md-from="\d+" data-md-to="\d+">/);
  });

  it("关闭 offsets 时不产出定位属性（插件侧输出不变）", () => {
    const plain = renderMarkdownToHtml("para **bold**");
    expect(plain).not.toContain("data-md-block");
    expect(plain).not.toContain("data-md-from");
  });
});

describe("renderMarkdownToHtml 列表与边界回归", () => {
  it("列表容器带 md-editor-list 类（样式层据此关闭原生 marker 防双重显示）", () => {
    const out = renderMarkdownToHtml("- item\n- [x] done\n\n1. one\n");
    expect(out).toContain('<ul class="md-editor-list">');
    expect(out).toContain('<ol class="md-editor-list">');
  });

  it("源文本 marker 不进入 DOM（圆点/序号只由自绘 span 承载）", () => {
    const out = renderMarkdownToHtml("- item\n\n2. two\n");
    expect(out).not.toContain(">- item");
    expect(out).not.toContain(">2. two");
    expect(out).toContain('<span class="md-editor-list-marker">•</span>');
    expect(out).toContain('<span class="md-editor-list-marker">2.</span>');
  });

  it("嵌套列表渲染为内层列表结构，内层 marker 不泄漏为正文", () => {
    const out = renderMarkdownToHtml("- outer\n  - inner\n    - deep\n");
    expect((out.match(/<ul class="md-editor-list">/g) ?? []).length).toBe(3);
    expect(out).toContain("inner");
    expect(out).not.toContain("- inner");
    expect(out).not.toContain("- deep");
  });

  it("数学区间外的段落尾随文本保留", () => {
    const out = renderMarkdownToHtml("$$x\n$$ tail text\n");
    expect(out).toContain("tail text");
  });

  it("表格 \\| 转义渲染为字面竖线", () => {
    const out = renderMarkdownToHtml("| x \\| y | z |\n| --- | --- |\n");
    expect(out).toContain("<th>x | y</th>");
    expect(out).toContain("<th>z</th>");
  });
});