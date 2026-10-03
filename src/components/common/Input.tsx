/**
 * 表单基元：文本输入、多行输入、勾选 / 单选（`Input` / `Textarea` / `Checkbox` / `Radio`）。
 * 统一聚焦环（`--focus-ring`）、边框与底色取值，故随主题切换一致。
 */
import { forwardRef, type InputHTMLAttributes, type ReactNode, type TextareaHTMLAttributes } from "react";
import { Check, Minus } from "lucide-react";

/**
 * 输入框外框 class（输入框 / 多行输入共用）。
 *
 * 边框色必须显式给出：Tailwind preflight 把 `border-color` 设为
 * `borderColor.DEFAULT`（冷灰 `#e5e7eb`），不随主题走——暗色下是刺眼亮边，
 * 须由本串按`--input-border` 取值。
 *
 * 本串自带 `w-full` / `px-2` / `py-1`；调用点要改这三者必须写 `!` 前缀类
 * （如 `!w-[260px]`）——同特异性的工具类靠样式表源序决胜，而 `w-full` 排在
 * 固定宽度档之后，不加 `!` 会被基元宽度吃掉。
 */
const FIELD_CLASS =
  "w-full bg-[var(--input-bg)] border border-[var(--input-border)] rounded-[var(--radius-sm)] px-2 py-1 text-ui " +
  "text-[var(--text-primary)] placeholder-[var(--input-placeholder)] outline-none " +
  "transition-colors focus:border-[var(--accent)] focus:shadow-[var(--focus-ring)] " +
  "disabled:opacity-50 disabled:cursor-not-allowed";

/**
 * 无框变体：画布节点重命名、表格单元格、文件树行内新建这类「就地编辑」用。
 * 它们要融进被编辑的表面（不画框），聚焦时靠文字色与光标表明状态。
 *
 * 本档**不预占边框槽位**（无 `border`、无 `border-transparent`、无 `focus:border-*`）：
 * 无框就该一个框都不画。调用点要下划线时写 `border-b border-b-[var(--accent)]`；
 * 若基元在此声明 `border-transparent`，那条简写会同时给四边上色，把下划线变成方框。
 */
const BORDERLESS_CLASS =
  "w-full bg-transparent rounded-[var(--radius-xs)] px-1 text-ui " +
  "text-[var(--text-primary)] placeholder-[var(--input-placeholder)] outline-none " +
  "transition-colors " +
  "disabled:opacity-50 disabled:cursor-not-allowed";

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  /** 输入框左侧图标。 */
  icon?: ReactNode;
  /**
   * 无框（就地编辑）：去掉底色与边框，融进被编辑的表面。
   * 画布节点重命名、表格单元格、文件树行内新建用此档——给它们画框会
   * 在每个正在编辑的单元格外多出一圈线。
   */
  borderless?: boolean;
}

/**
 * 单行文本输入。
 *
 * 支持 ref：搜索框、菜单内重命名等调用方要拿到 DOM 节点做聚焦与测量。
 */
export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { icon, borderless, className, ...rest },
  ref
) {
  const base = borderless ? BORDERLESS_CLASS : FIELD_CLASS;
  if (!icon) return <input ref={ref} className={`${base} ${className ?? ""}`} {...rest} />;
  return (
    <div className="relative flex items-center">
      <span
        className="absolute left-2 flex items-center pointer-events-none"
        style={{ color: "var(--text-muted)" }}
      >
        {icon}
      </span>
      <input
        ref={ref}
        className={`${base} pl-7 ${className ?? ""}`}
        {...rest}
      />
    </div>
  );
});

/**
 * 多行文本输入。
 *
 * 支持 ref：表格单元格编辑器、菜单内选项编辑要拿节点做聚焦与测量。
 * `borderless` 同 `Input`：就地编辑（表格单元格）用，避免给每个正在编辑的
 * 单元格画出一圈边框。
 */
export const Textarea = forwardRef<
  HTMLTextAreaElement,
  TextareaHTMLAttributes<HTMLTextAreaElement> & { borderless?: boolean }
>(function Textarea({ className, borderless, ...rest }, ref) {
  // 多行需要比单行略大的上下留白；`FIELD_CLASS` 自带 `py-1`，此处用 `!` 显式压过。
  // 行高不由基元给：表格单元格编辑态要与显示态逐行对齐（跟字号档走），
  // 无框就地编辑也不该被表单档的行高带偏，故由调用点按所在表面决定。
  const base = borderless ? BORDERLESS_CLASS : `${FIELD_CLASS} !py-1.5`;
  return <textarea ref={ref} className={`${base} resize-none ${className ?? ""}`} {...rest} />;
});

/**
 * 勾选框。`indeterminate` 用于「部分选中」（如表格全选、部分行已选）。
 * 未选中的方框由外层span 绘制（原生 input 透明覆盖其上，保证可点与可聚焦）。
 *
 * 支持 ref：工具类勾选（如工具分类的「全选」）要拿节点做测量或聚焦。
 * `bare` 供已经处在 `<label>` 内的调用点使用——本组件默认渲染 `<label>`，
 * 套在label 里会形成非法嵌套，且点击热区会意外覆盖整行。
 */
export const Checkbox = forwardRef<
  HTMLInputElement,
  {
    checked: boolean;
    /** 部分选中态（优先于 checked 渲染横杠）。 */
    indeterminate?: boolean;
    onChange: (checked: boolean) => void;
    label?: ReactNode;
    disabled?: boolean;
    /** 不渲染 `<label>` 包裹（调用点已有 label）。 */
    bare?: boolean;
    className?: string;
  } & Omit<InputHTMLAttributes<HTMLInputElement>, "type" | "checked" | "onChange">
>(function Checkbox(
  { checked, indeterminate, onChange, label, disabled, bare, className, ...rest },
  ref
) {
  // `bare` 下外层没有 `<label>` 承接className，须并到方框容器上，否则调用点
  // 传的定位类（`nodrag` 防节点拖拽、`ml-auto` 右对齐）会被静默丢弃
  const boxClass =
    `relative flex items-center justify-center flex-shrink-0 ` +
    (bare ? `${disabled ? "opacity-50" : "cursor-pointer"} ${className ?? ""}` : "");
  const box = (
    <span className={boxClass}>
      <input
        ref={ref}
        type="checkbox"
        checked={checked}
        // 部分选中是 AT 专有语义：只反映在 DOM 属性上，不改变表单提交值
        // （indeterminate 不参与序列化，提交仍只取 checked）。
        aria-checked={indeterminate ? "mixed" : checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="peer absolute inset-0 opacity-0 w-full h-full m-0 cursor-pointer"
        {...rest}
      />
      <span
        className="w-4 h-4 rounded-[var(--radius-xs)] border flex items-center justify-center
          transition-colors peer-focus-visible:shadow-[var(--focus-ring)]"
        style={{
          background: checked || indeterminate ? "var(--accent)" : "var(--input-bg)",
          borderColor: checked || indeterminate ? "var(--accent)" : "var(--input-border)",
        }}
      >
        {indeterminate ? (
          <Minus size={11} style={{ color: "var(--accent-fg)" }} />
        ) : checked ? (
          <Check size={11} style={{ color: "var(--accent-fg)" }} />
        ) : null}
      </span>
    </span>
  );
  if (bare) return box;
  return (
    <label
      className={`inline-flex items-center gap-2 ${disabled ? "opacity-50" : "cursor-pointer"} ${className ?? ""}`}
    >
      {box}
      {label && (
        <span className="text-ui" style={{ color: "var(--text-primary)" }}>
          {label}
        </span>
      )}
    </label>
  );
});

/** 单选按钮（配合 `Checkbox` 同一套视觉，仅形状为圆）。 */
export function Radio({
  checked,
  onChange,
  label,
  name,
  disabled,
  className,
}: {
  checked: boolean;
  onChange: () => void;
  label?: ReactNode;
  /**
   * 同组单选共用一个 name。缺省时各 radio 不属同一原生互斥组，
   * 方向键在组内移动这一标准键盘交互会失效。
   */
  name?: string;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <label
      className={`inline-flex items-center gap-2 ${disabled ? "opacity-50" : "cursor-pointer"} ${className ?? ""}`}
    >
      <span className="relative flex items-center justify-center flex-shrink-0">
        <input
          type="radio"
          name={name}
          checked={checked}
          disabled={disabled}
          onChange={onChange}
          className="peer absolute inset-0 opacity-0 w-full h-full m-0 cursor-pointer"
        />
        <span
          className="w-4 h-4 rounded-full border flex items-center justify-center
            transition-colors peer-focus-visible:shadow-[var(--focus-ring)]"
          style={{
            background: checked ? "var(--accent)" : "var(--input-bg)",
            borderColor: checked ? "var(--accent)" : "var(--input-border)",
          }}
        >
          {checked && (
            <span
              className="w-1.5 h-1.5 rounded-full"
              style={{ background: "var(--accent-fg)" }}
            />
          )}
        </span>
      </span>
      {label && (
        <span className="text-ui" style={{ color: "var(--text-primary)" }}>
          {label}
        </span>
      )}
    </label>
  );
}
