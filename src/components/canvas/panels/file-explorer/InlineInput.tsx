/** 行内编辑输入框（重命名 / 新建草稿共用）：Enter 提交、Esc 取消、失焦提交（挂载自动聚焦）。 */
import { Input } from "@/components/common/Input";

export function InlineInput({
  value,
  onChange,
  onCommit,
  onCancel,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  onCancel: () => void;
  placeholder?: string;
}) {
  return (
    <Input
      autoFocus
      borderless
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onBlur={onCommit}
      onKeyDown={(e) => {
        if (e.key === "Enter") onCommit();
        if (e.key === "Escape") onCancel();
      }}
      placeholder={placeholder}
      className="flex-1 !text-xs border-b border-b-[var(--accent)]"
    />
  );
}
