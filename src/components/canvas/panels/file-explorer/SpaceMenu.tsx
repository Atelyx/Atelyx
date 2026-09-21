/**
 * 协作空间条目右键菜单（文件面板列表层空间区）：
 * 空间设置（该空间的仓库级设置，未激活的空间同样可编辑）/ 重新连接 / 重命名（服务端 + 最近条目同步）/
 * 成员管理 / 邀请码 / 断开连接（仅移除本机条目）。
 *
 * 菜单壳用 `common/Menu`（悬停高亮/视口钳制/Esc 与外点关闭同全项目）。
 * 断开连接经确认弹窗（面板渲染 ConfirmDialog）；重命名走面板 inline 输入。
 */
import { Pencil, RefreshCw, Settings2, Ticket, Unlink, Users } from "lucide-react";
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
  onSettings: () => void;
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
  onSettings,
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
      {item("空间设置", <Settings2 size={14} />, onSettings)}
      {item("重新连接", <RefreshCw size={14} />, onReconnect)}
      {item("重命名", <Pencil size={14} />, onRename)}
      {item("成员管理", <Users size={14} />, onMembers)}
      {item("邀请码", <Ticket size={14} />, onInvite)}
      {item("断开连接", <Unlink size={14} />, onDisconnect, true)}
    </Menu>
  );
}
