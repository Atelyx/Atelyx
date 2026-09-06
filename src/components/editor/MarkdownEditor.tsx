/**
 * 统一 Markdown 渲染引擎（CodeMirror 6 实时预览编辑 + 只读视图）。
 *
 * 架构：文档模型 = 纯文本（与文件正文逐字节一致），编辑永不改写内容——「实时预览」是
 * 纯视觉装饰层（buildDecorations → markdownWidgets），每次击键/光标移动/只读切换实时重算。
 * 不存在「序列化回写」环节，编辑行为不会规范化正文。
 *
 * 使用形态：
 * - 笔记编辑：`readOnly` 动态切换（同一 EditorView 经 StateEffect 翻转，不重建 → 预览⇄编辑
 *   零跳变、选区/滚动/协作绑定全保留）；勾选框 toggle 经 onBodyChange 上报走保存链。
 * - 只读展示面（画布文本节点/对话气泡）：`MarkdownView` 薄封装，readOnly + 禁用勾选框。
 *
 * 语法：GFM（base = markdownLanguage，解锁表格/任务/删除线/Autolink）+ 自定义扩展
 * （wiki/标签/高亮/注释/数学/raw HTML/callout/脚注/图片尺寸），见 markdownDecorations.ts。
 *
 * 安全：装饰层只出 class + textContent / 已清洗 HTML（HtmlWidget），从不注入未清洗 HTML；
 * 链接点击经 shell 打开系统程序，webview 不导航。
 *
 * 与 frontmatter 解耦：只编辑正文 body，`onBodyChange` 输出完整正文 markdown，
 * 由 NoteEditor 用 `fmPrefix + body` 拼回完整 content（frontmatter 原样保留）。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import {
  EditorState,
  StateEffect,
  StateField,
  Transaction,
  type Extension,
  type TransactionSpec,
} from "@codemirror/state";
import {
  EditorView,
  keymap,
  type DecorationSet,
} from "@codemirror/view";
import {
  HighlightStyle,
  syntaxHighlighting,
} from "@codemirror/language";
import {
  markdown,
  markdownKeymap,
  markdownLanguage,
} from "@codemirror/lang-markdown";
import { languages } from "@codemirror/language-data";
import { defaultKeymap } from "@codemirror/commands";
import { tags } from "@lezer/highlight";
import type { Text as YText } from "yjs";
import type { Awareness } from "y-protocols/awareness";
import { yCollab } from "y-codemirror.next";
import { useAppStore } from "@/stores/appStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { collapseSoftLineBreaks } from "@/utils/softLineBreak";
import type { DecorationOptions } from "./markdownWidgets";
import { buildDecorations } from "./markdownDecorations";

// ===== 只读动态切换（同一视图不重建）=====

/** 只读翻转效果：dispatch 即切换，roField 与装饰同步重建。 */
const readOnlyEffect = StateEffect.define<boolean>();

/** 只读态视图 class：CSS 依此隐藏原生编辑光标、光标样式改 default（远程协作光标/选区保留）。
 * 走 EditorView.editorAttributes（函数源）而非插件 classList：CM 的 updateAttrs 每次以固定 class
 * 串整体 setAttribute，classList 加的类会在聚焦（class 串变化）瞬间被整段覆盖抹掉。 */
const readOnlyClass = EditorView.editorAttributes.of((view) => ({
  class: view.state.readOnly ? "cm-readonly" : "",
}));

// ===== 主题 =====

/** 语法高亮色板：全部映射现有主题 CSS 变量（浅/深主题自动跟随，无需额外配置）。
 * 语言 token（keyword/string/number 等）供围栏代码块语法色，替代内置高亮主题的角色。 */
const highlightStyle = HighlightStyle.define([
  { tag: tags.heading, fontWeight: "600", color: "var(--text-primary)" },
  { tag: tags.strong, fontWeight: "600" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strikethrough, textDecoration: "line-through" },
  { tag: tags.quote, color: "var(--text-secondary)" },
  { tag: tags.link, color: "var(--accent)" },
  { tag: tags.url, color: "var(--text-muted)" },
  { tag: tags.monospace, fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace" },
  { tag: tags.processingInstruction, color: "var(--text-muted)" },
  { tag: tags.comment, color: "var(--text-muted)" },
  { tag: tags.labelName, color: "var(--text-muted)" },
  { tag: tags.string, color: "var(--text-secondary)" },
  // 语言 token（代码块语法色）
  { tag: [tags.keyword, tags.controlKeyword, tags.moduleKeyword], color: "var(--accent)" },
  { tag: [tags.string, tags.regexp, tags.character], color: "var(--highlight-code)" },
  { tag: [tags.number, tags.bool, tags.null, tags.atom], color: "var(--highlight-code)" },
  { tag: [tags.variableName, tags.propertyName], color: "var(--text-primary)" },
  { tag: [tags.typeName, tags.className, tags.namespace], color: "var(--accent-hover)" },
  { tag: [tags.function(tags.variableName), tags.function(tags.propertyName)], color: "var(--accent)" },
  { tag: [tags.operator, tags.punctuation, tags.bracket, tags.brace, tags.paren], color: "var(--text-secondary)" },
]);

const editorTheme = EditorView.theme({
  "&": {
    height: "100%",
    backgroundColor: "transparent",
    color: "var(--text-primary)",
    // 与只读视图 text-sm（0.875rem）对齐，避免切换模式时字体跳动
    fontSize: "0.875rem",
  },
  ".cm-scroller": {
    fontFamily: "inherit",
    lineHeight: "1.625",
    padding: "1rem 0",
  },
  ".cm-content": {
    padding: "0 1rem",
    caretColor: "var(--accent)",
  },
  ".cm-cursor": { borderLeftColor: "var(--accent)", borderLeftWidth: "1.5px" },
  "&.cm-focused": { outline: "none" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": {
    backgroundColor: "color-mix(in srgb, var(--accent) 25%, transparent)",
  },
  ".cm-gutters": { display: "none" },
});

// ===== 实时预览装饰 StateField =====

/** 实时预览装饰：文档/选区/只读切换变化时全量重建（笔记规模下开销可忽略）。
 * 用 StateField + EditorView.decorations.from 而非 ViewPlugin：block 装饰（表格/公式/横隔条）
 * 只能经 standard decorations 提供，ViewPlugin 提供会抛 RangeError。 */
function livePreview(
  buildOpts: (readOnly: boolean) => DecorationOptions,
  dispatchRef: { current: (spec: TransactionSpec) => void },
  roField: StateField<boolean>,
): Extension {
  return StateField.define<DecorationSet>({
    create(state) {
      return buildDecorations(
        state,
        buildOpts(state.field(roField)),
        (spec) => dispatchRef.current(spec),
      );
    },
    update(value, tr) {
      if (
        tr.docChanged ||
        tr.selection !== undefined ||
        tr.effects.some((e) => e.is(readOnlyEffect))
      ) {
        return buildDecorations(
          tr.state,
          buildOpts(tr.state.field(roField)),
          (spec) => dispatchRef.current(spec),
        );
      }
      return value;
    },
    provide: (f) => EditorView.decorations.from(f),
  });
}

// ===== 组件 =====

/** 链接/定位回调（与 useVaultLinkHandlers + 画布 useWikiNodeLocate 对齐）。 */
export interface MarkdownEditorLinks {
  isVaultPathNote?: (href: string) => boolean;
  onOpenVaultPathNote?: (href: string) => void;
  onCreateNote?: (name: string) => void;
  onOpenNote?: (name: string) => void;
  isLocatable?: (value: string) => boolean;
  onLocate?: (value: string) => void;
}

interface Props {
  /** 当前正文（挂载时初始注入；外部同步时 replaceAll 的目标）。 */
  body: string;
  /** 非用户编辑的 content 更新序号（外部修改/冲突重载/加载完成时 NoteEditor 递增），变化即同步编辑器。 */
  syncSeq: number;
  /** 用户编辑回调：输出编辑器当前全文 markdown 正文。只读态仍可传（勾选框 toggle 上报走保存链）。 */
  onBodyChange?: (markdown: string) => void;
  /** 协作绑定：提供时进入 Yjs 协同编辑（y-codemirror 绑 Y.Text + 远端光标）；
   *  缺省 = 本地单写者纯文本编辑。撤销键不在本组件绑定（见 NoteEditor 窗口级路由）。 */
  collab?: { ytext: YText; awareness: Awareness };
  /** 编辑器实例外抛（NoteEditor 划词右键剪切/粘贴按选区 dispatch 用）；创建后赋值、卸载置 null。 */
  editorViewRef?: { current: EditorView | null };
  /** 协作挂载时 ytext 与 body 分歧的处置（NoteEditor 注入；参数 = ytext 正文 LF）。 */
  onCollabDivergence?: (ytextText: string) => void;
  /** 只读（默认实时视图/画布/对话展示面）：停用光标行显示原文规则，widget 恒渲染；可动态切换不重建。 */
  readOnly?: boolean;
  /** 任务勾选框可点（笔记可点写回；画布/对话只读展示面禁用态）。 */
  interactiveCheckbox?: boolean;
  /** 链接/定位回调（wiki/仓库路径/空链接新建/画布定位）。 */
  links?: MarkdownEditorLinks;
  /** @引用 胶囊（用户消息 displayContent 内 `@label` → 胶囊，点击定位/打开）。 */
  mentions?: { key: string; label: string }[];
  onMentionClick?: (key: string, label: string) => void;
  /** 宿主容器 class（笔记传 h-full；只读展示面随内容高度）。 */
  className?: string;
}

export function MarkdownEditor({
  body,
  syncSeq,
  onBodyChange,
  collab,
  editorViewRef,
  onCollabDivergence,
  readOnly = false,
  interactiveCheckbox = true,
  links,
  mentions,
  onMentionClick,
  className = "h-full",
}: Props) {
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  /** 装饰 widget 的 dispatch 转发：StateField 闭包捕获 ref 而非函数，view 创建后赋值才有效。 */
  const dispatchRef = useRef<(spec: TransactionSpec) => void>(() => {});
  /** 程序化注入的回放抑制：applyBody 的 dispatch 同步触发 updateListener，写入期间置位吞掉回放。 */
  const suppressRef = useRef(false);
  /** 上次注入内容：卸载 flush 时内容 === 注入目标则不重复上报。 */
  const lastAppliedRef = useRef("");
  const syncSeqRef = useRef(syncSeq);
  /** 回调/body/选项经 ref 转发：create 闭包在挂载时构建一次，捕获不到后续渲染的最新值。 */
  const onBodyChangeRef = useRef(onBodyChange);
  onBodyChangeRef.current = onBodyChange;
  const onCollabDivergenceRef = useRef(onCollabDivergence);
  onCollabDivergenceRef.current = onCollabDivergence;
  const bodyRef = useRef(body);
  bodyRef.current = body;
  const linksRef = useRef(links);
  linksRef.current = links;
  const interactiveRef = useRef(interactiveCheckbox);
  interactiveRef.current = interactiveCheckbox;
  const mentionsRef = useRef(mentions);
  mentionsRef.current = mentions;
  const onMentionClickRef = useRef(onMentionClick);
  onMentionClickRef.current = onMentionClick;
  /** 只读当前应用值（初始 = 挂载 prop；后续经 effect 同步到视图）。 */
  const readOnlyAppliedRef = useRef(readOnly);

  /** 程序化写入：记录注入目标 + 抑制回放，再全量替换（CRLF 注入前规范化为 LF）。 */
  const applyBody = useCallback((md: string) => {
    const view = viewRef.current;
    if (!view) return;
    const normalized = md.replace(/\r\n/g, "\n");
    lastAppliedRef.current = normalized;
    suppressRef.current = true;
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: normalized },
      // 外部同步不进撤销栈：Ctrl+Z 不应回滚到注入前的内容（与用户编辑隔离）
      annotations: [Transaction.addToHistory.of(false)],
    });
    suppressRef.current = false;
  }, []);

  // 创建编辑器（`new EditorView` 同步完成，无需 StrictMode 异步守卫；卸载即 destroy）
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    /** 只读状态字段：提供 EditorState.readOnly 拦截用户输入；装饰层经它同步 readOnly 渲染模式。
     * 初始值取挂载时 readOnly（笔记打开默认只读、MarkdownView 恒只读），后续由 effect 翻转。 */
    const roField = StateField.define<boolean>({
      create: () => readOnlyAppliedRef.current,
      update(value, tr) {
        for (const e of tr.effects) if (e.is(readOnlyEffect)) return e.value;
        return value;
      },
      provide: (f) => EditorState.readOnly.from(f),
    });
    /** 装饰选项工厂：每次重建装饰时取最新 ref（vaultRoot/回调经 store 实时读，防闭包陈旧）。 */
    const buildOpts = (ro: boolean): DecorationOptions => ({
      vaultRoot: useAppStore.getState().vaultRoot,
      readOnly: ro,
      interactiveCheckbox: interactiveRef.current,
      onOpenUrl: (url) => void useAppStore.getState().openUrl(url),
      onOpenPath: (path) => void useAppStore.getState().openInExplorer(path),
      readImage: async (src) => {
        try {
          return await useVaultStore.getState().readAttachmentDataUrl(src);
        } catch {
          return null;
        }
      },
      ...linksRef.current,
      mentions: mentionsRef.current,
      onMentionClick: onMentionClickRef.current,
      focusEditor: () => viewRef.current?.focus(),
    });
    const view = new EditorView({
      parent: host,
      state: EditorState.create({
        // 协作模式以收敛态 ytext 为编辑模型源（body 可能因预览期远端改动而滞后），
        // 非协作退回原逻辑用 body 初始化。
        doc: collab
          ? collab.ytext.toString()
          : bodyRef.current.replace(/\r\n/g, "\n"),
        extensions: [
          // addKeymap: false——Enter/Backspace 绑定统一由 keymap.of 组合提供；
          // base = markdownLanguage（GFM：表格/任务/删除线/Autolink 全量解锁）
          markdown({
            codeLanguages: languages,
            addKeymap: false,
            base: markdownLanguage,
          }),
          EditorView.lineWrapping,
          // keymap 协作/非协作一致（协作只多 yCollab 绑定，行为不因协作开关分叉）：
          // 撤销/重做不在 CM 内绑定——统一由 NoteEditor 窗口级路由接按文件持久栈；
          // CM 内置 history 与 y-undo 均随 EditorView 销毁丢失（预览↔编辑/重挂载即清），
          // 持久栈跨重挂载保留，也不与窗口路由双撤销
          keymap.of([...markdownKeymap, ...defaultKeymap]),
          ...(collab ? [yCollab(collab.ytext, collab.awareness)] : []),
          syntaxHighlighting(highlightStyle),
          editorTheme,
          roField,
          readOnlyClass,
          livePreview(buildOpts, dispatchRef, roField),
          EditorView.updateListener.of((update) => {
            if (update.docChanged && !suppressRef.current && onBodyChangeRef.current) {
              onBodyChangeRef.current(update.state.doc.toString());
            }
          }),
        ],
      }),
    });
    viewRef.current = view;
    dispatchRef.current = (spec) => view.dispatch(spec);
    if (editorViewRef) editorViewRef.current = view;
    if (collab) {
      // 协作：ytext 已作编辑模型源，不再用 body 覆盖。若 ytext 与 body 相悖
      // （预览期远端已改写本端未感知 / 本地有未落盘编辑）→ 交父级处置：干净收敛
      // content 到 ytext、有未落盘编辑则本地正文写回 ytext（防陈旧基线回退本地输入）。
      if (collab.ytext.toString() !== bodyRef.current.replace(/\r\n/g, "\n")) {
        onCollabDivergenceRef.current?.(collab.ytext.toString());
      }
    } else {
      // 非协作：初始注入挂载时的 body（原行为）
      applyBody(bodyRef.current);
    }
    return () => {
      // 卸载 flush：更新监听同步触发、正常编辑已实时上报，此处兜底取最新 doc
      // （复用回放抑制：内容 === 上次注入目标则不报，防注入回放误上报）
      const latest = view.state.doc.toString();
      if (latest !== lastAppliedRef.current) {
        onBodyChangeRef.current?.(latest);
      }
      viewRef.current = null;
      dispatchRef.current = () => {};
      if (editorViewRef) editorViewRef.current = null;
      view.destroy();
    };
    // collab 切换（重建视图）依赖其引用；applyBody 为稳定回调；editorViewRef 为父组件 useRef（引用稳定，
    // 仅为 exhaustive-deps 合规列入，不会触发重建）
  }, [applyBody, collab, editorViewRef]);

  // 只读切换：同一视图翻转（不重建 → 选区/滚动/协作绑定保留），装饰经 readOnlyEffect 同步
  useEffect(() => {
    if (readOnly === readOnlyAppliedRef.current) return;
    readOnlyAppliedRef.current = readOnly;
    const view = viewRef.current;
    if (!view) return;
    view.dispatch({ effects: readOnlyEffect.of(readOnly) });
  }, [readOnly]);

  // 外部同步：非用户编辑的 content 更新（watcher 外部修改 / 冲突重载 / 加载完成）。
  // 协作模式下不禁用：远端合入经 ytext 进视图（updateListener 上报），此处仅处理非协作场景。
  useEffect(() => {
    if (syncSeq === 0 || syncSeq === syncSeqRef.current) return;
    syncSeqRef.current = syncSeq;
    applyBody(bodyRef.current);
  }, [syncSeq, applyBody]);

  return <div ref={hostRef} className={className} data-markdown-editor />;
}

// ===== 只读展示面封装（画布文本节点 / 对话气泡 / AI 面板）=====

interface MarkdownViewProps {
  /** 静态正文（更新走 applyBody，不回调）。 */
  text: string;
  /** 链接/定位回调（与 MarkdownEditor 同构）。 */
  links?: MarkdownEditorLinks;
  /** 任务勾选框禁用态展示（缺省 true = 画布/对话只读面）。 */
  interactiveCheckbox?: boolean;
  /** @引用 胶囊（用户消息 `@label` → 胶囊）。 */
  mentions?: { key: string; label: string }[];
  onMentionClick?: (key: string, label: string) => void;
  /** 宿主容器 class（缺省随内容高度）。 */
  className?: string;
}

/** 只读 Markdown 渲染（与实时预览编辑同一引擎，渲染完全一致）。
 * 宽松换行关闭（softLineBreak=false）时，只读展示面折叠段内单换行为空格（见 utils/softLineBreak）。 */
export function MarkdownView({
  text,
  links,
  interactiveCheckbox = false,
  mentions,
  onMentionClick,
  className,
}: MarkdownViewProps) {
  const softLineBreak = useSettingsStore((s) => s.vaultConfig?.softLineBreak ?? true);
  const displayText = softLineBreak ? text : collapseSoftLineBreaks(text);
  const [syncSeq, setSyncSeq] = useState(0);
  const lastTextRef = useRef(displayText);
  useEffect(() => {
    if (displayText !== lastTextRef.current) {
      lastTextRef.current = displayText;
      setSyncSeq((s) => s + 1);
    }
  }, [displayText]);
  return (
    <MarkdownEditor
      body={displayText}
      syncSeq={syncSeq}
      readOnly
      interactiveCheckbox={interactiveCheckbox}
      links={links}
      mentions={mentions}
      onMentionClick={onMentionClick}
      className={className ?? ""}
    />
  );
}
