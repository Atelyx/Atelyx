/**
 * 笔记编辑器视图面板（全局当前打开笔记；无文件时占位引导）。
 */
import { FileText } from "lucide-react";
import { NoteEditor } from "@/components/editor/NoteEditor";
import { PanelPlaceholder } from "@/components/layout/PanelPlaceholder";
import { useAppStore } from "@/stores/appStore";

export function NoteView() {
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);

  if (!currentNoteFile) {
    return (
      <PanelPlaceholder
        viewKind="note"
        icon={<FileText size={64} strokeWidth={1.5} />}
        title="打开笔记"
        description="从左侧文件面板或搜索面板单击一个 .md 笔记开始编辑。"
      />
    );
  }
  return <NoteEditor key={currentNoteFile} file={currentNoteFile} />;
}
