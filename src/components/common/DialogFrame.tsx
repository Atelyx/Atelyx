/**
 * 通用模态弹窗外框：fixed 遮罩 + 居中面板。层级取 `constants/zLayers`；遮罩点击 / Esc /
 * 返回键的关闭行为由 props 统一裁决（嵌套确认弹窗打开时经 canClose 挂起）。面板宽度、
 * 高度与内边距由调用方经 panelClassName 定制，配色与投影规格由本组件统一提供。
 */
import { useEffect, useRef } from "react";
import type { ReactNode } from "react";
import { useBackHandler } from "@/hooks/useBackHandler";

export function DialogFrame({
  onClose,
  z,
  panelClassName,
  overlayClassName,
  ariaLabel,
  canClose,
  closeOnScrim = true,
  closeOnEsc = true,
  children,
}: {
  onClose: () => void;
  /** 层级（取 `Z_LAYERS` 表值）。 */
  z: number;
  /** 面板尺寸与布局类（宽度/最大高/内边距/flex 列向等）。 */
  panelClassName: string;
  /** 遮罩附加类（如移动端 p-4）。 */
  overlayClassName?: string;
  ariaLabel?: string;
  /** 关闭守卫：返回 false 时遮罩与 Esc 均不关闭（缺省恒可关）。 */
  canClose?: () => boolean;
  closeOnScrim?: boolean;
  closeOnEsc?: boolean;
  children: ReactNode;
}) {
  const stateRef = useRef({ onClose, canClose });
  stateRef.current = { onClose, canClose };

  useEffect(() => {
    if (!closeOnEsc) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && (stateRef.current.canClose?.() ?? true)) {
        stateRef.current.onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [closeOnEsc]);

  useBackHandler(true, () => {
    if (closeOnScrim && (stateRef.current.canClose?.() ?? true)) {
      stateRef.current.onClose();
      return true;
    }
    return false;
  });

  return (
    <div
      className={`fixed inset-0 flex items-center justify-center ${overlayClassName ?? ""}`}
      style={{ zIndex: z, background: "var(--scrim)" }}
      role="dialog"
      aria-modal="true"
      aria-label={ariaLabel}
      onClick={
        closeOnScrim
          ? (e) => {
              if (e.target === e.currentTarget && (canClose?.() ?? true)) onClose();
            }
          : undefined
      }
    >
      <div
        className={`rounded-[var(--radius-lg)] border shadow-[var(--shadow-pop)] ${panelClassName}`}
        style={{ background: "var(--bg-overlay)", borderColor: "var(--border)" }}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}
