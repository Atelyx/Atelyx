// @vitest-environment jsdom
/**
 * Markdown 内核渲染单测（需 DOM：raw HTML 清洗走 DOMPurify）。
 * 覆盖类名契约、文本转义、raw HTML 清洗（script/on* 剥除）。
 */
import { describe, expect, it } from "vitest";
import { renderMarkdownEditChunks, renderMarkdownEditHtml, renderMarkdownToHtml } from "./render";

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
    expect(html).toContain(BULLET);
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

/** 无序项的小圆点由样式画出（不写字形），产物是空 span + 类名。 */
const BULLET = '<span class="md-editor-list-marker md-editor-list-bullet"></span>';

describe("renderMarkdownToHtml 编辑面偏移模式", () => {
  const out = renderMarkdownToHtml("para **bold**", { offsets: true });

  it("块容器带块标记与源区间", () => {
    expect(out).toMatch(/<p data-md-block data-md-kind="paragraph" data-md-from="0" data-md-to="\d+" data-md-own>/);
  });

  it("行内元素带源区间，标记字符与纯文本都进 DOM（逐字符可定位）", () => {
    expect(out).toMatch(/<strong data-md-from="5" data-md-to="13" data-md-own>/);
    expect(out).toContain('<span class="md-marker" data-md-from="5" data-md-to="7">**</span>');
    expect(out).toContain('<span data-md-from="7" data-md-to="11">bold</span>');
    expect(out).toContain('<span class="md-marker" data-md-from="11" data-md-to="13">**</span>');
  });

  it("围栏代码正文带内容区间（供编辑面把偏移映射进 <code>）", () => {
    const code = renderMarkdownToHtml("```js\nconst x = 1;\n```", { offsets: true });
    expect(code).toMatch(/<code><span data-md-from="\d+" data-md-to="\d+">const x = 1;<\/span><\/code>/);
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
      '<p data-md-block data-md-kind="paragraph" data-md-from="0" data-md-to="1" data-md-own>' +
        '<span data-md-from="0" data-md-to="1">甲</span></p>' +
        '<div class="md-editor-gap" data-md-from="2" data-md-to="2">\u200B</div>' +
        '<div class="md-editor-gap" data-md-from="3" data-md-to="3">\u200B</div>' +
        '<p data-md-block data-md-kind="paragraph" data-md-from="4" data-md-to="5" data-md-own>' +
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

  it("松散列表项的续段空行同样渲染行元素（项内空行与项间同一口径）", () => {
    const loose = renderMarkdownToHtml("- 甲\n\n  乙");
    expect(loose.match(/md-editor-gap/g) ?? []).toHaveLength(1);
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
    expect(out).toContain(BULLET);
    expect(out).not.toContain("md-editor-source");
    // 光标驻留点 = 空项内容位（输入即落于此）
    expect(out).toMatch(/<span data-md-from="\d+" data-md-to="\d+">\u200B<\/span>/);
  });

  it("列表标记常驻 DOM 且默认隐藏，产物不随光标变化（揭示由视图层切 class）", () => {
    const src = "- 甲\n- 乙\n- 丙";
    const out = renderMarkdownEditHtml(src, { offsets: true, activeOffset: 5 });
    // 每项都是「源标记（隐藏）+ 自绘圆点 + 内容」同一结构，揭示只切 class
    expect(out.match(/class="md-marker"/g) ?? []).toHaveLength(3);
    expect(out.match(/md-editor-list-bullet/g) ?? []).toHaveLength(3);
    expect(out).toBe(renderMarkdownEditHtml(src, { offsets: true }));
    expect(out).not.toContain("md-editor-source");
  });

  it("标记后无空格的空项：标记归标记层，渲染态只剩圆点（不把 `-` 当正文）", () => {
    // 空项带一个流内零宽占位：圆点浮动不产生行盒，无占位时嵌套子列表会与圆点挤在同一行
    expect(renderMarkdownToHtml("-")).toBe(`<ul class="md-editor-list"><li>${BULLET}\u200B</li></ul>`);
    const out = renderMarkdownEditHtml("-", { offsets: true });
    expect(out).toContain('<span class="md-marker" data-md-from="0" data-md-to="1">-</span>');
    // 空项放零宽锚承载光标：揭示态显示源标记 `-`，与有空格时一致
    expect(out).toContain('<span data-md-from="1" data-md-to="1">\u200B</span>');
  });

  it("围栏末尾空行由独立块承载：可见一行，且光标能落在其上", () => {
    expect(renderMarkdownToHtml("```\nx\n```")).toContain("<pre><code>x</code></pre>");
    // 正文以换行结尾 → 末尾空行成块；正文区间止于换行前（末尾换行不进 <code>）
    expect(renderMarkdownToHtml("```\nx\n\n```")).toContain('<pre><code>x</code><div class="md-editor-code-blank"></div></pre>');
    const edit = renderMarkdownEditHtml("```\nx\n\n```", { offsets: true });
    // 第 6 位是末尾换行、第 7 位是空行起点：锚落在空行起点，光标直接驻留其上
    expect(edit).toContain('<code><span data-md-from="4" data-md-to="5">x</span></code>');
    expect(edit).toContain('<div class="md-editor-code-blank" data-md-from="6" data-md-to="6">\u200B</div>');
    expect(renderMarkdownEditHtml("```\nx\n\n\n```", { offsets: true })).toContain(
      '<div class="md-editor-code-blank" data-md-from="7" data-md-to="7">\u200B</div>',
    );
  });

  it("空正文围栏代码：编辑面补零宽锚（有落点），只读面保持原样", () => {
    expect(renderMarkdownToHtml("```\n```")).toContain("<pre><code></code></pre>");
    expect(renderMarkdownEditHtml("```\n```", { offsets: true })).toContain(
      '<code><span data-md-from="4" data-md-to="4">\u200B</span></code>',
    );
    // 正文整段都是换行时同理：换行归入空行块，正文仍是空锚
    expect(renderMarkdownEditHtml("```\n\n```", { offsets: true })).toContain(
      '<code><span data-md-from="4" data-md-to="4">\u200B</span></code>',
    );
  });

  it("列表项内容与其首个子块之间的源码空行渲染为真实空行（与项之间同一口径）", () => {
    // 「- 项目二\n\n  - 嵌套项目」：项目二与它的嵌套子列表之间有一个源码空行 → 渲染一行高
    const out = renderMarkdownToHtml("- 项目二\n\n  - 嵌套项目");
    expect(out.match(/md-editor-gap/g) ?? []).toHaveLength(1);
    // 无空行时不插行元素：嵌套列表紧贴上一行（不会平白多出一行）
    expect(renderMarkdownToHtml("- 项目二\n  - 嵌套项目")).not.toContain("md-editor-gap");
    // 松散项同样按源码渲染空行
    expect(renderMarkdownToHtml("- 甲\n\n  续段").match(/md-editor-gap/g) ?? []).toHaveLength(1);
  });

  it("子块缩进空白排在空行元素之前（揭示态不会平白多折出一行）", () => {
    // 缩进空白标记若排在块级空行元素之后，inline 空白会自成一个行盒——揭示态就多一行
    const out = renderMarkdownEditHtml("- 项目二\n\n  - 嵌套项目", { offsets: true });
    expect(out).toContain(
      '<span data-md-from="2" data-md-to="5">项目二</span>' +
        '<span class="md-marker" data-md-from="7" data-md-to="9">  </span>' +
        '<div class="md-editor-gap" data-md-from="6" data-md-to="6">\u200B</div>',
    );
  });

  it("分隔线带零宽锚（编辑面）：点击落点与光标能落在 `---` 所在行", () => {
    const src = "甲\n\n---\n\n乙";
    // 分隔线是块、源标记默认隐藏，没有可量出矩形的位置 → 没有锚点就点不进源码态
    expect(renderMarkdownEditHtml(src, { offsets: true })).toContain(
      '<div class="md-editor-divider" data-md-block data-md-kind="hr" data-md-from="3" data-md-to="6" data-md-own>' +
        '<span data-md-from="3" data-md-to="3">\u200B</span>' +
        '<span class="md-marker" data-md-from="3" data-md-to="6">---</span></div>',
    );
    // 只读面/插件面产物不变（不产出锚点与标记）
    expect(renderMarkdownToHtml(src)).toBe(
      '<p>甲</p><div class="md-editor-gap"></div><div class="md-editor-divider"></div>' +
        '<div class="md-editor-gap"></div><p>乙</p>',
    );
  });

  it("列表项带项级块标记与标记宿主（揭示与偏移映射细到项）", () => {
    const out = renderMarkdownEditHtml("- 甲\n- 乙", { offsets: true });
    expect(out).toContain(BULLET);
    expect(out).toMatch(
      /<li data-md-block data-md-kind="list" data-md-from="\d+" data-md-to="\d+" data-md-own><span class="md-marker"/,
    );
  });

  it("围栏代码：围栏行自持揭示区间，代码正文照常渲染（光标在正文里不塌陷）", () => {
    const src = "```js\nconst x = 1;\n```";
    const out = renderMarkdownEditHtml(src, { offsets: true });
    expect(out).toContain("md-editor-code-head");
    // 围栏标记自身即揭示宿主：区间只覆盖围栏行本身（不含两侧换行），
    // 光标停在正文首/末字符上时不会命中，块仍是渲染态
    expect(out).toContain('<span class="md-marker" data-md-own data-md-from="0" data-md-to="5">```js</span>');
    expect(out).toContain('<span class="md-marker" data-md-own data-md-from="19" data-md-to="22">```</span>');
    expect(out).toContain("const x = 1;");
    // 产物与光标位置无关（揭示纯靠切 class）
    expect(out).toBe(renderMarkdownEditHtml(src, { offsets: true, activeOffset: 8 }));
  });

  it("富装饰块（表格）随光标进出整块回显源码", () => {
    const src = "| a | b |\n| - | - |\n| 1 | 2 |";
    const idle = renderMarkdownEditHtml(src, { offsets: true });
    expect(idle).toContain("md-editor-table");
    const active = renderMarkdownEditHtml(src, { offsets: true, activeOffset: 8 });
    expect(active).toContain('<div class="md-editor-source"');
  });

  it("嵌套列表：各层源标记与圆点同在，产物不随光标变化", () => {
    const src = "- 甲\n  - 乙\n- 丙";
    const out = renderMarkdownEditHtml(src, { offsets: true, activeOffset: src.indexOf("乙") });
    expect(out).toBe(renderMarkdownEditHtml(src, { offsets: true }));
    // 外层两项、内层一项的源标记，外加内层子列表前的缩进空白（无可见承载，单独进标记层）
    expect(out.match(/class="md-marker"/g) ?? []).toHaveLength(4);
    expect(out).not.toContain("md-editor-source");
  });

  it("替换形态的渲染件带 md-yield（源标记显形时让位，两者不同现）", () => {
    expect(renderMarkdownEditHtml("![图](a.png)", { offsets: true })).toContain('<span class="md-yield"><img alt="图"');
    expect(renderMarkdownEditHtml("```js\nx\n```", { offsets: true })).toContain('<div class="md-editor-code-head">');
    expect(renderMarkdownEditHtml("> [!note]\n> 提示", { offsets: true })).toContain("md-editor-callout-badge md-yield");
    // 只读面与插件面产物不变（不产出标记层与让位壳）
    const readOnly = renderMarkdownToHtml("![图](a.png)", { offsets: false });
    expect(readOnly).toContain('<img alt="图"');
    expect(readOnly).not.toContain("md-yield");
  });

  it("裸 URL 整段是可见文本（不按 `<…>` 取首尾字符当标记）", () => {
    const out = renderMarkdownEditHtml("见 https://x.com/a 处", { offsets: true });
    expect(out).toContain('data-md-href="https://x.com/a"');
    expect(out).toContain('<span data-md-from="2" data-md-to="17">https://x.com/a</span>');
    expect(out).not.toContain("md-marker");
  });

  it("分隔线：源 `---` 进标记层，隐藏态只剩线", () => {
    const out = renderMarkdownEditHtml("---", { offsets: true });
    expect(out).toContain('<div class="md-editor-divider"');
    expect(out).toContain(
      'data-md-own><span data-md-from="0" data-md-to="0">\u200B</span>' +
        '<span class="md-marker" data-md-from="0" data-md-to="3">---</span>',
    );
  });

  it("嵌套引用：`>` 前缀随各自那段内容进标记层，不做成容器的独立子元素", () => {
    const src = "> 一级\n> > 二级\n> > > 三级";
    // 渲染态只见正文与嵌套竖线，`>` 一个都不外泄
    expect(renderMarkdownToHtml(src)).not.toContain("&gt;");
    const out = renderMarkdownEditHtml(src, { offsets: true });
    // 每层的行前缀与它那一行的正文同处一个段落内——`>` 才不会脱行单独显示
    expect(out).toContain(
      '<span class="md-marker" data-md-from="0" data-md-to="2">&gt; </span><span data-md-from="2" data-md-to="4">一级</span>',
    );
    expect(out).toContain(`<span class="md-marker" data-md-from="5" data-md-to="9">&gt; &gt; </span><span data-md-from="9"`);
    expect(out).toContain(`<span class="md-marker" data-md-from="12" data-md-to="18">&gt; &gt; &gt; </span><span data-md-from="18"`);
    // 引用容器自身不产出标记（它没有标记子元素，也就无需揭示宿主）
    expect(out).not.toMatch(/<blockquote[^>]*data-md-own/);
  });

  it("引用内多行段落：续行的 `>` 不进正文，只作标记（渲染态无 `>`、揭示态还原源码）", () => {
    const src = "> 甲\n> 乙";
    expect(renderMarkdownToHtml(src)).toBe('<blockquote class="md-quote"><p>甲\n乙</p></blockquote>');
    const out = renderMarkdownEditHtml(src, { offsets: true });
    expect(out).toContain('<span class="md-marker" data-md-from="0" data-md-to="2">&gt; </span>');
    expect(out).toContain('<span class="md-marker" data-md-from="4" data-md-to="6">&gt; </span>');
  });

  it("引用内列表：行前缀与外层项标记合成一段（揭示态即源文本）", () => {
    const out = renderMarkdownEditHtml("- 甲\n  > 引文\n  > - 内项", { offsets: true });
    expect(out).toContain('<span class="md-marker" data-md-from="0" data-md-to="2">- </span>');
    // 引用段落与引用内列表项各自带自己那行的完整前缀
    expect(out).toContain('<span class="md-marker" data-md-from="4" data-md-to="8">  &gt; </span>');
    expect(out).toContain('<span class="md-marker" data-md-from="11" data-md-to="17">  &gt; - </span>');
    expect(out).not.toContain("md-editor-source");
  });
});

describe("renderMarkdownEditHtml 输入法组合串", () => {
  it("组合串就地插入文本片段中间，两侧偏移保持原值（映射不随组合串漂移）", () => {
    const out = renderMarkdownEditHtml("甲乙", { offsets: true, composition: { at: 1, text: "ni" } });
    expect(out).toBe(
      '<p data-md-block data-md-kind="paragraph" data-md-from="0" data-md-to="2" data-md-own>' +
        '<span data-md-from="0" data-md-to="1">甲</span>' +
        '<span class="md-composition">ni</span>' +
        '<span data-md-from="1" data-md-to="2">乙</span></p>',
    );
  });

  it("标题里的组合串就地插入在文本片段处（标记层不受影响）", () => {
    const out = renderMarkdownEditHtml("# 标题", { offsets: true, composition: { at: 3, text: "ab" } });
    expect(out).toContain(
      '<span data-md-from="2" data-md-to="3">标</span>' +
        '<span class="md-composition">ab</span>' +
        '<span data-md-from="3" data-md-to="4">题</span>',
    );
    expect(out).toContain('<span class="md-marker" data-md-from="0" data-md-to="2"># </span>');
  });

  it("空项（只有标记）上的组合串就地插入，不退化成文末独立分片", () => {
    // 空项内容走零宽锚路径（不经过行内片段渲染），组合串必须一并插入，
    // 否则它没有落点、会被挂到文末，输入法候选框随之飞到文档底部
    const out = renderMarkdownEditHtml("- 甲\n  - ", { offsets: true, activeOffset: 8, composition: { at: 8, text: "ni" } });
    expect(out).toContain('\u200B</span><span class="md-composition">ni</span></li></ul>');
  });

  it("组合串替换选区时被替换的正文不再出渲染（不叠显旧文字）", () => {
    const out = renderMarkdownEditHtml("甲乙丙", { offsets: true, composition: { at: 1, text: "ni", remove: 2 } });
    expect(out).toContain('<span data-md-from="0" data-md-to="1">甲</span>');
    expect(out).toContain('<span class="md-composition">ni</span>');
    expect(out).not.toContain("乙</span>");
    expect(out).not.toContain("丙</span>");
  });

  it("组合串在空行上时落在该空行行元素内", () => {
    const out = renderMarkdownEditHtml("甲\n\n乙", { offsets: true, composition: { at: 2, text: "ab" } });
    expect(out).toContain('<div class="md-editor-gap" data-md-from="2" data-md-to="2">\u200B<span class="md-composition">ab</span></div>');
  });

  it("行内元素边界上的组合串插在元素前后（不在容器内外重复插入）", () => {
    const src = "a **b** c";
    const before = renderMarkdownEditHtml(src, { offsets: true, composition: { at: 2, text: "x" } });
    expect((before.match(/md-composition/g) ?? [])).toHaveLength(1);
    expect(before).toContain('<span class="md-composition">x</span><strong data-md-from="2" data-md-to="7"');
    const inside = renderMarkdownEditHtml(src, { offsets: true, composition: { at: 4, text: "x" } });
    expect((inside.match(/md-composition/g) ?? [])).toHaveLength(1);
    expect(inside).toContain(
      '<span class="md-marker" data-md-from="2" data-md-to="4">**</span>' +
        '<span class="md-composition">x</span><span data-md-from="4" data-md-to="5">b</span>',
    );
  });

  it("组合串落在标记字符里时就地插入（标记拆成前后两段，不飞到文末）", () => {
    // 行首 Home 后停在标题标记里
    const heading = renderMarkdownEditHtml("# 标题", { offsets: true, composition: { at: 0, text: "ni" } });
    expect(heading.match(/md-composition/g) ?? []).toHaveLength(1);
    expect(heading).toContain(
      '<span class="md-composition">ni</span><span class="md-marker" data-md-from="0" data-md-to="2"># </span>',
    );
    // 方向键停在 `**` 中间
    const bold = renderMarkdownEditHtml("a **b** c", { offsets: true, composition: { at: 3, text: "x" } });
    expect(bold.match(/md-composition/g) ?? []).toHaveLength(1);
    expect(bold).toContain(
      '<span class="md-marker" data-md-from="2" data-md-to="3">*</span>' +
        '<span class="md-composition">x</span>' +
        '<span class="md-marker" data-md-from="3" data-md-to="4">*</span>',
    );
    // 围栏行（标记自身即揭示宿主）
    const fence = renderMarkdownEditHtml("```js\nx\n```", { offsets: true, composition: { at: 1, text: "p" } });
    expect(fence.match(/md-composition/g) ?? []).toHaveLength(1);
    expect(fence).toContain('<span class="md-marker" data-md-own data-md-from="0" data-md-to="1">`</span>');
  });

  it("组合串只插一份：行内元素末端与所在内容区末端重合时", () => {
    // 「加粗元素末端」既是元素自身末端、也是所在段落内容区末端（内外两层都会回看同一偏移）
    const cases: [string, number][] = [
      ["**b**", 5],
      ["a **b**", 7],
      ["a **b** c", 7],
    ];
    for (const [src, at] of cases) {
      const out = renderMarkdownEditHtml(src, { offsets: true, composition: { at, text: "x" } });
      expect(out.match(/md-composition/g) ?? [], `${src} at ${at}`).toHaveLength(1);
    }
  });

  it("任意文档的任意偏移上恰好插一份（覆盖块与行内元素的全部边界）", () => {
    const docs = [
      "**b**",
      "a **b** c",
      "`cd` e",
      "# 标题",
      "- 甲\n  - 乙\n- 丙",
      "> 引用 **粗** 文字",
      "[标](http://x) 尾",
      "![图](a.png) 尾",
      "```js\nx\n```",
      "```\nx```",
      "```\n```",
      "```\nx\n\n```",
      "甲\n\n乙",
      "==高亮== 与 ~~删~~",
      "- ",
      "- [ ] 任务",
      "> - 内项",
      "[[笔记]] 与 %%注释%%",
      "1. 甲\n2. 乙",
      "---",
    ];
    for (const src of docs) {
      for (let at = 0; at <= src.length; at++) {
        const out = renderMarkdownEditHtml(src, { offsets: true, composition: { at, text: "x" } });
        expect(out.match(/md-composition/g) ?? [], `${JSON.stringify(src)} at ${at}`).toHaveLength(1);
        // 文末偏移的兜底分片本就落在正文末尾；换行偏移没有字形（光标位置与前一个字符的偏移
        // 重合）——这两类不要求就地插入。其余偏移必须就地插入：兜底分片是「只有组合串」的
        // 独立片，靠它说明该偏移没有落点、预览会跑到文末
        if (src[at] === "\n" || at === src.length) continue;
        const chunks = renderMarkdownEditChunks(src, { offsets: true, composition: { at, text: "x" } }).chunks;
        const owner = chunks.find((c) => c.html.includes("md-composition"));
        expect(owner?.html.trim(), `${JSON.stringify(src)} at ${at}`).not.toBe('<span class="md-composition">x</span>');
      }
    }
  });

  it("行内代码与链接标签里的组合串就地插入（不退化成文末独立分片）", () => {
    const code = renderMarkdownEditHtml("a `cd` b", { offsets: true, composition: { at: 4, text: "x" } });
    expect(code.match(/md-composition/g) ?? []).toHaveLength(1);
    expect(code).toContain('<span data-md-from="3" data-md-to="4">c</span><span class="md-composition">x</span>');
    const link = renderMarkdownEditHtml("[标签](http://x)", { offsets: true, composition: { at: 2, text: "x" } });
    expect(link.match(/md-composition/g) ?? []).toHaveLength(1);
    expect(link).toContain('<span class="md-composition">x</span><span data-md-from="2" data-md-to="3">签</span>');
  });

  it("组合串替换的选区整体覆盖行内元素时，该元素不再出渲染（不叠显旧文字）", () => {
    const out = renderMarkdownEditHtml("甲**乙**丙", { offsets: true, composition: { at: 1, text: "x", remove: 5 } });
    expect(out.match(/md-composition/g) ?? []).toHaveLength(1);
    expect(out).not.toContain("<strong");
    expect(out).toContain('<span data-md-from="6" data-md-to="7">丙</span>');
  });

  it("组合串没有可落分片时单独成片（空文档仍能显示未上屏文本与光标）", () => {
    const out = renderMarkdownEditHtml("", { offsets: true, composition: { at: 0, text: "ni" } });
    expect(out).toBe('<span class="md-composition">ni</span>');
  });

  it("文末换行之后（无块也无空行）的组合串同样成片", () => {
    const out = renderMarkdownEditHtml("abc\n", { offsets: true, composition: { at: 4, text: "ni" } });
    expect(out.endsWith('<span class="md-composition">ni</span>')).toBe(true);
    expect((out.match(/md-composition/g) ?? [])).toHaveLength(1);
  });

  it("未提供组合串时不产出占位元素", () => {
    expect(renderMarkdownToHtml("甲乙")).not.toContain("md-composition");
    expect(renderMarkdownEditHtml("甲乙", { offsets: true })).not.toContain("md-composition");
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
    expect(out).toContain(BULLET);
    expect(out).toContain('<span class="md-editor-list-marker">2.</span>');
  });

  it("嵌套列表渲染为内层列表结构，内层 marker 不泄漏为正文", () => {
    const out = renderMarkdownToHtml("- outer\n  - inner\n    - deep\n");
    expect((out.match(/<ul class="md-editor-list">/g) ?? []).length).toBe(3);
    expect(out).toContain("inner");
    expect(out).not.toContain("- inner");
    expect(out).not.toContain("- deep");
  });

  it("项内容剥掉子块前的换行与缩进（pre-wrap 下会渲染成多余空行）", () => {
    expect(renderMarkdownToHtml("- 甲\n  - 乙")).toBe(
      `<ul class="md-editor-list"><li>${BULLET}甲` +
        `<ul class="md-editor-list"><li>${BULLET}乙</li></ul></li></ul>`,
    );
    // 源码空行仍渲染成行元素（只是不落进项内容 span，避免 pre-wrap 再多折出一行）
    expect(renderMarkdownToHtml("- 甲\n\n  乙")).toBe(
      `<ul class="md-editor-list"><li>${BULLET}甲<div class="md-editor-gap"></div><p>乙</p></li></ul>`,
    );
  });

  it("项内容跨行时保留段内换行（剥的只是子块前那段空白）", () => {
    expect(renderMarkdownToHtml("- 甲\n  续行")).toBe(
      `<ul class="md-editor-list"><li>${BULLET}甲\n  续行</li></ul>`,
    );
    expect(renderMarkdownToHtml("- 甲\n  续行\n  - 子项")).toBe(
      `<ul class="md-editor-list"><li>${BULLET}甲\n  续行` +
        `<ul class="md-editor-list"><li>${BULLET}子项</li></ul></li></ul>`,
    );
  });

  it("空项带嵌套列表：父项独占一行占位，子列表另起一行（不与父项圆点同行）", () => {
    const out = renderMarkdownToHtml("- 甲\n- \n  - 乙");
    expect(out).toContain(`${BULLET}\u200B<ul class="md-editor-list">`);
    // 空项末尾为零宽占位，其后紧跟嵌套列表——渲染上父项圆点与子项圆点不会落在同一行
    expect(out.match(/\u200B<ul /g) ?? []).toHaveLength(1);
  });

  it("列表项内嵌标题：marker 与标题块为相邻兄弟，标题文本不重复", () => {
    const out = renderMarkdownToHtml("- ### 卡片笔记法 概述\n");
    expect(out).toBe(
      `<ul class="md-editor-list"><li>${BULLET}<h3>卡片笔记法 概述</h3></li></ul>`,
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