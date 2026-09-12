import { AlertTriangle, FileText, Pin, X } from "lucide-react";
import { useState } from "react";
import type { PendingAttachment } from "@/types";
import { Menu, MenuItem } from "@/components/common/Menu";

/**
 * 待发送附件托盘（临时附件通道）。
 * 纯展示组件：缩略图 chip 列表 + 移除；chip 右键「固定到画布」（仅无源附件）。
 * 右键菜单走公共 Menu（portal 到 body + 视口坐标，天然避开 React Flow transform 容器）。
 */
interface Props {
  attachments: PendingAttachment[];
  onRemove: (id: string) => void;
  onPin: (att: PendingAttachment) => void;
}

export function ConversationAttachmentTray({ attachments, onRemove, onPin }: Props) {
  const [menu, setMenu] = useState<{ att: PendingAttachment; x: number; y: number } | null>(null);

  if (attachments.length === 0) return null;

  return (
    <div className="nodrag relative flex flex-wrap gap-1.5 px-2 pt-1.5">
      {attachments.map((att) => (
        <div
          key={att.id}
          className="relative group/att border rounded flex items-center gap-1.5 px-1.5 py-1 text-xs"
          style={{ background: "var(--bg-tertiary)", borderColor: "var(--border)" }}
          // 非文本内容不注入模型，必须让用户看得懂那个警示图标（否则会以为文件已随消息发出）
          title={
            att.parseFailed
              ? `${att.filename ?? ""}（内容不是文本，不会发送给模型）`
              : (att.filename ?? "")
          }
          onContextMenu={(e) => {
            e.preventDefault();
            e.stopPropagation();
            if (att.sourceNodeId) return;
            // 视口坐标（e.clientX/Y）——Menu portal 到 body 后按视口渲染，无需 offsetParent 换算
            setMenu({ att, x: e.clientX, y: e.clientY });
          }}
        >
          {att.kind === "image" && att.payload ? (
            <img
              src={att.payload}
              alt=""
              className="h-8 w-8 object-cover rounded"
              draggable={false}
            />
          ) : (
            <FileText size={18} className="flex-shrink-0" />
          )}
          <span className="max-w-28 truncate">
            {att.filename || (att.kind === "image" ? "图片" : "文件")}
          </span>
          {att.parseFailed && <AlertTriangle size={12} style={{ color: "#f87171" }} />}
          <button
            onClick={() => onRemove(att.id)}
            className="hover:opacity-70 flex-shrink-0"
            title={att.sourceNodeId ? "取消引用（断开边）" : "移除附件"}
          >
            <X size={12} />
          </button>
        </div>
      ))}

      {menu && (
        <Menu x={menu.x} y={menu.y} onClose={() => setMenu(null)} widthClass="w-40" stopPointerDown>
          <MenuItem
            onClick={() => {
              onPin(menu.att);
              setMenu(null);
            }}
          >
            <span className="inline-flex items-center gap-1.5">
              <Pin size={14} />
              固定到画布
            </span>
          </MenuItem>
        </Menu>
      )}
    </div>
  );
}
