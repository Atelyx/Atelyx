/**
 * 插件平台 service：Rust `commands/plugin.rs` 的 invoke 封装。
 * 插件列表/安装/卸载/启停/更新/读入口/插件数据/默认组合播种都经这里，前端组件只经 store 触达。
 * 运行时（Cordis 内核/挂载器）在 `services/cordis`，不在此层。
 */
import { invoke } from "@tauri-apps/api/core";
import type { PluginPackageJson, PluginScope, PluginSourceKind, PluginType } from "@/types";

/** Rust `plugin_list` 返回行（原始清单由前端校验归一化）。 */
export interface PluginRow {
  id: string;
  name: string;
  version: string;
  type: PluginType;
  scope: PluginScope;
  /** 安装目录（空 = 实现随应用编译，无磁盘目录）。 */
  installDir: string;
  sourceKind: PluginSourceKind;
  enabled: boolean;
  manifest: PluginPackageJson;
}

/** 列出全部插件行（先按默认组合清单增量播种随应用分发的行，再列出磁盘包行）。 */
export function pluginList(defaults: PluginPackageJson[]): Promise<PluginRow[]> {
  return invoke<PluginRow[]>("plugin_list", { defaults });
}

/** 安装插件：来源为 GitHub `owner/repo`（市场）或完整 git 地址；新装一律停用（含替代同名行），
 *  由用户显式启用；只有更新才沿用原行启停状态。 */
export function pluginInstall(repo: string, scope: PluginScope): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_install", { repo, scope });
}

/** 从本地目录安装插件（junction/符号链接实时引用，源目录改动即时生效；新装同样一律停用）。 */
export function pluginInstallLocal(path: string, scope: PluginScope): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_install_local", { path, scope });
}

/** 卸载插件（删安装目录/链接 + 清理状态记录；无落位目录的行只清记录）。 */
export function pluginUninstall(id: string, scope: PluginScope): Promise<void> {
  return invoke("plugin_uninstall", { id, scope });
}

/** 启用/停用插件（前端先确认权限再启用）。 */
export function pluginSetEnabled(id: string, enabled: boolean): Promise<void> {
  return invoke("plugin_set_enabled", { id, enabled });
}

/** 恢复默认装配（补播种缺失的默认组合行；管理 UI「恢复默认装配」入口，调用后重载插件列表）。 */
export function pluginSeedDefault(entries: PluginPackageJson[]): Promise<void> {
  return invoke("plugin_seed_default", { entries });
}

/** 更新插件（备份 → 安装 → 失败回滚）。 */
export function pluginUpdate(id: string): Promise<PluginRow> {
  return invoke<PluginRow>("plugin_update", { id });
}

/** 读取插件入口源码（path 缺省 = 清单 main）。 */
export function pluginReadEntry(id: string, path?: string): Promise<string> {
  return invoke<string>("plugin_read_entry", { id, path });
}

/** 读取插件自持数据（单 JSON 对象）。 */
export function pluginReadState(id: string): Promise<unknown> {
  return invoke<unknown>("plugin_read_state", { id });
}

/** 写入插件自持数据（原子写）。 */
export function pluginWriteState(id: string, data: unknown): Promise<void> {
  return invoke("plugin_write_state", { id, data });
}

/** 读取插件键值存储（单 JSON 对象，独立于 state.json）。 */
export function pluginKvRead(id: string): Promise<Record<string, unknown>> {
  return invoke<Record<string, unknown>>("plugin_kv_read", { id });
}

/** 写插件键值存储的一个键（Rust 侧读改写串行，调用方无需整表往返）。 */
export function pluginKvSet(id: string, key: string, value: unknown): Promise<void> {
  return invoke("plugin_kv_set", { id, key, value });
}

/** 删插件键值存储的一个键（不存在 = no-op）。 */
export function pluginKvDelete(id: string, key: string): Promise<void> {
  return invoke("plugin_kv_delete", { id, key });
}

/** 整表覆盖插件键值存储（ctx.storage.clear 用）。 */
export function pluginKvWrite(id: string, data: Record<string, unknown>): Promise<void> {
  return invoke("plugin_kv_write", { id, data });
}
