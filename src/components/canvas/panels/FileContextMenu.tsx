/**
 * 文件面板行右键菜单：创建副本 / 重命名 / 删除。
 *
 * 「删除」文字恒为红色；点击后**菜单内就地**切确认态（红色「确认删除」+「取消」），
 * 确认才执行删除——不用系统 confirm。
 *
 * 关闭：Esc / 点击菜单外（pointerdown，mousedown 会被树行 pointerdown 的 preventDefault 抑制派发）；
 * 容器 stopPointerDown 阻止事件到达树行处理器，防按钮 click 被宿主抑制。
 */
import { BookmarkMinus, BookmarkPlus, Copy, FileOutput, Pencil, Trash2 } from "lucide-react";
import { useState } from "react";
import { Menu, MenuDivider, MenuItem } from "@/components/common/Menu";
import { MenuSlotList } from "@/components/plugins/MenuSlot";
import { useSpaceViewerOnly } from "@/hooks/useIsSpaceVault";
import { SPACE_VIEWER_NOTICE } from "@/constants/space";

interface Props {
  x: number;
  y: number;
  /** 进入 inline 重命名（复用文件面板的编辑态）。 */
  onRename: () => void;
  /** 创建同目录副本（同名自动加序号），执行完由父级关闭菜单。 */
  onDuplicate: () => void | Promise<void>;
  /** 确认删除后执行（异步删除），执行完由父级关闭菜单。 */
  onDelete: () => void | Promise<void>;
  /** 注册/注销系统提示词（仅 `.md` 行提供；undefined = 非笔记不显示该项）。 */
  onTogglePrompt?: () => void;
  /** 当前笔记是否已注册为系统提示词（决定菜单项文案与图标）。 */
  promptMarked?: boolean;
  /** 转换为画布（仅外部白板 `.canvas` 行提供；undefined = 非白板不显示该项）。 */
  onConvert?: () => void;
  onClose: () => void;
}

export function FileContextMenu({ x, y, onRename, onDuplicate, onDelete, onTogglePrompt, promptMarked, onConvert, onClose }: Props) {
  const [confirming, setConfirming] = useState(false);
  // 协作空间内提示词标记只读：入口禁用防「写被拒不回滚」的虚假可编辑态
  // 提示词标记：本地仓库与空间都会写（空间写团队层，对所有成员生效），仅查看者不可写
  const viewerOnly = useSpaceViewerOnly();

  return (
    <Menu
      x={x}
      y={y}
      onClose={onClose}
      widthClass="w-44"
      repositionDeps={[confirming]}
      stopPointerDown
    >
      {confirming ? (
        // 就地确认态：红色「确认删除」+「取消」
        <div className="px-3 py-1.5">
          <p className="text-xs mb-1.5" style={{ color: "var(--text-muted)" }}>
            删除后不可恢复
          </p>
          <MenuItem
            onClick={() => {
              void onDelete();
              onClose();
            }}
            danger
            className="rounded mb-1"
          >
            <span className="inline-flex items-center gap-1.5">
              <Trash2 size={14} />
              确认删除
            </span>
          </MenuItem>
          <MenuItem
            onClick={() => setConfirming(false)}
            className="rounded"
          >
            取消
          </MenuItem>
        </div>
      ) : (
        <>
          {onConvert && (
            <>
              <MenuItem
                onClick={() => {
                  onConvert();
                  onClose();
                }}
                title="生成同目录 .atlx 画布副本（原文件保留）"
              >
                <span className="inline-flex items-center gap-1.5">
                  <FileOutput size={14} />
                  转换为画布
                </span>
              </MenuItem>
              <MenuDivider />
            </>
          )}
          {onTogglePrompt && (
            <>
              <MenuItem
                disabled={viewerOnly}
                onClick={() => {
                  onTogglePrompt();
                  onClose();
                }}
                title={viewerOnly ? SPACE_VIEWER_NOTICE : promptMarked ? "注销后不再出现在系统提示词候选中" : "注册后可作为对话/面板的系统提示词"}
              >
                <span className="inline-flex items-center gap-1.5">
                  {promptMarked ? <BookmarkMinus size={14} /> : <BookmarkPlus size={14} />}
                  {promptMarked ? "注销提示词" : "注册为提示词"}
                </span>
              </MenuItem>
              <MenuDivider />
            </>
          )}
          <MenuItem
            onClick={() => {
              onDuplicate();
              onClose();
            }}
            title="在同目录创建副本（同名自动加序号）"
          >
            <span className="inline-flex items-center gap-1.5">
              <Copy size={14} />
              创建副本
            </span>
          </MenuItem>
          <MenuDivider />
          <MenuItem
            onClick={() => {
              onRename();
              onClose();
            }}
          >
            <span className="inline-flex items-center gap-1.5">
              <Pencil size={14} />
              重命名
            </span>
          </MenuItem>
          <MenuItem onClick={() => setConfirming(true)} danger>
            <span className="inline-flex items-center gap-1.5">
              <Trash2 size={14} />
              删除
            </span>
          </MenuItem>
          {/* 插件贡献区：文件行右键菜单（list 槽，priority 降序） */}
          <MenuSlotList target="file" />
        </>
      )}
    </Menu>
  );
}
