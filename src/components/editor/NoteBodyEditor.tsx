/**
 * 笔记正文编辑面：会话全文的正文（不含 frontmatter）交给统一 CodeMirror 引擎渲染与编辑。
 * 笔记面板与画布文本节点共用本组件；保存、协作绑定、撤销、冲突都在会话里，这里只渲染与提交。
 */

import { useMemo, type RefObject } from "react";
import type { EditorView } from "@codemirror/view";
import { MarkdownEditor, type MarkdownEditorLinks } from "@/components/editor/MarkdownEditor";
import type { NoteEditorBinding } from "@/types/noteSurface";
import { parseFrontmatter } from "@/utils/frontmatter";

interface Props {
  file: string;
  /** 全文（含 frontmatter）；本组件只渲染与提交正文。 */
  content: string;
  syncSeq: number;
  binding: NoteEditorBinding | null;
  readOnly?: boolean;
  interactiveCheckbox?: boolean;
  links?: MarkdownEditorLinks;
  editorViewRef?: RefObject<EditorView | null>;
  /** 正文（LF）变更提交给会话。 */
  onBodyChange: (bodyLF: string) => void;
  /** 协作挂载时 ytext 与正文分歧（参数 = ytext 正文 LF）。 */
  onCollabDivergence?: (ytextText: string) => void;
}

export function NoteBodyEditor({
  file,
  content,
  syncSeq,
  binding,
  readOnly = false,
  interactiveCheckbox,
  links,
  editorViewRef,
  onBodyChange,
  onCollabDivergence,
}: Props) {
  const body = useMemo(() => parseFrontmatter(content).body, [content]);
  return (
    // 只读面（笔记面板的阅读态）不参与笔记撤销路由：撤销会经会话写盘，与「阅读/编辑分离」相悖
    <div data-note-file={readOnly ? undefined : file} className="h-full">
      <MarkdownEditor
        body={body}
        syncSeq={syncSeq}
        readOnly={readOnly}
        collab={binding ?? undefined}
        interactiveCheckbox={interactiveCheckbox}
        links={links}
        editorViewRef={editorViewRef}
        onBodyChange={onBodyChange}
        onCollabDivergence={onCollabDivergence}
      />
    </div>
  );
}
