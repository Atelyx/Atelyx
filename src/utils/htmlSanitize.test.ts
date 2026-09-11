// @vitest-environment jsdom
/**
 * raw HTML 清洗白名单回归测试（必须在 DOM 环境跑：无 window 时 DOMPurify 直接返回原文，断言会空转）。
 * 覆盖：on* 事件 / javascript: URL / 高风险标签 / class、style、id / data-* 恒剥除；
 * 白名单内属性保留；data-note-file 注入后 closest() 取不到（笔记撤销与右键路由不被劫持）。
 */
import { describe, expect, it } from "vitest";
import { sanitizeHtmlFragment } from "./htmlSanitize";

/** 清洗后挂进容器，返回可查询的宿主元素。 */
function parse(html: string): HTMLElement {
  const host = document.createElement("div");
  host.innerHTML = sanitizeHtmlFragment(html);
  return host;
}

describe("sanitizeHtmlFragment", () => {
  it("剥除 on* 事件属性（属性与内联值都不留）", () => {
    const el = parse('<img src="x.png" onerror="alert(1)"><div onclick="alert(2)">x</div>');
    expect(el.querySelector("[onerror]")).toBeNull();
    expect(el.querySelector("[onclick]")).toBeNull();
    expect(el.innerHTML).not.toContain("alert(");
  });

  it("剥除 javascript: URL", () => {
    expect(parse('<a href="javascript:alert(1)">x</a>').querySelector("a")?.getAttribute("href")).toBeNull();
  });

  it("剥除 script / iframe / 表单类标签", () => {
    const el = parse('<script>alert(1)</script><iframe src="x"></iframe><form><input name="a"></form>');
    expect(el.querySelector("script")).toBeNull();
    expect(el.querySelector("iframe")).toBeNull();
    expect(el.querySelector("form")).toBeNull();
    expect(el.querySelector("input")).toBeNull();
  });

  it("剥除 class / style / id", () => {
    const div = parse('<div class="md-editor-html" style="position:fixed" id="root">x</div>').querySelector("div");
    expect(div?.getAttribute("class")).toBeNull();
    expect(div?.getAttribute("style")).toBeNull();
    expect(div?.getAttribute("id")).toBeNull();
  });

  it("剥除 data-*（ALLOW_DATA_ATTR 独立于 ALLOWED_ATTR 判定，须显式关闭）", () => {
    const el = parse('<div data-note-file="另一篇.md" data-note-content="x" data-drop-panel="files">x</div>');
    expect(el.querySelector("[data-note-file]")).toBeNull();
    expect(el.querySelector("[data-note-content]")).toBeNull();
    expect(el.querySelector("[data-drop-panel]")).toBeNull();
    expect(el.innerHTML).not.toContain("另一篇.md");
  });

  it("data-note-file 注入后 closest() 取不到（笔记撤销路由不被劫持）", () => {
    const inner = parse('<div data-note-file="另一篇.md">x</div>').firstElementChild;
    expect(inner).not.toBeNull();
    expect(inner?.closest("[data-note-file]")).toBeNull();
  });

  it("白名单内属性保留", () => {
    const el = parse(
      '<a href="https://e.com" title="t">x</a><img src="a.png" alt="图" width="10">' +
        '<table><tbody><tr><td colspan="2">c</td></tr></tbody></table>',
    );
    expect(el.querySelector("a")?.getAttribute("href")).toBe("https://e.com");
    expect(el.querySelector("a")?.getAttribute("title")).toBe("t");
    expect(el.querySelector("img")?.getAttribute("src")).toBe("a.png");
    expect(el.querySelector("img")?.getAttribute("alt")).toBe("图");
    expect(el.querySelector("img")?.getAttribute("width")).toBe("10");
    expect(el.querySelector("td")?.getAttribute("colspan")).toBe("2");
  });

  it("排版标签与媒体标签保留", () => {
    const el = parse(
      '<kbd>Ctrl</kbd><details><summary>s</summary>d</details><video src="v.mp4" controls poster="p.png"></video>',
    );
    expect(el.querySelector("kbd")).not.toBeNull();
    expect(el.querySelector("details")).not.toBeNull();
    expect(el.querySelector("video")?.hasAttribute("controls")).toBe(true);
    expect(el.querySelector("video")?.getAttribute("poster")).toBe("p.png");
  });
});
