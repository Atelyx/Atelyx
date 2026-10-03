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
      // 底色与渐变图分开写：切换时底色参与 transition-colors 平滑过渡，渐变图即时开关
      // （用 background 简写会把底色置为 transparent，关闭瞬间轨道会先变透明再淡入）
      style={{
        backgroundColor: checked ? "var(--accent)" : "var(--text-muted)",
        backgroundImage: checked ? "var(--accent-grad)" : "none",
      }}
    >
      <span
        className="absolute top-0.5 left-0.5 w-5 h-5 rounded-full bg-white shadow transition-transform"
        style={{ transform: checked ? "translateX(20px)" : "translateX(0)" }}
      />
    </button>
  );
}
