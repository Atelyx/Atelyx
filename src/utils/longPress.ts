/**
 * 触屏长按 → 合成 contextmenu：复用全项目既有右键菜单，所有 `onContextMenu` 处理器无需改动。
 *
 * 编辑器与可编辑区保留系统长按（选词、粘贴菜单），不合成。长按触发后短暂抑制紧随的 click，
 * 避免「开了菜单又触发了行点击」。仅在移动端壳挂载时安装，桌面不受影响。
 */
const LONG_PRESS_MS = 500;
/** 移动超过该距离视为滚动/拖动，取消长按。 */
const MOVE_TOLERANCE_PX = 10;
/** 长按触发后抑制 click 的时长（覆盖随后的 tap 抬起）。 */
const CLICK_SUPPRESS_MS = 800;
/** 系统长按已弹出右键菜单的判定窗口（WebView 自带 contextmenu 时不再合成）。 */
const NATIVE_GUARD_MS = 1000;
/** 不合成右键菜单的区域：可编辑区保留系统长按（选词/粘贴）；可拖拽的文件树行用长按起拖（见 useVaultDrag）。
 *  树容器（data-dir=""，仅作落点）不在其列，长按仍唤出空白区菜单。 */
const SKIP_SELECTOR = "input, textarea, [contenteditable='true'], [data-file], [data-dir]:not([data-dir=''])";

export function installLongPressContextMenu(): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let target: HTMLElement | null = null;
  let startX = 0;
  let startY = 0;
  let suppressUntil = 0;
  let suppressTarget: HTMLElement | null = null;
  let lastNativeContextMenuAt = 0;

  const reset = (): void => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
    target = null;
  };

  const onPointerDown = (e: PointerEvent): void => {
    if (e.pointerType !== "touch") return;
    const el = e.target as HTMLElement | null;
    if (!el || el.closest(SKIP_SELECTOR)) return;
    reset();
    target = el;
    startX = e.clientX;
    startY = e.clientY;
    timer = setTimeout(() => {
      timer = undefined;
      const pressed = target;
      target = null;
      if (!pressed || !pressed.isConnected) return;
      if (Date.now() - lastNativeContextMenuAt < NATIVE_GUARD_MS) return;
      suppressUntil = Date.now() + CLICK_SUPPRESS_MS;
      suppressTarget = pressed;
      pressed.dispatchEvent(
        new MouseEvent("contextmenu", {
          bubbles: true,
          cancelable: true,
          clientX: startX,
          clientY: startY,
          button: 2,
        }),
      );
    }, LONG_PRESS_MS);
  };

  const onPointerMove = (e: PointerEvent): void => {
    if (timer === undefined) return;
    if (Math.abs(e.clientX - startX) > MOVE_TOLERANCE_PX || Math.abs(e.clientY - startY) > MOVE_TOLERANCE_PX) {
      reset();
    }
  };

  // 只吞掉长按目标上紧随抬起的 click（防同时触发行点击）；期间用户点菜单项等其它目标不受影响
  const onClickCapture = (e: MouseEvent): void => {
    if (Date.now() >= suppressUntil) return;
    const el = e.target as HTMLElement | null;
    if (!el || !suppressTarget || !(suppressTarget === el || suppressTarget.contains(el))) return;
    suppressUntil = 0;
    suppressTarget = null;
    e.stopPropagation();
    e.preventDefault();
  };

  // 只记录系统真实事件（合成事件 isTrusted = false），用于避让 WebView 自带长按菜单
  const onContextMenu = (e: MouseEvent): void => {
    if (e.isTrusted) lastNativeContextMenuAt = Date.now();
  };

  window.addEventListener("pointerdown", onPointerDown, true);
  window.addEventListener("pointermove", onPointerMove, true);
  window.addEventListener("pointerup", reset, true);
  window.addEventListener("pointercancel", reset, true);
  window.addEventListener("click", onClickCapture, true);
  window.addEventListener("contextmenu", onContextMenu, true);

  return () => {
    reset();
    window.removeEventListener("pointerdown", onPointerDown, true);
    window.removeEventListener("pointermove", onPointerMove, true);
    window.removeEventListener("pointerup", reset, true);
    window.removeEventListener("pointercancel", reset, true);
    window.removeEventListener("click", onClickCapture, true);
    window.removeEventListener("contextmenu", onContextMenu, true);
  };
}
