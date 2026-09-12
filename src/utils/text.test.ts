/**
 * @标签 原位插入（utils/text.ts，纯函数）。
 * 关键不变量：插入位置只由传入的 `prev` 决定——文本变换不读调用方渲染期闭包，
 * 因此第二次调用看到的是第一次调用写回的文本，不会因闭包陈旧而覆盖。
 */
import { describe, it, expect } from "vitest";
import { insertMentionTag } from "./text";

describe("insertMentionTag", () => {
  it("空文本插入：无前导空格，标签后带尾随空格，光标落在尾随空格后", () => {
    const r = insertMentionTag("", 0, 0, "@笔记A");
    expect(r.text).toBe("@笔记A ");
    expect(r.caret).toBe(5);
  });

  it("前文不以空白结尾：补一个前导分隔空格", () => {
    const r = insertMentionTag("你好", 2, 2, "@笔记A");
    expect(r.text).toBe("你好 @笔记A ");
    // 光标 = 插入位置(2) + 前导空格(1) + 标签(4) + 尾随空格(1)
    expect(r.caret).toBe(8);
  });

  it("前文以空白结尾：不再补前导空格", () => {
    const r = insertMentionTag("你好 ", 3, 3, "@笔记A");
    expect(r.text).toBe("你好 @笔记A ");
  });

  it("替换 @ 到光标间的过滤词", () => {
    // 输入「看 @笔」，atIdx 指向 @，光标在末尾 → 过滤词「笔」被替换
    const r = insertMentionTag("看 @笔", 2, 4, "@笔记A");
    expect(r.text).toBe("看 @笔记A ");
  });

  it("二次插入：第一次的文本被保留，不是被后写覆盖", () => {
    // 锚点取自渲染期闭包（两次都在 0），文本变换按已更新的 prev 计算
    const first = insertMentionTag("", 0, 0, "@A");
    const second = insertMentionTag(first.text, 0, 0, "@B");
    // 第一次的结果原样作为第二次的前缀（旧实现按闭包空文本拼接会丢掉 @A）
    expect(second.text).toBe("@B @A ");
    // 锚点相同时新标签插在旧标签之前，随后到达的标签落在前方——顺序由调用方控制，非本函数职责
    // 光标 = 插入位置(0) + 无前导空格(0) + 标签(2) + 尾随空格(1)
    expect(second.caret).toBe(3);
  });

  it("越界区间被夹到文本长度内（不抛错、不越界拼接）", () => {
    const r = insertMentionTag("ab", 99, 99, "@X");
    expect(r.text).toBe("ab @X ");
    // 插入位置夹到 2 → 光标 = 2 + 前导空格(1) + 标签(2) + 尾随空格(1)
    expect(r.caret).toBe(6);
  });
});
