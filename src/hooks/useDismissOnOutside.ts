/**
 * 弹层菜单关闭交互：Esc 关闭 + 点击菜单外关闭（pointerdown 监听——树行/表行的
 * pointerdown 会 preventDefault 抑制 mousedown 派发，用 pointerdown 才能可靠捕获）。
 * menuRef 由调用方持有挂到菜单容器（容器内元素自行 stopPropagation，防点按钮被抢先关闭）；
 * `excludeRef` = 外点判定额外排除的触发器：点自身不关，toggle 语义归 trigger 的 click 处理。
 * `opts.escape` = false 时只挂外点监听：Esc 归调用方分层逻辑（本 hook 是 window 级监听、
 * 无栈语义，多层各自挂载时一按 Esc 会全部触发）。
 */
import { useEffect, useRef, type RefObject } from "react";

export function useDismissOnOutside(
  onClose: () => void,
  menuRef: RefObject<HTMLDivElement>,
  excludeRef?: RefObject<HTMLElement | null>,
  opts?: { escape?: boolean },
): void {
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  const escapeRef = useRef(opts?.escape ?? true);
  escapeRef.current = opts?.escape ?? true;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && escapeRef.current) closeRef.current();
    };
    const onDown = (e: PointerEvent) => {
      const target = e.target as Node;
      if (menuRef.current?.contains(target)) return;
      if (excludeRef?.current?.contains(target)) return;
      // 弹层未渲染（ref 为空）时无物可关，不触发 onClose（PopupLayer 常驻挂载但 anchor 为空时点任意处）
      if (!menuRef.current) return;
      closeRef.current();
    };
    window.addEventListener("keydown", onKey);
    document.addEventListener("pointerdown", onDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      document.removeEventListener("pointerdown", onDown);
    };
    // ref 为稳定引用（useRef），加入依赖仅为消除 exhaustive-deps，不会重挂监听
  }, [menuRef, excludeRef]);
}
