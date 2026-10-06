/**
 * 统一弹层壳：全项目所有浮层（右键菜单/下拉/工具浮层/状态栏计算菜单/仓库切换列表/历史会话）的唯一入口。
 *
 * 锚点定位 + 方向 + 实测尺寸钳制/翻转 + Esc/外点关闭 + portal + 容器样式全部收敛于此；
 * 方向语义见 align prop，portal 与 triggerRef 排除的原因见下方实现注释。
 */
import { createPortal } from "react-dom";
import type { ReactNode, RefObject } from "react";
import { useClampedMenuPosition } from "@/hooks/useClampedMenuPosition";
import { useDismissOnOutside } from "@/hooks/useDismissOnOutside";
import { useBackHandler } from "@/hooks/useBackHandler";

/** 弹层锚点：触发器 rect 或点击坐标；flipY = 向上翻转锚点（仅 align="top" 用）。 */
export interface PopupAnchor {
  x: number;
  y: number;
  /** 最小宽度（触发器宽度兜底），缺省 = 内容自然宽度。 */
  minWidth?: number;
  /** 下方空间不足时向上翻转的锚点（底边贴此值，通常 = 触发器顶边 - 间隙）。 */
  flipY?: number;
}

interface PopupLayerProps {
  /** null = 关闭（不渲染）。 */
  anchor: PopupAnchor | null;
  onClose: () => void;
  /** 外点排除区（自身 trigger 区域）：点它不关，开/关语义由调用方 click 控制。 */
  triggerRef?: RefObject<HTMLElement | null>;
  /** 弹出方向：top = 向下展开（缺省，顶边贴锚点 y，配 flipY 可在下方空间不足时向上翻转）；
   *  bottom = 向上展开（底边贴锚点 y，过高时贴视口顶兜底）。水平一律左边缘贴锚点 x 并视口钳制，
   *  minWidth 防窄于触发器。 */
  align?: "top" | "bottom";
  /** 宽度 class（如 "w-44" / "w-64"）。 */
  widthClass?: string;
  /** 容器内容 class（默认 "py-1"；内容非菜单项列表时覆盖，如 "p-1"）。 */
  contentClassName?: string;
  /** 层级 class（画布内下拉/工具浮层需高过 React Flow 选中节点的 +1000 抬升，传 z-[1100]）。 */
  zClass?: string;
  /** 内容高度可能变化的场景（如删除确认态切换）重新定位，防贴视口底部溢出。 */
  repositionDeps?: unknown[];
  /** 阻止 pointerdown 冒泡（React Flow / 文件树行等宿主有 pointerdown 拦截时传）。 */
  stopPointerDown?: boolean;
  children: ReactNode;
}

export function PopupLayer({
  anchor,
  onClose,
  triggerRef,
  align = "top",
  widthClass,
  contentClassName = "py-1",
  zClass = "z-50",
  repositionDeps,
  stopPointerDown,
  children,
}: PopupLayerProps) {
  const { ref, pos } = useClampedMenuPosition(
    anchor?.x ?? 0,
    anchor?.y ?? 0,
    repositionDeps ?? [],
    align === "top" ? { flipY: anchor?.flipY } : { alignBottom: true },
  );
  useDismissOnOutside(onClose, ref, triggerRef);
  // 返回键优先关掉最上层浮层（后开先关）
  useBackHandler(!!anchor, () => {
    onClose();
    return true;
  });

  if (!anchor) return null;
  // portal 挂 body：画布节点带 transform，fixed 会被 transform 祖先捕获错位，脱离节点 DOM 树后按视口坐标渲染。
  // useDismissOnOutside 的 triggerRef 排除自身 trigger（开/关 toggle 归调用方 click，不加 stopPropagation——
  // 其它弹层打开时点本 trigger 仍能被外点关闭，实现弹层互斥）
  return createPortal(
    <div
      ref={ref}
      // data-popup-layer：宿主全局外点监听（如 NoteEditor 的「点外退出编辑」）按此标记排除弹层内操作
      data-popup-layer
      className={`fixed border rounded-[var(--radius-md)] shadow-[var(--shadow-pop)] ${zClass} ${widthClass ?? ""} ${contentClassName}`}
      style={{
        left: pos.x,
        top: pos.y,
        minWidth: anchor.minWidth,
        background: "var(--bg-overlay)",
        borderColor: "var(--border)",
        // 浮层是唯一真正压在内容之上的面：背景模糊由皮肤决定（默认 none = 不模糊）
        backdropFilter: "var(--glass-filter)",
        WebkitBackdropFilter: "var(--glass-filter)",
      }}
      onClick={(e) => e.stopPropagation()}
      onPointerDown={stopPointerDown ? (e) => e.stopPropagation() : undefined}
    >
      {children}
    </div>,
    document.body,
  );
}
