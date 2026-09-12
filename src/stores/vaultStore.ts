/**
 * 仓库文件树状态 + CRUD（自由文件夹结构，兼容通用笔记工具）。
 *
 * 职责：持有全仓库文件树（`list_vault_tree`，跳过隐藏/排除目录），封装文件管理面板用的
 * 新建/重命名/删除，调 `services/vault`。canvases 列表仍由 `appStore` 维护（与画布 CRUD 同源）。
 * watcher 事件到达时调 `loadFiles` 刷新树。
 *
 * 无固定 画布/笔记/附件 目录：`.md` 笔记可在任意文件夹，`file` 字段即相对仓库根路径
 * （如 `项目A/提示词.md`），不用目录名拼接。
 *
 * 分层：组件不直调 service → FileExplorerPanel 用本 store。
 */
import { create } from "zustand";
import {
  createFolder as createFolderSvc,
  copyVaultFile,
  copyVaultFolder,
  deleteAttachment,
  deleteFolder as deleteFolderSvc,
  deleteNote,
  listVaultTree,
  readAttachmentDataUrl as readAttachmentDataUrlSvc,
  remapSideloads,
  remapSideloadsByDir,
  renameAttachment as renameAttachmentSvc,
  renameFolder as renameFolderSvc,
  renameNote as renameNoteSvc,
  scanWikiBacklinks as scanWikiBacklinksSvc,
  scanVaultTags as scanVaultTagsSvc,
  rebuildInternalLinks as rebuildInternalLinksSvc,
  writeNote,
} from "@/services/vault";
import { migrateHistoryFile, setHistoryAuthor } from "@/services/history";
import {
  createTableVault,
  deleteTableVault,
  moveTableVault,
  readTableVault,
  renameTableVault,
  writeTableVault,
} from "@/services/table";
import { subscribeVaultFileChanges } from "@/services/watcher";
import { isSelfSaveEcho, markSelfSave } from "@/utils/selfSave";
import { useAppStore } from "@/stores/appStore";
import { emitVaultEvent, emitVaultEventAsync, type VaultEvent } from "@/utils/vaultEvents";
import { useSettingsStore } from "@/stores/settingsStore";
import { useUiStateStore } from "@/stores/uiStateStore";
import { baseName, dedupeFilename, parentDir, sanitizeFilename, siblingPath, stripExt } from "@/utils/filename";
import { errText } from "@/types";
import type { BacklinkRow, CanvasFileRow, DeleteFolderResult, FileTreeNode, RebuildLinksResult, TagRow, VaultFileChange } from "@/types";

/**
 * 文本节点 `.md` 文件名约定：`<sanitized-title>.md`（标题即文件名，无 id 后缀）。
 * 同名由 `dedupeFilename` 自动加序号（-2、-3）。重命名同步更新 .atlx 引用。
 */

/**
 * 软件内正在进行的重命名（old → new）。renameNote/renameAttachment 记录，
 * watcher 收到旧路径的删除事件时据此跳过（file 引用已同步，防误标文件缺失）。
 * 谓词供领域订阅者判定（内核只提供事实，不代领域决定是否跳过）。
 */
let pendingRename: { oldFile: string; newFile: string } | null = null;
export function isPendingRenameOldPath(path: string): boolean {
  return pendingRename?.oldFile === path;
}

/** 最近一次软件内笔记重命名（保留跨渲染周期，供窗口联动区分「重命名」与「真删除」）。 */
let lastNoteRename: { oldFile: string; newFile: string } | null = null;
/** 文件若是最近一次重命名的旧路径，返回新路径；否则 null（真删除/外部变化）。 */
export function lastNoteRenameTarget(oldFile: string): string | null {
  return lastNoteRename?.oldFile === oldFile ? lastNoteRename.newFile : null;
}

/** 最近一次软件内表格重命名/移动（同 lastNoteRename，供表格窗口联动）。 */
let lastTableRename: { oldFile: string; newFile: string } | null = null;
/** 文件若是最近一次表格重命名/移动的旧路径，返回新路径；否则 null。 */
export function lastTableRenameTarget(oldFile: string): string | null {
  return lastTableRename?.oldFile === oldFile ? lastTableRename.newFile : null;
}

/** 软件内正在进行的文件夹重命名（old → new）。watcher 收到旧目录下文件事件时据此跳过（同 pendingRename）。 */
let pendingFolderRename: { oldDir: string; newDir: string } | null = null;
/** 路径是否位于最近一次软件内文件夹重命名的旧目录下（watcher 旧路径事件跳过重读）。 */
export function isPendingFolderRenameOldPath(path: string): boolean {
  return pendingFolderRename !== null && path.startsWith(`${pendingFolderRename.oldDir}/`);
}

/** 最近一次软件内文件夹重命名（保留跨渲染周期，供窗口联动区分「重命名」与「真删除」）。 */
let lastFolderRename: { oldDir: string; newDir: string } | null = null;
/** 文件若位于最近一次文件夹重命名的旧目录下，返回 remap 后新路径；否则 null。 */
export function lastFolderRenameTarget(file: string): string | null {
  if (!lastFolderRename) return null;
  const { oldDir, newDir } = lastFolderRename;
  const prefix = `${oldDir}/`;
  return file.startsWith(prefix) ? `${newDir}/${file.slice(prefix.length)}` : null;
}

/** 相对路径的小写扩展名（不含点；无扩展名 = 空串）。AI 文件工具按扩展名分发用。 */
function relExt(path: string): string {
  const i = path.lastIndexOf(".");
  return i > path.lastIndexOf("/") ? path.slice(i + 1).toLowerCase() : "";
}

/** 画布列表行（rename/move/deleteCanvas 按 file 定位、title 供去重排除；列表未命中时用占位行兜底）。 */
function canvasRowOf(file: string): CanvasFileRow {
  const found = useAppStore.getState().canvases.find((c) => c.file === file);
  if (found) return found;
  return { id: "", title: stripExt(baseName(file)), file, updatedAt: 0 };
}

/** loadFiles 并发守卫：递增序号，仅最后一次发起者的扫描结果落盘（后台填充与 watcher 触发并发时防旧结果覆盖）。 */
let loadFilesSeq = 0;

/** 文件监听订阅状态（startFileWatcher 幂等启停用）。
 * watcherGen = 订阅代数：每次启/停递增，在途订阅完成时校验代数一致才保留——
 * 防「enable → disable → enable」竞态下旧订阅结果覆盖新订阅、前一个 unlisten 丢失泄漏（双监听常驻）。 */
let watcherActive = false;
let watcherUnlisten: (() => void) | undefined;
let watcherGen = 0;

/**
 * 投递文件动作事件：领域反应失败不阻断内核簿记。
 * 侧文件迁移/提示词重映射/「上次打开」同步等属内核职责，不能因某个订阅方（含用户插件）抛错被跳过；
 * 领域订阅方各自负责自身失败降级（此处只记日志）。
 */
async function emitFileEvent(event: VaultEvent): Promise<void> {
  try {
    await emitVaultEventAsync(event);
  } catch (e) {
    console.error("仓库文件事件投递失败", e);
  }
}

/**
 * renameNote/moveNote 共用核心：pendingRename 记录 + 服务调用 + 自写抑制 + 自动保存基准 +
 * 画布节点同步（text file）+ 树刷新 + 重命名记录。
 * newTitle 为 null = 移动（title 不变，只改 file）；否则 = 重命名（title 一并更新）。
 */
async function applyNoteFileChange(oldFile: string, newFile: string, newTitle: string | null): Promise<void> {
  pendingRename = { oldFile, newFile };
  try {
    const { rewritten } = await renameNoteSvc(oldFile, newFile);
    // rename_note 会扫描更新所有 .atlx 的 file 引用（写 .atlx），标记自写抑制 watcher 误报
    markSelfSave();
    // 磁盘 .atlx 已变：画布订阅者据 `note:renamed|moved` 同步乐观锁基准与节点 file/title，
    // 须在函数返回前完成（下一次自动保存依赖基准已更新）；撤销栈路径迁移同由笔记订阅者承担。
    // `rewritten`（被代写正文的其它笔记）随事件下发：它们的自写回波被上面的抑制窗口吞掉，
    // 订阅方只能据此作废其正文缓存。
    await emitFileEvent({
      kind: newTitle === null ? "note:moved" : "note:renamed",
      oldPath: oldFile,
      newPath: newFile,
      newTitle,
      rewritten,
    });
    await useVaultStore.getState().loadFiles();
    // 记录本次重命名（跨渲染保留）：窗口联动据此把打开的笔记切到新文件，而非误判删除关闭
    lastNoteRename = { oldFile, newFile };
    // 侧文件先确保在新编码名下（存量旧编码侧文件迁移），再随重命名迁移——
    // Rust remap_sideloads 只按新编码名查找，未迁移则旧文件在重命名后孤儿化
    await migrateHistoryFile("note", oldFile).catch(() => {});
    // 历史侧文件随迁（新路径继续累积、旧版本不丢）；失败静默降级，不阻塞重命名主流程
    await remapSideloads(oldFile, newFile).catch(() => {});
    // 系统提示词标记按路径引用：重命名/移动后同步 promptNotes，防标记指向旧路径失效
    await useSettingsStore.getState().remapPromptNote(oldFile, newFile);
    // Agent 引用的提示词笔记同款同步（agents.json 的 systemPromptFile 指向旧路径失效）
    await useSettingsStore.getState().remapAgentPromptNote(oldFile, newFile);
    // 「上次打开」的笔记随路径更新（否则下次进入仓库尝试恢复旧路径）
    useUiStateStore.getState().renameLastFile("note", oldFile, newFile);
  } finally {
    pendingRename = null;
  }
}

/** renameAttachment/moveAttachment 共用核心（media 节点 file 同步归画布订阅者）。 */
async function applyAttachmentFileChange(
  oldFile: string,
  newFile: string,
  kind: "attachment:renamed" | "attachment:moved",
): Promise<void> {
  pendingRename = { oldFile, newFile };
  try {
    await renameAttachmentSvc(oldFile, newFile);
    // rename_attachment 会扫描更新所有 .atlx 的 media 引用（写 .atlx），标记自写抑制 watcher 误报
    markSelfSave();
    // 磁盘 .atlx 已变：画布订阅者同步乐观锁基准 + 引用该附件的 media 节点 file（防回写覆盖旧值）
    await emitFileEvent({
      kind,
      oldPath: oldFile,
      newPath: newFile,
    });
    await useVaultStore.getState().loadFiles();
  } finally {
    pendingRename = null;
  }
}

/**
 * renameTable/moveTable 共用核心：pendingRename 记录 + 服务调用 + 自写抑制 + 树刷新 + 重命名记录；
 * 画布 table 节点引用同步与乐观锁基准由画布订阅者据事件承担（模式同 applyNoteFileChange）。
 * 服务命令内部已按 title/新路径扫描更新全部 .atlx 的 table 节点引用。
 */
async function applyTableFileChange(oldFile: string, newFile: string, newTitle: string | null): Promise<void> {
  pendingRename = { oldFile, newFile };
  try {
    if (newTitle !== null) {
      await renameTableVault(oldFile, newTitle);
    } else {
      await moveTableVault(oldFile, newFile);
    }
    markSelfSave();
    await emitFileEvent({
      kind: newTitle === null ? "table:moved" : "table:renamed",
      oldPath: oldFile,
      newPath: newFile,
      newTitle,
    });
    await useVaultStore.getState().loadFiles();
    lastTableRename = { oldFile, newFile };
    // 侧文件先确保在新编码名下，再随重命名迁移（同 applyNoteFileChange）
    await migrateHistoryFile("table", oldFile).catch(() => {});
    // 历史侧文件随迁（表格 kind 目录）；失败静默降级
    await remapSideloads(oldFile, newFile).catch(() => {});
    // 「上次打开」的表格随路径更新（否则下次进入仓库尝试恢复旧路径）
    useUiStateStore.getState().renameLastFile("table", oldFile, newFile);
  } finally {
    pendingRename = null;
  }
}

/** renameFolder/moveFolder 共用核心：pendingFolderRename 记录 + 服务调用 + 自写抑制 + 树/列表刷新；
 *  画布打开路径与节点引用的前缀同步归画布订阅者（据 `folder:renamed|moved` 事件）。 */
async function applyFolderFileChange(
  oldDir: string,
  newDir: string,
  kind: "folder:renamed" | "folder:moved",
): Promise<void> {
  pendingFolderRename = { oldDir, newDir };
  try {
    const { rewritten } = await renameFolderSvc(oldDir, newDir);
    // 立即记录本次重命名/移动（跨渲染保留）：目录已移动，后续任何渲染间隙的窗口联动
    // 据此把打开的笔记切到新文件，而非误判删除关闭（放 loadFiles/loadList 之后
    // 会留出 IPC await 间隙，联动 effect 先跑导致笔记窗口被误关）
    lastFolderRename = { oldDir, newDir };
    // 目录下全部历史侧文件随迁（解码文件名按前缀改写）；失败静默降级
    await remapSideloadsByDir(oldDir, newDir).catch(() => {});
    // rename_folder 会扫描更新所有 .atlx 的目录前缀引用（写 .atlx），标记自写抑制 watcher 误报
    markSelfSave();
    // 当前画布文件若位于该目录下：先同步打开路径（旧路径已不存在，方法内部自带前缀守卫）；
    // 画布订阅者再同步其运行时路径/乐观锁基准/节点前缀引用（磁盘 .atlx 已被 rename_folder 更新
    // updatedAt，防下次保存误判「已被外部修改」）
    useAppStore.getState().renameCurrentCanvasFile(oldDir, newDir);
    // `rewritten`（被代写正文的笔记，可能在目录前缀之外）随事件下发：自写回波被抑制窗口吞掉，
    // 订阅方只能据此作废其正文缓存
    await emitFileEvent({
      kind,
      oldDir,
      newDir,
      rewritten,
    });
    // 系统提示词标记 / 文件夹图标颜色 / 展开集合 / 上次打开文件：前缀同步（防标记与恢复指向失效路径）
    await useSettingsStore.getState().remapPromptNotesByDir(oldDir, newDir);
    await useSettingsStore.getState().remapAgentPromptNotesByDir(oldDir, newDir);
    await useSettingsStore.getState().remapFolderColorsByDir(oldDir, newDir);
    useUiStateStore.getState().renameByDir(oldDir, newDir);
    await useVaultStore.getState().loadFiles();
    await useAppStore.getState().loadList();
  } finally {
    pendingFolderRename = null;
  }
}

/** 递归提取树中指定扩展名的文件（.md 笔记 / .atb 表格两个收集器共用，仅扩展名不同）。 */
function collectByExt(
  nodes: FileTreeNode[],
  ext: string,
): { name: string; file: string }[] {
  const out: { name: string; file: string }[] = [];
  for (const n of nodes) {
    if (n.isDir) {
      out.push(...collectByExt(n.children, ext));
    } else if (n.name.toLowerCase().endsWith(ext)) {
      out.push({ name: n.name, file: n.path });
    }
  }
  return out;
}

/** 按相对路径查树节点（dir = "" 返回根容器）。 */
function findNode(nodes: FileTreeNode[], path: string): FileTreeNode | null {
  for (const n of nodes) {
    if (n.path === path) return n;
    if (n.isDir) {
      const hit = findNode(n.children, path);
      if (hit) return hit;
    }
  }
  return null;
}

/** 取某文件夹下的条目名集合（dirsOnly = 只取文件夹；否则只取文件），用于同目录防重名。 */
function siblingNames(dir: string, dirsOnly: boolean): string[] {
  const tree = useVaultStore.getState().tree;
  const node = dir === "" ? { children: tree } : findNode(tree, dir);
  return (node?.children ?? [])
    .filter((c) => (dirsOnly ? c.isDir : !c.isDir))
    .map((c) => c.name);
}

/** 取某文件夹下的文件名集合（不含子目录），用于同目录防重名。 */
function siblingFileNames(dir: string): string[] {
  return siblingNames(dir, false);
}

/** 取某文件夹下的文件夹名集合，用于同目录文件夹防重名。 */
function siblingDirNames(dir: string): string[] {
  return siblingNames(dir, true);
}

/** 复制文件为同目录副本（dedupe 防重名 + 刷新树），返回新相对路径。duplicateNote/duplicateAttachment 共用。 */
async function applyFileDuplicate(file: string): Promise<string> {
  const dir = parentDir(file);
  const name = dedupeFilename(baseName(file), siblingFileNames(dir));
  const newFile = dir ? `${dir}/${name}` : name;
  await copyVaultFile(file, newFile);
  await useVaultStore.getState().loadFiles();
  return newFile;
}

/** 历史版本类型（历史面板三 kind 共用；组件不直连 service，经本 store 取类型）。 */
export type { HistoryVersion } from "@/services/history";

interface VaultFileState {
  /** 全仓库文件树（递归，跳过隐藏/排除目录与 `.tmp`）。 */
  tree: FileTreeNode[];
  /** 全部 `.md` 笔记（递归提取，file = 相对仓库根路径；系统提示词下拉/存在检查用）。 */
  noteList: { name: string; file: string }[];
  /** 全部 `.atb` 表格（递归提取，file = 相对仓库根路径；表格窗口联动/AI 填行目标选择用）。 */
  tableList: { name: string; file: string }[];
  /** 按相对路径查树节点类型（dir/file；不存在 = null）。@引用 路径块的目录 `/` 后缀标注用。 */
  pathKind: (path: string) => "dir" | "file" | null;
  /** 全仓库标签词汇表（属性区 tags 候选建议数据源；按需加载，Rust 侧指纹缓存保证开销可控）。 */
  vaultTags: TagRow[] | null;
  /** 拉取全仓库标签词汇表（失败静默置 null，调用方降级为无候选）。 */
  loadVaultTags: () => Promise<void>;
  /** 拉取全仓库文件树（watcher 事件/挂载时调用）。canvases 走 appStore.loadList。 */
  loadFiles: () => Promise<void>;
  /**
   * 新建空 `.md` 笔记，返回相对路径（`<dir>/<name>.md`，dir 空 = 根目录；同名自动加序号）。
   * 调用方拿到路径后可进入 inline 重命名。
   */
  createNote: (title: string, dir?: string) => Promise<string>;
  /**
   * 重命名 `.md`：新路径 = 同目录 `<sanitized-newTitle>.md`（同名自动加序号，排除自身）。
   * 返回实际落盘路径（被去重时 ≠ 期望名，调用方据此提示）。
   * 服务端 `rename_note` 会同步更新所有 .atlx 的 text 节点 file 引用。
   */
  renameNote: (oldFile: string, newTitle: string) => Promise<string>;
  /**
   * 移动 `.md` 到目标文件夹（保持文件名，目标同名自动加序号；同目录 = no-op 返回原路径），
   * 返回实际落盘路径（被去重时 ≠ 目标名，调用方据此提示）。同 renameNote 更新引用/树/窗口联动。
   */
  moveNote: (oldFile: string, targetDir: string) => Promise<string>;
  /** 删除 `.md`（不更新 .atlx 引用，断链由前端 TextNode 显示空正文降级）。 */
  deleteNote: (file: string) => Promise<void>;
  /**
   * 复制 `.md` 为同目录副本（同名自动加序号，如 `笔记-2.md`），返回新相对路径。
   * 副本是独立文件，不更新 .atlx 引用、不自动打开。
   */
  duplicateNote: (file: string) => Promise<string>;
  /**
   * 重命名附件：新文件名含扩展名由调用方给。dedupe 防与同目录现有文件重名。
   * 服务端 `rename_attachment` 同步更新所有 .atlx 的 media 节点 file 引用。
   */
  renameAttachment: (oldFile: string, newName: string) => Promise<void>;
  /**
   * 移动附件到目标文件夹（保持文件名，目标同名自动加序号；同目录 = no-op 返回原路径），
   * 返回实际落盘路径。同 renameAttachment 更新 media 引用/树。
   */
  moveAttachment: (oldFile: string, targetDir: string) => Promise<string>;
  /** 删除附件。 */
  deleteAttachment: (file: string) => Promise<void>;
  /** 复制附件为同目录副本（同名自动加序号），返回新相对路径。 */
  duplicateAttachment: (file: string) => Promise<string>;
  /**
   * 新建空 `.atb` 表格（自带「名称」文本字段；同名自动加序号），
   * 返回 { id, file, title }（title 可能被去重改名）。
   */
  createTable: (title: string, dir?: string) => Promise<{ id: string; file: string; title: string }>;
  /**
   * 重命名 `.atb`：新路径 = 同目录 `<sanitized-newTitle>.atb`（同名自动加序号，排除自身）。
   * 返回实际落盘路径（被去重时 ≠ 期望名，调用方据此提示）。
   * 服务端 `rename_table_vault` 同步更新所有 .atlx 的 table 节点 file 引用 + 文件内 title。
   */
  renameTable: (oldFile: string, newTitle: string) => Promise<string>;
  /** 移动 `.atb` 到目标文件夹（保持文件名，目标同名自动加序号；同目录 = no-op），同步 table 节点引用。 */
  moveTable: (oldFile: string, targetDir: string) => Promise<string>;
  /** 删除 `.atb`（不更新 .atlx 引用，画布 table 节点断链降级）。 */
  deleteTable: (file: string) => Promise<void>;
  /**
   * 同目录重命名任意仓库文件（AI 工具入口，扩展名分发到对应动作；.md/.atb/.atlx 标题随文件名同步）。
   * newName 须为纯文件名（含扩展名，扩展名不可变更）；目标重名自动加序号，
   * actualPath 恒为实际落盘路径。失败 ok=false 不抛断。
   */
  renameFile: (oldPath: string, newName: string) => Promise<{ ok: boolean; summary: string; actualPath: string }>;
  /**
   * 移动任意仓库文件到目标文件夹（AI 工具入口，扩展名分发到对应动作；保持文件名）。
   * targetDir 为相对仓库根目录（空串 = 仓库根）；目标重名自动加序号，
   * actualPath 恒为实际落盘路径。失败 ok=false 不抛断。
   */
  moveFile: (oldPath: string, targetDir: string) => Promise<{ ok: boolean; summary: string; actualPath: string }>;
  /** 按路径删除任意单个仓库文件（AI 工具入口；.atb 连带删除其私有附件目录）。失败 ok=false 不抛断。 */
  deleteFile: (path: string) => Promise<{ ok: boolean; summary: string }>;
  /**
   * 复制 `.atb` 为同目录副本：内部 title/id 随新文件名更新（标题即文件名），返回新相对路径。
   * 副本无画布引用，不自动打开。
   */
  duplicateTable: (file: string) => Promise<string>;
  /** 新建文件夹（相对仓库根路径，如 `项目A/素材`，自动建父目录），返回相对路径。 */
  createFolder: (dir: string) => Promise<string>;
  /**
   * 删除文件夹（相对仓库根路径）。force=false 空目录直接删；非空返回 needsConfirm（未删），
   * 调用方弹窗确认后以 force=true 递归删除。删除成功后清理：目录内画布（当前打开的复位画布状态）、
   * 上次打开文件、展开集合，并刷新数据源。
   */
  deleteFolder: (dir: string, force?: boolean) => Promise<DeleteFolderResult>;
  /**
   * 重命名文件夹：新路径 = 同父目录 `<sanitized-newTitle>`（同名自动加序号，排除自身）。
   * 返回实际落盘的目录路径（被去重时 ≠ 期望名，调用方据此提示）。
   * 服务端 `rename_folder` 同步更新所有 .atlx 的目录前缀引用；前端同步当前画布节点引用/打开路径/
   * 提示词标记/UI 状态，并记录 `lastFolderRename` 供窗口联动切到新路径。
   */
  renameFolder: (oldDir: string, newTitle: string) => Promise<string>;
  /**
   * 移动文件夹到目标目录（保持目录名，目标同名自动加序号，排除自身）。返回实际落盘的目录路径。
   * 非法嵌套（移到自身/自身后代）静默 no-op 返回原路径；移到自身祖先 = 原地 no-op。
   * 引用/状态联动同 `renameFolder`（共用 `rename_folder` 服务）。
   */
  moveFolder: (oldDir: string, targetDir: string) => Promise<string>;
  /**
   * 复制文件夹为同父目录副本（递归复制全部内容，同名自动加序号），返回新相对路径。
   * 副本是独立目录，内部相对路径引用随整体复制仍有效，无需链接维护。
   */
  duplicateFolder: (dir: string) => Promise<string>;
  /** 查询反链（`[[笔记名]]` 或 `[label](基于仓库的路径)`；Rust 索引缓存，组件不直调 service，走本 store）。 */
  scanWikiBacklinks: (noteName: string, noteFile: string) => Promise<BacklinkRow[]>;
  /** 一键重建内部链接（设置 → 编辑器入口；Rust 字节级跨度改写，组件不直调 service，走本 store）。 */
  rebuildInternalLinks: () => Promise<RebuildLinksResult>;
  /** 读附件为 dataURL（仅图片扩展名；失败抛错由调用方降级）。组件不直调 service，走本 store。 */
  readAttachmentDataUrl: (file: string) => Promise<string>;
  /** 设置历史记录作者（进入仓库/身份变化时调用；画布/笔记/表格共用的署名）。 */
  historySetAuthor: (name: string, device: string) => void;
  /**
   * 仓库文件监听启停（幂等）：订阅 Rust watcher 事件并按 kind 分发到各 store。
   * 工作区挂载时 enable（App.tsx 调），回启动页 disable。分层：订阅副作用归 store，组件不直连 service。
   */
  startFileWatcher: (enabled: boolean) => void;
}

/**
 * 重命名文件公共骨架（note/table 共用）：新标题净化 + 补扩展名 + 同目录防重名 + no-op 判断。
 * 落盘变更由 apply 注入（各文件类型的引用同步差异）；
 * attachment 重命名不走此骨架——用户输入保留原样（不净化、不补扩展名）。
 */
async function renameVaultFile(
  oldFile: string,
  newTitle: string,
  ext: string,
  fallback: string,
  apply: (newFile: string) => Promise<void>,
): Promise<string> {
  const oldDir = parentDir(oldFile);
  const base = sanitizeFilename(newTitle) || fallback;
  const newName = dedupeFilename(
    `${base}${ext}`,
    siblingFileNames(oldDir).filter((n) => n !== baseName(oldFile)),
  );
  const newFile = oldDir ? `${oldDir}/${newName}` : newName;
  if (newFile === oldFile) return newFile;
  await apply(newFile);
  return newFile;
}

/** 移动文件公共骨架（note/attachment/table 共用）：保持文件名 + 目标目录防重名 + no-op 判断。 */
async function moveVaultFile(
  oldFile: string,
  targetDir: string,
  apply: (newFile: string) => Promise<void>,
): Promise<string> {
  const name = baseName(oldFile);
  const existing = siblingFileNames(targetDir).filter(
    (n) => (targetDir ? `${targetDir}/${n}` : n) !== oldFile,
  );
  const safe = dedupeFilename(name, existing);
  const newFile = targetDir ? `${targetDir}/${safe}` : safe;
  if (newFile === oldFile) return oldFile;
  await apply(newFile);
  return newFile;
}

export const useVaultStore = create<VaultFileState>((set, get) => ({
  tree: [],
  noteList: [],
  tableList: [],
  vaultTags: null,

  pathKind: (path) => {
    const node = findNode(get().tree, path);
    return node ? (node.isDir ? "dir" : "file") : null;
  },

  loadFiles: async () => {
    const seq = ++loadFilesSeq;
    try {
      const tree = await listVaultTree();
      if (seq !== loadFilesSeq) return;
      set({ tree, noteList: collectByExt(tree, ".md"), tableList: collectByExt(tree, ".atb") });
    } catch (e) {
      console.error("加载仓库文件树失败", e);
    }
  },

  createNote: async (title, dir = "") => {
    const base = sanitizeFilename(title) || "未命名";
    // 同名自动加序号（-2、-3），保证同目录不重名
    const name = dedupeFilename(`${base}.md`, siblingFileNames(dir));
    const file = dir ? `${dir}/${name}` : name;
    await writeNote(file, "");
    await get().loadFiles();
    return file;
  },

  renameNote: async (oldFile, newTitle) => {
    return renameVaultFile(oldFile, newTitle, ".md", "未命名", (newFile) =>
      applyNoteFileChange(oldFile, newFile, newTitle),
    );
  },

  moveNote: async (oldFile, targetDir) => {
    return moveVaultFile(oldFile, targetDir, (newFile) =>
      applyNoteFileChange(oldFile, newFile, null),
    );
  },

  deleteNote: async (file) => {
    await deleteNote(file);
    // 文件已删：清掉该文件的撤销栈与挂起输入（防残留内存；挂起输入不清会在下次 flush 时
    // 经 writeNote 重建已删除文件）——归笔记订阅者（`note:deleted` 事件）
    emitVaultEvent({ kind: "note:deleted", path: file });
    // 删除的是「上次打开」的笔记：清空 uiState 记录（否则下次进入仓库尝试恢复已删文件）
    if (useUiStateStore.getState().lastNoteFile === file) {
      useUiStateStore.getState().closeFile("note");
    }
    await get().loadFiles();
  },

  duplicateNote: async (file) => {
    // 同目录复制，同名自动加序号（-2、-3）；副本不自动打开、不更新引用
    return applyFileDuplicate(file);
  },

  renameAttachment: async (oldFile, newName) => {
    const oldDir = parentDir(oldFile);
    const existing = siblingFileNames(oldDir).filter(
      (n) => (oldDir ? `${oldDir}/${n}` : n) !== oldFile,
    );
    const safe = dedupeFilename(newName.trim() || "未命名", existing);
    const newFile = oldDir ? `${oldDir}/${safe}` : safe;
    if (newFile === oldFile) return;
    await applyAttachmentFileChange(oldFile, newFile, "attachment:renamed");
  },

  moveAttachment: async (oldFile, targetDir) => {
    return moveVaultFile(oldFile, targetDir, (newFile) =>
      applyAttachmentFileChange(oldFile, newFile, "attachment:moved"),
    );
  },

  deleteAttachment: async (file) => {
    await deleteAttachment(file);
    await get().loadFiles();
  },

  duplicateAttachment: async (file) => {
    return applyFileDuplicate(file);
  },

  createTable: async (title, dir = "") => {
    const base = sanitizeFilename(title) || "未命名表格";
    // 同名自动加序号（-2、-3），保证同目录不重名
    const name = dedupeFilename(`${base}.atb`, siblingFileNames(dir));
    const actual = name.replace(/\.atb$/i, "");
    const { id, file } = await createTableVault(actual, dir);
    await get().loadFiles();
    return { id, file, title: actual };
  },

  renameTable: async (oldFile, newTitle) => {
    return renameVaultFile(oldFile, newTitle, ".atb", "未命名表格", (newFile) =>
      applyTableFileChange(oldFile, newFile, baseName(newFile).replace(/\.atb$/i, "")),
    );
  },

  moveTable: async (oldFile, targetDir) => {
    return moveVaultFile(oldFile, targetDir, (newFile) =>
      applyTableFileChange(oldFile, newFile, null),
    );
  },

  deleteTable: async (file) => {
    await deleteTableVault(file);
    // 删除的是「上次打开」的表格：清空 uiState 记录（否则下次进入仓库尝试恢复已删文件）
    if (useUiStateStore.getState().lastTableFile === file) {
      useUiStateStore.getState().closeFile("table");
    }
    await get().loadFiles();
  },

  renameFile: async (oldPath, newName) => {
    const old = oldPath.trim().replace(/^\/+|\/+$/g, "");
    const name = newName.trim().replace(/^\/+|\/+$/g, "");
    if (!old || !name) {
      return { ok: false, summary: "路径为空", actualPath: old || name };
    }
    const ext = relExt(old);
    if (relExt(name) !== ext) {
      return { ok: false, summary: "不允许更改文件扩展名", actualPath: old };
    }
    if (parentDir(name)) {
      return { ok: false, summary: "重命名仅限同目录：newName 须为纯文件名，跨目录请用 move_file", actualPath: old };
    }
    if (name === baseName(old)) {
      return { ok: true, summary: "名称未变化，无需重命名", actualPath: old };
    }
    // 防抖窗口内的未落盘编辑先落盘：改名后旧 timer 的路径守卫会跳过保存，不 flush 会丢编辑
    await useAppStore.getState().flushAllPending();
    try {
      // renameNote/renameTable 返回完整落盘路径（非文件名）
      let actual: string;
      if (ext === "md") actual = await get().renameNote(old, stripExt(name));
      else if (ext === "atb") actual = await get().renameTable(old, stripExt(name));
      else if (ext === "atlx") {
        // renameCanvas 失败会抛错，成功即已落盘（Rust 侧先写新文件再删旧文件）；前端预测名与 Rust
        // 落盘名的一致性由两侧 sanitizeFilename 的对齐用例锁定，不在运行时盘读复核（会多两次 IPC）
        const actualTitle = await useAppStore.getState().renameCanvas(canvasRowOf(old), stripExt(name));
        actual = siblingPath(old, `${sanitizeFilename(actualTitle)}.atlx`);
      } else {
        // 附件类：renameAttachment 内部还会 dedupe，这里预计算同名防「实际名 ≠ 汇报名」
        const existing = siblingFileNames(parentDir(old)).filter((n) => siblingPath(old, n) !== old);
        const safe = dedupeFilename(name, existing);
        await get().renameAttachment(old, safe);
        actual = siblingPath(old, safe);
      }
      const note = actual === siblingPath(old, name) ? "" : "（新名经去重/净化自动调整）";
      return { ok: true, summary: `已重命名「${old}」→「${actual}」${note}`, actualPath: actual };
    } catch (e) {
      return { ok: false, summary: `重命名失败：${errText(e)}`, actualPath: old };
    }
  },

  moveFile: async (oldPath, targetDir) => {
    const old = oldPath.trim().replace(/^\/+|\/+$/g, "");
    const dir = targetDir.trim().replace(/^\/+|\/+$/g, "");
    if (!old) {
      return { ok: false, summary: "路径为空", actualPath: old };
    }
    if (parentDir(old) === dir) {
      return { ok: true, summary: "目标目录与当前目录相同，无需移动", actualPath: old };
    }
    // 防抖窗口内的未落盘编辑先落盘：移动后旧 timer 的路径守卫会跳过保存，不 flush 会丢编辑
    await useAppStore.getState().flushAllPending();
    const ext = relExt(old);
    try {
      let actual: string;
      if (ext === "md") actual = await get().moveNote(old, dir);
      else if (ext === "atb") actual = await get().moveTable(old, dir);
      else if (ext === "atlx") actual = await useAppStore.getState().moveCanvas(canvasRowOf(old), dir);
      else actual = await get().moveAttachment(old, dir);
      return { ok: true, summary: `已移动「${old}」→「${actual}」`, actualPath: actual };
    } catch (e) {
      return { ok: false, summary: `移动失败：${errText(e)}`, actualPath: old };
    }
  },

  deleteFile: async (path) => {
    const p = path.trim().replace(/^\/+|\/+$/g, "");
    if (!p) return { ok: false, summary: "路径为空" };
    // 防抖窗口内的未落盘编辑先落盘，防删除后残留 timer 把旧状态写回重建文件
    await useAppStore.getState().flushAllPending();
    const ext = relExt(p);
    try {
      if (ext === "md") {
        await get().deleteNote(p);
      } else if (ext === "atb") {
        await get().deleteTable(p);
      } else if (ext === "atlx") {
        // deleteCanvas 失败会抛错，成功即已删盘（Rust 侧文件不存在时本就报错）
        await useAppStore.getState().deleteCanvas(canvasRowOf(p));
      } else {
        await get().deleteAttachment(p);
      }
      return { ok: true, summary: `已删除「${p}」` };
    } catch (e) {
      return { ok: false, summary: `删除失败：${errText(e)}` };
    }
  },

  duplicateTable: async (file) => {
    // 读原表 → 重写 title/id 落同目录新文件（「标题即文件名」规范，写盘路径由 title 决定）
    const table = await readTableVault(file);
    const dir = parentDir(file);
    const name = dedupeFilename(baseName(file), siblingFileNames(dir));
    const newFile = dir ? `${dir}/${name}` : name;
    const newTitle = name.replace(/\.atb$/i, "");
    await writeTableVault(
      { ...table, id: crypto.randomUUID(), title: newTitle },
      newFile,
    );
    await get().loadFiles();
    return newFile;
  },

  createFolder: async (dir) => {
    await createFolderSvc(dir);
    await get().loadFiles();
    return dir;
  },

  deleteFolder: async (dir, force = false) => {
    // 目录内 .md 删除后不产生逐文件事件（Rust 目录事件不投递）：删除前收集，成功删除后按 note:deleted 投递，
    // 供笔记域清挂起输入与正文缓存（否则同路径重建的新笔记会读到旧正文、被旧挂起输入覆盖）
    const notesInDir = collectByExt(findNode(get().tree, dir)?.children ?? [], ".md").map((n) => n.file);
    const result = await deleteFolderSvc(dir, force);
    if (!result.deleted) return result;
    for (const file of notesInDir) emitVaultEvent({ kind: "note:deleted", path: file });
    // 目录内画布全部消失：当前打开的画布在目录内 → 清空运行时状态 + 画布槽/标签（同 deleteCanvas 联动）
    const appStore = useAppStore.getState();
    const hadCanvases = appStore.closeCanvasIfInDir(dir);
    // 删除的是「上次打开」的笔记：清空 uiState 记录（否则下次进入仓库尝试恢复已删文件）
    if (useUiStateStore.getState().lastNoteFile?.startsWith(`${dir}/`)) {
      useUiStateStore.getState().closeFile("note");
    }
    // 展开集合清理该目录及子目录条目（残留条目无害但保持整洁）
    useUiStateStore.getState().removeExpandedByDir(dir);
    await get().loadFiles();
    if (hadCanvases) await appStore.loadList();
    return result;
  },

  renameFolder: async (oldDir, newTitle) => {
    const oldParent = parentDir(oldDir);
    const base = sanitizeFilename(newTitle) || "未命名";
    // 同名自动加序号（排除自身，改名到原名不重复）
    const newName = dedupeFilename(
      base,
      siblingDirNames(oldParent).filter((n) => (oldParent ? `${oldParent}/${n}` : n) !== oldDir),
    );
    const newDir = oldParent ? `${oldParent}/${newName}` : newName;
    if (newDir === oldDir) return newDir;
    await applyFolderFileChange(oldDir, newDir, "folder:renamed");
    return newDir;
  },

  moveFolder: async (oldDir, targetDir) => {
    // 保持目录名，目标文件夹同名自动加序号（排除自身 = 同目录移动 no-op）
    const name = baseName(oldDir);
    const existing = siblingDirNames(targetDir).filter(
      (n) => (targetDir ? `${targetDir}/${n}` : n) !== oldDir,
    );
    const safe = dedupeFilename(name, existing);
    const newDir = targetDir ? `${targetDir}/${safe}` : safe;
    if (newDir === oldDir) return oldDir;
    // 非法嵌套（移到自身/自身后代）静默 no-op：目录移进自己内部会让自己消失（各平台行为不一致）
    if (targetDir === oldDir || targetDir.startsWith(`${oldDir}/`)) return oldDir;
    await applyFolderFileChange(oldDir, newDir, "folder:moved");
    return newDir;
  },

  duplicateFolder: async (dir) => {
    const parent = parentDir(dir);
    const name = dedupeFilename(baseName(dir), siblingDirNames(parent));
    const newDir = parent ? `${parent}/${name}` : name;
    await copyVaultFolder(dir, newDir);
    await get().loadFiles();
    // 目录内可能含 .atlx：画布列表同步刷新（否则文件面板画布区不显示副本画布）
    await useAppStore.getState().loadList();
    return newDir;
  },

  scanWikiBacklinks: (noteName, noteFile) => scanWikiBacklinksSvc(noteName, noteFile),
  loadVaultTags: async () => {
    try {
      // 失败静默置 null：候选下拉降级为无建议，不影响手动输入标签
      set({ vaultTags: await scanVaultTagsSvc() });
    } catch {
      set({ vaultTags: null });
    }
  },
  rebuildInternalLinks: async () => {
    // 代写产生的 watcher 事件未被自写抑制（此处不调 markSelfSave）：正文缓存由订阅方按事件作废
    return rebuildInternalLinksSvc();
  },
  readAttachmentDataUrl: (file) => readAttachmentDataUrlSvc(file),

  historySetAuthor: (name, device) =>
    setHistoryAuthor({ id: device || name, name: name || device || "用户", device: device || "" }),

  startFileWatcher: (enabled) => {
    // 幂等：同一状态重复调用不动作（App 的 view effect 可能多次触发相同值）
    if (enabled === watcherActive) return;
    if (!enabled) {
      watcherGen++;
      watcherUnlisten?.();
      watcherUnlisten = undefined;
      watcherActive = false;
      return;
    }
    watcherActive = true;
    const gen = ++watcherGen;
    // 订阅是异步的（listen 往返），期间若被 disable/重新 enable（gen 已递增），
    // 完成时按代数丢弃本次订阅——否则旧订阅覆盖 watcherUnlisten 导致前一个泄漏常驻
    void (async () => {
      const unlisten = await subscribeVaultFileChanges((c: VaultFileChange) => {
        // 内核只做两件事：①不改文件树的内容写（自写回波）跳过全树重扫；②把变化投给领域订阅者。
        // 领域反应（重载/冲突/预览刷新/外部编辑标记/会话合并）与「重命名中旧路径」的抑制判定
        // 归各领域自身（各 kind 口径不同，统一过滤会改变行为）。
        if (c.kind === "chat") {
          // AI 对话历史（.atelyx/对话历史/*.jsonl|*.meta.json）：不刷文件树（.atelyx/ 不在树内）
          emitVaultEvent({ kind: "chat:changed", path: c.path });
          return;
        }
        if (c.kind === "note") {
          emitVaultEvent({ kind: "note:changed", path: c.path });
          if (!isSelfSaveEcho(c.path)) void get().loadFiles();
          return;
        }
        if (c.kind === "table") {
          emitVaultEvent({ kind: "table:changed", path: c.path });
          if (!isSelfSaveEcho(c.path)) void get().loadFiles();
          return;
        }
        if (c.kind === "canvas") {
          // 「当前画布」判据与「是否刷列表/文件树」同属画布域知识（含重载/冲突决策），统一归画布订阅者
          emitVaultEvent({ kind: "canvas:changed", path: c.path });
          return;
        }
        // attachment：画布媒体节点预览刷新归画布订阅者；树结构可能变化，一律重扫
        emitVaultEvent({ kind: "attachment:changed", path: c.path });
        void get().loadFiles();
      });
      if (gen !== watcherGen) {
        unlisten();
      } else {
        watcherUnlisten = unlisten;
      }
    })();
  },
}));
