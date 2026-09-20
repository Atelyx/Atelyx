/**
 * 协作空间条目右键菜单（文件面板列表层空间区）：
 * 重命名（服务端 + 最近条目同步）/ 成员管理 / 邀请码 / 断开连接（仅移除本机条目）/ 重新连接。
 *
 * 独立组件承载钳制定位（实测尺寸须在挂载后计算，与 VaultMenu 同模式）。
 * 断开连接经确认弹窗（面板渲染 ConfirmDialog）；重命名走面板 inline 输入。
 */
import { useEffect } from "react";
import { Pencil, RefreshCw, Ticket, Unlink, Users } from "lucide-react";
import { useClampedMenuPosition } from "@/hooks/useClampedMenuPosition";

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

export function SpaceMenu({ x, y, onClose, onRename, onMembers, onInvite, onDisconnect, onReconnect }: SpaceMenuProps) {
  const { ref, pos } = useClampedMenuPosition(x, y, []);

  // 点击菜单外部关闭（与 VaultMenu 同语义）
  useEffect(() => {
    const close = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest("[data-space-menu]")) onClose();
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [onClose]);

  const item = (label: string, icon: React.ReactNode, onClick: () => void, danger = false) => (
    <button
      onClick={() => {
        onClick();
        onClose();
      }}
      className="w-full text-left px-3 py-1.5 text-sm hover:bg-[var(--hover)] inline-flex items-center gap-2"
      style={{ color: danger ? "#f87171" : "var(--text-primary)" }}
    >
      {icon}
      {label}
    </button>
  );

  return (
    <div
      ref={ref}
      className="fixed z-50 rounded-md shadow-2xl py-1 min-w-[180px]"
      style={{ left: pos.x, top: pos.y, background: "var(--bg-tertiary)", border: "1px solid var(--border)" }}
      data-space-menu
    >
      {item("重新连接", <RefreshCw size={14} />, onReconnect, false)}
      {item("重命名", <Pencil size={14} />, onRename, false)}
      {item("成员管理", <Users size={14} />, onMembers, false)}
      {item("邀请码", <Ticket size={14} />, onInvite, false)}
      {item("断开连接", <Unlink size={14} />, onDisconnect, true)}
    </div>
  );
}
