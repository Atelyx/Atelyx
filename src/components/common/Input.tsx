/**
 * 表单基元：文本输入、多行输入、勾选 / 单选（`Input` / `Textarea` / `Checkbox` / `Radio`）。
 * 统一聚焦环（`--focus-ring`）、边框与底色取值，故随主题切换一致。
 */
import type { InputHTMLAttributes, ReactNode, TextareaHTMLAttributes } from "react";
import { Check, Minus } from "lucide-react";

/** 输入框外框 class（输入框 / 多行输入共用）。 */
const FIELD_CLASS =
  "w-full bg-[var(--input-bg)] border rounded-[var(--radius-sm)] px-2 py-1 text-ui " +
  "text-[var(--text-primary)] placeholder-[var(--input-placeholder)] outline-none " +
  "transition-colors focus:border-[var(--accent)] focus:shadow-[var(--focus-ring)] " +
  "disabled:opacity-50 disabled:cursor-not-allowed";

export interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  /** 输入框左侧图标。 */
  icon?: ReactNode;
}

/** 单行文本输入。 */
export function Input({ icon, className, ...rest }: InputProps) {
  if (!icon) {
    return <input className={`${FIELD_CLASS} ${className ?? ""}`} {...rest} />;
  }
  return (
    <div className="relative flex items-center">
      <span
        className="absolute left-2 flex items-center pointer-events-none"
        style={{ color: "var(--text-muted)" }}
      >
        {icon}
      </span>
      <input className={`${FIELD_CLASS} pl-7 ${className ?? ""}`} {...rest} />
    </div>
  );
}

/** 多行文本输入。 */
export function Textarea({ className, ...rest }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={`${FIELD_CLASS} resize-none leading-relaxed ${className ?? ""}`}
      {...rest}
    />
  );
}

/**
 * 勾选框。`indeterminate` 用于「部分选中」（如表格全选、部分行已选）。
 * 未选中的方框由外层span 绘制（原生 input 透明覆盖其上，保证可点与可聚焦）。
 */
export function Checkbox({
  checked,
  indeterminate,
  onChange,
  label,
  disabled,
  className,
}: {
  checked: boolean;
  /** 部分选中态（优先于 checked 渲染横杠）。 */
  indeterminate?: boolean;
  onChange: (checked: boolean) => void;
  label?: ReactNode;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <label
      className={`inline-flex items-center gap-2 ${disabled ? "opacity-50" : "cursor-pointer"} ${className ?? ""}`}
    >
      <span className="relative flex items-center justify-center flex-shrink-0">
        <input
          type="checkbox"
          checked={checked}
          // 部分选中是 AT 专有语义：只反映在 DOM 属性上，不改变表单提交值
          // （indeterminate 不参与序列化，提交仍只取 checked）。
          aria-checked={indeterminate ? "mixed" : checked}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked)}
          className="peer absolute inset-0 opacity-0 w-full h-full m-0 cursor-pointer"
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
      {label && (
        <span className="text-ui" style={{ color: "var(--text-primary)" }}>
          {label}
        </span>
      )}
    </label>
  );
}

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
