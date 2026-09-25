/**
 * 内容面契约：仓库内容 I/O 的统一方法面。
 *
 * 三个消费缝——前端服务层 facade（stores/视图）、AI 工具能力注入（ToolCapabilities）、
 * 插件 ctx.vault——都经工厂取后端后走本契约；后端按激活仓库身份分派：
 * 个人仓库 = 本地 Tauri 命令，协作空间 = 空间服务端 API。
 *
 * 契约一律使用仓库内相对路径（`/` 分隔），引用格式（wiki 链接/附件引用/表格图片引用）不变；
 * 返回值即前端运行时形状（如 readTable 的行已归一化，磁盘旧形态不出后端）。
 * 追加写与行级编辑是「读 → 改 → 写」的前端组合逻辑，不设独立后端方法。
 */
import type {
  BacklinkRow,
  CanvasCreateResult,
  CanvasFile,
  CanvasFileRow,
  CanvasPatch,
  DatedNote,
  DeleteFolderResult,
  FileTreeNode,
  GlobVaultResult,
  GrepVaultResult,
  LinkRewriteResult,
  ListDirResult,
  ReadWindowResult,
  RebuildLinksResult,
  RepoHistoryResult,
  TableCreateResult,
  TableFile,
  TablePatch,
  TagRow,
} from "@/types";

/** 仓库运行时身份：个人仓库 = root 绝对路径；协作空间 = serverUrl + spaceId。 */
export type VaultIdentity =
  | { kind: "local"; root: string }
  | { kind: "space"; serverUrl: string; spaceId: string };

/** 表格图片导入源：本机文件字节由前端读为 base64（路径在协作空间不可达，字节统一经前端传输）。 */
export interface TableImageSource {
  fileName: string;
  base64Data: string;
}

export interface ContentBackend {
  // ===== 树与列举 =====
  /** 全仓库文件树（跳过隐藏/排除目录与临时文件）。 */
  listTree(): Promise<FileTreeNode[]>;
  /** 单层列出目录条目（目录在前；dir 缺省 = 仓库根）。 */
  listDir(dir?: string): Promise<ListDirResult>;

  // ===== 读 =====
  /** 读仓库内任意文本文件（超出仓库根/不存在抛错）。 */
  readFile(file: string): Promise<string>;
  /** 分页读文本文件：带绝对行号的窗口（offset 1-based 默认 1；limit 缺省见常量）。 */
  readFileWindow(file: string, opts?: { offset?: number; limit?: number }): Promise<ReadWindowResult>;
  /** 读 .md 笔记正文。 */
  readNote(file: string): Promise<string>;
  /** 文件是否存在（元数据查询不读内容，不限文件类型；文件或所在目录已删除 = false，路径非法抛错）。 */
  fileExists(file: string): Promise<boolean>;
  /** 读 .atlx 画布（磁盘格式）。 */
  readCanvas(file: string): Promise<CanvasFile>;
  /** 读 .atb 表格（行已归一化到内存形态）。 */
  readTable(file: string): Promise<TableFile>;
  /** 枚举画布列表（按 updatedAt 倒序）。 */
  listCanvases(): Promise<CanvasFileRow[]>;
  /** 读附件为 dataURL（`data:<mime>;base64,...`）。 */
  readAttachmentDataUrl(file: string): Promise<string>;

  // ===== 写 =====
  /** 写仓库内任意文本文件（原子写 + 自动建父目录）。 */
  writeFile(file: string, content: string): Promise<void>;
  /** 写 .md 笔记（原子写，自动建父目录）。 */
  writeNote(file: string, content: string): Promise<void>;
  /** 写 .atlx 画布（整体原子写；title 变更即同目录改名）。返回写入后的 updatedAt（秒，展示用时间戳）。 */
  writeCanvas(canvas: CanvasFile, file: string): Promise<number>;
  /** 写 .atb 表格（整体原子写；语义同 writeCanvas）。 */
  writeTable(table: TableFile, file: string): Promise<number>;
  /** 新建空画布，返回 { id, file }（file = 相对仓库根路径；dir 空 = 根目录）。 */
  createCanvas(title: string, dir: string): Promise<CanvasCreateResult>;
  /** 新建空表格，返回 { id, file }。 */
  createTable(title: string, dir: string): Promise<TableCreateResult>;

  // ===== 增量补丁 =====
  /** 画布增量补丁按稳定 id 合并落盘；返回写入后的 { updatedAt, file }（file = 落盘后的相对路径，改名漂移用）。 */
  patchCanvas(
    patch: CanvasPatch,
    file: string,
  ): Promise<{ updatedAt: number; file: string } | null>;
  /** 表格增量补丁按稳定 id 合并落盘（field/row 全序重排随补丁携带）；返回同 patchCanvas。 */
  patchTable(
    patch: TablePatch,
    file: string,
  ): Promise<{ updatedAt: number; file: string } | null>;

  // ===== 结构变更 =====
  /** 重命名 .md 笔记 + 扫描画布更新引用；返回被改写 `.md` 清单（内部链接归一化）。 */
  renameNote(oldFile: string, newFile: string): Promise<LinkRewriteResult>;
  /** 重命名画布（更新 .atlx 内 title + 同目录重命名文件）。 */
  renameCanvas(file: string, newTitle: string): Promise<void>;
  /** 移动画布文件到新路径（跨目录）。 */
  moveCanvas(oldFile: string, newFile: string): Promise<void>;
  /** 重命名表格（更新 .atb 内 title + 同目录改文件名 + 同步画布引用）。 */
  renameTable(file: string, newTitle: string): Promise<void>;
  /** 移动表格文件到新路径（跨目录 + 同步画布引用）。 */
  moveTable(oldFile: string, newFile: string): Promise<void>;
  /** 重命名附件 + 扫描画布更新 media 节点引用。 */
  renameAttachment(oldFile: string, newFile: string): Promise<void>;
  /** 重命名文件夹（移动整个目录 + 改写画布内位于该目录下文件的引用）。 */
  renameFolder(oldDir: string, newDir: string): Promise<LinkRewriteResult>;
  /** 删除 .md 笔记（不更新画布引用）。 */
  deleteNote(file: string): Promise<void>;
  /** 删除附件（不更新画布引用）。 */
  deleteAttachment(file: string): Promise<void>;
  /** 删除画布文件（不删笔记/附件，文件可跨画布共享）。 */
  deleteCanvas(file: string): Promise<void>;
  /** 删除表格文件（不更新画布引用，节点断链降级）。 */
  deleteTable(file: string): Promise<void>;
  /** 删除文件夹；force=false 空目录直接删，非空返回 needsConfirm 供弹窗，确认后 force=true 递归删。 */
  deleteFolder(dir: string, force: boolean): Promise<DeleteFolderResult>;
  /** 新建文件夹（自动建父目录），返回相对路径。 */
  createFolder(dir: string): Promise<string>;
  /** 复制仓库内文件为同目录副本（纯字节复制；`.atlx`/`.atb` 的 id 由后端重新生成）。 */
  copyFile(oldFile: string, newFile: string): Promise<void>;
  /** 复制文件夹为同父目录副本（递归复制全部内容）。 */
  copyFolder(oldDir: string, newDir: string): Promise<void>;
  /** 一键重建内部链接：全仓库 .md 统一规范为 `[名](基于仓库的路径)`。 */
  rebuildLinks(): Promise<RebuildLinksResult>;

  // ===== 历史 =====
  /** 迁移单文件的全部历史侧文件到新编码路径（重命名/移动后调用；源不存在静默跳过）。 */
  remapSideloads(oldFile: string, newFile: string): Promise<void>;
  /** 迁移某文件夹下全部历史侧文件到新目录前缀（语义同 remapSideloads）。 */
  remapSideloadsByDir(oldDir: string, newDir: string): Promise<void>;

  // ===== 附件 =====
  /** 附件字节写入未入库临时区，返回仓库相对路径引用。 */
  writeTempAttachment(canvasId: string, fileName: string, base64Data: string): Promise<string>;
  /** 把未入库附件复制进仓库附件文件夹，返回仓库相对路径。 */
  importAttachment(rel: string, fileName: string): Promise<{ file: string }>;
  /** 把本机图片（base64 字节 + 文件名）落为表格附件，返回唯一相对路径供单元格引用。 */
  importTableImage(image: TableImageSource, tableId: string): Promise<string>;
  /** 按引用回收某画布的未入库附件，返回删除文件数。 */
  cleanupCanvasTempAttachments(canvasId: string, canvasFile: string): Promise<number>;
  /** 回收表格孤儿图片附件，返回删除文件数。 */
  cleanupTableAttachments(file: string): Promise<number>;

  // ===== 索引 =====
  /** 查询反链（`[[笔记名]]` 或 `[label](路径)` 两种写法）。 */
  scanBacklinks(noteName: string, noteFile: string): Promise<BacklinkRow[]>;
  /** 全仓库标签词汇表（frontmatter `tags` + 正文内联 `#标签`）。 */
  scanTags(): Promise<TagRow[]>;
  /** 按 glob 模式枚举文件路径（只返回文件；上限内联 + total）。 */
  glob(pattern: string, opts?: { path?: string }): Promise<GlobVaultResult>;
  /** 正则搜索文件内容，返回匹配行（含行号与路径；上限内联 + total）。 */
  grep(pattern: string, opts?: { path?: string; include?: string }): Promise<GrepVaultResult>;
  /** 仓库历史聚合（版本流 + 按日计数）。 */
  repoHistoryAggregate(): Promise<RepoHistoryResult>;

  // ===== 主页聚合 =====
  /** 扫描全仓 `.md` frontmatter 的 `date`/`due`（带日期笔记，自动进日历）。 */
  listDatedNotes(): Promise<DatedNote[]>;
}
