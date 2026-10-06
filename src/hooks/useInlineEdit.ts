/**
 * 双击 inline 编辑（标题/label 等）：editing + draft + 提交/取消 + Escape 拦截 blur 误提交。
 * 收敛节点标题（对话/文本/分组）等处的重复模式；提交/取消经 inputProps 内部接线，不外露。
 * inputProps 直接展开到 <input>（start 后需聚焦时调用方自传 autoFocus）。
 */
import { useCallback, useState, useRef, type ChangeEvent, type KeyboardEvent } from "react";

export function useInlineEdit({
  value,
  onCommit,
  onCancel,
}: {
  /** 当前值（start 时 draft 的初始值）。 */
  value: string;
  /** 提交回调（draft 原样传入，trim/空值策略由调用方决定）。 */
  onCommit: (draft: string) => void;
  /** 取消回调（Escape 时触发，调用方清理外部编辑态）。 */
  onCancel?: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  // Escape 取消标记：拦截 input 卸载触发的 blur 误提交（提交/取消同走 commit 路径，flag 先行）
  const cancelRef = useRef(false);

  const start = useCallback(() => {
    cancelRef.current = false;
    setDraft(value);
    setEditing(true);
  }, [value]);

  const commit = useCallback(() => {
    if (cancelRef.current) {
      cancelRef.current = false;
      return;
    }
    setEditing(false);
    onCommit(draft);
  }, [onCommit, draft]);

  const cancel = useCallback(() => {
    cancelRef.current = true;
    setEditing(false);
    onCancel?.();
  }, [onCancel]);

  const inputProps = {
    value: draft,
    onChange: (e: ChangeEvent<HTMLInputElement>) => setDraft(e.target.value),
    onBlur: commit,
    onKeyDown: (e: KeyboardEvent<HTMLInputElement>) => {
      if (e.key === "Enter") commit();
      else if (e.key === "Escape") cancel();
    },
  };

  return { editing, start, inputProps };
}
