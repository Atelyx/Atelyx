/**
 * 仓库文件管理面板（仓库树）：仓库 = 树的顶级条目，与文件树无缝同构。
 *
 * 激活仓库行高亮（与当前打开文件同色）并就地展开其文件树（见 `VaultRows`），其余仓库收起；
 * 点击其他仓库 = 激活切换（完整切换流程，切换后该仓库行经最近排序置顶、容器滚回顶部）。
 * 工具条承载仓库级入口：打开文件夹为仓库、新增协作空间（浮层内切创建 / 纳管服务器文件夹 /
 * 输邀请码加入三条途径）。无仓库时树区空态引导打开。
 * 无仓库时树区空态引导创建；树内操作（跳过隐藏 `.` 开头目录与排除文件夹，
 * 见 `.atelyx/config.json` 的 `excludeFolders`）支持展开折叠、排序下拉、
 * 文件夹行右键新建（画布 / 笔记 / 文件夹，inline 输入框 Enter 创建，落该文件夹；
 * 文件树空白处右键 = 落仓库根目录）+ 创建副本 / 重命名 / 删除（空目录直接删，非空弹窗确认递归删）、
 * 文件行右键创建副本 / 重命名 / 删除（菜单内确认）。
 *
 * 交互：
 * - 单击 `.atlx` → 打开画布；单击 `.md` → 打开笔记编辑器；`.md`/附件拖到画布 → 建节点
 * - `.atlx` / `.md` 均可位于任意文件夹（无固定 画布/笔记/附件 目录）
 *
 * 分层：用 `vaultStore`（文件树/笔记 CRUD）+ `appStore`（画布 CRUD/切换/建仓）+ `canvasStore`（建节点），
 * 不直调 service。canvas 相关的定位动作走 props 回调。
 *
 * 递归树渲染 / 指针拖拽 / 文件操作 hooks / 菜单组件 / 纯函数见 `./file-explorer/`。
 */
import {
  ArrowUpDown,
  ChevronsDownUp,
  ChevronsUpDown,
  FolderOpen,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore } from "@/stores/vaultStore";
import { useSpaceAuthStore } from "@/stores/spaceAuthStore";
import { useSpaceDirectoryStore } from "@/stores/spaceDirectoryStore";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { FileContextMenu } from "@/components/canvas/panels/FileContextMenu";
import { ConfirmDialog } from "@/components/common/ConfirmDialog";
import { baseName, noteTitleFromFile, tableTitleFromFile } from "@/utils/filename";
import type { CanvasFileRow } from "@/types";
import { collectDirPaths, DEFAULT_SORT_KEY, isSortKey } from "./file-explorer/sort";
import { useCommitEditing, useDuplicateAction, type Editing, type MenuTarget } from "./file-explorer/actions";
import { useVaultDrag } from "./file-explorer/useVaultDrag";
import { SortMenu } from "./file-explorer/SortMenu";
import { FolderCreateMenu } from "./file-explorer/FolderCreateMenu";
import { FolderColorMenu } from "./file-explorer/FolderColorMenu";
import { VaultRows } from "./file-explorer/VaultRows";
import { SpaceRows, type SpaceEntry } from "./file-explorer/SpaceRows";
import { SpaceAddPopover } from "./file-explorer/SpaceAddPopover";
import { SpaceMenu } from "./file-explorer/SpaceMenu";
import { SpaceMembersDialog } from "./file-explorer/SpaceMembersDialog";
import { SpaceInviteDialog } from "./file-explorer/SpaceInviteDialog";

interface PanelProps {
  /** 单击画布行：打开画布并激活画布窗口（页面层包装 openCanvas + setActiveWindow）。 */
  onOpenCanvasFile: (row: CanvasFileRow) => void;
  /** 单击 `.md`：在工作区主编辑区打开笔记编辑器。 */
  onOpenNoteForEdit: (file: string, title: string) => void;
  /** 单击 `.atb`：在工作区主编辑区打开表格编辑器。 */
  onOpenTableFile: (file: string, title: string) => void;
  /** 当前笔记窗口打开的文件（相对仓库根路径）；笔记区用它高亮当前打开的行（与画布区对称）。 */
  openedNoteFile: string | null;
  /** 当前表格窗口打开的文件（相对仓库根路径）；表格行高亮用（与笔记区对称）。 */
  openedTableFile: string | null;
  /** 右键 `.canvas` 行「转换为画布」：页面层执行转换并打开新画布。 */
  onConvertWhiteboard: (file: string) => void;
}

export function FileExplorerPanel({ onOpenCanvasFile, onOpenNoteForEdit, onOpenTableFile, openedNoteFile, openedTableFile, onConvertWhiteboard }: PanelProps) {
  const vaultRoot = useAppStore((s) => s.vaultRoot);
  const tree = useVaultStore((s) => s.tree);
  const loadFiles = useVaultStore((s) => s.loadFiles);
  const deleteFolder = useVaultStore((s) => s.deleteFolder);
  const deleteNote = useVaultStore((s) => s.deleteNote);
  const deleteTable = useVaultStore((s) => s.deleteTable);
  const deleteAttachment = useVaultStore((s) => s.deleteAttachment);
  // 系统提示词标记（独立落盘 .atelyx/prompt-notes.json）：右键菜单显示注册/注销状态
  const promptFiles = useSettingsStore((s) => s.promptNotes);
  const togglePromptNote = useSettingsStore((s) => s.togglePromptNote);
  // 文件夹图标颜色（独立落盘 .atelyx/folder-colors.json）：右键色板设置/还原
  const folderColors = useSettingsStore((s) => s.folderColors);
  const setFolderColor = useSettingsStore((s) => s.setFolderColor);

  const canvases = useAppStore((s) => s.canvases);
  const currentCanvasFile = useAppStore((s) => s.currentCanvasFile);
  const deleteCanvas = useAppStore((s) => s.deleteCanvas);

  // 展开集合（初始空 = 默认全部折叠：进入仓库只显示顶层文件夹；点文件夹展开）。
  // 展开状态仓库级持久化（uiStateStore → .atelyx/ui-state.json），进入仓库自动恢复上次展开情况
  const expanded = useUiStateStore((s) => s.fileExplorerExpanded);
  const toggleExpanded = useUiStateStore((s) => s.toggleExpanded);
  const toggleExpandAll = useUiStateStore((s) => s.toggleExpandAll);
  // 「展开/收起全部」：收集当前树全部文件夹路径；全部展开时按钮切换为收起
  const dirPaths = useMemo(() => collectDirPaths(tree), [tree]);
  const allExpanded = dirPaths.length > 0 && dirPaths.every((p) => expanded.has(p));
  // 排序方式下拉气泡（图标按钮触发）
  const [sortMenu, setSortMenu] = useState<{ x: number; y: number } | null>(null);

  // 重名自动加序号的提醒（3s 后自动消失）
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice(null), 3000);
    return () => clearTimeout(t);
  }, [notice]);

  // 指针拖拽（拖拽会话 / 幽灵 / 悬停目标高亮）
  const { dragGhost, dropDir, dragHint, startPotentialDrag } = useVaultDrag(setNotice);

  // 非空文件夹删除确认弹窗（空文件夹直接删，无确认）
  const [confirmDelete, setConfirmDelete] = useState<{ dir: string; name: string; count: number } | null>(null);
  /** 删除文件夹：先试非递归（空目录直接删）；非空返回 needsConfirm → 弹窗确认后递归删。 */
  const handleDeleteFolder = useCallback(async (dir: string) => {
    try {
      const res = await deleteFolder(dir);
      if (res.needsConfirm) {
        setConfirmDelete({ dir, name: baseName(dir), count: res.itemCount });
      }
    } catch (err) {
      console.error("删除文件夹失败", err);
      setNotice("删除文件夹失败，请重试");
    }
  }, [deleteFolder]);

  const duplicateAction = useDuplicateAction(setNotice);

  // 排序方式存仓库级配置（.atelyx/config.json），跨会话/跨仓库各自独立
  const vaultSort = useSettingsStore((s) => s.vaultConfig?.fileExplorerSort);
  const setFileExplorerSort = useSettingsStore((s) => s.setFileExplorerSort);
  const sortKey = isSortKey(vaultSort) ? vaultSort : DEFAULT_SORT_KEY;

  useEffect(() => {
    if (vaultRoot) void loadFiles();
  }, [loadFiles, vaultRoot]);

  // 仓库树：激活仓库行收起集合（面板挂载期间记忆；切换仓库后新激活仓库默认展开）
  const [collapsedVaults, setCollapsedVaults] = useState<Set<string>>(new Set());
  const toggleVaultCollapsed = useCallback((root: string) => {
    setCollapsedVaults((prev) => {
      const next = new Set(prev);
      if (next.has(root)) next.delete(root);
      else next.add(root);
      return next;
    });
  }, []);

  const pickVaultDirectory = useAppStore((s) => s.pickVaultDirectory);
  const selectVault = useAppStore((s) => s.selectVault);
  const selectSpace = useAppStore((s) => s.selectSpace);
  const recentVaults = useAppStore((s) => s.recentVaults);
  const recentSpaces = useAppStore((s) => s.recentSpaces);
  const vaultIdentity = useAppStore((s) => s.vaultIdentity);
  const switchingVaultRoot = useAppStore((s) => s.switchingVaultRoot);
  // 空间区数据：登录服务器 + 各服务器空间列表（组件不直调 service，经 spaceDirectoryStore 编排）
  const authServers = useSpaceAuthStore((s) => s.servers);
  const spacesByServer = useSpaceDirectoryStore((s) => s.spacesByServer);
  const loadingByServer = useSpaceDirectoryStore((s) => s.loadingByServer);
  const errorByServer = useSpaceDirectoryStore((s) => s.errorByServer);
  const loadServerSpaces = useSpaceDirectoryStore((s) => s.loadServerSpaces);
  const renameSpace = useSpaceDirectoryStore((s) => s.renameSpace);
  const forgetSpace = useSpaceDirectoryStore((s) => s.forgetSpace);

  // 已登录服务器的空间列表加载（登录态变化即刷新；失败可见，重连/操作会重拉）
  useEffect(() => {
    for (const s of authServers) void loadServerSpaces(s.serverUrl);
  }, [authServers, loadServerSpaces]);

  /** 空间区合并条目：服务端列表（按登录服务器展开）在前，最近条目兜底补齐
   *  （服务端未加载/加载失败/非本机最近的空间仍可见可点）。key = serverUrl#spaceId 去重。 */
  const spaceEntries = useMemo<SpaceEntry[]>(() => {
    const map = new Map<string, SpaceEntry>();
    for (const s of authServers) {
      for (const sum of spacesByServer[s.serverUrl] ?? []) {
        map.set(`${s.serverUrl}#${sum.spaceId}`, {
          serverUrl: s.serverUrl,
          spaceId: sum.spaceId,
          name: sum.name,
          role: sum.role,
        });
      }
    }
    for (const r of recentSpaces) {
      const key = `${r.serverUrl}#${r.spaceId}`;
      if (!map.has(key)) {
        map.set(key, { serverUrl: r.serverUrl, spaceId: r.spaceId, name: r.name, role: "" });
      }
    }
    return [...map.values()];
  }, [authServers, spacesByServer, recentSpaces]);

  const hasServers = authServers.length > 0;
  const spaceListLoading = authServers.some((s) => loadingByServer[s.serverUrl]);
  const spaceListError =
    authServers.map((s) => errorByServer[s.serverUrl]).find((e): e is string => !!e) ?? null;

  // 切换进行中禁掉打开入口，防在切换中叠一次进仓
  const openFolderBusy = switchingVaultRoot !== null;

  /** 打开文件夹为仓库（系统目录选择器；进仓成功后文件树随 vaultRoot 加载）。 */
  const openFolderAsVault = useCallback(async () => {
    if (openFolderBusy) return;
    try {
      const path = await pickVaultDirectory();
      if (path) await selectVault(path);
    } catch (e) {
      console.error("选择文件夹失败", e);
    }
  }, [openFolderBusy, pickVaultDirectory, selectVault]);

  // 树容器：切换仓库后滚回顶部——激活仓库行经最近排序已是第一行，展开的文件树随即从容器顶开始
  const treeScrollRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    treeScrollRef.current?.scrollTo({ top: 0 });
  }, [vaultRoot]);

  // 右键菜单
  const [menu, setMenu] = useState<{ x: number; y: number; target: MenuTarget } | null>(null);
  // 文件夹颜色色板弹层（图标颜色 popup：文件夹路径 + 触发坐标）
  const [colorMenu, setColorMenu] = useState<{ x: number; y: number; dir: string } | null>(null);
  // inline 输入（行内重命名 / 新建草稿）
  const [editing, setEditing] = useState<Editing | null>(null);

  const commitEditing = useCommitEditing({
    onNotice: setNotice,
    onEditingChange: setEditing,
    onOpenCanvasFile,
    onOpenTableFile,
  });

  /** 画布行：从 canvases 列表按 file 找（扫描失败/损坏 .atlx 不在列表，无 row 不提供画布操作）。 */
  const canvasRowOf = (path: string): CanvasFileRow | undefined =>
    canvases.find((c) => c.file === path);

  const openMenu = useCallback((x: number, y: number, target: MenuTarget) => setMenu({ x, y, target }), []);

  // ===== 空间区交互状态 =====
  // 行内重命中的空间条目 key（serverUrl#spaceId）
  const [renamingSpaceKey, setRenamingSpaceKey] = useState<string | null>(null);
  // 成员管理 / 邀请码弹窗
  const [spaceDialog, setSpaceDialog] = useState<{
    kind: "members" | "invite";
    serverUrl: string;
    spaceId: string;
    name: string;
  } | null>(null);
  // 断开连接确认（仅移除本机最近条目，不删服务端数据）
  const [removingSpace, setRemovingSpace] = useState<{ serverUrl: string; spaceId: string; name: string } | null>(null);

  /** 空间行内重命名提交：renameSpace 失败可见（服务端拒绝/网络）。 */
  const handleSpaceRenameCommit = useCallback(
    (key: string, name: string) => {
      const idx = key.indexOf("#");
      const serverUrl = key.slice(0, idx);
      const spaceId = key.slice(idx + 1);
      setRenamingSpaceKey(null);
      renameSpace(serverUrl, spaceId, name)
        .then(() => setNotice("空间已重命名"))
        .catch((e) => {
          console.error("重命名空间失败", e);
          setNotice(`重命名失败：${e instanceof Error ? e.message : String(e)}`);
        });
    },
    [renameSpace],
  );

  /** 内嵌文件树的公共属性（本地仓库行与空间条目同构复用）。 */
  const fileTreeProps = {
    sortKey,
    expanded,
    toggleExpanded,
    editing,
    onEditingChange: setEditing,
    onCommitEditing: commitEditing,
    dropDir,
    folderColors,
    currentCanvasFile,
    openedNoteFile,
    openedTableFile,
    canvasRowOf,
    startPotentialDrag,
    onOpenCanvasFile,
    onOpenNoteForEdit,
    onOpenTableFile,
    onOpenMenu: openMenu,
  };

  return (
    <div
      className="h-full flex flex-col text-sm overflow-hidden"
      style={{ background: "var(--bg-secondary)", color: "var(--text-primary)" }}
    >
      {/* 工具条：打开文件夹（仓库级入口）+ 新增协作空间 + 排序方式下拉气泡 + 展开/收起全部 */}
      <div className="px-2 py-1.5 border-b flex items-center gap-1" style={{ borderColor: "var(--border)" }}>
        {/* 插件贡献区：文件面板工具条左侧（list 槽，priority 降序） */}
        <SlotListMount slot="toolbar/files/left" />
        <button
          onClick={() => void openFolderAsVault()}
          className="flex items-center justify-center w-7 h-7 rounded hover:bg-[var(--hover)]"
          style={{ color: "var(--text-muted)" }}
          title="打开文件夹为仓库"
          disabled={openFolderBusy}
        >
          <FolderOpen size={15} />
        </button>
        <SpaceAddPopover onNotice={setNotice} />
        <button
          onClick={(e) => {
            const rect = e.currentTarget.getBoundingClientRect();
            setSortMenu({ x: rect.right, y: rect.bottom });
          }}
          className="flex items-center justify-center w-7 h-7 rounded hover:bg-[var(--hover)]"
          style={{ color: "var(--text-muted)" }}
          title="排序方式"
        >
          <ArrowUpDown size={14} />
        </button>
        <button
          onClick={() => {
            if (dirPaths.length === 0) return;
            toggleExpandAll(dirPaths);
          }}
          className="flex items-center justify-center w-7 h-7 rounded hover:bg-[var(--hover)]"
          style={{ color: "var(--text-muted)" }}
          title={allExpanded ? "收起全部文件夹" : "展开全部文件夹"}
        >
          {allExpanded ? <ChevronsDownUp size={14} /> : <ChevronsUpDown size={14} />}
        </button>
        {sortMenu && (
          <SortMenu
            x={sortMenu.x}
            y={sortMenu.y}
            value={sortKey}
            onChange={(k) => {
              void setFileExplorerSort(k);
              setSortMenu(null);
            }}
            onClose={() => setSortMenu(null)}
          />
        )}
        {/* 插件贡献区：文件面板工具条（list 槽，priority 降序） */}
        <SlotListMount slot="toolbar/files" />
      </div>

      {/* 重名自动加序号提醒 */}
      {notice && (
        <div
          className="px-3 py-1 text-xs border-b"
          style={{ color: "#f59e0b", borderColor: "var(--border)" }}
        >
          {notice}
        </div>
      )}

      {/* 树容器：仓库 = 顶级条目（激活仓库高亮展开其文件树），空白处右键 = 在仓库根目录新建 */}
      <div
        ref={treeScrollRef}
        className="flex-1 overflow-auto py-1 px-2"
        data-dir=""
        style={{ background: dropDir === "" ? "color-mix(in srgb, var(--accent) 25%, transparent)" : undefined }}
        onContextMenu={(e) => {
          // 无激活仓库无根目录可建：拦截原生菜单保持全面板右键行为一致
          e.preventDefault();
          if (!vaultRoot) return;
          e.stopPropagation();
          setMenu({ x: e.clientX, y: e.clientY, target: { kind: "folder", dir: "" } });
        }}
      >
        {recentVaults.length === 0 && spaceEntries.length === 0 && !hasServers ? (
          <div className="flex flex-col items-center justify-center gap-3 py-14 text-center">
            <p className="text-sm" style={{ color: "var(--text-secondary)" }}>
              还没有仓库
            </p>
            <p className="text-xs" style={{ color: "var(--text-muted)" }}>
              打开一个文件夹作为仓库（可用工具条的打开按钮，或此处）。
            </p>
            <button
              onClick={() => void openFolderAsVault()}
              className="flex items-center gap-1.5 px-2.5 py-1.5 rounded text-xs border"
              style={{ borderColor: "var(--border)", color: "var(--text-secondary)" }}
            >
              <FolderOpen size={13} />
              打开文件夹
            </button>
          </div>
        ) : (
          <ul>
            <VaultRows
              vaults={recentVaults}
              vaultRoot={vaultRoot}
              switchingTo={switchingVaultRoot}
              onEnter={(root) => void selectVault(root)}
              collapsedVaults={collapsedVaults}
              toggleVaultCollapsed={toggleVaultCollapsed}
              tree={tree}
              fileTree={fileTreeProps}
            />
            <SpaceRows
              entries={spaceEntries}
              hasServers={hasServers}
              loading={spaceListLoading}
              listError={spaceListError}
              identity={vaultIdentity}
              switchingTo={switchingVaultRoot}
              tree={tree}
              fileTree={fileTreeProps}
              renamingKey={renamingSpaceKey}
              onRenameCommit={handleSpaceRenameCommit}
              onRenameCancel={() => setRenamingSpaceKey(null)}
              onOpenMenu={openMenu}
            />
          </ul>
        )}
      </div>

      {/* 拖拽幽灵（pointer 模拟拖拽时跟随鼠标；下方追加悬停目标的动作提示） */}
      {dragGhost && (
        <div
          className="fixed z-[9999] pointer-events-none px-2 py-1 rounded shadow-lg"
          style={{
            left: dragGhost.x + 10,
            top: dragGhost.y + 10,
            background: "var(--bg-tertiary)",
            color: "var(--text-primary)",
            border: "1px solid var(--border)",
          }}
        >
          <div className="text-xs">{dragGhost.label}</div>
          {dragHint && (
            <div className="mt-0.5 text-[10px] whitespace-nowrap" style={{ color: "var(--accent)" }}>
              {dragHint}
            </div>
          )}
        </div>
      )}

      {/* 文件夹右键菜单：新建画布 / 新建笔记 / 新建文件夹 + 重命名 / 删除（根目录仅新建） */}
      {(() => {
        const folderTarget = menu?.target.kind === "folder" ? menu.target : null;
        if (!folderTarget) return null;
        return (
          <FolderCreateMenu
            x={menu!.x}
            y={menu!.y}
            canManage={folderTarget.dir !== ""}
            currentColor={folderColors?.[folderTarget.dir]}
            onCreate={(type) => {
              setEditing({ kind: "creating", dir: folderTarget.dir, type, value: "" });
              setMenu(null);
            }}
            onColor={() => {
              const { x, y } = menu!;
              setMenu(null);
              setColorMenu({ x, y, dir: folderTarget.dir });
            }}
            onRename={() => {
              setEditing({
                kind: "folder",
                dir: folderTarget.dir,
                value: baseName(folderTarget.dir),
              });
              setMenu(null);
            }}
            onDuplicate={() => {
              setMenu(null);
              void duplicateAction({ kind: "folder", dir: folderTarget.dir });
            }}
            onDelete={() => {
              setMenu(null);
              void handleDeleteFolder(folderTarget.dir);
            }}
            onClose={() => setMenu(null)}
          />
        );
      })()}

      {/* 文件夹图标颜色色板（右键「图标颜色」打开；预设/自定义/默认，选后即时应用） */}
      {colorMenu && (
        <FolderColorMenu
          x={colorMenu.x}
          y={colorMenu.y}
          currentColor={folderColors?.[colorMenu.dir]}
          onChange={(c) => {
            void setFolderColor(colorMenu.dir, c);
          }}
          onClose={() => setColorMenu(null)}
        />
      )}

      {/* 空间条目右键菜单：重新连接 / 重命名 / 成员管理 / 邀请码 / 断开连接 */}
      {menu?.target.kind === "space" && (() => {
        const t = menu.target;
        return (
          <SpaceMenu
            serverUrl={t.serverUrl}
            spaceId={t.spaceId}
            name={t.name}
            role={t.role}
            x={menu.x}
            y={menu.y}
            onClose={() => setMenu(null)}
            onRename={() => setRenamingSpaceKey(`${t.serverUrl}#${t.spaceId}`)}
            onMembers={() => setSpaceDialog({ kind: "members", serverUrl: t.serverUrl, spaceId: t.spaceId, name: t.name })}
            onInvite={() => setSpaceDialog({ kind: "invite", serverUrl: t.serverUrl, spaceId: t.spaceId, name: t.name })}
            onDisconnect={() => setRemovingSpace({ serverUrl: t.serverUrl, spaceId: t.spaceId, name: t.name })}
            onReconnect={() => {
              void selectSpace({ serverUrl: t.serverUrl, spaceId: t.spaceId, name: t.name });
            }}
          />
        );
      })()}

      {/* 成员管理 / 邀请码弹窗（操作经 spaceDirectoryStore，失败弹窗内可见） */}
      {spaceDialog?.kind === "members" && (
        <SpaceMembersDialog
          serverUrl={spaceDialog.serverUrl}
          spaceId={spaceDialog.spaceId}
          spaceName={spaceDialog.name}
          onClose={() => setSpaceDialog(null)}
        />
      )}
      {spaceDialog?.kind === "invite" && (
        <SpaceInviteDialog
          serverUrl={spaceDialog.serverUrl}
          spaceId={spaceDialog.spaceId}
          spaceName={spaceDialog.name}
          onClose={() => setSpaceDialog(null)}
        />
      )}

      {/* 断开连接确认：只移除本机最近条目，不影响服务端空间与其成员 */}
      {removingSpace && (
        <ConfirmDialog
          title={`断开协作空间「${removingSpace.name}」？`}
          description="只从本机列表移除该空间入口，不影响服务器上的空间与数据；需要时可通过邀请码重新加入。"
          confirmText="断开"
          onCancel={() => setRemovingSpace(null)}
          onConfirm={() => {
            const { serverUrl, spaceId } = removingSpace;
            setRemovingSpace(null);
            void forgetSpace(serverUrl, spaceId).catch(() => setNotice("断开失败，请重试"));
          }}
        />
      )}

      {/* 文件行右键菜单：重命名 / 删除（菜单内确认） */}
      {menu && menu.target.kind !== "folder" && menu.target.kind !== "space" && (() => {
        const t = menu.target;
        return (
          <FileContextMenu
            x={menu.x}
            y={menu.y}
            onRename={() => {
              if (t.kind === "canvas") setEditing({ kind: "canvas", file: t.row.file, value: t.row.title });
              else if (t.kind === "note") setEditing({ kind: "note", file: t.file, value: noteTitleFromFile(t.file) });
              else if (t.kind === "table") setEditing({ kind: "table", file: t.file, value: tableTitleFromFile(t.file) });
              else if (t.kind === "attachment") setEditing({ kind: "attachment", file: t.file, value: t.name });
              setMenu(null);
            }}
            onDuplicate={() => {
              setMenu(null);
              void duplicateAction(t);
            }}
            onDelete={() => {
              if (t.kind === "canvas") void deleteCanvas(t.row).catch(() => setNotice("删除画布失败，请重试"));
              else if (t.kind === "note") void deleteNote(t.file).catch(() => setNotice("删除笔记失败，请重试"));
              else if (t.kind === "table") void deleteTable(t.file).catch(() => setNotice("删除表格失败，请重试"));
              else if (t.kind === "attachment") void deleteAttachment(t.file).catch(() => setNotice("删除附件失败，请重试"));
              setMenu(null);
            }}
            onTogglePrompt={t.kind === "note" ? () => void togglePromptNote(t.file) : undefined}
            promptMarked={t.kind === "note" ? promptFiles.includes(t.file) : undefined}
            onConvert={
              t.kind === "attachment" && t.name.toLowerCase().endsWith(".canvas")
                ? () => onConvertWhiteboard(t.file)
                : undefined
            }
            onClose={() => setMenu(null)}
          />
        );
      })()}

      {/* 非空文件夹删除确认弹窗（确认后递归删除） */}
      {confirmDelete && (
        <ConfirmDialog
          title={`删除文件夹「${confirmDelete.name}」？`}
          description={`文件夹包含 ${confirmDelete.count} 个文件/文件夹，删除后不可恢复。`}
          confirmText="删除"
          onCancel={() => setConfirmDelete(null)}
          onConfirm={() => {
            const { dir } = confirmDelete;
            setConfirmDelete(null);
            void deleteFolder(dir, true).catch(() => setNotice("删除文件夹失败，请重试"));
          }}
        />
      )}
    </div>
  );
}
