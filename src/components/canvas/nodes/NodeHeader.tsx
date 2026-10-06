/**
 * 画布节点头部外壳：类型色条 + 左侧图标 + 标题（双击两态编辑）+ 右侧操作区。
 * 类名与 DOM 结构对各调用节点收敛在此，改头部外观只动这一处；差异走插槽（icon / actions），不堆布尔开关。
 */
import type { ReactNode } from "react";
import { Input } from "@/components/common/Input";
import { NODE_TYPE_BAR_CLASS } from "@/constants/canvas";
import { useInlineEdit } from "@/hooks/useInlineEdit";

interface Props {
  /** 类型色条颜色（NODE_TYPE_COLORS.*）。 */
  typeColor: string;
  /** 色条之后、标题之前的图标槽。 */
  icon: ReactNode;
  /** 标题原文；空值时回退 fallbackTitle。 */
  title: string;
  /** 标题为空时的展示文案。 */
  fallbackTitle: string;
  /** 重命名编辑态（useInlineEdit 返回值：editing / start / inputProps）。 */
  renameEdit: ReturnType<typeof useInlineEdit>;
  /** 禁用重命名（文件缺失 / 只读白板）：禁用时标题不响应双击、不显示提示。 */
  renameDisabled?: boolean;
  /** 右侧操作区（含各自的包裹元素，结构差异由调用方决定）。 */
  actions?: ReactNode;
}

export function NodeHeader({
  typeColor,
  icon,
  title,
  fallbackTitle,
  renameEdit,
  renameDisabled,
  actions,
}: Props) {
  return (
    <header
      className="px-3 py-1.5 border-b rounded-t-md text-xs font-medium flex-shrink-0 flex items-center justify-between gap-1"
      style={{
        cursor: "grab",
        borderColor: "var(--border-subtle)",
        background: "var(--bg-tertiary)",
        color: "var(--text-secondary)",
      }}
    >
      <span
        className={NODE_TYPE_BAR_CLASS}
        style={{ background: typeColor }}
      />
      <span className="inline-flex items-center gap-1 min-w-0 flex-1 overflow-hidden">
        {icon}
        {renameEdit.editing ? (
          <Input
            {...renameEdit.inputProps}
            autoFocus
            borderless
            onClick={(e) => e.stopPropagation()}
            className="nodrag min-w-0 !text-xs"
          />
        ) : (
          <span
            className="truncate"
            title={renameDisabled ? undefined : "双击重命名"}
            onDoubleClick={renameDisabled ? undefined : renameEdit.start}
          >
            {title || fallbackTitle}
          </span>
        )}
      </span>
      {actions}
    </header>
  );
}
