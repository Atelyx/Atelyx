/**
 * @提及 输入框（画布对话节点 / AI 对话面板共用）：透明 textarea 承载输入（文本即真相），
 * overlay 渲染 @引用标签作视觉装饰层，滚动以 transform 同步。
 *
 * 胶囊整体化交互（点击选中、整删、方向键跳边界）见 handleKeyDown 内各分支；
 * 对齐前提见 INPUT_FONT 与 .mention-capsule 锚点。差异由 props 表达：背景层/class/占位/其他键处理。
 */
import {
  useCallback,
  useEffect,
  useRef,
  type CSSProperties,
  type KeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import type { MentionSeg } from "@/utils/text";

/**
 * overlay 与 textarea 严格一致的字体（CSS 未给 textarea 设 font，UA 默认不同会导致标签错位）。
 *
 * 字号/行高走字阶变量：应用级「字体大小」设置改的是根字号，故两者必须同为 rem 量纲——
 * 若字号写死 px 而行高用 rem，缩放后行高不跟随，overlay 标签会与光标错行。
 * 三个消费点（overlay、textarea、测量 probe）同源于此，改动须一并。
 */
export const INPUT_FONT: CSSProperties = {
  fontFamily: "var(--font-sans)",
  fontSize: "var(--fs-body)",
  lineHeight: "1.4rem",
};

/** 测量 probe 用 cssText：与 INPUT_FONT 同源，但需字面量拼接而非对象展开。 */
const INPUT_FONT_CSS =
  "font-family:var(--font-sans);font-size:var(--fs-body);line-height:1.4rem;";

/**
 * 光标行高度测量 probe（模块级单例）：与内容层同排版（INPUT_FONT + pre-wrap + break-words），
 * 渲染光标前文本测得折行高度——计算「光标行底对齐 content 区底」的目标 scrollTop。
 */
const measureProbe = (() => {
  if (typeof document === "undefined") return null;
  const el = document.createElement("div");
  el.style.cssText =
    "position:absolute;visibility:hidden;pointer-events:none;left:-9999px;top:0;" +
    "white-space:pre-wrap;overflow-wrap:break-word;" +
    INPUT_FONT_CSS;
  document.body.appendChild(el);
  return el;
})();
const probeRef = { current: measureProbe };

interface MentionTextareaProps {
  value: string;
  onChange: (value: string) => void;
  /** 输入框分段（@提及 → 标签段，其余普通文本段）。 */
  segments: MentionSeg[];
  /** 胶囊被移除（退格/Del/输入替换）→ 引用层清理（断边/移出 mentions/托盘），文本删除由本组件负责。 */
  onRemoveMention: (seg: MentionSeg) => void;
  placeholder: string;
  rows?: number;
  /** 调用方持有的 textarea 引用（光标定位/焦点恢复用）。 */
  textareaRef?: RefObject<HTMLTextAreaElement>;
  /** 其他按键处理（画布 = @picker 打开坐标计算 + Enter 发送；面板 = Enter 发送）。 */
  onKeyDown?: (e: KeyboardEvent<HTMLTextAreaElement>) => void;
  onPaste?: (e: React.ClipboardEvent<HTMLTextAreaElement>) => void;
  /** 容器额外 class（画布 = flex-1 min-w-0 overflow-hidden；面板空）。 */
  containerClassName?: string;
  /** overlay 额外 class（含 padding/字体大小，与 textarea 对齐）。 */
  overlayClassName?: string;
  /** textarea 额外 class（含 padding/字体大小/焦点样式）。 */
  textareaClassName?: string;
  /** 输入区底色层（画布 = input-bg 背景 div；面板自带背景）。 */
  backgroundLayer?: ReactNode;
  spellCheck?: boolean;
}

/** @提及 输入框：透明 textarea + @标签 overlay（滚动同步 + 胶囊整体化交互内置）。 */
export function MentionTextarea({
  value,
  onChange,
  segments,
  onRemoveMention,
  placeholder,
  rows,
  textareaRef,
  onKeyDown,
  onPaste,
  containerClassName,
  overlayClassName,
  textareaClassName,
  backgroundLayer,
  spellCheck,
}: MentionTextareaProps) {
  const clipRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);

  // 三层同步（直接改 DOM 不 setState，无重渲染循环）：
  // 1. 裁剪层 inset = textarea 的 padding 实测——盒子恰好 = content 区：文本可见区止于
  //    content 区底部（工具条上方），超出消隐（textarea 的文本超 content 区会渲染到 padding
  //    区 = 工具条位置，视口裁剪整个盒子拦不住它）
  // 2. 内容层宽度 = textarea 实际文本区宽（clientWidth 已排除边框与滚动条，再减 padding）——
  //    折行一致才能滚动精确对齐
  // 3. 内容层 transform = -scrollTop
  // 触发时机：value 变化（受控更新/滚动条出现都不发 scroll/ResizeObserver 事件，实测 RO
  // 不响应滚动条占宽）→ 每渲染强制对齐；onScroll（用户滚动实时）；ResizeObserver（尺寸变化）
  const sync = useCallback(() => {
    const ta = textareaRef?.current;
    const clip = clipRef.current;
    const content = contentRef.current;
    if (!ta || !clip || !content) return;
    const style = getComputedStyle(ta);
    clip.style.top = style.paddingTop;
    clip.style.bottom = style.paddingBottom;
    clip.style.left = style.paddingLeft;
    clip.style.right = style.paddingRight;
    const padL = parseFloat(style.paddingLeft) || 0;
    const padR = parseFloat(style.paddingRight) || 0;
    content.style.width = `${ta.clientWidth - padL - padR}px`;
    content.style.transform = `translateY(${-ta.scrollTop}px)`;
  }, [textareaRef]);

  useEffect(() => {
    sync();
    const ta = textareaRef?.current;
    if (!ta) return;
    const ro = new ResizeObserver(sync);
    ro.observe(ta);
    return () => ro.disconnect();
  }, [sync, textareaRef]);

  useEffect(() => {
    sync();
  }, [value, sync]);

  // 光标跟随（以工具条上方为准）：textarea 原生自动滚动把光标行底对齐「可视区底部」
  // （= padding 区 = 工具条后面，被工具条盖住看不见）。改为精确滚动：光标行底对齐
  // content 区底部（工具条上方）——目标 scrollTop = 光标前文本渲染高度 - content 区高，
  // 用与内容层同排版的隐藏 probe 元素测量光标前文本高度（折行一致）。
  useEffect(() => {
    const ta = textareaRef?.current;
    const content = contentRef.current;
    const probe = probeRef.current;
    if (!ta || !content || !probe) return;
    // 内容未超一屏：光标必在 content 区内，无需滚动
    if (ta.scrollHeight <= ta.clientHeight) return;
    const pos = ta.selectionStart ?? ta.value.length;
    const style = getComputedStyle(ta);
    const width = content.offsetWidth;
    if (probe.style.width !== `${width}px`) probe.style.width = `${width}px`;
    probe.textContent = value.slice(0, pos);
    const prefixH = probe.offsetHeight;
    const padTop = parseFloat(style.paddingTop) || 0;
    const padBottom = parseFloat(style.paddingBottom) || 0;
    const contentH = ta.clientHeight - padTop - padBottom;
    const target = Math.max(0, prefixH - contentH);
    if (Math.abs(ta.scrollTop - target) > 0.5) {
      ta.scrollTop = target;
      content.style.transform = `translateY(${-target}px)`;
    }
  }, [value, textareaRef]);

  /** 整删胶囊 + 引用层清理；光标复位到删除点。
   * seg 来自 splitMentions（已吞两侧装饰空格），直接按段范围删除。
   * 引用清理在此显式按段身份调用（精确，不依赖 onChange 文本检测——同名相邻段会误判）。 */
  const removeMentionAt = (seg: MentionSeg) => {
    onChange(value.slice(0, seg.start) + value.slice(seg.start + seg.text.length));
    onRemoveMention(seg);
    requestAnimationFrame(() => {
      const ta = textareaRef?.current;
      if (ta) ta.setSelectionRange(seg.start, seg.start);
    });
  };

  return (
    <div className={`relative ${containerClassName ?? ""}`}>
      {backgroundLayer}
      {/* 渲染层：@提及 标签（强调色系）+ 普通文本。
          视口层固定不动（裁剪窗口），滚动位移只作用于内容层——
          transform 挂视口会把「文本开头」整体推出输入框（overflow 裁剪在 transform 前计算）。
          定位/裁剪全部 inline 硬约束：不依赖 Tailwind 类（absolute/inset-0/overflow-hidden 任一生成失败
          都会让视口被内容撑开——文本溢出到输入框外）。 */}
      <div
        aria-hidden
        className={`pointer-events-none ${overlayClassName ?? ""}`}
        style={{
          ...INPUT_FONT,
          color: "var(--text-primary)",
          position: "absolute",
          top: 0,
          right: 0,
          bottom: 0,
          left: 0,
          overflow: "hidden",
        }}
      >
        {/* 裁剪层：盒子 = content 区（inset = textarea padding 实测）——文本可见区止于
            content 区底部（工具条上方），超出消隐；transform 滚动只作用于内层内容 */}
        <div
          ref={clipRef}
          style={{
            position: "absolute",
            overflow: "hidden",
          }}
        >
          {/* 内容层：transform 模拟 textarea 滚动；宽度实测对齐 textarea 文本区
              （折行一致才能滚动精确对齐，不猜滚动条/边框宽度） */}
          <div ref={contentRef} className="whitespace-pre-wrap break-words">
            {segments.map((s, i) =>
              // span 本体须保持纯文本宽（padding/宽度变化会让其后文本与 textarea 光标错位）；
              // 视觉胶囊由 .mention-capsule 的背景 + box-shadow 外扩绘制（不占布局，跨行段渲染正确）
              s.mention ? (
                <span key={i} className="mention-capsule">
                  {s.text}
                </span>
              ) : (
                <span key={i}>{s.text}</span>
              )
            )}
          </div>
        </div>
      </div>
      <textarea
        ref={textareaRef}
        value={value}
        onChange={(e) => {
          const newValue = e.target.value;
          // 原生输入路径（选中替换/拖选删除）导致胶囊文本消失 → 引用层兜底清理。
          // 注意：removeMentionAt 走 props onChange 不经此处，其引用清理已显式精确完成
          for (const s of segments) {
            if (s.mention && value.includes(s.text) && !newValue.includes(s.text)) {
              onRemoveMention(s);
            }
          }
          onChange(newValue);
        }}
        onKeyDown={(e) => {
          // IME 组合期间键是「上屏候选词」操作，不拦截
          if (e.nativeEvent.isComposing) {
            onKeyDown?.(e);
            return;
          }
          const ta = textareaRef?.current;
          const caret = ta?.selectionStart ?? 0;
          const selEnd = ta?.selectionEnd ?? caret;
          const selected = selEnd > caret;
          // 与光标/选中区相关的胶囊（选中态 = 相交；光标态 = 在胶囊内/边界）
          const seg = segments.find((s) => {
            if (!s.mention) return false;
            const segEnd = s.start + s.text.length;
            if (selected) return caret < segEnd && selEnd > s.start;
            return caret >= s.start && caret <= segEnd;
          });
          if (seg) {
            const segEnd = seg.start + seg.text.length;
            if (e.key === "Backspace" || e.key === "Delete") {
              if (selected) {
                // 选区恰等于胶囊范围 → 整删胶囊；跨胶囊/含其他文本的选区 → 放行原生删除整个选区
                // （引用清理由 onChange 兜底检测完成）
                if (caret === seg.start && selEnd === segEnd) {
                  e.preventDefault();
                  removeMentionAt(seg);
                  return;
                }
              } else if (
                (e.key === "Backspace" && caret === segEnd) ||
                (e.key === "Delete" && caret === seg.start)
              ) {
                e.preventDefault();
                removeMentionAt(seg);
                return;
              }
            } else if (e.key === "ArrowLeft") {
              // 光标在胶囊内/末尾 → 整体跳到开头；在开头不拦截（正常移出）
              if (caret > seg.start && caret <= segEnd) {
                e.preventDefault();
                ta?.setSelectionRange(seg.start, seg.start);
                return;
              }
            } else if (e.key === "ArrowRight") {
              // 光标在胶囊内/开头 → 整体跳到末尾；在末尾不拦截（正常移出）
              if (caret >= seg.start && caret < segEnd) {
                e.preventDefault();
                ta?.setSelectionRange(segEnd, segEnd);
                return;
              }
            }
          }
          onKeyDown?.(e);
        }}
        onMouseUp={() => {
          // 点击胶囊 → 原生选中胶囊全文（整体操作：退格/Del 删除、输入替换）
          const ta = textareaRef?.current;
          if (!ta) return;
          const caret = ta.selectionStart;
          if ((ta.selectionEnd ?? caret) > caret) return; // 拖选不覆盖
          const seg = segments.find(
            (s) => s.mention && caret >= s.start && caret < s.start + s.text.length
          );
          if (seg) ta.setSelectionRange(seg.start, seg.start + seg.text.length);
        }}
        onScroll={(e) => {
          // 内容层与 textarea 滚动同步（直接改 DOM，避免滚动触发重渲染）
          if (contentRef.current) {
            contentRef.current.style.transform = `translateY(${-e.currentTarget.scrollTop}px)`;
          }
        }}
        onPaste={onPaste}
        spellCheck={spellCheck}
        placeholder={placeholder}
        rows={rows}
        className={`resize-none outline-none mention-input ${textareaClassName ?? ""}`}
        style={{
          ...INPUT_FONT,
          background: "transparent",
          color: "transparent",
          caretColor: "var(--text-primary)",
        }}
      />
    </div>
  );
}
