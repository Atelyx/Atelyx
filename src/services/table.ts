/**
 * 多维表格（.atb）文件读写 service。
 *
 * 内容 I/O 经内容面按激活仓库取后端（services/content）；类型对齐 `types/table.ts`。
 * 重命名/移动由后端扫描所有 .atlx 同步 table 节点 file 引用（链接维护）。
 */
import { invoke } from "@tauri-apps/api/core";
import { computeTablePatch } from "@/utils/table";
import { getActiveContentBackend } from "@/services/content/factory";
import type { TableImageSource } from "@/services/content/contract";
import type { TableCreateResult, TableField, TableFile, TableRow } from "@/types";

/** 新建空表格（自带一个「名称」文本字段），返回 { id, file }（dir 空 = 根目录）。 */
export async function createTableVault(title: string, dir: string): Promise<TableCreateResult> {
  return getActiveContentBackend().createTable(title, dir);
}

/** 读 .atb 文件（按相对仓库根路径）。行归一化（图片单元格旧形态 → 内存形态）在后端读路径完成。 */
export async function readTableVault(file: string): Promise<TableFile> {
  return getActiveContentBackend().readTable(file);
}

/** 写 .atb 文件（原子写；title 变更自动改文件名并同步画布引用）。
 * 返回写入后的 updatedAt（秒，展示用时间戳）。 */
export async function writeTableVault(table: TableFile, file: string): Promise<number> {
  return getActiveContentBackend().writeTable(table, file);
}

/** 增量保存基线快照（与当前运行时状态按引用 diff：未变实体引用相同，O(N) 指针比对无深比较）。 */
export interface TableSaveSnapshot {
  fields: TableField[];
  rows: TableRow[];
}

/**
 * 增量保存 .atb（自动保存主路径）：与上次保存快照按引用 diff，只序列化变化/新增/删除的
 * 字段与行，经 `patch_table_vault` 按稳定 id 合并到磁盘全量文件——image dataURL 大字段不重传；
 * 顺序变化（排序/插列）经 `fieldOrder`/`rowOrder` 携带。
 * 空补丁返回 null——调用方跳过 IPC（磁盘已一致）。
 * 返回写入后的 { updatedAt, file }（title 变更重命名时 file = 新相对路径）。
 */
export async function patchTableVault(opts: {
  file: string;
  tableId: string;
  fields: TableField[];
  rows: TableRow[];
  lastSaved: TableSaveSnapshot;
}): Promise<{ updatedAt: number; file: string } | null> {
  const { file, tableId, fields, rows, lastSaved } = opts;
  // diff 计算与协作实时广播共用同一纯函数（顺序变化也在此捕获，见 utils/table.ts）
  const patch = computeTablePatch({ tableId, fields, rows, lastSaved });
  if (!patch) return null;
  return getActiveContentBackend().patchTable(patch, file);
}

/** 重命名表格（更新 .atb 内 title + 同目录改文件名 + 同步画布 table 节点引用）。 */
export async function renameTableVault(file: string, newTitle: string): Promise<void> {
  await getActiveContentBackend().renameTable(file, newTitle);
}

/** 移动表格文件到新路径（跨目录 + 同步画布 table 节点引用）。 */
export async function moveTableVault(oldFile: string, newFile: string): Promise<void> {
  await getActiveContentBackend().moveTable(oldFile, newFile);
}

/** 删除表格文件（不更新 .atlx 引用，画布 table 节点断链降级）。 */
export async function deleteTableVault(file: string): Promise<void> {
  await getActiveContentBackend().deleteTable(file);
}

/**
 * 把本机图片（前端读为 base64 的字节 + 文件名）落为表格附件，返回唯一相对路径供单元格引用
 * （每次导入新文件，删除后重导不覆盖旧文件、不撞显示缓存）。
 * 字节由前端读出后传输：协作空间里本机路径对服务端不可达，个人仓库与空间共用同一形状。
 */
export async function importTableImage(
  image: TableImageSource,
  tableId: string,
): Promise<string> {
  return getActiveContentBackend().importTableImage(image, tableId);
}

/**
 * 回收表格孤儿图片附件（切表/关闭表格时 fire-and-forget 调用）：删除附件目录中
 * 未被 .atb 任一 image 单元格引用的文件。会话内不调用——删除后 Ctrl+Z 可恢复引用；
 * 切表/关闭时该表撤销栈与显示缓存已清，无跨会话恢复路径。返回删除文件数。
 */
export async function cleanupTableAttachments(file: string): Promise<number> {
  return getActiveContentBackend().cleanupTableAttachments(file);
}

/** 保存 dataURL 图片到系统 Downloads 文件夹（放大预览右键「下载」用；重名自动加序号）。 */
export async function saveImageToDownloads(fileName: string, dataUrl: string): Promise<void> {
  await invoke("save_image_to_downloads", { fileName, dataUrl });
}

/** 导出表格为 .xlsx（目标路径来自系统保存对话框）。 */
export async function exportTableXlsx(table: TableFile, targetPath: string): Promise<void> {
  await invoke("export_table_xlsx", { table, targetPath });
}
