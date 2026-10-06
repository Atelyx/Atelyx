/**
 * 协作空间条目右键菜单：重新连接 / 重命名 / 成员管理 / 邀请码 / 断开连接（断开只移除本机最近条目）。
 * 重命名由面板走 inline 输入、断开由面板弹确认框，本组件只派发意图。
 */
import { Pencil, RefreshCw, Ticket, Unlink, Users } from "lucide-react";
import { Menu, MenuItem } from "@/components/common/Menu";

export interface SpaceMenuProps {
  serverUrl: string;
  spaceId: string;
  name: string;
  /** 本账号在该空间的成员角色（owner/editor；空串 = 未知，服务端裁决）。 */
  role: string;
  x: number;
  y: number;
  onClose: () => void;
  onRename: () => void;
  onMembers: () => void;
  onInvite: () => void;
  onDisconnect: () => void;
  onReconnect: () => void;
}

export function SpaceMenu({
  x,
  y,
  onClose,
  onRename,
  onMembers,
  onInvite,
  onDisconnect,
  onReconnect,
}: SpaceMenuProps) {
  /** 各项：执行动作后关菜单（与其余右键菜单同语义）。 */
  const item = (
    label: string,
    icon: React.ReactNode,
    onClick: () => void,
    danger = false,
  ) => (
    <MenuItem
      danger={danger}
      onClick={() => {
        onClick();
        onClose();
      }}
    >
      <span className="inline-flex items-center gap-1.5">
        {icon}
        {label}
      </span>
    </MenuItem>
  );

  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-48" stopPointerDown>
      {item("重新连接", <RefreshCw size={14} />, onReconnect)}
      {item("重命名", <Pencil size={14} />, onRename)}
      {item("成员管理", <Users size={14} />, onMembers)}
      {item("邀请码", <Ticket size={14} />, onInvite)}
      {item("断开连接", <Unlink size={14} />, onDisconnect, true)}
    </Menu>
  );
}
