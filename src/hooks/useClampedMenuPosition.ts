import { useLayoutEffect, useRef, useState } from "react";
import { clampMenuPosition } from "@/utils/contextMenu";

/**
 * 弹层定位 hook：菜单 fixed 定位在锚点坐标，挂载后按实测尺寸定位（useLayoutEffect
 * 在 paint 前调整无闪烁），靠近窗口右/下边缘收敛到视口内（`PopupLayer` 统一弹层壳使用）。
 * 方向语义：缺省顶边贴锚点 y 向下展开；`flipY` = 下方放不下且上方放得下时，底边贴 flipY
 * 向上翻转；`alignBottom` = 底边贴锚点 y 向上展开，过高时贴视口顶兜底。
 * `deps` 供内容尺寸变化时重新定位（如删除确认态切换改变菜单高度）；缺省仅坐标变化时重算。
 */
export interface ClampMenuOpts {
  /** 向上翻转锚点（底边贴此值展开），缺省 = 不翻转。 */
  flipY?: number;
  /** 底边贴锚点 y 向上展开，缺省 = 顶边贴锚点 y 向下展开。 */
  alignBottom?: boolean;
}

/** 视口边距（与 clampMenuPosition 默认一致）。 */
const VIEWPORT_GAP = 6;

export function useClampedMenuPosition(
  x: number,
  y: number,
  deps: unknown[] = [],
  opts?: ClampMenuOpts,
) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    if (opts?.alignBottom) {
      // 底边贴锚点 y：顶边 = y - h，再整体钳制（过高时贴视口顶兜底）
      setPos(clampMenuPosition(x, y - h, w, h));
    } else if (
      opts?.flipY !== undefined &&
      y + h > window.innerHeight - VIEWPORT_GAP &&
      opts.flipY - h >= VIEWPORT_GAP
    ) {
      // 下方放不下且上方放得下 → 向上翻转（底边贴 flipY，不再盖住触发器）
      setPos(clampMenuPosition(x, opts.flipY - h, w, h));
    } else {
      setPos(clampMenuPosition(x, y, w, h));
    }
    // opts 字段为原始值（非对象引用），deps 由调用方显式传入，spread 后无法静态校验，属有意为之
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [x, y, opts?.flipY, opts?.alignBottom, ...deps]);
  return { ref, pos };
}
