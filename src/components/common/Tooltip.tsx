/**
 * 悬停提示（替换浏览器原生 `title`）：原生提示延迟长（约1s 才出现）、样式不可控、
 * 且各浏览器位置与外观不一致。这里统一为浮层，锚定触发器、实测尺寸钳制、防贴边翻转。
 *
 * 由 `PopupLayer` 承载（全项目浮层唯一入口），故与菜单/下拉共享外点与 Esc 关闭语义。
 * 触屏无悬停，调用方不应依赖本组件传达关键信息（关键信息须写在可见文案里）。
 */
import { cloneElement, useEffect, useRef, useState } from "react";
import type { ReactElement, ReactNode } from "react";
import { PopupLayer } from "@/components/common/PopupLayer";

/** 出现延迟（ms）：指针掠过即显示会让长列表扫过时闪成一片。 */
const OPEN_DELAY_MS = 400;
/** 关闭延迟（ms）：给指针从触发器移到提示自身一段余量，划过间隙不闪烁。 */
const CLOSE_DELAY_MS = 120;

interface TooltipProps {
  /** 提示文案。 */
  content: ReactNode;
  /**
   * 触发元素：单个能挂 ref 的元素。组件会往其上合并 ref 与鼠标/焦点事件，
   * 故children 的原有事件与 ref 不会被覆盖。
   */
  children: ReactElement;
  /** 提示出现在触发器下方（缺省）还是上方。 */
  placement?: "bottom" | "top";
  /** 不显示提示（元素已自带 title、或提示为纯装饰时）。 */
  disabled?: boolean;
}

/**
 * 悬停提示：悬停/聚焦 `children` 后浮出提示，移开后延迟收起。
 *
 * 不额外包一层元素：包装层会破坏 flex/grid 子项关系（`display:contents`虽能透传布局，
 * 但元素自身不生成盒子，`getBoundingClientRect()` 恒为 0，锚点会落到视口左上角），
 * 故把 ref 与事件直接合并进唯一的子元素。
 */
export function Tooltip({ content, children, placement = "bottom", disabled }: TooltipProps) {
  const triggerRef = useRef<HTMLElement>(null);
  const [anchor, setAnchor] = useState<{ x: number; y: number; flipY?: number } | null>(null);
  const openTimer = useRef<number | undefined>(undefined);
  const closeTimer = useRef<number | undefined>(undefined);

  const clearTimers = () => {
    window.clearTimeout(openTimer.current);
    window.clearTimeout(closeTimer.current);
  };

  // 卸载时清掉待触发的定时器：否则在延迟窗口内卸载（列表项被筛掉、面板关闭）
  // 仍会对已卸载组件 setState。
  useEffect(() => clearTimers, []);

  /** 按触发器实测 rect 算锚点；下方空间不足时由 flipY 向上翻转。 */
  const computeAnchor = () => {
    const r = triggerRef.current?.getBoundingClientRect();
    // 宽或高为0 = 触发器不可见（隐藏、未布局完、折叠中），此时不展示
    if (!r || r.width === 0 || r.height === 0) return null;
    return placement === "top"
      ? { x: r.left, y: r.top - 4, flipY: r.bottom + 4 }
      : { x: r.left, y: r.bottom + 4, flipY: r.top - 4 };
  };

  const open = () => {
    if (disabled) return;
    clearTimers();
    openTimer.current = window.setTimeout(() => {
      const a = computeAnchor();
      if (a) setAnchor(a);
    }, OPEN_DELAY_MS);
  };

  /** 延迟关闭：指针移出触发器时不立刻消失，避免掠过即闪。 */
  const scheduleClose = () => {
    clearTimers();
    closeTimer.current = window.setTimeout(() => setAnchor(null), CLOSE_DELAY_MS);
  };

  const close = () => {
    clearTimers();
    setAnchor(null);
  };

  const child = children;
  const childProps = child.props as {
    onMouseEnter?: (e: React.MouseEvent) => void;
    onMouseLeave?: (e: React.MouseEvent) => void;
    onFocus?: (e: React.FocusEvent) => void;
    onBlur?: (e: React.FocusEvent) => void;
  };
  // 合并而非覆盖子元素自己的处理器：调用方可能已在按钮上挂了 hover 行为。
  //
  // 子元素的 ref 不能从 `child.props.ref` 读：React 18 已把 ref 从 props 剥离到
  // `element.ref`（读 props.ref 会拿到 undefined 并触发开发警告），合并逻辑会静默失效、
  // 把调用方（如浮层定位依赖的 IconButton ref）丢成 null。故取元素上的 ref 字段，
  // 函数 ref 与对象 ref 都要转交。
  const childRef = (child as { ref?: React.Ref<HTMLElement> }).ref;
  const trigger = cloneElement(child, {
    ref: (node: HTMLElement | null) => {
      (triggerRef as React.MutableRefObject<HTMLElement | null>).current = node;
      if (typeof childRef === "function") childRef(node);
      else if (childRef) (childRef as React.MutableRefObject<HTMLElement | null>).current = node;
    },
    onMouseEnter: (e: React.MouseEvent) => {
      childProps.onMouseEnter?.(e);
      open();
    },
    onMouseLeave: (e: React.MouseEvent) => {
      childProps.onMouseLeave?.(e);
      scheduleClose();
    },
    onFocus: (e: React.FocusEvent) => {
      childProps.onFocus?.(e);
      open();
    },
    onBlur: (e: React.FocusEvent) => {
      childProps.onBlur?.(e);
      scheduleClose();
    },
  });

  return (
    <>
      {trigger}
      {anchor && (
        <PopupLayer
          anchor={anchor}
          onClose={close}
          zClass="z-[1200]"
          contentClassName="px-2 py-1 max-w-[260px]"
          // 排除触发器自身：否则按下按钮时提示会被当「点别处」立即关掉，
          // 表现为点击瞬间提示闪一下消失（收起延迟还没到）。
          triggerRef={triggerRef as React.RefObject<HTMLElement | null>}
          stopPointerDown
        >
          <div
            className="text-micro leading-relaxed break-words"
            style={{ color: "var(--text-secondary)" }}
          >
            {content}
          </div>
        </PopupLayer>
      )}
    </>
  );
}
