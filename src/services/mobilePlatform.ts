/**
 * 移动端平台能力 service（安卓本地仓库的系统交互）。
 *
 * 命令对应 `src-tauri/src/commands/mobile.rs`，桌面端一律返回可读错误：
 * 桌面选目录走系统原生弹窗、仓库根由用户直接指定，没有这些交互。
 * 调用点只在 `stores/mobileVaultStore`（组件层不直连 services，见分层约束）与 `services/shell`。
 */
import { invoke } from "@tauri-apps/api/core";
import type { AbsoluteDirListing } from "@/types";

/** 「所有文件访问权限」（MANAGE_EXTERNAL_STORAGE）是否已授予。 */
export async function hasAllFilesAccess(): Promise<boolean> {
  return invoke<boolean>("android_has_all_files_access");
}

/** 拉起系统设置页申请「所有文件访问权限」（拉起即返回，需用户手动开启开关）。 */
export async function requestAllFilesAccess(): Promise<void> {
  return invoke<void>("android_request_all_files_access");
}

/** 用系统默认程序打开外部 URL（安卓无 shell 插件，走原生 Intent）。 */
export async function openExternalUrl(url: string): Promise<void> {
  return invoke<void>("android_open_url", { url });
}

/** 应用内部私有目录下的回落仓库根（不存在则创建）。 */
export async function privateVaultPath(): Promise<string> {
  return invoke<string>("android_private_vault_path");
}

/** 外部存储根目录（已授予「所有文件访问权限」时的目录浏览起点）。 */
export async function storageRoot(): Promise<string> {
  return invoke<string>("android_storage_root");
}

/** 列出任意绝对路径下的子目录（自研目录浏览的数据源）。 */
export async function listAbsoluteDir(path: string): Promise<AbsoluteDirListing> {
  return invoke<AbsoluteDirListing>("list_absolute_dir", { path });
}
