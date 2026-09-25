/**
 * 仓库外授权目录文件命令的 invoke 封装（`ctx.fs` 的 Rust 调用面）。
 * `pluginId` 由内核按当前 fiber 归属传入（见 services/cordis/kernel.ts 的 `fs` 服务）——
 * 插件代码不传、看不到它，无法指定他人命名空间。
 */
import { invoke } from "@tauri-apps/api/core";
import type { ListDirResult } from "@/types";

/** 读仓库外授权文件全文（非 UTF-8 返回替换字符容错）。 */
export function externalReadFile(pluginId: string, path: string): Promise<string> {
  return invoke<string>("external_read_file", { pluginId, path });
}

/** 写仓库外授权文件（原子写 + 补齐授权根内缺失的父目录）。 */
export function externalWriteFile(pluginId: string, path: string, content: string): Promise<void> {
  return invoke("external_write_file", { pluginId, path, content });
}

/** 单层列出仓库外授权目录条目（目录在前、按名称升序；含 `.` 开头项）。 */
export function externalListDir(pluginId: string, path: string): Promise<ListDirResult> {
  return invoke<ListDirResult>("external_list_dir", { pluginId, path });
}

/** 创建仓库外授权目录（补齐父目录；已存在且是目录 = 成功）。 */
export function externalCreateFolder(pluginId: string, path: string): Promise<void> {
  return invoke("external_create_folder", { pluginId, path });
}

/** 仓库外授权文件同目录重命名；返回实际落盘路径（绝对）。 */
export function externalRenameFile(pluginId: string, path: string, newName: string): Promise<string> {
  return invoke<string>("external_rename_file", { pluginId, path, newName });
}

/** 把仓库外授权文件移动到另一授权目录；返回实际落盘路径（绝对）。 */
export function externalMoveFile(pluginId: string, path: string, targetDir: string): Promise<string> {
  return invoke<string>("external_move_file", { pluginId, path, targetDir });
}

/** 删除仓库外授权文件（目录误传拒绝）。 */
export function externalDeleteFile(pluginId: string, path: string): Promise<void> {
  return invoke("external_delete_file", { pluginId, path });
}

/** 删除仓库外授权目录结果（非空且未 force = needsConfirm，不删除）。 */
export interface ExternalDeleteDirResult {
  deleted: boolean;
  needsConfirm: boolean;
  itemCount: number;
}

/** 删除仓库外授权目录（非空且未 force 时返回 needsConfirm）。 */
export function externalDeleteDir(pluginId: string, path: string, force: boolean): Promise<ExternalDeleteDirResult> {
  return invoke<ExternalDeleteDirResult>("external_delete_dir", { pluginId, path, force });
}

/** 返回当前插件的私有文件目录绝对路径（不存在则创建）；私有目录在插件 data/files 下，
 *  授权目录之外无条件可达，其余 fs 命令把它并入安全根即可读写。 */
export function externalPrivateDir(pluginId: string): Promise<string> {
  return invoke<string>("external_private_dir", { pluginId });
}

/** 写仓库外授权文件（字节形态，base64 进出；原子写 + 补齐授权根内缺失的父目录）。 */
export function externalWriteFileBase64(pluginId: string, path: string, base64Data: string): Promise<void> {
  return invoke("external_write_file_base64", { pluginId, path, base64Data });
}

/** 读仓库外授权文件为 dataURL（mime 按扩展名推断，未知扩展名 = application/octet-stream）。 */
export function externalReadFileDataUrl(pluginId: string, path: string): Promise<string> {
  return invoke<string>("external_read_file_data_url", { pluginId, path });
}
