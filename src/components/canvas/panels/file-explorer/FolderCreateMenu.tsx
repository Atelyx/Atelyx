import {
  Copy,
  FileText,
  FolderPlus,
  Palette,
  Pencil,
  StickyNote,
  Table,
  Trash2,
} from "lucide-react";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import { MenuSlotList } from "@/components/plugins/MenuSlot";

/** 文件夹右键菜单：新建画布 / 新建笔记 / 新建表格 / 新建文件夹 + 图标颜色 + 创建副本 + 重命名 / 删除（根目录仅新建）。 */
export function FolderCreateMenu({
  x,
  y,
  canManage,
  currentColor,
  onCreate,
  onColor,
  onDuplicate,
  onRename,
  onDelete,
  onClose,
}: {
  x: number;
  y: number;
  /** 非根目录才有创建副本/重命名/删除；树空白处右键 = 根目录，仅新建。 */
  canManage: boolean;
  /** 当前文件夹图标颜色（hex；未设置 = undefined）。 */
  currentColor?: string;
  onCreate: (type: "canvas" | "note" | "table" | "folder") => void;
  onColor: () => void;
  onDuplicate: () => void;
  onRename: () => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  return (
    <Menu x={x} y={y} onClose={onClose} widthClass="w-44" stopPointerDown>
      <MenuItem onClick={() => onCreate("canvas")}>
        <FileText size={14} /> 新建画布
      </MenuItem>
      <MenuItem onClick={() => onCreate("note")}>
        <StickyNote size={14} /> 新建笔记
      </MenuItem>
      <MenuItem onClick={() => onCreate("table")}>
        <Table size={14} /> 新建表格
      </MenuItem>
      <MenuItem onClick={() => onCreate("folder")}>
        <FolderPlus size={14} /> 新建文件夹
      </MenuItem>
      {canManage && (
        <>
          <MenuDivider />
          <MenuItem onClick={onColor} title="设置该文件夹的图标颜色（仓库级持久化）">
            <Palette size={14} />
            <span className="flex-1">图标颜色</span>
            {currentColor && (
              <span className="w-3 h-3 rounded-full flex-shrink-0" style={{ background: currentColor }} />
            )}
          </MenuItem>
        </>
      )}
      {canManage && (
        <>
          <MenuDivider />
          <MenuItem onClick={onDuplicate}>
            <Copy size={14} /> 创建副本
          </MenuItem>
          <MenuDivider />
          <MenuItem onClick={onRename}>
            <Pencil size={14} /> 重命名
          </MenuItem>
          <MenuItem onClick={onDelete} danger>
            <Trash2 size={14} /> 删除
          </MenuItem>
        </>
      )}
      {/* 插件贡献区：文件目录行右键菜单（list 槽，priority 降序） */}
      <MenuSlotList target="folder" />
    </Menu>
  );
}
