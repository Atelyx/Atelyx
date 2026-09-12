/**
 * 外部链接协议判定（utils/markdown.ts，纯函数）。
 * 判据放行范围 = 「谁能被系统默认程序打开」，放宽等于放宽 shell 能力，凭据面须逐项锁死。
 */
import { describe, it, expect } from "vitest";
import { isOpenableUrl } from "./markdown";

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
