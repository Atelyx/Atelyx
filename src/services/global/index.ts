/**
 * 全局配置 service。
 *
 * 读写 `app_data_dir/global.json`，对应 Rust `commands/global.rs`。
 * 承载：最近打开仓库列表 + 自动更新开关 + 应用级界面外观（主题插件激活 + 主题设置 themeSettings +
 * 字号/字体）+ 自动恢复上次打开文件（AI 供应商/搜索源等仓库级配置走各仓库 `.atelyx/config.json`；
 * 应用级 UI 使用状态走 `services/layout` 的 Rust 迷你窗口管理器，见 `layout.rs`）。
 * recentVaults 的去重/排序/截断逻辑在此层维护（Rust 只做整文件读写）。
 *
 * **写入走 `updateGlobalConfig`（read-modify-write + 串行化）**，勿直接 `writeGlobalConfig`：
 * global.json 现由 appStore（recentVaults/自动更新开关）与 settingsStore（界面外观/自动恢复）共同写入，
 * 直接整体写会覆盖对方字段。`updateGlobalConfig` 只 patch 顶层字段，且通过模块级
 * promise 链串行化，避免并发 read-modify-write 丢更新。
 */
import { invoke } from "@tauri-apps/api/core";
import type { GlobalConfig, GlobalConfigRead, RecentVault, VaultInfo } from "@/types";

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
 * 写全局配置（原子写，整体覆盖）。
 * 内部用：调用方应改用 `updateGlobalConfig` 做 read-modify-write，避免覆盖其他字段。
 */
async function writeGlobalConfig(config: GlobalConfig): Promise<void> {
  await invoke("write_global_config", { config });
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

// ===== 写串行化 =====
// 模块级 promise 链：让并发 updateGlobalConfig 互斥执行 read-modify-write，
// 避免两个调用同时 read → 都基于旧值 merge → 后写者覆盖先写者的 patch。
let writeChain: Promise<unknown> = Promise.resolve();
function serialized<T>(fn: () => Promise<T>): Promise<T> {
  const run = writeChain.then(fn);
  // 链不因单次失败断裂：吞掉 reject 让后续调用仍能排队
  writeChain = run.then(
    () => undefined,
    () => undefined,
  );
  return run;
}

/**
 * Read-modify-write global.json：读当前值 → 浅合并 patch 顶层字段 → 写回。
 * 串行化保证并发调用不丢更新。`patch.theme` / `patch.recentVaults`
 * 整体替换对应字段（不做深合并），调用方传完整子对象。
 * 返回本次读到的损坏备份文件名（`null` = 无损坏）：调用方据此提示用户
 * （读到的空配置会与 patch 一起写回，等于把其余字段重置，必须可见）。
 */
export async function updateGlobalConfig(patch: Partial<GlobalConfig>): Promise<string | null> {
  return serialized(async () => {
    const { config: current, corruptBackup } = await readGlobalConfig();
    const merged: GlobalConfig = { ...current, ...patch };
    await writeGlobalConfig(merged);
    return corruptBackup;
  });
}

