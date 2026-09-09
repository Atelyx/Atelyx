/**
 * 布尔滑动开关（设置页统一样式，与主题模式切换同款）：
 * 开 = 金色滑块右移，关 = 灰色；由调用方处理 onChange。
 */
export function ToggleSwitch({
  checked,
  onChange,
  title,
  disabled,
}: {
  checked: boolean;
  onChange: (checked: boolean) => void;
  title?: string;
  /** 禁用（只读不响应；title 提示原因）。 */
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-disabled={disabled}
      onClick={() => {
        if (!disabled) onChange(!checked);
      }}
      title={title}
      className={`relative w-11 h-6 rounded-full transition-colors flex-shrink-0 ${disabled ? "opacity-50" : ""}`}
      style={{ background: checked ? "var(--accent)" : "#64748b" }}
    >
      <span
        className="absolute top-0.5 left-0.5 w-5 h-5 rounded-full bg-white shadow transition-transform"
        style={{ transform: checked ? "translateX(20px)" : "translateX(0)" }}
      />
    </button>
  );
}
