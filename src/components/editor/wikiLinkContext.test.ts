/**
 * wikiLinkTriggerContext 单测：文本级片段判定 + 代码/HTML 区间语法树守卫。
 */
import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { syntaxTree } from "@codemirror/language";
import { wikiLinkTriggerContext } from "./wikiLinkContext";

function stateOf(md: string): EditorState {
  return EditorState.create({
    doc: md,
    extensions: [markdown({ addKeymap: false, base: markdownLanguage })],
  });
}

/** 等待语法树同步解析到指定片段可用（EditorState.create 时同步完成首屏解析）。 */
function treeReady(state: ReturnType<typeof stateOf>): boolean {
  return syntaxTree(state).length >= state.doc.length;
}

describe("wikiLinkTriggerContext", () => {
  it("光标在未闭合 `[[查询词` 内触发，from 为文档绝对偏移", () => {
    const state = stateOf("前文 [[草稿");
    const pos = state.doc.length;
    const ctx = wikiLinkTriggerContext(state, pos);
    expect(ctx).toEqual({ from: 3, query: "草稿" });
  });

  it("已闭合 `]]` 不触发", () => {
    const state = stateOf("[[笔记]]");
    const pos = 4; // `[[笔记` 与 `]]` 之间
    expect(wikiLinkTriggerContext(state, pos)).toBeNull();
  });

  it("`【【` 等效触发，from 为文档绝对偏移", () => {
    const state = stateOf("前文【【草稿");
    const ctx = wikiLinkTriggerContext(state, state.doc.length);
    expect(ctx).toEqual({ from: 2, query: "草稿" });
  });

  it("`【【…】】` 已闭合不触发", () => {
    const state = stateOf("【【笔记】】");
    expect(wikiLinkTriggerContext(state, 4)).toBeNull();
  });

  it("行内代码内不触发（语法不生效处不弹候选）", () => {
    const md = "正文 `[[代码内` 后文";
    const state = stateOf(md);
    expect(treeReady(state)).toBe(true);
    // 光标落在行内代码区间内（反引号之间）
    const open = md.indexOf("`", 2);
    const pos = open + 4; // `[[代` 之后
    expect(wikiLinkTriggerContext(state, pos)).toBeNull();
  });

  it("围栏代码块内不触发", () => {
    const md = "```\n[[块内\n```";
    const state = stateOf(md);
    expect(treeReady(state)).toBe(true);
    const pos = md.indexOf("内") + 1;
    expect(wikiLinkTriggerContext(state, pos)).toBeNull();
  });

  it("代码区间外的同名片段照常触发（守卫不扩大化）", () => {
    const md = "`代码` 之后 [[正文片段";
    const state = stateOf(md);
    expect(treeReady(state)).toBe(true);
    const ctx = wikiLinkTriggerContext(state, md.length);
    expect(ctx?.query).toBe("正文片段");
    expect(ctx?.from).toBe(md.indexOf("[["));
  });
});
