/**
 * 弹层菜单公共壳（右键菜单/浮层菜单共用）——`PopupLayer` 统一弹层壳的薄包装
 * （坐标锚定 + 向下弹出 + z-50 + 菜单项/分隔线统一样式）。
 *
 * 容器内元素无需再 stopPropagation 防提前关闭（外部关闭检测用 contains 判定）；
 * 但 React Flow / 文件树行等宿主有 pointerdown 拦截的场景需传 stopPointerDown
 * （阻止事件到达宿主处理器，防按钮 click 被宿主 preventDefault 抑制）。
 */
import type { ReactNode } from "react";
import { PopupLayer } from "@/components/common/PopupLayer";

interface MenuProps {
  x: number;
  y: number;
  onClose: () => void;
  /** 宽度 class（如 "w-44" / "w-56"），缺省 w-48。 */
  widthClass?: string;
  /** 容器内容 class（默认 "py-1"；内容非菜单项列表时覆盖，如 "p-2.5"）。 */
  contentClassName?: string;
  /** 内容高度可能变化的场景（如删除确认态切换）重新钳制，防贴视口底部溢出。 */
  repositionDeps?: unknown[];
  /** 阻止 pointerdown 冒泡（React Flow / 文件树行等宿主有 pointerdown 拦截时传）。 */
  stopPointerDown?: boolean;
  /** 层级 class（全屏遮罩内弹菜单需高过遮罩 z-index 时传，缺省 z-50）。 */
  zClass?: string;
  children: ReactNode;
}

/** 弹层菜单容器：fixed 定位 + 视口钳制 + Esc/点击外部关闭。 */
export function Menu({ x, y, onClose, widthClass = "w-48", contentClassName, repositionDeps, stopPointerDown, zClass, children }: MenuProps) {
  return (
    <PopupLayer
      anchor={{ x, y }}
      onClose={onClose}
      widthClass={widthClass}
      contentClassName={contentClassName}
      zClass={zClass}
      repositionDeps={repositionDeps}
      stopPointerDown={stopPointerDown}
    >
      {children}
    </PopupLayer>
  );
}

interface MenuItemProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /** 危险操作样式（红字，hover 红底白字）。 */
  danger?: boolean;
  /** 禁用项保持默认指针（不显示 not-allowed）——占用/状态指示类禁用项传 true。 */
  noDisabledCursor?: boolean;
}

/** 统一菜单项（w-full 左对齐 + 图标文本同行 + hover 强调色）；danger = 删除类操作；
 *  disabled = 灰显不可点（协作禁删等场景）。hover 文字加 !important：调用方可经 style
 *  覆盖默认文字色（如「本组/当前」强调色），但 hover 时仍统一为 accent-fg，避免强调色文字
 *  落在强调色底上看不见。 */
export function MenuItem({ danger, style, className, disabled, noDisabledCursor, ...rest }: MenuItemProps) {
  return (
    <button
      {...rest}
      disabled={disabled}
      className={`w-full text-left px-3 py-1.5 text-sm inline-flex items-center gap-1.5 ${
        disabled
          ? noDisabledCursor
            ? ""
            : "cursor-not-allowed"
          : danger
            ? "text-[#f87171] hover:bg-red-600 hover:text-white"
            : "hover:bg-[var(--accent)] hover:!text-[var(--accent-fg)]"
      } ${className ?? ""}`}
      style={
        // 调用方 style.color 为 undefined 时回落默认色：直接 `{ color: 默认, ...style }` 展开
        // 会把 color: undefined 顶掉默认色——占用/状态类禁用项不传色，须保持灰显
        disabled
          ? { ...style, color: style?.color ?? "var(--text-muted)" }
          : danger
            ? undefined
            : { ...style, color: style?.color ?? "var(--text-primary)" }
      }
    />
  );
}

/** 菜单内分隔线。 */
export function MenuDivider() {
  return <hr className="my-1" style={{ borderColor: "var(--border)" }} />;
}
