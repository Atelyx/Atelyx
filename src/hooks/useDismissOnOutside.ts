/**
 * 弹层菜单关闭交互：Esc 关闭 + 点击菜单外关闭（pointerdown 监听——树行/表行的
 * pointerdown 会 preventDefault 抑制 mousedown 派发，用 pointerdown 才能可靠捕获）。
 * menuRef 由调用方持有并挂到菜单容器（容器内元素需自行 stopPropagation，防点按钮被抢先关闭）。
 *
 * `excludeRef` 可选：外点判定额外排除的区域（触发器 trigger）——点自身 trigger 不关，
 * 开/关 toggle 语义归 trigger 的 click 处理（PopupLayer 的 triggerRef 即此用途）。
 *
 * `opts.escape` = false 时只挂外点监听、不监听 Esc：Esc 归调用方自己的分层逻辑管
 * （如浮层宿主「只关最上层」——本 hook 是 window 级监听、无栈语义，多层各自挂载时
 * 一按 Esc 会全部触发）。缺省 true，既有调用方行为不变。
 *
 * 挂载期间监听、卸载清理；onClose 用 ref 存最新回调，监听不随每次渲染重绑。
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
