/**
 * 仓库外文件命令的 invoke 封装（`ctx.fs` 的 Rust 调用面）：作用域为任意绝对路径。
 * 插件调用边界只做上下文校验（`services/cordis/kernel.ts` 的 `fs` 服务按当前 fiber 取调用方插件 id，非插件上下文同步拒绝）；
 * id 不进命令参数，调用归属由 audit 层记录。
 */
import { invoke } from "@tauri-apps/api/core";
import type { ListDirResult } from "@/types";

/** 读仓库外文件全文（非 UTF-8 返回替换字符容错）。 */
export function externalReadFile(path: string): Promise<string> {
  return invoke<string>("external_read_file", { path });
}

/** 写仓库外文件（原子写 + 补齐缺失的父目录）。 */
export function externalWriteFile(path: string, content: string): Promise<void> {
  return invoke("external_write_file", { path, content });
}

/** 单层列出仓库外目录条目（目录在前、按名称升序；含 `.` 开头项）。 */
export function externalListDir(path: string): Promise<ListDirResult> {
  return invoke<ListDirResult>("external_list_dir", { path });
}

/** 创建仓库外目录（补齐父目录；已存在且是目录 = 成功）。 */
export function externalCreateFolder(path: string): Promise<void> {
  return invoke("external_create_folder", { path });
}

/** 仓库外文件同目录重命名；返回实际落盘路径（绝对）。 */
export function externalRenameFile(path: string, newName: string): Promise<string> {
  return invoke<string>("external_rename_file", { path, newName });
}

/** 把仓库外文件移动到另一目录；返回实际落盘路径（绝对）。 */
export function externalMoveFile(path: string, targetDir: string): Promise<string> {
  return invoke<string>("external_move_file", { path, targetDir });
}

/** 删除仓库外文件（目录误传拒绝）。 */
export function externalDeleteFile(path: string): Promise<void> {
  return invoke("external_delete_file", { path });
}

/** 删除仓库外目录结果（非空且未 force = needsConfirm，不删除）。 */
export interface ExternalDeleteDirResult {
  deleted: boolean;
  needsConfirm: boolean;
  itemCount: number;
}

/** 删除仓库外目录（非空且未 force 时返回 needsConfirm）。 */
export function externalDeleteDir(path: string, force: boolean): Promise<ExternalDeleteDirResult> {
  return invoke<ExternalDeleteDirResult>("external_delete_dir", { path, force });
}

/** 返回当前插件的私有文件目录绝对路径（不存在则创建）；私有目录在插件 data/files 下，
 *  随插件卸载清除、更新保留。 */
export function externalPrivateDir(pluginId: string): Promise<string> {
  return invoke<string>("external_private_dir", { pluginId });
}

/** 写仓库外文件（字节形态，base64 进出；原子写 + 补齐缺失的父目录）。 */
export function externalWriteFileBase64(path: string, base64Data: string): Promise<void> {
  return invoke("external_write_file_base64", { path, base64Data });
}

/** 读仓库外文件为 dataURL（mime 按扩展名推断，未知扩展名 = application/octet-stream）。 */
export function externalReadFileDataUrl(path: string): Promise<string> {
  return invoke<string>("external_read_file_data_url", { path });
}
