// @vitest-environment jsdom
/**
 * Markdown 内核渲染单测（需 DOM：raw HTML 清洗走 DOMPurify）。
 * 覆盖类名契约、文本转义、raw HTML 清洗（script/on* 剥除）。
 */
import { describe, expect, it } from "vitest";
import { renderMarkdownEditHtml, renderMarkdownToHtml } from "./render";

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

  it("行内 HTML 注释隐藏（注释内容不渲染，首尾文本保留）", () => {
    const out = renderMarkdownToHtml("前文 <!-- 注释内容 --> 后文");
    expect(out).not.toContain("注释内容");
    expect(out).toContain("前文");
    expect(out).toContain("后文");
  });

  it("列表项内的行内 HTML 注释隐藏", () => {
    const out = renderMarkdownToHtml("- 注释：<!-- 这是注释，不会显示 -->\n");
    expect(out).not.toContain("这是注释");
    expect(out).toContain("注释：");
  });

  it("链接与图片的源码 title 透传到 title 属性", () => {
    const out = renderMarkdownToHtml('[示例](https://a.com "站点标题")\n\n![替代](https://b.com/i.png "图片标题")');
    expect(out).toContain('title="站点标题"');
    expect(out).toContain('title="图片标题"');
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

describe("renderMarkdownToHtml 空行行元素", () => {
  it("块间每个源码空行渲染为一个真实行元素（所见即所得，间距 = 空行数 × 行高）", () => {
    const out = renderMarkdownToHtml("甲\n\n\n\n乙");
    expect(out).toBe('<p>甲</p>' + '<div class="md-editor-gap"></div>'.repeat(3) + '<p>乙</p>');
  });

  it("单个空行同样是一个行元素", () => {
    const out = renderMarkdownToHtml("甲\n\n乙");
    expect(out).toBe('<p>甲</p><div class="md-editor-gap"></div><p>乙</p>');
  });

  it("相邻块之间无空行时不插入行元素", () => {
    const out = renderMarkdownToHtml("````js\nx\n````\n甲");
    expect(out).not.toContain("md-editor-gap");
  });

  it("偏移模式每个空行带自己的点区间与零宽锚（光标可驻留、点击直接命中该空行）", () => {
    const out = renderMarkdownToHtml("甲\n\n\n乙", { offsets: true });
    expect(out).toBe(
      '<p data-md-block data-md-kind="paragraph" data-md-from="0" data-md-to="1">' +
        '<span data-md-from="0" data-md-to="1">甲</span></p>' +
        '<div class="md-editor-gap" data-md-from="2" data-md-to="2">\u200B</div>' +
        '<div class="md-editor-gap" data-md-from="3" data-md-to="3">\u200B</div>' +
        '<p data-md-block data-md-kind="paragraph" data-md-from="4" data-md-to="5">' +
        '<span data-md-from="4" data-md-to="5">乙</span></p>',
    );
  });

  it("CRLF 行尾残余不影响空行计数", () => {
    const out = renderMarkdownToHtml("甲\r\n\r\n\r\n乙");
    expect(out.match(/md-editor-gap/g) ?? []).toHaveLength(2);
  });

  it("引用内的段落间空行同样渲染为行元素", () => {
    const quote = renderMarkdownToHtml("> 甲\n>\n> 乙");
    expect(quote.match(/md-editor-gap/g) ?? []).toHaveLength(1);
  });

  it("松散列表项的续段空行不渲染行元素（解析层把续段并入单一段落，项内无块间隙）", () => {
    const loose = renderMarkdownToHtml("- 甲\n\n  乙");
    expect(loose.match(/md-editor-gap/g) ?? []).toHaveLength(0);
  });

  it("文档首部的空行同样渲染为行元素（文首 Enter 开出的新行可见）", () => {
    const out = renderMarkdownToHtml("\n\n甲");
    expect(out).toBe('<div class="md-editor-gap"></div><p>甲</p>');
  });

  it("文档尾部的空行同样渲染为行元素（文末 Enter 开出的新行可见，光标有落点）", () => {
    const out = renderMarkdownToHtml("甲\n\n");
    expect(out).toBe('<p>甲</p><div class="md-editor-gap"></div>');
  });

  it("文末单个换行（正常文件结尾）不产生多余空行", () => {
    const out = renderMarkdownToHtml("甲\n");
    expect(out).toBe("<p>甲</p>");
  });

  it("首尾空行在偏移模式带点区间与零宽锚", () => {
    const head = renderMarkdownToHtml("\n\n甲", { offsets: true });
    expect(head).toContain('<div class="md-editor-gap" data-md-from="1" data-md-to="1">\u200B</div>');
    const tail = renderMarkdownToHtml("甲\n\n", { offsets: true });
    expect(tail).toContain('<div class="md-editor-gap" data-md-from="2" data-md-to="2">\u200B</div>');
  });
});

describe("renderMarkdownToHtml 列表项级编辑", () => {
  it("列表项间空行同样渲染为行元素（与块间同语义）", () => {
    const out = renderMarkdownToHtml("- 甲\n\n\n- 乙\n");
    expect(out.match(/md-editor-gap/g) ?? []).toHaveLength(2);
  });

  it("空项（只有标记）编辑态恒渲染态：自绘 marker 在、内容位放零宽锚、不回显源码", () => {
    const src = "- 甲\n- ";
    const out = renderMarkdownEditHtml(src, { offsets: true, activeOffset: src.length });
    expect(out).toContain('<span class="md-editor-list-marker">•</span>');
    expect(out).not.toContain("md-editor-source");
    // 光标驻留点 = 空项内容位（输入即落于此）
    expect(out).toMatch(/<span data-md-from="\d+" data-md-to="\d+">\u200B<\/span>/);
  });

  it("编辑态列表逐项活动：所在项回显源码、其余项保持渲染", () => {
    const src = "- 甲\n- 乙\n- 丙";
    const out = renderMarkdownEditHtml(src, { offsets: true, activeOffset: 5 });
    expect(out).toMatch(
      /<li data-md-block data-md-kind="list" data-md-from="\d+" data-md-to="\d+"><div class="md-editor-source" data-md-block data-md-kind="list" data-md-from="\d+" data-md-to="\d+"><span data-md-from="\d+" data-md-to="\d+">- 乙<\/span><\/div><\/li>/,
    );
    expect(out).not.toContain("- 甲</span></div></li>");
    expect(out).not.toContain("- 丙</span></div></li>");
  });

  it("编辑态渲染项也带项级块标记（活动判定与偏移映射细到项）", () => {
    const out = renderMarkdownEditHtml("- 甲\n- 乙", { offsets: true, activeOffset: 5 });
    expect(out).toMatch(/<li data-md-block data-md-kind="list" data-md-from="\d+" data-md-to="\d+"><span class="md-editor-list-marker">/);
  });

  it("代码块随活动判定：光标不在时保持渲染，光标进入才回显源码", () => {
    const src = "```js\nconst x = 1;\n```";
    const idle = renderMarkdownEditHtml(src, { offsets: true });
    expect(idle).toContain("md-editor-code-head");
    const active = renderMarkdownEditHtml(src, { offsets: true, activeOffset: 8 });
    expect(active).toContain('<div class="md-editor-source"');
    expect(active).toContain("```js");
  });

  it("嵌套列表下探到最小项：光标在子项内时仅子项回显源码，父项保持渲染", () => {
    const src = "- 甲\n  - 乙\n- 丙";
    const out = renderMarkdownEditHtml(src, { offsets: true, activeOffset: src.indexOf("乙") });
    expect(out).toMatch(
      /<li data-md-block data-md-kind="list" data-md-from="\d+" data-md-to="\d+"><div class="md-editor-source" data-md-block data-md-kind="list" data-md-from="\d+" data-md-to="\d+"><span data-md-from="\d+" data-md-to="\d+">- 乙<\/span><\/div><\/li>/,
    );
    expect(out).not.toContain("- 甲</span></div></li>");
    expect(out).not.toContain("- 丙</span></div></li>");
  });

  it("引用内的列表同样下探：光标在引用内列表项时引用整块源码、外层项保持渲染", () => {
    const src = "- 甲\n  > 引文\n  > - 内项";
    const out = renderMarkdownEditHtml(src, { offsets: true, activeOffset: src.indexOf("内项") });
    // 外层项保持渲染（marker 在），引用整块回显源码（无渲染形态包装，与顶层活动块一致）
    expect(out).toContain('<span class="md-editor-list-marker">•</span>');
    expect(out).not.toContain('<blockquote class="md-quote"');
    expect(out).toMatch(/<div class="md-editor-source" data-md-block data-md-kind="blockquote" data-md-from="\d+" data-md-to="\d+">/);
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

  it("列表项内嵌标题：marker 与标题块为相邻兄弟，标题文本不重复", () => {
    const out = renderMarkdownToHtml("- ### 卡片笔记法 概述\n");
    expect(out).toBe(
      '<ul class="md-editor-list"><li><span class="md-editor-list-marker">•</span><h3>卡片笔记法 概述</h3></li></ul>',
    );
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