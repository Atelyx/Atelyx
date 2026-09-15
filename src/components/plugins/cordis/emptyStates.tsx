/**
 * 视图空态贡献（empty/<viewKind> 槽）：各内置视图插件贡献自己的空态引导——
 * 停用对应插件即回退视图内 PanelPlaceholder 的兜底引导；外部插件可高 priority 替换空态。
 */
import { FileText, Palette, Table as TableIcon } from "lucide-react";
import { PanelPlaceholder } from "@/components/layout/PanelPlaceholder";

/** 画布空态（empty/canvas）。 */
export function CanvasEmptyState() {
  return (
    <PanelPlaceholder
      icon={<Palette size={64} strokeWidth={1.5} />}
      title="打开画布"
      description="从左侧文件面板或搜索面板单击一个 .atlx 画布开始编辑。"
    />
  );
}

/** 笔记空态（empty/note）。 */
export function NoteEmptyState() {
  return (
    <PanelPlaceholder
      icon={<FileText size={64} strokeWidth={1.5} />}
      title="打开笔记"
      description="从左侧文件面板或搜索面板单击一个 .md 笔记开始编辑。"
    />
  );
}

/** 表格空态（empty/table）。 */
export function TableEmptyState() {
  return (
    <PanelPlaceholder
      icon={<TableIcon size={64} strokeWidth={1.5} />}
      title="打开表格"
      description="从左侧文件面板或搜索面板单击一个 .atb 表格开始编辑。"
    />
  );
}
