/**
 * utils/markdown.ts 纯函数单测：外部链接协议判定、wiki 链接候选上下文检测、链接 href 编码。
 */
import { describe, it, expect } from "vitest";
import {
  creatableLinkRangeInLine,
  decodeLinkHref,
  encodeMarkdownLinkHref,
  isOpenableUrl,
  wikiLinkContextAt,
} from "./markdown";

describe("isOpenableUrl", () => {
  it("放行 http/https/mailto/xmpp", () => {
    expect(isOpenableUrl("https://example.com/a?b=1")).toBe(true);
    expect(isOpenableUrl("http://127.0.0.1:1420/x")).toBe(true);
    expect(isOpenableUrl("mailto:someone@example.com")).toBe(true);
    expect(isOpenableUrl("xmpp:someone@example.com")).toBe(true);
  });

  it("拒绝本地文件与脚本协议（含大小写变体）", () => {
    expect(isOpenableUrl("file:///etc/passwd")).toBe(false);
    expect(isOpenableUrl("FILE:///C:/Windows/System32/calc.exe")).toBe(false);
    expect(isOpenableUrl("javascript:alert(1)")).toBe(false);
    expect(isOpenableUrl("JavaScript:alert(1)")).toBe(false);
    expect(isOpenableUrl("data:text/html,<script>1</script>")).toBe(false);
    expect(isOpenableUrl("ms-settings:privacy")).toBe(false);
    expect(isOpenableUrl("blob:https://example.com/uuid")).toBe(false);
    expect(isOpenableUrl("tel:+8613800000000")).toBe(false);
    expect(isOpenableUrl("smb://host/share")).toBe(false);
  });

  it("协议名周围的空白/换行不影响判定（URL 解析会剥除前导 C0 与空白）", () => {
    expect(isOpenableUrl("  https://example.com")).toBe(true);
    expect(isOpenableUrl("\n\tjavascript:alert(1)")).toBe(false);
  });

  it("非法 URL 与空值不抛错、一律拒绝", () => {
    expect(isOpenableUrl("not a url")).toBe(false);
    expect(isOpenableUrl("")).toBe(false);
    expect(isOpenableUrl("//example.com/x")).toBe(false);
    expect(isOpenableUrl("example.com")).toBe(false);
    expect(isOpenableUrl("https://")).toBe(false);
  });
});

describe("wikiLinkContextAt", () => {
  it("无 `[[` 片段返回 null", () => {
    expect(wikiLinkContextAt("普通文本", "")).toBeNull();
    expect(wikiLinkContextAt("", "")).toBeNull();
  });

  it("单个 `[` 不触发", () => {
    expect(wikiLinkContextAt("[", "")).toBeNull();
    expect(wikiLinkContextAt("前文 [", "")).toBeNull();
  });

  it("`[[` 空查询触发，from 指向 `[[` 起点", () => {
    expect(wikiLinkContextAt("[[", "")).toEqual({ from: 0, query: "" });
    expect(wikiLinkContextAt("前文[[", "")).toEqual({ from: 2, query: "" });
  });

  it("`[[查询词` 触发，from = `[[` 在 before 内的下标", () => {
    expect(wikiLinkContextAt("[[笔记", "")).toEqual({ from: 0, query: "笔记" });
    expect(wikiLinkContextAt("hello [[note draft", "")).toEqual({ from: 6, query: "note draft" });
  });

  it("`【【` 等效触发，查询词与 from 同语义", () => {
    expect(wikiLinkContextAt("【【", "")).toEqual({ from: 0, query: "" });
    expect(wikiLinkContextAt("前文【【草稿", "")).toEqual({ from: 2, query: "草稿" });
  });

  it("已闭合（after 以 `]]` 或 `】】` 起始）不触发", () => {
    expect(wikiLinkContextAt("[[笔记", "]] 后文")).toBeNull();
    expect(wikiLinkContextAt("[[笔记", "]]后续无空格")).toBeNull();
    expect(wikiLinkContextAt("【【笔记", "】】后文")).toBeNull();
  });

  it("片段含 `]` / `|` / `[` / `【` / `】` / 换行不触发", () => {
    expect(wikiLinkContextAt("[[笔]记", "")).toBeNull();
    expect(wikiLinkContextAt("[[笔记|", "")).toBeNull();
    expect(wikiLinkContextAt("[[笔[记", "")).toBeNull();
    expect(wikiLinkContextAt("[[笔\n记", "")).toBeNull();
    expect(wikiLinkContextAt("【【笔】记", "")).toBeNull();
    expect(wikiLinkContextAt("【【笔【记", "")).toBeNull();
  });

  it("before 中间的闭合链接不影响后方未闭合片段", () => {
    expect(wikiLinkContextAt("[[旧]] 新[[草稿", "")).toEqual({ from: 7, query: "草稿" });
  });
});

describe("creatableLinkRangeInLine", () => {
  it("定位 wiki 链接（含别名），label = 别名或目标", () => {
    expect(creatableLinkRangeInLine("引用 [[缺失笔记]] 结尾", 5)).toEqual({
      start: 3,
      end: 11,
      label: "缺失笔记",
    });
    expect(creatableLinkRangeInLine("a [[目标|别名]] b", 5)).toEqual({
      start: 2,
      end: 11,
      label: "别名",
    });
  });

  it("定位空路径链接 `[label]()`（含空白路径）", () => {
    expect(creatableLinkRangeInLine("x [新建]() y", 4)).toEqual({ start: 2, end: 8, label: "新建" });
    expect(creatableLinkRangeInLine("[新建]( )", 0)).toEqual({ start: 0, end: 7, label: "新建" });
  });

  it("空 wiki 目标与空 label 不算可新建", () => {
    expect(creatableLinkRangeInLine("[[]] 与 []()", 0)).toBeNull();
  });

  it("落点不在任何链接区间内返回 null；对齐区间末端也算命中", () => {
    expect(creatableLinkRangeInLine("引用 [[笔记]] 结尾", 0)).toBeNull();
    expect(creatableLinkRangeInLine("引用 [[笔记]] 结尾", 9)).toEqual({
      start: 3,
      end: 9,
      label: "笔记",
    });
  });

  it("普通路径链接（url 非空）不算可新建", () => {
    expect(creatableLinkRangeInLine("[label](path.md)", 3)).toBeNull();
  });
});

describe("encodeMarkdownLinkHref", () => {
  it("转义空格、括号与 `%`，且 `%` 先行转义保证往返", () => {
    expect(encodeMarkdownLinkHref("a b(c)d.md")).toBe("a%20b%28c%29d.md");
    expect(encodeMarkdownLinkHref("100% 完成.md")).toBe("100%25%20完成.md");
  });

  it("中文等合法字符保留原样", () => {
    expect(encodeMarkdownLinkHref("文件夹/我的笔记.md")).toBe("文件夹/我的笔记.md");
  });

  it("与 decodeLinkHref 往返一致", () => {
    const raw = "docs/我的 笔记(1).md";
    expect(decodeLinkHref(encodeMarkdownLinkHref(raw))).toBe(raw);
  });
});
