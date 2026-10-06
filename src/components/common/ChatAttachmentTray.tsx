import { AlertTriangle, FileText, Pin, X } from "lucide-react";
import { useState } from "react";
import type { PendingAttachment } from "@/types";
import { Menu, MenuItem } from "@/components/common/Menu";
import { IconButton } from "@/components/common/Button";

/**
 * 待发送附件托盘（临时附件通道，画布对话节点与 AI 对话面板共用）：缩略图 chip 列表 + 移除。
 * chip 右键「固定到画布」（仅无源附件，面板不传 onPin）；右键菜单走公共 Menu
 * （portal 到 body + 视口坐标，天然避开 React Flow transform 容器）。
 */
interface Props {
  attachments: PendingAttachment[];
  onRemove: (id: string) => void;
  /** 固定到画布（仅画布对话节点传；面板附件无画布归属，不传即无右键菜单）。 */
  onPin?: (att: PendingAttachment) => void;
}

export function ChatAttachmentTray({ attachments, onRemove, onPin }: Props) {
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
            if (att.sourceNodeId || !onPin) return;
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
          {att.parseFailed && <AlertTriangle size={12} style={{ color: "var(--danger)" }} />}
          <IconButton
            onClick={() => onRemove(att.id)}
            className="flex-shrink-0"
            variant="ghost"
            size="xs"
            icon={<X size={12} />}
            label={att.sourceNodeId ? "取消引用（断开边）" : "移除附件"}
          />
        </div>
      ))}

      {menu && (
        <Menu x={menu.x} y={menu.y} onClose={() => setMenu(null)} widthClass="w-40" stopPointerDown>
          <MenuItem
            onClick={() => {
              onPin?.(menu.att);
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
