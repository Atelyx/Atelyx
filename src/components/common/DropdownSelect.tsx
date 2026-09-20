/**
 * 通用下拉选择器（自绘弹层，替代原生 select 统一 UI 风格）。
 *
 * 受控组件：value 与选项匹配时显示该选项 label，否则显示 placeholder；
 * 「清除/空值」由调用方传 `{ value: "" }` 选项实现（与原 `<option value="">` 一一对应）。
 * 触发按钮只负责结构（flex：prefixIcon + label + 箭头——图标为 flex 兄弟项，
 * 与 label/ChevronDown 同排垂直居中），尺寸/颜色/边框由调用方 className/style 决定。
 *
 * 弹层 = `PopupLayer` 统一壳（锚定按钮 + portal + 钳制/向上翻转 + 外点关闭排除自身 trigger，
 * 与全项目所有浮层同一套机制）；`group` 选项渲染分组头（对应原生 optgroup）。
 */
import { Check, ChevronDown } from "lucide-react";
import { useRef, type CSSProperties, type ReactNode } from "react";
import { PopupLayer } from "@/components/common/PopupLayer";
import { usePopupAnchor } from "@/hooks/usePopupAnchor";

export interface DropdownOption {
  value: string;
  label: ReactNode;
  /** 可选分组头（对应原生 optgroup label）。 */
  group?: string;
}

interface Props {
  value: string;
  onChange: (value: string) => void;
  options: DropdownOption[];
  /** value 为空且无对应选项时触发按钮的兜底显示。 */
  placeholder?: ReactNode;
  /** 触发按钮 label 前的图标（面板工具条图标按钮用；与 label/ChevronDown 同排）。 */
  prefixIcon?: ReactNode;
  /** options 为空时弹层内显示的占位提示（缺省 = 不显示提示，空列表）。 */
  emptyText?: ReactNode;
  /** 禁用态（灰显不可点，如空间内只读层入口）。 */
  disabled?: boolean;
  /** 触发按钮样式（尺寸/颜色/边框等），完全覆盖组件默认结构类之外的样式。 */
  className?: string;
  style?: CSSProperties;
  title?: string;
}

export function DropdownSelect({
  value,
  onChange,
  options,
  placeholder,
  prefixIcon,
  emptyText,
  disabled,
  className,
  style,
  title,
}: Props) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const { anchor, toggle, close } = usePopupAnchor(triggerRef);

  const selected = options.find((o) => o.value === value);

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={toggle}
        disabled={disabled}
        title={title}
        aria-haspopup="listbox"
        aria-expanded={!!anchor}
        className={`flex items-center gap-1 min-w-0 cursor-pointer outline-none focus:ring-1 focus:ring-[var(--accent)] disabled:cursor-not-allowed disabled:opacity-50 ${className ?? ""}`}
        style={style}
      >
        {prefixIcon}
        <span className="flex-1 min-w-0 truncate text-left">
          {selected ? selected.label : placeholder}
        </span>
        <ChevronDown size={12} className="flex-shrink-0" />
      </button>
      <PopupLayer
        anchor={anchor}
        onClose={close}
        triggerRef={triggerRef}
        zClass="z-[1100]"
      >
        <div role="listbox" className="max-h-64 overflow-y-auto">
          {options.length === 0 && emptyText != null && (
            <div className="px-3 py-2 text-[11px]" style={{ color: "var(--text-muted)" }}>
              {emptyText}
            </div>
          )}
          {options.map((o, i) => (
            <div key={`${o.value}-${i}`}>
              {o.group && (i === 0 || options[i - 1].group !== o.group) && (
                <div
                  className="px-3 pt-1.5 pb-0.5 text-[10px] select-none"
                  style={{ color: "var(--text-muted)" }}
                >
                  {o.group}
                </div>
              )}
              <button
                type="button"
                role="option"
                aria-selected={o.value === value}
                onClick={() => {
                  onChange(o.value);
                  close();
                }}
                className="w-full text-left px-3 py-1.5 text-[13px] hover:bg-[var(--hover)] inline-flex items-center justify-between gap-2"
                style={{
                  color:
                    o.value === value
                      ? "var(--accent)"
                      : "var(--text-primary)",
                }}
              >
                <span className="truncate">{o.label}</span>
                {o.value === value && (
                  <Check size={12} className="flex-shrink-0" />
                )}
              </button>
            </div>
          ))}
        </div>
      </PopupLayer>
    </>
  );
}
