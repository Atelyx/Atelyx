/**
 * 首帧骨架的契约：配色与 index.css 主题基色同值、形状与真 chrome 同档、只对撕裂窗口生效，
 * 以及跨文件静态首帧的对齐项（角色前缀、主题缓存键）。
 * 首帧骨架是构建期注入 index.html 的原文（见 vite.config.ts 的 bootSplashPlugin），
 * 这些不变量在运行时没有断言点，漏了只表现为启动时闪一次异色或形状错位。
 */
import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import {
  BOOT_SCHEME_KEY,
  BOOT_SPLASH_ID,
  bootSplashMarkup,
  bootSplashStyle,
  PANEL_SKELETON_CONTENT_ROWS,
  PANEL_SKELETON_TAB_PILLS,
  PANEL_SKELETON_TABBAR_REM,
  PANEL_SKELETON_TITLEBAR_REM,
} from "@/constants/panelSkeleton";

/** 取 index.css 指定主题块里的变量值（`:root {` 浅色 / `:root.dark {` 深色）。 */
function themeVar(css: string, selector: string, name: string): string {
  const at = css.indexOf(selector);
  expect(at, `index.css 缺少 ${selector} 块`).toBeGreaterThan(-1);
  const block = css.slice(at, css.indexOf("}", at));
  const value = block.split(`${name}:`)[1];
  expect(value, `${selector} 缺少 ${name}`).toBeTruthy();
  return value.split(";")[0].trim().toLowerCase();
}

const html = readFileSync(new URL("../../index.html", import.meta.url), "utf8");
const style = bootSplashStyle();
const markup = bootSplashMarkup();

describe("首帧骨架", () => {
  const css = readFileSync(new URL("../styles/index.css", import.meta.url), "utf8");

  it("三档配色与 index.css 同值，且深浅各走自己的分支", () => {
    for (const [scheme, selector] of [
      ["dark", "html.boot-panel.dark"],
      ["light", "html.boot-panel:not(.dark)"],
    ] as const) {
      const root = scheme === "dark" ? ":root.dark {" : ":root {";
      const bg = themeVar(css, root, "--bg-primary");
      const surface = themeVar(css, root, "--bg-secondary");
      const line = themeVar(css, root, "--border");
      const block = themeVar(css, root, "--bg-tertiary");
      expect(style).toContain(`${selector} #${BOOT_SPLASH_ID}{background:${bg}}`);
      expect(style).toContain(`${selector} .bs-title{background:${surface};border-bottom:1px solid ${line}}`);
      expect(style).toContain(`${selector} .bs-tabs{background:${surface};border-bottom:1px solid ${line}}`);
      expect(style).toContain(
        `${selector} .bs-title i,${selector} .bs-tabs i,${selector} .bs-body i{background:${block}}`,
      );
    }
  });

  it("只对撕裂窗口生效：默认隐藏，铺开需 html.boot-panel（主窗口启动期形状不同，不铺）", () => {
    expect(style).toContain(`#${BOOT_SPLASH_ID}{display:none`);
    expect(style).toContain(`html.boot-panel #${BOOT_SPLASH_ID}{display:flex`);
    expect(html).toContain("boot-panel");
    expect(html).toContain("__TAURI_INTERNALS__");
  });

  it("档位与真 chrome 的 Tailwind 类同值：h-9 = 2.25rem、h-7 = 1.75rem", () => {
    expect(PANEL_SKELETON_TITLEBAR_REM).toBe(2.25);
    expect(PANEL_SKELETON_TABBAR_REM).toBe(1.75);
    // 真 chrome 用同名档位：改档位时这里先红，提醒同步骨架常量（它们是首帧唯一的形状来源）
    const tabBar = readFileSync(new URL("../components/layout/PanelTabBar.tsx", import.meta.url), "utf8");
    expect(tabBar).toContain("h-7 ");
    const panelRoot = readFileSync(new URL("../components/layout/PanelWindowRoot.tsx", import.meta.url), "utf8");
    expect(panelRoot).toContain("h-9 ");
    expect(style).toContain(`.bs-title{height:${PANEL_SKELETON_TITLEBAR_REM}rem`);
    expect(style).toContain(`.bs-tabs{height:${PANEL_SKELETON_TABBAR_REM}rem`);
  });

  it("形状与 React 骨架同源：行宽/标签宽逐个落进标记", () => {
    for (const w of PANEL_SKELETON_TAB_PILLS) expect(markup).toContain(`width:${w}px`);
    for (const w of PANEL_SKELETON_CONTENT_ROWS) expect(markup).toContain(`width:${w}%`);
  });

  it("首帧自带 box-sizing：无 Tailwind preflight 时 border 不撑高（与 React 骨架错位）", () => {
    expect(style).toContain(`#${BOOT_SPLASH_ID},#${BOOT_SPLASH_ID} *{box-sizing:border-box}`);
  });

  it("跨文件静态首帧对齐：角色前缀与主题缓存键", () => {
    // 角色判定读窗口 label：前缀与 panelStore 同值，改前缀时这里先红
    const store = readFileSync(new URL("../stores/panelStore.ts", import.meta.url), "utf8");
    const prefix = /PANEL_LABEL_PREFIX = "([^"]+)"/.exec(store)?.[1];
    expect(prefix, "panelStore 未导出 PANEL_LABEL_PREFIX").toBeTruthy();
    expect(html).toContain(`label.indexOf("${prefix}") === 0`);
    // 首帧主题缓存的读侧只能写死在 index.html（静态首帧），键名与常量同值
    expect(html).toContain(`localStorage.getItem("${BOOT_SCHEME_KEY}")`);
  });
});
