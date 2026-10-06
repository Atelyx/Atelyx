/**
 * 按钮基元：全项目按钮的统一形态与交互态（尺寸 / 变体 / 焦点环 / 禁用 / 加载）。
 * 业务代码一律用 `Button` / `IconButton`，不再各写各的 className 与内联色。
 */
import { forwardRef } from "react";
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { Loader2 } from "lucide-react";
import { Tooltip } from "@/components/common/Tooltip";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "subtle" | "danger" | "dangerSolid";
export type ButtonSize = "2xs" | "xs" | "sm" | "md" | "lg" | "touch";

/** 各档的 class：方角=图标按钮边长，含字=文字按钮控件高；字号走字阶变量，控件高固定、字号变化不撑开布局。
 *  2xs 16 属性行内联小徽标；xs 20 表格工具条/属性 chip/列表行内联操作；sm 24 工具条/面板头/次要动作；
 *  md 28 默认（面板头主按钮、空态行动）；lg 32 视图标题栏/画布节点内操作；touch 44 移动端触控目标。 */
const SIZE_CLASS: Record<ButtonSize, string> = {
  "2xs": "h-4 px-1 text-micro gap-0.5 rounded-[var(--radius-xs)]",
  xs: "h-5 px-1.5 text-micro gap-0.5 rounded-[var(--radius-xs)]",
  sm: "h-6 px-2 text-micro gap-1 rounded-[var(--radius-sm)]",
  md: "h-7 px-3 text-ui gap-1.5 rounded-[var(--radius-sm)]",
  lg: "h-8 px-3.5 text-ui gap-1.5 rounded-[var(--radius-md)]",
  touch: "min-h-11 px-4 text-body gap-2 rounded-[var(--radius-md)]",
};

/**
 * 变体：底色 / 渐变图 / 文字色 / hover 反馈。`ghost` 无底色，hover 才出底（工具条与列表内联动作）。
 * 强调色留给需要引起注意的动作，常规动作用 `ghost` / `secondary`，破坏性动作用 `danger`。
 *
 * 底色与渐变图分开写（不用 `background` 简写）：简写会把 background-color 置为 `transparent`，
 * 而 hover 只换底色、渐变图即时移除——底色从 transparent 淡入，按钮会先掉底再渐显。
 * 分开写则底色恒为实色，`transition-colors` 平滑过渡；渐变图叠在底色之上（默认等价纯色）。
 *
 * hover 一律用 `!important` 的任意值类：底色与文字色经内联 `style` 给出（便于主题插件
 * 与调用方动态覆盖），而内联样式在层叠中优先于任何无 `!important` 的作者规则——
 * 不用 `!important` 则悬停反馈会被内联底色吃掉，表现为「鼠标移上去没反应」。
 */
const VARIANT_STYLE: Record<
  ButtonVariant,
  { backgroundColor: string; backgroundImage: string; color: string; hover: string }
> = {
  primary: {
    backgroundColor: "var(--accent)",
    backgroundImage: "var(--accent-grad)",
    color: "var(--accent-fg)",
    hover: "hover:!bg-none hover:!bg-[var(--accent-hover)]",
  },
  secondary: {
    backgroundColor: "var(--bg-tertiary)",
    backgroundImage: "none",
    color: "var(--text-primary)",
    hover: "hover:!bg-[var(--hover)]",
  },
  ghost: {
    backgroundColor: "transparent",
    backgroundImage: "none",
    color: "var(--text-secondary)",
    hover: "hover:!bg-[var(--hover)] hover:!text-[var(--text-primary)]",
  },
  subtle: {
    backgroundColor: "transparent",
    backgroundImage: "none",
    color: "var(--text-muted)",
    hover: "hover:!text-[var(--text-primary)]",
  },
  danger: {
    backgroundColor: "transparent",
    backgroundImage: "none",
    color: "var(--danger)",
    hover: "hover:!bg-[color-mix(in_srgb,var(--danger)_12%,transparent)]",
  },
  // 实心底：不可逆动作的最终确认（与红字 `danger` 区分）
  dangerSolid: {
    backgroundColor: "var(--danger-fill)",
    backgroundImage: "none",
    color: "#ffffff",
    hover: "hover:!opacity-90",
  },
};

interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "type"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** 左侧图标（传 null 不占位）。 */
  icon?: ReactNode;
  /**
   * 加载中：显示转圈并禁用点击。转圈与 `icon` 互斥渲染，若两者尺寸不同
   * （icon 常为 12/13px，转圈固定 14px）按钮宽度会有 1–2px 变化。
   */
  loading?: boolean;
  /** 撑满容器宽度（对话框底栏、设置表单提交）。 */
  block?: boolean;
  type?: "button" | "submit" | "reset";
}

/**
 * 文字 / 带图标按钮。
 *
 * 支持 ref：浮层按触发器 rect 定位、工具条按按钮宽度对齐，都依赖拿到 DOM 节点。
 */
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  {
    variant = "secondary",
    size = "md",
    icon,
    loading,
    block,
    disabled,
    className,
    children,
    type = "button",
    style,
    ...rest
  },
  ref
) {
  const v = VARIANT_STYLE[variant];
  return (
    <button
      ref={ref}
      type={type}
      disabled={disabled || loading}
      className={`inline-flex items-center justify-center font-medium whitespace-nowrap flex-shrink-0
        transition-colors outline-none focus-visible:shadow-[var(--focus-ring)]
        disabled:opacity-40 disabled:cursor-not-allowed ${SIZE_CLASS[size]} ${v.hover}
        ${block ? "w-full" : ""} ${className ?? ""}`}
      // 调用方 style 覆盖默认色（动态状态色场景），故置于展开之后
      style={{ backgroundColor: v.backgroundColor, backgroundImage: v.backgroundImage, color: v.color, ...style }}
      {...rest}
    >
      {loading ? <Loader2 size={14} className="animate-spin flex-shrink-0" /> : icon}
      {children}
    </button>
  );
});

interface IconButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "type" | "children"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** 图标内容。 */
  icon: ReactNode;
  /**
   * 无障碍名与提示文案。图标按钮无可视文字，屏幕阅读器与悬停提示都靠它——
   * 必填，避免出现「无名按钮」。
   */
  label: string;
  loading?: boolean;
  /** 关掉悬停提示（提示写在别处、或该按钮在密集列表里不宜逐个浮现）。 */
  noTooltip?: boolean;
  type?: "button" | "submit" | "reset";
}

/**
 * 纯图标按钮（正方形）。`label` 必填：无文字按钮须有可访问名。
 *
 * 提示走 `Tooltip` 浮层而非原生 `title`：原生提示延迟约 1s、样式不可控、位置各浏览器不一致。
 * 故只写 `aria-label`（供屏幕阅读器），不写 `title`——两者同时存在会出现双重提示。
 */
export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  {
    variant = "ghost",
    size = "sm",
    icon,
    label,
    loading,
    noTooltip,
    disabled,
    className,
    type = "button",
    style,
    ...rest
  },
  ref
) {
  const v = VARIANT_STYLE[variant];
  const btn = (
    <button
      ref={ref}
      type={type}
      disabled={disabled || loading}
      aria-label={label}
      className={`inline-flex items-center justify-center flex-shrink-0
        transition-colors outline-none focus-visible:shadow-[var(--focus-ring)]
        disabled:opacity-40 disabled:cursor-not-allowed
        ${SQUARE_SIZE_CLASS[size]} ${v.hover} ${className ?? ""}`}
      style={{ backgroundColor: v.backgroundColor, backgroundImage: v.backgroundImage, color: v.color, ...style }}
      {...rest}
    >
      {loading ? <Loader2 size={14} className="animate-spin" /> : icon}
    </button>
  );
  return noTooltip ? btn : <Tooltip content={label}>{btn}</Tooltip>;
});

/**
 * 正方形边长（与 SIZE_CLASS 的高度档一致），并带上该档圆角。
 * 圆角写在这里而非由调用点补：迁移前的裸 `<button>` 普遍带 `rounded`/`rounded-sm`，
 * 缺了这层圆角会让 hover 底色变成硬直角方块，与所在面板的圆角风格不一致。
 */
const SQUARE_SIZE_CLASS: Record<ButtonSize, string> = {
  "2xs": "w-4 h-4 rounded-[var(--radius-xs)]",
  xs: "w-5 h-5 rounded-[var(--radius-xs)]",
  sm: "w-6 h-6 rounded-[var(--radius-sm)]",
  md: "w-7 h-7 rounded-[var(--radius-sm)]",
  lg: "w-8 h-8 rounded-[var(--radius-md)]",
  touch: "w-11 h-11 rounded-[var(--radius-md)]",
};
