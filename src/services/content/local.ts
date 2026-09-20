/**
 * 个人仓库内容后端：契约方法 → 本地 Tauri 命令的逐项委托。
 *
 * root 无关——当前仓库根由 Rust 侧 VaultState 持有，命令不带 root 参数；
 * 仓库切换 = open_vault 换 root，本后端对象跨仓库复用。
 * 命令对应 `src-tauri/src/commands/{vault,table,filesearch,temp_attachment,home}.rs`。
 */
import { invoke } from "@tauri-apps/api/core";
import { READ_WINDOW_DEFAULT_LINES } from "@/constants/tools";
import { normalizeTableRow } from "@/utils/table";
import type {
  BacklinkRow,
  CanvasCreateResult,
  CanvasFile,
  CanvasFileRow,
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
  TagRow,
} from "@/types";
import type { ContentBackend, TableImageSource } from "./contract";

export const localBackend: ContentBackend = {
  // ===== 树与列举 =====
  listTree: () => invoke<FileTreeNode[]>("list_vault_tree"),
  listDir: (dir) => invoke<ListDirResult>("list_vault_dir", { dir: dir ?? "" }),

  // ===== 读 =====
  readFile: (file) => invoke<string>("read_vault_file", { file }),
  readFileWindow: (file, opts) =>
    invoke<ReadWindowResult>("read_vault_file_window", {
      file,
      offset: opts?.offset ?? 1,
      limit: opts?.limit ?? READ_WINDOW_DEFAULT_LINES,
    }),
  readNote: (file) => invoke<string>("read_note", { file }),
  readCanvas: (file) => invoke<CanvasFile>("read_canvas_vault", { file }),
  async readTable(file) {
    const t = await invoke<TableFile>("read_table_vault", { file });
    // 磁盘→内存唯一咽喉：图片单元格旧形态 string[] 归一化（内存/历史比对恒为新形态）
    return { ...t, rows: t.rows.map(normalizeTableRow) };
  },
  listCanvases: () => invoke<CanvasFileRow[]>("list_canvases_vault"),
  readAttachmentDataUrl: (file) => invoke<string>("read_attachment_data_url", { file }),

  // ===== 写 =====
  writeFile: (file, content) => invoke("write_vault_file", { file, content }).then(() => undefined),
  writeNote: (file, content) => invoke("write_note", { file, content }).then(() => undefined),
  writeCanvas: (canvas, file, baseUpdatedAt) =>
    invoke<number>("write_canvas_vault", { canvas, file, baseUpdatedAt }),
  writeTable: (table, file, baseUpdatedAt) =>
    invoke<number>("write_table_vault", { table, file, baseUpdatedAt }),
  createCanvas: (title, dir) => invoke<CanvasCreateResult>("create_canvas_vault", { title, dir }),
  createTable: (title, dir) => invoke<TableCreateResult>("create_table_vault", { title, dir }),

  // ===== 增量补丁 =====
  patchCanvas: (patch, file, baseUpdatedAt) =>
    invoke<{ updatedAt: number; file: string } | null>("patch_canvas_vault", {
      patch,
      file,
      baseUpdatedAt,
    }),
  patchTable: (patch, file, baseUpdatedAt, force) =>
    invoke<{ updatedAt: number; file: string } | null>("patch_table_vault", {
      patch,
      file,
      baseUpdatedAt,
      force,
    }),

  // ===== 结构变更 =====
  renameNote: (oldFile, newFile) => invoke<LinkRewriteResult>("rename_note", { oldFile, newFile }),
  async renameCanvas(file, newTitle) {
    await invoke("rename_canvas_vault", { file, newTitle });
  },
  async moveCanvas(oldFile, newFile) {
    await invoke("move_canvas_vault", { oldFile, newFile });
  },
  async renameTable(file, newTitle) {
    await invoke("rename_table_vault", { file, newTitle });
  },
  async moveTable(oldFile, newFile) {
    await invoke("move_table_vault", { oldFile, newFile });
  },
  async renameAttachment(oldFile, newFile) {
    await invoke("rename_attachment", { oldFile, newFile });
  },
  renameFolder: (oldDir, newDir) => invoke<LinkRewriteResult>("rename_folder", { oldDir, newDir }),
  async deleteNote(file) {
    await invoke("delete_note", { file });
  },
  async deleteAttachment(file) {
    await invoke("delete_attachment", { file });
  },
  async deleteCanvas(file) {
    await invoke("delete_canvas_vault", { file });
  },
  async deleteTable(file) {
    await invoke("delete_table_vault", { file });
  },
  deleteFolder: (dir, force) => invoke<DeleteFolderResult>("delete_folder", { dir, force }),
  createFolder: (dir) => invoke<string>("create_folder", { dir }),
  async copyFile(oldFile, newFile) {
    await invoke("copy_vault_file", { oldFile, newFile });
  },
  async copyFolder(oldDir, newDir) {
    await invoke("copy_vault_folder", { oldDir, newDir });
  },
  rebuildLinks: () => invoke<RebuildLinksResult>("rebuild_internal_links"),

  // ===== 历史 =====
  async remapSideloads(oldFile, newFile) {
    await invoke("remap_sideloads", { oldFile, newFile });
  },
  async remapSideloadsByDir(oldDir, newDir) {
    await invoke("remap_sideloads_by_dir", { oldDir, newDir });
  },

  // ===== 附件 =====
  writeTempAttachment: (canvasId, fileName, base64Data) =>
    invoke<string>("write_temp_attachment", { canvasId, fileName, base64Data }),
  importAttachment: (rel, fileName) =>
    invoke<{ file: string }>("import_vault_attachment", { rel, fileName }),
  importTableImage: (image: TableImageSource, tableId) =>
    invoke<string>("import_table_image_vault", {
      fileName: image.fileName,
      data: image.base64Data,
      tableId,
    }),
  cleanupCanvasTempAttachments: (canvasId, canvasFile) =>
    invoke<number>("cleanup_canvas_temp_attachments", { canvasId, canvasFile }),
  cleanupTableAttachments: (file) => invoke<number>("cleanup_table_attachments_vault", { file }),

  // ===== 索引 =====
  scanBacklinks: (noteName, noteFile) =>
    invoke<BacklinkRow[]>("scan_wiki_backlinks", { noteName, noteFile }),
  scanTags: () => invoke<TagRow[]>("scan_vault_tags"),
  glob: (pattern, opts) => invoke<GlobVaultResult>("glob_vault", { pattern, path: opts?.path }),
  grep: (pattern, opts) =>
    invoke<GrepVaultResult>("grep_vault", {
      pattern,
      path: opts?.path,
      include: opts?.include,
    }),
  repoHistoryAggregate: () => invoke<RepoHistoryResult>("list_repo_history"),
};
