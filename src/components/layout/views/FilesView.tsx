/**
 * 文件面板视图面板（薄包装：白板转换入口 + 当前打开文件高亮）。
 */
import { FileExplorerPanel } from "@/components/canvas/panels/FileExplorerPanel";
import { useAppStore } from "@/stores/appStore";

export function FilesView() {
  const convertWhiteboard = useAppStore((s) => s.convertWhiteboard);
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const currentTableFile = useAppStore((s) => s.currentTableFile);

  return (
    <FileExplorerPanel
      openedNoteFile={currentNoteFile}
      openedTableFile={currentTableFile}
      onConvertWhiteboard={(file) => void convertWhiteboard(file)}
    />
  );
}
