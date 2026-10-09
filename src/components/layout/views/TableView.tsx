/**
 * 表格编辑器视图面板（全局当前打开表格；无文件时占位引导）。
 * panelId 用于聚焦判定（覆盖编辑键盘监听门控，同画布快捷键惯例）。
 * 内容自载（同 CanvasView 惯例）：跨窗口只镜像 currentTableFile 路径（tableStore 是每窗口独立运行时态），需按文件加载磁盘内容；tableFile 已同则跳过防重复读盘。
 */
import { useEffect } from "react";
import { Table as TableIcon } from "lucide-react";
import { TableEditor } from "@/components/table/TableEditor";
import { PanelPlaceholder } from "@/components/layout/PanelPlaceholder";
import { useAppStore } from "@/stores/appStore";
import { useTableStore } from "@/stores/tableStore";

export function TableView({ panelId }: { panelId: string }) {
  const currentTableFile = useAppStore((s) => s.currentTableFile);
  const tableFile = useTableStore((s) => s.tableFile);
  const load = useTableStore((s) => s.load);

  useEffect(() => {
    if (currentTableFile && tableFile !== currentTableFile) {
      void load(currentTableFile);
    }
  }, [currentTableFile, tableFile, load]);

  if (!currentTableFile) {
    return (
      <PanelPlaceholder
        viewKind="table"
        icon={<TableIcon size={64} strokeWidth={1.5} />}
        title="打开表格"
        description="在文件面板或搜索面板单击一个 .atb 表格开始编辑。"
      />
    );
  }
  return <TableEditor key={currentTableFile} panelId={panelId} />;
}
