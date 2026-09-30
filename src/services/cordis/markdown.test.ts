/**
 * Markdown 渲染服务契约测试：与内核同一份实现，保证插件侧渲染结果与应用内一致。
 */
// @vitest-environment jsdom
import { describe, expect, it } from "vitest";
import { createMarkdownService } from "./markdown";

describe("ctx.markdown 渲染服务", () => {
  it("renderHtml 出已清洗 HTML 并命中既有 class 契约", () => {
    const html = createMarkdownService().renderHtml("# 标题\n\n- 项 `码`\n\n> [!note] 提示\n");
    expect(html).toContain("<h1>标题</h1>");
    expect(html).toContain("<code>码</code>");
    expect(html).toContain("md-editor-callout-badge");
  });

  it("raw HTML 经白名单清洗，脚本与事件属性被剥离", () => {
    const html = createMarkdownService().renderHtml('<div onclick="x()">a</div>\n\n<script>alert(1)</script>\n');
    expect(html).toContain("a");
    expect(html).not.toContain("onclick");
    expect(html).not.toContain("<script");
  });

  it("parse 返回块规格（偏移基于源文本）", () => {
    const doc = createMarkdownService().parse("# 标\n\n正文");
    expect(doc.source).toBe("# 标\n\n正文");
    expect(doc.blocks.map((b) => b.kind)).toEqual(["heading", "paragraph"]);
    expect(doc.blocks[0]?.from).toBe(0);
  });

  it("renderToFragment 在有 DOM 时返回片段", () => {
    const fragment = createMarkdownService().renderToFragment("**粗**");
    expect(fragment?.querySelector("strong")?.textContent).toBe("粗");
  });
});