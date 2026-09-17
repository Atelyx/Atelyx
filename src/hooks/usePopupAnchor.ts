/**
 * 触发器锚定弹层状态（PopupLayer 配套）：点击 trigger 时按其实测视口 rect 计算弹层锚点，
 * 再次点击关闭。`align="bottom"` 用于固定向上弹出的触发器（底边贴触发器顶边），
 * 缺省 = 向下弹出 + 下方空间不足向上翻转（flipY = 触发器顶边）。
 *
 * 供 DropdownSelect / 历史会话按钮等
 * 所有「锚定按钮弹出」的浮层共用，消除各自手写 anchor 状态的重复。
 */
import { useCallback, useState } from "react";
import type { RefObject } from "react";
import type { PopupAnchor } from "@/components/common/PopupLayer";

export function usePopupAnchor(
  triggerRef: RefObject<HTMLElement | null>,
  opts?: { align?: "top" | "bottom" },
) {
  const [anchor, setAnchor] = useState<PopupAnchor | null>(null);

  const toggle = useCallback(() => {
    if (anchor) {
      setAnchor(null);
      return;
    }
    const r = triggerRef.current?.getBoundingClientRect();
    if (!r) return;
    setAnchor(
      opts?.align === "bottom"
        ? { x: r.left, y: r.top - 4, minWidth: r.width }
        : { x: r.left, y: r.bottom + 2, minWidth: r.width, flipY: r.top - 2 },
    );
  }, [anchor, triggerRef, opts?.align]);

  const close = useCallback(() => setAnchor(null), []);

  return { anchor, toggle, close };
}
