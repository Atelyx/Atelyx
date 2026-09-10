import { BookmarkPlus, ClipboardCopy, Trash2 } from "lucide-react";
import { useCallback } from "react";
import { useCanvasStore } from "@/stores/canvasStore";
import { useNodeCollab } from "@/hooks/useNodeCollab";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import type { TextData } from "@/types";

interface Props {
  nodeId: string;
  x: number;
  y: number;
  onClose: () => void;
}

export function NodeContextMenu({ nodeId, x, y, onClose }: Props) {
  const addNode = useCanvasStore((s) => s.addNode);
  const nodes = useCanvasStore((s) => s.nodes);
  // 画布内文本节点（无 file）提供「保存为笔记」入口：落盘生成仓库 .md 并转笔记节点
  const node = nodes.find((n) => n.id === nodeId);
  const isUnsavedText = node?.type === "text" && !(node.data as unknown as TextData).file;
  // 协作：对话节点被其他对端独占编辑 → 禁删（防误删进行中的编辑；移动/缩放仍允许）
  const { lockedByPeer } = useNodeCollab(nodeId);
  const deleteDisabled = node?.type === "conversation" && lockedByPeer !== null;

  const handleSaveAsNote = useCallback(() => {
    void useCanvasStore
      .getState()
      .saveTextNodeAsNote(nodeId)
      .catch(() => useCanvasStore.setState({ error: "保存为笔记失败，请重试" }));
    onClose();
  }, [nodeId, onClose]);

  const handleDelete = useCallback(() => {
    const { onNodesChange, pushUndo } = useCanvasStore.getState();
    // 右键删除需入栈，否则无法 Ctrl+Z 撤销
    pushUndo();
    onNodesChange([{ type: "remove" as const, id: nodeId }]);
    onClose();
  }, [nodeId, onClose]);

  const handleDuplicate = useCallback(() => {
    const original = nodes.find((n) => n.id === nodeId);
    if (!original) return;
    const newNode = {
      ...original,
      id: crypto.randomUUID(),
      position: {
        x: original.position.x + 40,
        y: original.position.y + 40,
      },
      selected: false,
      // 深拷贝 data，避免与源节点共享内部引用（如数组字段）
      data: structuredClone(original.data),
    };
    addNode(newNode);
    onClose();
  }, [nodeId, nodes, addNode, onClose]);

  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-56" stopPointerDown>
      {isUnsavedText && (
        <>
          <MenuItem onClick={handleSaveAsNote}>
            <span className="inline-flex items-center gap-1.5">
              <BookmarkPlus size={14} />
              保存为笔记
            </span>
          </MenuItem>
          <MenuDivider />
        </>
      )}
      <MenuItem onClick={handleDuplicate}>
        <span className="inline-flex items-center gap-1.5">
          <ClipboardCopy size={14} />
          复制节点
        </span>
      </MenuItem>
      <MenuDivider />
      <MenuItem
        onClick={handleDelete}
        danger
        disabled={deleteDisabled}
        title={
          deleteDisabled
            ? `${lockedByPeer?.nickname} 正在编辑该对话，禁止删除`
            : undefined
        }
      >
        <span className="inline-flex items-center gap-1.5">
          <Trash2 size={14} />
          删除节点
        </span>
      </MenuItem>
    </Menu>
  );
}
