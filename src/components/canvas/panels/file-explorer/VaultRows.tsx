/**
 * 仓库树顶级行（与文件树同构的仓库列表）：仓库 = 树的顶级条目，行样式与文件夹行一致。
 *
 * 激活仓库行高亮（与当前打开文件同色）并就地展开其文件树，其余仓库收起；
 * 点击其他仓库 = 激活切换（完整切换流程），点击激活仓库行 = 展开/收起其文件树；
 * 右键菜单：在文件管理器中打开 / 从列表移除（不删文件、不影响激活态）。
 *
 * 分层：只读 appStore + 调 selectVault / removeRecentVault / openInExplorer。
 */
import { useEffect, useState } from "react";
import type { PointerEvent as ReactPointerEvent } from "react";
import { ChevronDown, ChevronRight, HardDrive, Loader2 } from "lucide-react";
import { useAppStore } from "@/stores/appStore";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { useClampedMenuPosition } from "@/hooks/useClampedMenuPosition";
import type { RecentVault, FileTreeNode } from "@/types";
import type { FileTreeProps } from "./FileTree";
import { FileTree } from "./FileTree";

/** 传给内嵌文件树的属性（nodes/depth/parentDir 由仓库行自己给）。 */
export type VaultTreeProps = Omit<FileTreeProps, "nodes" | "depth" | "parentDir">;

interface VaultRowsProps {
  vaults: RecentVault[];
  /** 当前激活仓库 root（null = 未激活任何仓库）。 */
  vaultRoot: string | null;
  /** 切换进行中的目标仓库 root（null = 空闲）：目标行显示加载动画，全部行禁点。 */
  switchingTo: string | null;
  /** 点击仓库行：激活切换（激活仓库行由组件内部处理展开/收起）。 */
  onEnter: (root: string) => void;
  /** 用户收起的激活仓库（面板挂载期间记忆；切换仓库后新激活仓库默认展开）。 */
  collapsedVaults: Set<string>;
  toggleVaultCollapsed: (root: string) => void;
  /** 激活仓库的文件树（仅激活仓库有内容；未激活 = 空数组）。 */
  tree: FileTreeNode[];
  fileTree: VaultTreeProps;
}

/** 仓库行右键菜单（独立组件承载钳制定位：实测尺寸须在挂载后计算）。 */
function VaultMenu({
  root,
  active,
  switching,
  x,
  y,
  onClose,
  onRemove,
}: {
  root: string;
  /** 该仓库是否为当前激活仓库（激活仓库不可移出列表，否则树中无其条目而它仍激活）。 */
  active: boolean;
  /** 该仓库是否为切换进行中的目标（切换中移除会让激活行失去列表条目）。 */
  switching: boolean;
  x: number;
  y: number;
  onClose: () => void;
  onRemove: (root: string) => void;
}) {
  const openInExplorer = useAppStore((s) => s.openInExplorer);
  const { ref, pos } = useClampedMenuPosition(x, y, [root]);

  return (
    <div
      ref={ref}
      className="fixed z-50 rounded-md shadow-2xl py-1 min-w-[180px]"
      style={{ left: pos.x, top: pos.y, background: "var(--bg-tertiary)", border: "1px solid var(--border)" }}
      data-vault-menu
    >
      <button
        onClick={() => {
          void openInExplorer(root);
          onClose();
        }}
        className="w-full text-left px-3 py-1.5 text-sm hover:bg-[var(--hover)]"
        style={{ color: "var(--text-primary)" }}
      >
        在文件管理器中打开
      </button>
      <button
        onClick={() => onRemove(root)}
        disabled={active || switching}
        title={active ? "当前激活的仓库不能移出列表，先切换到其他仓库" : switching ? "正在切换到该仓库" : undefined}
        className="w-full text-left px-3 py-1.5 text-sm hover:bg-[var(--hover)] disabled:opacity-40 disabled:hover:bg-transparent disabled:cursor-default"
        style={{ color: "#f87171" }}
      >
        从列表移除
      </button>
    </div>
  );
}

export function VaultRows({ vaults, vaultRoot, switchingTo, onEnter, collapsedVaults, toggleVaultCollapsed, tree, fileTree }: VaultRowsProps) {
  const [menu, setMenu] = useState<{ root: string; x: number; y: number } | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const removeRecentVault = useAppStore((s) => s.removeRecentVault);

  // 点击菜单/行外部关闭右键菜单（data-vault-menu 区域内不关）
  useEffect(() => {
    if (!menu) return;
    const close = (e: MouseEvent) => {
      if (!(e.target as HTMLElement).closest("[data-vault-menu]")) setMenu(null);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [menu]);

  const onRowContextMenu = (e: ReactPointerEvent | React.MouseEvent, root: string) => {
    e.preventDefault();
    e.stopPropagation();
    setMenu({ root, x: e.clientX, y: e.clientY });
  };

  return (
    <>
      {vaults.map((v) => {
        const active = v.root === vaultRoot;
        const expanded = active && !collapsedVaults.has(v.root);
        const switching = v.root === switchingTo;
        return (
          <li key={v.root}>
            <div
              className="flex items-center gap-1 px-2 py-1 min-h-8 select-none cursor-default rounded-md hover:bg-[var(--hover)]"
              style={{
                // 激活仓库行高亮 = 与当前打开文件行同色
                background: active ? "color-mix(in srgb, var(--accent) 20%, transparent)" : undefined,
              }}
              title={v.root}
              onClick={() => {
                // 切换进行中全部行禁点（重入由 appStore 守卫兜底，此处不发起无谓调用）
                if (switchingTo) return;
                if (active) toggleVaultCollapsed(v.root);
                else onEnter(v.root);
              }}
              onContextMenu={(e) => onRowContextMenu(e, v.root)}
            >
              <span className="flex items-center">{expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}</span>
              {switching ? (
                <Loader2 size={14} className="animate-spin" style={{ color: "var(--accent)" }} />
              ) : (
                <HardDrive size={14} style={{ color: active ? "var(--accent)" : "var(--text-muted)" }} />
              )}
              <span className="flex-1 truncate text-xs" style={{ color: "var(--text-primary)" }}>
                {v.name}
              </span>
            </div>
            {expanded && (
              <ul className="relative">
                {/* 展开指示线：与文件夹展开线同位（仓库行 depth 0） */}
                <div
                  className="absolute top-0 bottom-0 w-px pointer-events-none z-10"
                  style={{ left: 13, background: "var(--text-muted)", opacity: 0.6 }}
                />
                <FileTree nodes={tree} depth={1} parentDir="" {...fileTree} />
              </ul>
            )}
          </li>
        );
      })}

      {menu && (
        <VaultMenu
          root={menu.root}
          active={menu.root === vaultRoot}
          switching={menu.root === switchingTo}
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          onRemove={(root) => {
            setRemoving(root);
            setMenu(null);
          }}
        />
      )}

      {/* 移除确认（只移出列表，不删文件、不影响激活态） */}
      {removing && (
        <ConfirmDialog
          title="从列表移除该仓库？"
          description="只从仓库列表移除入口，不删除磁盘上的任何文件。"
          confirmText="移除"
          onCancel={() => setRemoving(null)}
          onConfirm={() => {
            const root = removing;
            setRemoving(null);
            void removeRecentVault(root);
          }}
        />
      )}
    </>
  );
}
