/**
 * #提及插入骨架（画布对话节点与 AI 面板共用）：替换「# 到光标」区间为提及标签、
 * 登记引用、光标复位到尾随空格之后、收起 picker。
 */
import type { Dispatch, RefObject, SetStateAction } from "react";
import { insertMentionTag } from "@/utils/text";

export function useMentionInsert(opts: {
  /** 输入框元素（光标读取与复位目标）。 */
  textareaRef: RefObject<HTMLTextAreaElement | null>;
  /** 当前输入文本（仅用于 caret 缺省值；插入位置在 setInput(prev) 内按 prev 计算——
   *  渲染期闭包的 input 已含上一次入队结果，两次插入同 tick 到达会互相覆盖）。 */
  input: string;
  /** # 触发时光标位置（# 尚未插入；替换区间起点，负值按 0 处理）。 */
  atIdx: number;
  setInput: Dispatch<SetStateAction<string>>;
  /** 插入后收起：setPicker(null) + setAtIdx(-1)。 */
  closePicker: () => void;
}) {
  const { textareaRef, input, atIdx, setInput, closePicker } = opts;
  return (mentionText: string, record?: () => void) => {
    const caret = textareaRef.current?.selectionStart ?? input.length;
    const insertAt = Math.min(Math.max(atIdx, 0), input.length);
    const end = Math.max(caret, insertAt);
    let caretAfter = 0;
    setInput((prev) => {
      const { text, caret: next } = insertMentionTag(prev, insertAt, end, mentionText);
      caretAfter = next;
      return text;
    });
    record?.();
    // 光标移到尾随空格之后（继续输入不紧贴胶囊）
    requestAnimationFrame(() => {
      const ta = textareaRef.current;
      if (ta) {
        ta.focus();
        ta.setSelectionRange(caretAfter, caretAfter);
      }
    });
    closePicker();
  };
}
