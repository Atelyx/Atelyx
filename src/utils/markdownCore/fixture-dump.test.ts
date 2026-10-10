/**
 * Markdown 内核对拍快照（TS ⇄ Rust 对拍的唯一期望源）。
 *
 * 双职能：
 * - 常规跑（pnpm test）= 守卫：语料解析结果须与 `fixtures/markdown/parse.json` 快照逐值一致，
 *   内核变更后快照不同步即失败；
 * - `MD_DUMP=1 pnpm vitest run src/utils/markdownCore/fixture-dump.test.ts` = 重新生成快照，
 *   Rust 侧（atelyx-core 对拍测试）以快照为解析期望。
 *
 * 语料 = parse.test.ts 全部输入 + 边角补充；resolveLink 以固定映射编码进快照
 * （命中映射用该形态，其余 "plain"；external/create 由内核内置判定，不进映射）。
 */
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import { parseMarkdown } from "./parse";
import type { LinkForm, MarkdownDocument, ParseOptions } from "@/types/markdown";

interface CorpusCase {
  name: string;
  source: string;
  /** mention 胶囊候选。 */
  mentions?: { key: string; label: string }[];
  /** resolveLink 的确定性编码：href 命中映射用该形态，其余 "plain"。 */
  links?: Record<string, string>;
}

const corpus: CorpusCase[] = [
  {
    name: "blocks-overview",
    source: [
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
    ].join("\n"),
  },
  { name: "setext-headings", source: "Title\n=====\n\nsub\n---\n" },
  { name: "html-comment-block", source: "<!-- c -->\n" },
  { name: "footnote-single-line", source: "[^b]: singleword\n" },
  { name: "empty-source", source: "" },
  { name: "blank-lines-only", source: "\n\n\n" },
  { name: "indented-code", source: "    code line\n" },
  { name: "loose-list", source: "- a\n- b\n\n- c\n" },
  { name: "nested-blockquote", source: "> quote\n> > nested\n" },
  { name: "cjk-inline", source: "# 中文标题 ==中文高亮== #中文标签" },
  {
    name: "inline-kitchen-sink",
    source:
      "**bold** _em_ ~~del~~ `code` [ext](https://x.com) [path](notes/a.md) [new]() [plain](ftp://x) " +
      "[[wiki|W]] #tag ==hl== %%note%% $e=mc^2$ [^fn] @Alice ![img|100x50](p.png)",
    mentions: [{ key: "u1", label: "Alice" }],
    links: { "notes/a.md": "path" },
  },
  {
    name: "mention-vs-tag",
    source: "#Alice #unknown",
    mentions: [{ key: "u1", label: "Alice" }],
  },
  {
    name: "mention-punct-label",
    source: '#示例"标题"',
    mentions: [{ key: "f1", label: '示例"标题"' }],
  },
  {
    name: "mention-word-boundary",
    source: "#示例标题",
    mentions: [{ key: "f2", label: "示例" }],
  },
  { name: "autolink-angle", source: "see <https://x.example> now" },
  { name: "autolink-bare", source: "see https://bare.example now" },
  {
    name: "skip-fence-inline-code",
    source: ["```", "#tag $$", "```", "", "inline `#tag` here", "", "real #tag here"].join("\n"),
  },
  { name: "skip-tag-in-link", source: "[see #tag](https://x.com)" },
  { name: "skip-math-in-fence", source: ["```", "$$", "x", "$$", "```"].join("\n") },
  { name: "nested-list-deep", source: "- outer\n  - inner\n    - deep\n" },
  { name: "math-paragraph-overlap", source: "$$x\n$$ tail text\n" },
  { name: "math-fence-overlap", source: "```text\n$$\n```\nhello $$ world\n" },
  { name: "math-empty-body", source: "$$\n$$\n" },
  { name: "crlf-footnote", source: "[^a]: note text\r\nbody\r\n" },
  {
    name: "table-escaped-pipe",
    source: "| x \\| y | z |\n| --- | --- |\n| a | b |\n",
  },
  { name: "html-tags-nonascent", source: "<b>x</b> plain #tag <i>y</i>\n" },
  { name: "math-multiple-paragraphs", source: "$$a$$\ntext\n$$b$$\n" },
  { name: "math-cross-paragraph", source: "$$x\n\n$$y\n" },
  { name: "math-cross-paragraph-middle", source: "$$x\n\nmiddle\n\n$$y\n" },
  { name: "table-trailing-escape", source: "| a \\|\n| --- |\n" },
  { name: "html-pair-merge", source: "text <b>x</b> end\n" },
  // ===== 边角补充（强调/换行/实体/转义/围栏/列表/引用/表格/打断）=====
  { name: "emphasis-nested", source: "**bold *inner* end**\n" },
  { name: "emphasis-intra-word", source: "foo_bar_baz and a*b*c\n" },
  { name: "emphasis-mixed", source: "***both*** and **a *b* c**\n" },
  { name: "strike-basic", source: "~~del~~ keep ~one~\n" },
  { name: "hardbreak", source: "a  \nb\nc\n" },
  { name: "entities", source: "&amp; &#65; &#x41; plain & tail\n" },
  { name: "escapes", source: '\\*not em\\* and \\\`tick\\\`' },
  { name: "fence-multi-line", source: "```\nline1\nline2\n```\n" },
  { name: "fence-unclosed", source: "```\ncode tail" },
  { name: "fence-empty", source: "```\n```\n" },
  { name: "fence-info-extra", source: "```js extra\ncode\n```\n" },
  { name: "fence-tilde", source: "~~~\ncode\n~~~\n" },
  { name: "indented-code-multi", source: "    a\n\n    b\n" },
  { name: "list-multi-para", source: "- a\n\n  b\n" },
  { name: "list-nested-quote", source: "- > quoted\n" },
  { name: "quote-multi-para", source: "> a\n>\n> b\n" },
  { name: "quote-list", source: "> - item\n> - two\n" },
  { name: "table-aligns", source: "| l | c | r |\n| :- | :-: | -: |\n| 1 | 2 | 3 |\n" },
  { name: "atx-trailing-hash", source: "## head ##\n" },
  { name: "atx-no-space", source: "#nospace plain\n" },
  { name: "hr-variants", source: "***\n\n___\n" },
  { name: "para-interrupt-list", source: "para\n- item\n" },
  { name: "para-interrupt-atx", source: "para\n# head\n" },
  { name: "link-title", source: '[t](https://x.com "title") tail\n' },
  { name: "link-paren-url", source: "[t](url(sub)) tail\n" },
  { name: "reference-link", source: "[foo][bar] tail\n\n[bar]: https://x.com\n" },
  { name: "wikilink-variants", source: "[[target]] [[a|b]] [[]] tail\n" },
  { name: "footnote-def-multi", source: "[^a]: first\n[^b]: second\n" },
  { name: "html-inline-attrs", source: '<a href="x">y</a> tail\n' },
  { name: "comment-inline", source: "a <!-- c --> b\n" },
  { name: "math-mix", source: "$$a$$ inline $b$ tail\n" },
  { name: "cjk-punct", source: "**中文**「引号」#标签\n" },
  { name: "tag-variants", source: "#a #a-b #a/b #1 #_x tail\n" },
  { name: "autolink-trailing-punct", source: "see https://x.com/path. now\n" },
  { name: "atx-indented-in-nested-list", source: "- # 顶层\n\t- ## 二级\n\t  # 三级\n" },
  // ===== 缩进/Tab 块起点（块 from 不含行首缩进，Tab 按列推进）=====
  { name: "indented-atx-heading", source: "  # h\n" },
  { name: "indented-hr", source: "  ---\n" },
  { name: "indented-table", source: "  | a | b |\n  | --- | --- |\n  | c | d |\n" },
  { name: "indented-html-block", source: "  <div>hi</div>\n" },
  { name: "indented-fence", source: "  ```\ncode\n  ```\n" },
  { name: "indented-footnote-def", source: "  [^a]: x\n" },
  { name: "tab-heading-is-code", source: "\t# h\n" },
  { name: "tab-quote-in-item", source: "- x\n\t> q\n" },
  { name: "tab-html-in-item", source: "- x\n\t<div>hi</div>\n" },
  { name: "tab-table-in-item", source: "- x\n\t| a | b |\n\t| --- | --- |\n\t| c | d |\n" },
  { name: "quote-space-tab", source: "> \tq\n" },
  { name: "quote-tab-nested", source: ">\t>\tq\n" },
  { name: "tab-indented-code-double", source: "\t\tcode\n" },
  { name: "tab-indented-code-space-tab", source: "    \tcode\n" },
  { name: "item-tab-indented-code", source: "- x\n\n\t\tqual\n" },
  // ===== 打字中途态（易碎输入，锁定为安全解析）=====
  { name: "link-trailing-backslash", source: "[a\\\n" },
  { name: "declaration-multi-line", source: "<!DOCTYPE html>\ntext\n" },
  { name: "callout-bang-only", source: "> [!\n" },
  { name: "callout-cjk-name", source: "> [!中文] x\n" },
  { name: "fence-close-longer-run", source: "```\ncode\n`````\n" },
];

const FIXTURE_PATH = path.resolve(process.cwd(), "fixtures/markdown/parse.json");
const DUMP = !!process.env.MD_DUMP;

function optionsOf(c: CorpusCase): ParseOptions {
  return {
    mentions: c.mentions,
    resolveLink: c.links ? (href: string): LinkForm => (c.links?.[href] ?? "plain") as LinkForm : undefined,
  };
}

describe.skipIf(!DUMP)("markdown 对拍快照生成（MD_DUMP=1）", () => {
  it("全量语料写单文件快照", () => {
    const cases = corpus.map((c) => ({
      name: c.name,
      source: c.source,
      mentions: c.mentions ?? null,
      links: c.links ?? null,
      expected: parseMarkdown(c.source, optionsOf(c)),
    }));
    writeFileSync(FIXTURE_PATH, JSON.stringify({ cases }, null, 2) + "\n");
  });
});

describe.skipIf(DUMP)("markdown 对拍快照守卫", () => {
  const snapshot = JSON.parse(readFileSync(FIXTURE_PATH, "utf8")) as {
    cases: (CorpusCase & { expected: MarkdownDocument })[];
  };
  const readCase = (name: string): CorpusCase & { expected: MarkdownDocument } => {
    const found = snapshot.cases.find((c) => c.name === name);
    if (!found) throw new Error(`快照缺语料: ${name}`);
    return found;
  };

  it("语料规模兜底（防语料清单被误清空）", () => {
    expect(corpus.length).toBeGreaterThan(20);
  });

  for (const c of corpus) {
    it(c.name, () => {
      const fixture = readCase(c.name);
      expect(parseMarkdown(fixture.source, optionsOf(fixture))).toEqual(fixture.expected);
    });
  }
});
