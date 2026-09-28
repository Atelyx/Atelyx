/**
 * 把组件存在期间（`active` 为真）的返回键处理登记到返回栈（见 `utils/mobileBack`）。
 * 处理器以 ref 保存，登记不随每次渲染重建；`active` 变化时挂载/注销。
 */
import { useEffect, useRef } from "react";
import { registerBackHandler } from "@/utils/mobileBack";

export function useBackHandler(active: boolean, handler: () => boolean): void {
  const ref = useRef(handler);
  ref.current = handler;
  useEffect(() => {
    if (!active) return;
    return registerBackHandler(() => ref.current());
  }, [active]);
}
