/**
 * 表格节点（仓库 `.atb` 文件引用）。
 *
 * 引用模型同笔记节点：`.atlx` 只存 `{title, file}`，内容快照（snapshot）运行时从
 * `.atb` 读取（打开/重读时读盘），持久化时剥离。
 * 快照可被 @提及 / 连边接入对话注入。
 *
 * 交互：header 标题双击重命名（renameTable 同步画布引用）；「打开表格」按钮与底部行
 * 经 `useFileNavigation` 打开表格窗口（组件本层调 appStore）。
 */
import { AlertTriangle, Table as TableIcon, ExternalLink } from "lucide-react";
import type { NodeProps } from "@xyflow/react";
import type { TableData } from "@/types";
import { useCanvasStore } from "@/stores/canvasStore";
import { useVaultStore } from "@/stores/vaultStore";
import { IconButton } from "@/components/common/Button";
import { useInlineEdit } from "@/hooks/useInlineEdit";
import {
  DEFAULT_TABLE_NODE_HEIGHT,
  DEFAULT_TABLE_NODE_WIDTH,
  NODE_TYPE_COLORS,
} from "@/constants/canvas";
import { tableTitleFromFile } from "@/utils/filename";
import { useFileNavigation } from "@/hooks/useFileNavigation";
import { ResizeHandle } from "./ResizeHandle";
import { ConnectionFrame } from "./ConnectionFrame";
import { NodeHeader } from "./NodeHeader";

export function TableNode({ id, data, width, height, selected }: NodeProps) {
  const { title, file, snapshot, fileMissing } = data as unknown as TableData;
  const readOnly = useCanvasStore((s) => s.readOnly);
  const nav = useFileNavigation();
  // 标题双击 inline 编辑（公共 useInlineEdit：Enter/失焦提交，Esc 取消并拦 blur 误提交）
  const renameEdit = useInlineEdit({
    value: title ?? "",
    onCommit: (v) => {
      void commitRename(v);
    },
  });

  /** 确认重命名：renameTable 改名 + 扫全部 .atlx 更新引用（模式同 TextNode 笔记节点）。 */
  const commitRename = async (draftTitle: string) => {
    const t = draftTitle.trim();
    if (!t || t === title) return;
    try {
      const newFile = await useVaultStore.getState().renameTable(file, t);
      // 用去重后的实际标题（同名自动加序号时 ≠ 草稿），与文件内 title/窗口标签保持一致
      const actualTitle = tableTitleFromFile(newFile);
      useCanvasStore
        .getState()
        .updateNodeData(id, { title: actualTitle, file: newFile });
    } catch (e) {
      console.error("重命名表格失败", e);
      useCanvasStore.setState({ error: "重命名表格失败，请重试" });
    }
  };

  const openTableWindow = () => {
    if (fileMissing || readOnly) return;
    nav.openTable(file);
  };

  // 快照预览：字段行 + 前 3 数据行（注入文本完整走 snapshot，此处仅摘要展示）
  const lines = (snapshot ?? "").split("\n").slice(0, 4);
  const rowCount = (snapshot ?? "").match(/^行\d+：/gm)?.length ?? 0;

  return (
    <div
      className="rounded-md border flex flex-col text-sm"
      style={{
        width: width ?? DEFAULT_TABLE_NODE_WIDTH,
        height: height ?? DEFAULT_TABLE_NODE_HEIGHT,
        minWidth: 240,
        minHeight: 120,
        background: "var(--bg-secondary)",
        borderColor: selected ? "var(--accent)" : "var(--border)",
        boxShadow: selected ? "var(--shadow-pop), var(--accent-glow)" : "var(--shadow-pop)",
        position: "relative",
      }}
    >
      <ConnectionFrame topType="source" selected={selected} />

      <NodeHeader
        typeColor={NODE_TYPE_COLORS.table}
        icon={<TableIcon size={14} className="flex-shrink-0" />}
        title={title}
        fallbackTitle="表格"
        renameEdit={renameEdit}
        renameDisabled={fileMissing || readOnly}
        actions={
          !fileMissing &&
          !readOnly && (
            <div
              className="flex items-center nodrag"
              onClick={(e) => e.stopPropagation()}
            >
              <IconButton
                icon={<ExternalLink size={13} />}
                label="打开表格窗口"
                onClick={openTableWindow}
                size="xs"
                className="nodrag"
              />
            </div>
          )
        }
      />

      <div
        className="nodrag nowheel overflow-auto flex-1 min-h-0 px-3 py-2"
        style={{ userSelect: "text", cursor: "text" }}
      >
        {fileMissing ? (
          <p
            className="text-xs flex items-center gap-1"
            style={{ color: "var(--danger)" }}
          >
            <AlertTriangle size={14} className="flex-shrink-0" />
            文件缺失（已在文件管理器中删除或重命名）
          </p>
        ) : (
          <div
            className="text-xs whitespace-pre-wrap break-words"
            style={{ color: "var(--text-secondary)", lineHeight: 1.6 }}
          >
            {lines.map((l, i) => (
              <div
                key={i}
                style={{
                  color:
                    i === 0 ? "var(--text-primary)" : "var(--text-secondary)",
                }}
              >
                {l}
              </div>
            ))}
          </div>
        )}
      </div>

      {/* 底部：行数统计 + 打开提示（fileMissing 不显示） */}
      {!fileMissing && (
        <div
          className="px-3 py-1 border-t rounded-b-md text-micro flex items-center justify-between flex-shrink-0"
          style={{
            borderColor: "var(--border-subtle)",
            color: "var(--text-muted)",
            cursor: "pointer",
          }}
          onClick={openTableWindow}
        >
          <span>{rowCount > 0 ? `${rowCount} 行` : "空表格"}</span>
          <span style={{ color: "var(--accent)" }}>打开表格 ↗</span>
        </div>
      )}

      {!readOnly && <ResizeHandle />}
    </div>
  );
}
