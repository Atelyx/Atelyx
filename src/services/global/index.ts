/**
 * 全局配置 service。
 *
 * 读写 `app_data_dir/global.json`，对应 Rust `commands/global.rs`。
 * 承载：最近打开仓库列表 + 自动更新开关 + 应用级界面外观（主题插件激活 + 主题设置 themeSettings +
 * 字号/字体）+ 自动恢复上次打开文件（AI 供应商/搜索源等仓库级配置走各仓库 `.atelyx/config.json`；
 * 应用级 UI 使用状态走 `services/layout` 的 Rust 迷你窗口管理器，见 `layout.rs`）。
 * recentVaults 的截断上限在此层维护（去重/归一化在 Rust 读写路径完成）。
 *
 * **写入走补丁命令**：`updateGlobalConfig` → Rust `patch_global_config`（锁内读-合并-原子写）。
 * global.json 由 appStore（recentVaults/自动更新开关/最近空间）与 settingsStore（界面外观/自动恢复/
 * 协作配置）共同写入，合并必须在后端单点完成——跨窗口（主/撕裂窗口各有独立 webview）前端
 * 自己做 read-modify-write 会互相覆盖丢字段。
 */
import { invoke } from "@tauri-apps/api/core";
import type { GlobalConfig, GlobalConfigRead, RecentSpace, RecentVault, VaultInfo } from "@/types";

const MAX_RECENT_VAULTS = 10;

/** 读全局配置（文件不存在返回空配置）。`corruptBackup` 非空 = 原文损坏已备份为磁盘上该文件名。 */
export async function readGlobalConfig(): Promise<GlobalConfigRead> {
  return invoke<GlobalConfigRead>("read_global_config");
}

/** 获取本机设备名（协作身份默认值）。 */
export async function getHostname(): Promise<string> {
  return invoke<string>("get_hostname");
}

/**
 * 把某仓库登记为最近打开：去重（按 root）+ 置顶 + 更新时间 + 截断上限。
 * 返回更新后的 recentVaults（调用方负责落盘）。
 */
export function bumpRecentVault(
  recentVaults: RecentVault[],
  info: VaultInfo,
  nowSec: number,
): RecentVault[] {
  const filtered = recentVaults.filter((v) => v.root !== info.root);
  return [{ root: info.root, name: info.name, lastOpenedAt: nowSec }, ...filtered].slice(
    0,
    MAX_RECENT_VAULTS,
  );
}

/** 从最近列表移除某仓库（按 root）。返回更新后的列表。 */
export function removeRecentVault(recentVaults: RecentVault[], root: string): RecentVault[] {
  return recentVaults.filter((v) => v.root !== root);
}

/** 协作空间仓库条目的稳定去重键（serverUrl + spaceId）。 */
export function spaceKey(serverUrl: string, spaceId: string): string {
  return `${serverUrl}#${spaceId}`;
}

const MAX_RECENT_SPACES = 10;

/**
 * 把某协作空间仓库登记为最近打开：去重（按 `serverUrl#spaceId` 键）+ 置顶 + 更新时间 + 截断上限。
 * 返回更新后的 spaces 列表（调用方负责落盘）。与 bumpRecentVault 同口径。
 */
export function bumpRecentSpace(
  recentSpaces: RecentSpace[],
  entry: RecentSpace,
  nowSec: number,
): RecentSpace[] {
  const key = spaceKey(entry.serverUrl, entry.spaceId);
  const filtered = recentSpaces.filter((s) => spaceKey(s.serverUrl, s.spaceId) !== key);
  return [
    { serverUrl: entry.serverUrl, spaceId: entry.spaceId, name: entry.name, openedAt: nowSec },
    ...filtered,
  ].slice(0, MAX_RECENT_SPACES);
}

/** 从最近空间列表移除某条目（按 `serverUrl#spaceId` 键）。返回更新后的列表。 */
export function removeRecentSpace(recentSpaces: RecentSpace[], key: string): RecentSpace[] {
  return recentSpaces.filter((s) => spaceKey(s.serverUrl, s.spaceId) !== key);
}

/**
 * 补丁写全局配置：补丁直接交 Rust `patch_global_config`，读盘 → 顶层合并 → 原子写在后端
 * 互斥完成。补丁按顶层字段整体替换（`null` 删除该键；值为 `undefined` 的键经 JSON 序列化
 * 自然缺席、不影响存量字段），与各调用方「整对象提交子字段」的用法一致。
 * 返回本次读到的损坏备份文件名（`null` = 无损坏）：损坏时按空配置 + 补丁写回，等于把
 * 其余字段重置，调用方据此提示用户（必须可见）。
 */
export async function updateGlobalConfig(
  patch: { [K in keyof GlobalConfig]?: GlobalConfig[K] | null },
): Promise<string | null> {
  const { corruptBackup } = await invoke<GlobalConfigRead>("patch_global_config", { patch });
  return corruptBackup;
}
