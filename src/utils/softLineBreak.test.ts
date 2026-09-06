/**
 * collapseSoftLineBreaks 单测：段内单换行折叠、结构边界（代码/公式/块级起始）保留换行。
 */
import { describe, expect, it } from "vitest";
import { collapseSoftLineBreaks } from "./softLineBreak";

describe("collapseSoftLineBreaks", () => {
  it("折叠段内单换行为空格", () => {
    expect(collapseSoftLineBreaks("第一行\n第二行")).toBe("第一行 第二行");
    expect(collapseSoftLineBreaks("a\nb\nc")).toBe("a b c");
  });

  it("空行分隔的段落保留换行", () => {
    expect(collapseSoftLineBreaks("段落一\n\n段落二")).toBe("段落一\n\n段落二");
  });

  it("代码围栏内部换行保留", () => {
    const src = "```ts\nconst a = 1;\nconst b = 2;\n```\n正文";
    expect(collapseSoftLineBreaks(src)).toBe(src);
  });

  it("块级数学 $$ 内部换行保留", () => {
    const src = "$$\na = b\nc = d\n$$\n正文";
    expect(collapseSoftLineBreaks(src)).toBe(src);
  });

  it("块级起始行（标题/列表/引用/分割线/脚注定义）保留换行", () => {
    expect(collapseSoftLineBreaks("# 标题\n正文")).toBe("# 标题\n正文");
    expect(collapseSoftLineBreaks("- 项目\n正文")).toBe("- 项目\n正文");
    expect(collapseSoftLineBreaks("> 引用\n正文")).toBe("> 引用\n正文");
    expect(collapseSoftLineBreaks("---\n正文")).toBe("---\n正文");
    expect(collapseSoftLineBreaks("[^1]: 定义\n正文")).toBe("[^1]: 定义\n正文");
  });

  it("上一行硬换行（行尾 ≥2 空格）保留换行", () => {
    expect(collapseSoftLineBreaks("硬换行  \n下一行")).toBe("硬换行  \n下一行");
  });

  it("空文档与单行不抛错", () => {
    expect(collapseSoftLineBreaks("")).toBe("");
    expect(collapseSoftLineBreaks("只有一行")).toBe("只有一行");
  });
});
