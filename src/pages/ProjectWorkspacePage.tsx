/**
 * 工作区页面（可自定义布局）。
 *
 * 全局 chrome：标题栏（仓库名 + 布局 tabs + 右操作区（设置/全屏/窗口控制））。
 * 面板网格由 `WorkspaceGrid` 按激活布局渲染，
 * 文件打开/关闭/恢复联动在此层（跨 store 一致性），视图渲染全在面板内部。
 */
import { Maximize, Settings } from "lucide-react";
import { useEffect } from "react";
import { useAppStore } from "@/stores/appStore";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { useVaultStore, lastFolderRenameTarget, lastNoteRenameTarget, lastTableRenameTarget } from "@/stores/vaultStore";
import { SettingsModal } from "@/components/settings/SettingsModal";
import { TitleBarControls } from "@/components/common/TitleBarControls";
import { LayoutTabs } from "@/components/layout/LayoutTabs";
import { WorkspaceGrid } from "@/components/layout/WorkspaceGrid";
import { SlotListMount } from "@/components/plugins/SlotHost";
import { noteTitleFromFile, tableTitleFromFile } from "@/utils/filename";
import { HOME_LAYOUT_ID, type FileTreeNode } from "@/types";

export function ProjectWorkspacePage() {
  const toggleFullscreen = useAppStore((s) => s.toggleFullscreen);
  const minimizeWindow = useAppStore((s) => s.minimizeWindow);
  const toggleMaximizeWindow = useAppStore((s) => s.toggleMaximizeWindow);
  const closeWindow = useAppStore((s) => s.closeWindow);

  // 设置弹窗（全局：面板内「前往设置」入口经 openSettings 打开，可指定初始 tab）
  const settingsModal = useAppStore((s) => s.settingsModal);
  const openSettings = useAppStore((s) => s.openSettings);
  const closeSettings = useAppStore((s) => s.closeSettings);

  // 当前打开文件状态（面板渲染入口；打开动作在 appStore，联动 effect 在此层）
  const currentCanvasFile = useAppStore((s) => s.currentCanvasFile);
  const currentNoteFile = useAppStore((s) => s.currentNoteFile);
  const currentTableFile = useAppStore((s) => s.currentTableFile);
  const vaultId = useAppStore((s) => s.vaultId);

  const vaultNoteList = useVaultStore((s) => s.noteList);
  const vaultTableList = useVaultStore((s) => s.tableList);

  // 应用级 UI 使用状态：上次打开的画布/笔记/表格，进仓库自动恢复
  const uiLoaded = useUiStateStore((s) => s.loaded);
  const lastCanvasFile = useUiStateStore((s) => s.lastCanvasFile);
  const lastNoteFile = useUiStateStore((s) => s.lastNoteFile);
  const lastTableFile = useUiStateStore((s) => s.lastTableFile);
  const autoRestoreFiles = useSettingsStore((s) => s.autoRestoreFiles);

  // 当前打开的文件状态与激活布局（面板网格渲染入口）
  const activeLayoutId = useUiStateStore((s) => s.activeLayoutId);
  const activeTree = useUiStateStore((s) => {
    const layout =
      s.workspaceLayouts.find((l) => l.id === s.activeLayoutId) ?? s.workspaceLayouts[0];
    return layout.tree;
  });

  // 当前打开的笔记从列表消失 → 区分处理：软件内重命名/文件夹重命名（切到新文件）；真删除/外部删除（关闭笔记）
  useEffect(() => {
    if (!currentNoteFile) return;
    const stillExists = vaultNoteList.some((n) => n.file === currentNoteFile);
    if (!stillExists) {
      const newFile =
        lastNoteRenameTarget(currentNoteFile) ?? lastFolderRenameTarget(currentNoteFile);
      if (newFile) {
        useAppStore.getState().openNote(
          newFile,
          noteTitleFromFile(newFile),
        );
      } else {
        useAppStore.getState().closeNote();
      }
    }
  }, [vaultNoteList, currentNoteFile]);
  // 当前打开的表格从列表消失 → 同笔记：软件内重命名/文件夹重命名切到新文件（重载内容）；
  // 真删除/外部删除静默关闭（不 flush——防写回重建已删文件，只清内存态）
  useEffect(() => {
    if (!currentTableFile) return;
    const stillExists = vaultTableList.some((t) => t.file === currentTableFile);
    if (!stillExists) {
      const newFile =
        lastTableRenameTarget(currentTableFile) ?? lastFolderRenameTarget(currentTableFile);
      if (newFile) {
        const newTitle = tableTitleFromFile(newFile);
        useAppStore.getState().openTable(newFile, newTitle);
      } else {
        useAppStore.getState().closeTableSilent();
      }
    }
  }, [vaultTableList, currentTableFile]);

  // AI 对话面板会话与表格改动落盘：进仓库读盘 + 离开（回仓库选择页/切仓库）时 flush 防 debounce 丢改动，
  // 均已归入领域生命周期注册表分发（builtin.aichat 的 onVaultEntered/onVaultExit、builtin.table 的 onVaultExit）

  // 历史记录作者登记（应用级全局，三 kind——画布/笔记/表格——共用同一身份）：
  // 身份随协作昵称/设备名变化刷新；未打开笔记时画布/表格历史也能正确署名
  const collabNickname = useSettingsStore((s) => s.collabNickname);
  const collabDevice = useSettingsStore((s) => s.deviceName);
  useEffect(() => {
    useVaultStore.getState().historySetAuthor(
      collabNickname || collabDevice || "用户",
      collabDevice || "",
    );
  }, [collabNickname, collabDevice]);

  /** 进仓库后恢复上次打开的文件（设置「自动恢复上次打开的文件」开启时）。
   * 依赖 uiLoaded（uiState 已从磁盘加载）+ canvases/noteList（文件树已刷新）就绪后才执行，
   * 文件已被外部删除/移动则静默跳过（降级占位，不报错）。
   * openCanvas/openNote/openTable 内部记录「上次打开」；聚焦由 WorkspaceGrid 兜底。 */
  useEffect(() => {
    if (!uiLoaded) return;
    if (!autoRestoreFiles) return;
    const store = useAppStore.getState();
    // 画布：lastCanvasFile 能在当前画布列表命中才打开（文件缺失/已删除则跳过）；
    // 外部白板（.canvas）不在画布列表，从文件树命中后合成行打开（只读查看）
    if (lastCanvasFile && !store.currentCanvasFile) {
      const row = store.canvases.find((c) => c.file === lastCanvasFile);
      if (row) {
        store.openCanvas(row);
      } else if (lastCanvasFile.toLowerCase().endsWith(".canvas")) {
        const hit = findFileInTree(useVaultStore.getState().tree, lastCanvasFile);
        if (hit) {
          store.openCanvas({
            id: hit.path,
            title: hit.name.replace(/\.canvas$/i, ""),
            file: hit.path,
            updatedAt: hit.updatedAt,
          });
        }
      }
    }
    // 笔记：lastNoteFile 能在笔记列表命中才打开（文件缺失/已删除则跳过）
    if (lastNoteFile && !store.currentNoteFile) {
      const note = vaultNoteList.find((n) => n.file === lastNoteFile);
      if (note) store.openNote(note.file, note.name.replace(/\.md$/i, ""));
    }
    // 表格：lastTableFile 能在表格列表命中才打开（文件缺失/已删除则跳过）
    if (lastTableFile && !store.currentTableFile) {
      const table = vaultTableList.find((t) => t.file === lastTableFile);
      if (table) store.openTable(table.file, table.name.replace(/\.atb$/i, ""));
    }
  }, [
    uiLoaded,
    autoRestoreFiles,
    lastCanvasFile,
    lastNoteFile,
    lastTableFile,
    vaultNoteList,
    vaultTableList,
    currentCanvasFile,
    currentNoteFile,
    currentTableFile,
  ]);

  /** 「进仓库时打开主页」开关：每次进入仓库（挂载/切仓库）若开启则激活主页布局。
   * 依赖 uiLoaded：ui-state 从磁盘加载完成前不得激活——否则随后 load 会用磁盘 activeLayoutId 覆盖。 */
  const defaultHomeLayout = useSettingsStore((s) => s.defaultHomeLayout);
  useEffect(() => {
    if (!defaultHomeLayout) return;
    if (!uiLoaded) return;
    const ui = useUiStateStore.getState();
    if (ui.workspaceLayouts.some((l) => l.id === HOME_LAYOUT_ID)) {
      ui.activateLayout(HOME_LAYOUT_ID);
    }
  }, [defaultHomeLayout, vaultId, uiLoaded]);

  /** 全屏切换（视图控制图标，经 store 转发到 services）。 */
  const handleToggleFullscreen = () => {
    void toggleFullscreen().catch((e) => {
      console.error("全屏切换失败", e);
    });
  };

  return (
    <div
      className="h-full w-full flex flex-col"
      style={{ background: "var(--bg-primary)" }}
    >
      <div className="flex-1 flex flex-col min-h-0">
        {/* 标题栏横条：仓库名 + 布局 tabs → 右操作区（设置/全屏/窗口控制，常驻） */}
        <div
          className="h-9 flex items-center gap-1 pl-1 pr-1 flex-shrink-0 select-none"
          style={{ background: "var(--bg-secondary)", borderBottom: "1px solid var(--border)" }}
          data-tauri-drag-region
        >
          <LayoutTabs />

            {/* 右操作区（常驻）：设置 + 全屏（ml-auto 贴右缘，窗口控制在其后） */}
            <div className="ml-auto flex-shrink-0 flex items-center" data-tauri-drag-region>
              {/* 插件贡献区：标题栏右操作区（list 槽，priority 降序；容器避让窗口拖拽） */}
              <span data-tauri-drag-region="false" className="flex items-center">
                <SlotListMount slot="titlebar/right" />
              </span>
              <button
                onClick={(e) => { e.stopPropagation(); openSettings(); }}
                className="w-8 h-8 flex items-center justify-center rounded-md hover:opacity-80"
                style={{ color: "var(--text-secondary)" }}
                title="设置"
                data-tauri-drag-region="false"
              >
                <Settings size={16} />
              </button>
              <button
                onClick={(e) => { e.stopPropagation(); handleToggleFullscreen(); }}
                className="w-8 h-8 flex items-center justify-center rounded-md hover:opacity-80"
                style={{ color: "var(--text-secondary)" }}
                title="全屏"
                data-tauri-drag-region="false"
              >
                <Maximize size={16} />
              </button>
            </div>
            <TitleBarControls
              onMinimize={() => void minimizeWindow()}
              onMaximize={() => void toggleMaximizeWindow()}
              onClose={() => void closeWindow()}
            />
          </div>

          {/* 面板网格（激活布局；key 保证切布局整树重挂，defaultSize 恢复各面板比例） */}
          <div className="flex-1 min-h-0">
            <WorkspaceGrid key={activeLayoutId ?? "default"} tree={activeTree} />
          </div>
        </div>

      {settingsModal && (
        <SettingsModal initialTab={settingsModal.tab} onClose={closeSettings} />
      )}
    </div>
  );
}

/** 在文件树中按相对路径查找文件（恢复上次打开的外部白板用，.canvas 不在画布列表）。 */
function findFileInTree(nodes: FileTreeNode[], path: string): FileTreeNode | null {
  for (const n of nodes) {
    if (n.path === path) return n;
    if (n.isDir) {
      const hit = findFileInTree(n.children, path);
      if (hit) return hit;
    }
  }
  return null;
}
